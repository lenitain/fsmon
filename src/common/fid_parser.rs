use crate::common::EventType;
use crate::common::filters::PathOptions;
use anyhow::{Context, Result};
use fanotify_fid::consts::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE,
    FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR, FAN_MARK_ADD, FAN_MODIFY, FAN_MOVE_SELF,
    FAN_MOVED_FROM, FAN_MOVED_TO, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC,
};
use fanotify_fid::prelude::*;
use fanotify_fid::types::{FidEvent, HandleKey};
use libc;
use moka::sync::Cache;
use std::collections::VecDeque;
use std::ffi::CString;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

// ---- FanFd wrapper for AsyncFd ----

/// Newtype wrapper around a raw fanotify file descriptor.
/// Implements `AsRawFd` and `AsFd` so it can be used with `AsyncFd`.
pub struct FanFd(pub RawFd);

impl std::fmt::Debug for FanFd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FanFd").field(&self.0).finish()
    }
}

impl AsRawFd for FanFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

// ---- FsGroup: one per unique filesystem ----

/// A group of fds for a single filesystem.
/// One fanotify fd + one directory fd per filesystem, shared by all paths on it.
pub struct FsGroup {
    pub dev_id: u64,
    pub fan_fd: OwnedFd,
    pub mount_fd: OwnedFd,
    pub ref_count: usize,
}

impl std::fmt::Debug for FsGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsGroup")
            .field("dev_id", &self.dev_id)
            .field("ref_count", &self.ref_count)
            .finish()
    }
}

/// Convert an EventType to its fanotify kernel flag.
pub fn event_type_to_kernel_flag(t: &EventType) -> u64 {
    match t {
        EventType::Access => FAN_ACCESS,
        EventType::Modify => FAN_MODIFY,
        EventType::CloseWrite => FAN_CLOSE_WRITE,
        EventType::CloseNowrite => FAN_CLOSE_NOWRITE,
        EventType::Open => FAN_OPEN,
        EventType::OpenExec => FAN_OPEN_EXEC,
        EventType::Attrib => FAN_ATTRIB,
        EventType::Create => FAN_CREATE,
        EventType::Delete => FAN_DELETE,
        EventType::DeleteSelf => FAN_DELETE_SELF,
        EventType::MovedFrom => FAN_MOVED_FROM,
        EventType::MovedTo => FAN_MOVED_TO,
        EventType::MoveSelf => FAN_MOVE_SELF,
        EventType::FsError => FAN_FS_ERROR,
    }
}

/// Build kernel mask from PathOptions: explicit types or default.
pub fn path_mask_from_options(opts: &PathOptions) -> u64 {
    match &opts.event_types {
        Some(types) if !types.is_empty() => {
            types.iter().fold(FAN_EVENT_ON_CHILD | FAN_ONDIR, |m, t| {
                m | event_type_to_kernel_flag(t)
            })
        }
        _ => DEFAULT_EVENT_MASK,
    }
}

/// Convert a fanotify event mask to fsmon's EventType enum.
pub fn mask_to_event_types(mask: u64) -> smallvec::SmallVec<[EventType; 8]> {
    const BITS: [(u64, EventType); 14] = [
        (FAN_ACCESS, EventType::Access),
        (FAN_MODIFY, EventType::Modify),
        (FAN_CLOSE_WRITE, EventType::CloseWrite),
        (FAN_CLOSE_NOWRITE, EventType::CloseNowrite),
        (FAN_OPEN, EventType::Open),
        (FAN_OPEN_EXEC, EventType::OpenExec),
        (FAN_ATTRIB, EventType::Attrib),
        (FAN_CREATE, EventType::Create),
        (FAN_DELETE, EventType::Delete),
        (FAN_DELETE_SELF, EventType::DeleteSelf),
        (FAN_MOVED_FROM, EventType::MovedFrom),
        (FAN_MOVED_TO, EventType::MovedTo),
        (FAN_MOVE_SELF, EventType::MoveSelf),
        (FAN_FS_ERROR, EventType::FsError),
    ];
    BITS.iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, t)| *t)
        .collect()
}


/// Read and parse FID events, using a moka cache for path recovery.
///
/// # Design
///
/// Path recovery uses a **three-tier priority chain**:
///   1. Local `handle_map` — batch-internal knowledge propagation
///   2. Persistent `dir_cache` — cross-batch knowledge (e.g. events from
///      previous reads that cached directory handles)
///   3. `resolve_file_handle` — direct syscall fallback
///
/// The tiers are ordered by freshness: local map is per-batch and always
/// up-to-date for the current read cycle; the persistent cache may be stale
/// but covers handles seen in previous cycles; the syscall always reflects
/// current filesystem state but fails for deleted objects.
///
/// # Why not just use the persistent cache for everything?
///
/// Previously, `dir_cache` was both updated and read within the same loop,
/// serving as both the cross-batch store AND the within-batch coordination
/// channel.  This broke down when `rm -rf DIR` deletes a directory and all
/// contents in one process: ALL events arrive in a single `read()`.  The
/// directory handle in child events is stale (ESTALE), and there was no
/// prior event to seed the cache.
///
/// The fix separates concerns:
/// - The **local `handle_map`** is built from resolved events in this batch.
///   If event A resolves path="/dir/file" via DFID_NAME, its `self_handle`
///   (the directory handle) is added to the local map.  Event B inside the
///   same directory finds it through the local map.  No persistent cache
///   needed for within-batch coordination.
/// - The **persistent `dir_cache`** only serves as long-term memory across
///   read cycles.  It is updated AFTER the local resolve loop completes.
pub fn read_fid_events_cached(
    fan_fd: &OwnedFd,
    mount_fds: &[OwnedFd],
    dir_cache: &Cache<HandleKey, PathBuf>,
    buf: &mut Vec<u8>,
) -> Vec<FidEvent> {
    let mut store = crate::common::dir_cache::MokaPathStore(dir_cache.clone());
    match fanotify_fid::read::read_fid_events(fan_fd, mount_fds, buf, Some(&mut store)) {
        Ok(events) => events,
        Err(FanotifyError::Read(code)) if code == libc::EAGAIN => Vec::new(),
        Err(err) => {
            eprintln!("[WARNING] fanotify read error on fd {}: {err}", fan_fd.as_raw_fd());
            Vec::new()
        }
    }
}

// ---- Constants ----

/// Capacity for the moka directory handle cache (path→handle key reverse lookup).
/// 100k covers ~10s of thousands of directories with room to spare.
/// moka uses W-TinyLFU eviction when this limit is reached.
pub const DIR_CACHE_CAP: u64 = 100_000;

/// TTL for directory handle cache entries.
/// After 1 hour of no access, entries are automatically evicted.
/// This prevents stale entries when directories are deleted/renamed.
pub const DIR_CACHE_TTL_SECS: u64 = 3600;

pub const FILE_SIZE_CACHE_CAP: usize = 10_000;

/// Default mask: 8 core events (FS_ERROR excluded — only works with FS marks).
/// Use --types all to get all 14 (FS_ERROR included, but only effective on FS marks).
pub const DEFAULT_EVENT_MASK: u64 = FAN_CLOSE_WRITE
    | FAN_ATTRIB
    | FAN_CREATE
    | FAN_DELETE
    | FAN_DELETE_SELF
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_MOVE_SELF
    | FAN_EVENT_ON_CHILD
    | FAN_ONDIR;

/// Chown a file or directory to the original user (daemon runs as root).
/// Resolves the original user from SUDO_UID/SUDO_GID env vars.
///
/// Returns `Ok(true)` if chown succeeded, `Ok(false)` if the filesystem
/// does not support ownership changes (vfat/exfat/NFS no_root_squash, etc.),
/// and `Err` for genuine errors (bad path, IO failure).
pub fn chown_to_user(path: &Path) -> std::io::Result<bool> {
    let (uid, gid) = crate::common::config::resolve_uid_gid();
    let cpath = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains null"))?;
    match nix::unistd::chown(
        cpath.as_c_str(),
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    ) {
        Ok(()) => Ok(true),
        Err(
            nix::errno::Errno::EPERM | nix::errno::Errno::EOPNOTSUPP | nix::errno::Errno::ENOSYS,
        ) => {
            // FS doesn't support ownership (vfat/exfat/NFS no_root_squash)
            Ok(false)
        }
        Err(e) => Err(std::io::Error::other(e)),
    }
}

// ---- Directory marking (used by inode mark fallback mode) ----

/// Open a directory safely with TOCTOU-resistant flags (F-017).
///
/// Uses `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` to:
/// - Ensure the target is a directory (`O_DIRECTORY`)
/// - Refuse to follow symlinks (`O_NOFOLLOW`)
/// - Prevent fd leakage to child processes (`O_CLOEXEC`)
pub fn open_dir_safe(path: &Path) -> Result<OwnedFd> {
    nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .with_context(|| format!("open_dir_safe failed: {}", path.display()))
}

/// Mark a single directory using fd-level operations (F-017).
///
/// Opens the directory with `O_NOFOLLOW` to prevent TOCTOU symlink races,
/// then marks relative to the directory fd using `Path::new(".")`.
/// Strips FAN_FS_ERROR (only works with FS marks).
pub fn mark_directory_at(fan_fd: &OwnedFd, dir_fd: &OwnedFd, mask: u64) -> Result<()> {
    let safe_mask = mask & !FAN_FS_ERROR;
    fanotify_mark(
        fan_fd,
        FAN_MARK_ADD,
        safe_mask,
        dir_fd.as_raw_fd(),
        Path::new("."),
    )
    .context("fanotify_mark (fd-level) failed")
}

/// Mark a single directory. Strips FAN_FS_ERROR (only works with FS marks).
///
/// For new code, prefer `mark_directory_at()` + `open_dir_safe()` which
/// avoids TOCTOU races (F-017).
pub fn mark_directory(fan_fd: &OwnedFd, mask: u64, path: &Path) -> Result<()> {
    let safe_mask = mask & !FAN_FS_ERROR;
    fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path)
        .with_context(|| format!("fanotify_mark failed: {}", path.display()))
}

/// Recursively traverse and mark all subdirectories using iterative BFS (F-021).
///
/// Uses fd-level operations (`open_dir_safe` + `mark_directory_at`) to avoid
/// TOCTOU races (F-017). Skips symlinks (F-008). Supports optional depth limit.
///
/// Returns a list of **newly discovered** subdirectories (excluding `dir` itself)
/// so the caller can generate synthetic CREATE events for them.
pub fn mark_recursive(fan_fd: &OwnedFd, mask: u64, dir: &Path) -> Vec<PathBuf> {
    mark_recursive_with_depth(fan_fd, mask & !FAN_FS_ERROR, dir, None)
}

/// Like `mark_recursive`, but with a configurable depth limit.
///
/// `max_depth = None` means unlimited depth (backward compatible).
/// `max_depth = Some(0)` means only mark the root directory itself.
pub fn mark_recursive_with_depth(
    fan_fd: &OwnedFd,
    mask: u64,
    dir: &Path,
    max_depth: Option<u32>,
) -> Vec<PathBuf> {
    let safe_mask = mask & !FAN_FS_ERROR;
    let mut discovered = Vec::new();
    // BFS queue: (path, depth)
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((dir.to_path_buf(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        // Check depth limit
        if let Some(max) = max_depth
            && depth > max
        {
            continue;
        }

        // Open directory with fd-level safety (F-017)
        let dir_fd = match open_dir_safe(&current) {
            Ok(fd) => fd,
            Err(_) => continue,
        };

        // Mark this directory using fd-level operation
        if depth > 0 {
            // depth=0 is the root dir (already marked by caller)
            let _ = mark_directory_at(fan_fd, &dir_fd, safe_mask);
            discovered.push(current.clone());
        }

        // Read directory entries
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip symlinks to prevent following links to unexpected locations (F-008)
            if let Ok(metadata) = entry.metadata() {
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.file_type().is_dir() {
                    queue.push_back((path, depth + 1));
                }
            }
        }
    }
    discovered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::EventType;
    use fanotify_fid::consts::{
        FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE,
        FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR, FAN_MODIFY, FAN_MOVE_SELF,
        FAN_MOVED_FROM, FAN_MOVED_TO, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC,
    };

    // ---- event_type_to_kernel_flag ----

    #[test]
    fn test_event_type_to_kernel_flag_all() {
        let cases = [
            (EventType::Access, FAN_ACCESS),
            (EventType::Modify, FAN_MODIFY),
            (EventType::CloseWrite, FAN_CLOSE_WRITE),
            (EventType::CloseNowrite, FAN_CLOSE_NOWRITE),
            (EventType::Open, FAN_OPEN),
            (EventType::OpenExec, FAN_OPEN_EXEC),
            (EventType::Attrib, FAN_ATTRIB),
            (EventType::Create, FAN_CREATE),
            (EventType::Delete, FAN_DELETE),
            (EventType::DeleteSelf, FAN_DELETE_SELF),
            (EventType::MovedFrom, FAN_MOVED_FROM),
            (EventType::MovedTo, FAN_MOVED_TO),
            (EventType::MoveSelf, FAN_MOVE_SELF),
            (EventType::FsError, FAN_FS_ERROR),
        ];
        for (event_type, expected_flag) in &cases {
            assert_eq!(
                event_type_to_kernel_flag(event_type),
                *expected_flag,
                "mismatch for {:?}",
                event_type
            );
        }
    }

    #[test]
    fn test_event_type_to_kernel_flag_bitwise_or() {
        let access = event_type_to_kernel_flag(&EventType::Access);
        let modify = event_type_to_kernel_flag(&EventType::Modify);
        let combined = access | modify;
        assert!(combined & FAN_ACCESS != 0);
        assert!(combined & FAN_MODIFY != 0);
        assert!(combined & FAN_CREATE == 0);
    }

    // ---- mask_to_event_types ----

    #[test]
    fn test_mask_to_event_types_single() {
        let types = mask_to_event_types(FAN_CREATE);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }

    #[test]
    fn test_mask_to_event_types_multiple() {
        let mask = FAN_CREATE | FAN_DELETE | FAN_MODIFY;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 3);
        assert!(types.contains(&EventType::Create));
        assert!(types.contains(&EventType::Delete));
        assert!(types.contains(&EventType::Modify));
    }

    #[test]
    fn test_mask_to_event_types_none() {
        let types = mask_to_event_types(0);
        assert!(types.is_empty());
    }

    #[test]
    fn test_mask_to_event_types_all() {
        let mask = FAN_ACCESS
            | FAN_MODIFY
            | FAN_CLOSE_WRITE
            | FAN_CLOSE_NOWRITE
            | FAN_OPEN
            | FAN_OPEN_EXEC
            | FAN_ATTRIB
            | FAN_CREATE
            | FAN_DELETE
            | FAN_DELETE_SELF
            | FAN_FS_ERROR
            | FAN_MOVED_FROM
            | FAN_MOVED_TO
            | FAN_MOVE_SELF;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 14);
    }

    #[test]
    fn test_mask_to_event_types_with_extra_flags() {
        let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }

    // ---- path_mask_from_options ----

    fn make_test_opts(event_types: Option<Vec<EventType>>) -> PathOptions {
        PathOptions {
            size_filter: None,
            event_types,
            recursive: false,
            cmd: None,
            max_depth: None,
        }
    }

    #[test]
    fn test_path_mask_from_options_specific_types() {
        let opts = make_test_opts(Some(vec![
            EventType::Create,
            EventType::Delete,
            EventType::Modify,
        ]));
        let mask = path_mask_from_options(&opts);
        assert!(mask & FAN_CREATE != 0, "should include FAN_CREATE");
        assert!(mask & FAN_DELETE != 0, "should include FAN_DELETE");
        assert!(mask & FAN_MODIFY != 0, "should include FAN_MODIFY");
        assert!(mask & FAN_OPEN == 0, "should NOT include FAN_OPEN");
        // Always-present flags
        assert!(
            mask & FAN_EVENT_ON_CHILD != 0,
            "should include FAN_EVENT_ON_CHILD"
        );
        assert!(mask & FAN_ONDIR != 0, "should include FAN_ONDIR");
    }

    #[test]
    fn test_path_mask_from_options_default() {
        let opts = make_test_opts(None);
        let mask = path_mask_from_options(&opts);
        assert_eq!(mask, DEFAULT_EVENT_MASK, "should equal DEFAULT_EVENT_MASK");
        assert!(mask & FAN_CLOSE_WRITE != 0);
        assert!(mask & FAN_CREATE != 0);
        assert!(
            mask & FAN_ACCESS == 0,
            "default should NOT include FAN_ACCESS"
        );
        assert!(
            mask & FAN_FS_ERROR == 0,
            "default should NOT include FAN_FS_ERROR"
        );
    }

    #[test]
    fn test_path_mask_from_options_empty_types() {
        let opts = make_test_opts(Some(vec![]));
        let mask = path_mask_from_options(&opts);
        // Empty list should fall back to default mask
        assert_eq!(mask, DEFAULT_EVENT_MASK);
    }

    // ---- DEFAULT_EVENT_MASK ----

    #[test]
    fn test_default_event_mask_contents() {
        const _: () = {
            assert!(DEFAULT_EVENT_MASK & FAN_CLOSE_WRITE != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_ATTRIB != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_CREATE != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_DELETE != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_DELETE_SELF != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_MOVED_FROM != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_MOVED_TO != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_MOVE_SELF != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_EVENT_ON_CHILD != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_ONDIR != 0);
            // Should NOT include (FS_ERROR only works with FS marks)
            assert!(DEFAULT_EVENT_MASK & FAN_FS_ERROR == 0);
            assert!(DEFAULT_EVENT_MASK & FAN_ACCESS == 0);
            assert!(DEFAULT_EVENT_MASK & FAN_OPEN == 0);
        };
    }

    // ---- constant values ----

    #[test]
    fn test_constants_are_positive() {
        const _: () = {
            assert!(FILE_SIZE_CACHE_CAP > 0, "FILE_SIZE_CACHE_CAP should be > 0");
            assert!(DEFAULT_EVENT_MASK > 0, "DEFAULT_EVENT_MASK should be > 0");
        };
    }

}
