use anyhow::{Context, Result};
use std::ffi::CString;
use dashmap::DashMap;
use fanotify_fid::prelude::*;
use fanotify_fid::types::{FidEvent, HandleKey};
use fanotify_fid::handle::resolve_file_handle;
use fanotify_fid::consts::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE,
    FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR, FAN_MARK_ADD,
    FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_ONDIR,
    FAN_OPEN, FAN_OPEN_EXEC,
};
use crate::filters::PathOptions;
use crate::EventType;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

// ---- FanFd wrapper for AsyncFd ----

/// Newtype wrapper around a raw fanotify file descriptor.
/// Implements `AsRawFd` and `AsFd` so it can be used with `AsyncFd`.
pub struct FanFd(pub RawFd);

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
    pub is_fs_mark: bool,
    pub fan_fd: OwnedFd,
    pub mount_fd: OwnedFd,
    pub ref_count: usize,
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
            types.iter()
                .fold(FAN_EVENT_ON_CHILD | FAN_ONDIR, |m, t| m | event_type_to_kernel_flag(t))
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
    BITS.iter().filter(|(bit, _)| mask & bit != 0).map(|(_, t)| *t).collect()
}

/// Read and parse FID events, using a `DashMap`-based cache for path recovery.
pub fn read_fid_events_dashmap(
    fan_fd: &OwnedFd,
    mount_fds: &[OwnedFd],
    dir_cache: &DashMap<HandleKey, PathBuf>,
    buf: &mut Vec<u8>,
) -> Vec<FidEvent> {
    // Delegate raw read + parse to fanotify-fid (no cache = first pass only)
    let mut events = match fanotify_fid::read::read_fid_events(fan_fd, mount_fds, buf, None) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    // Second-pass: DashMap-based cache recovery (multiple passes for nested deletions).
    // Inlined instead of using fanotify_fid::resolve_with_cache because that
    // takes &HashMap — copying the entire DashMap on every event is too expensive.
    for _ in 0..10 {
        // Update cache from successfully-resolved events
        for ev in events.iter() {
            if ev.path.as_os_str().is_empty() { continue; }
            if let Some(ref key) = ev.self_handle {
                dir_cache.entry(key.clone()).or_insert_with(|| ev.path.clone());
            }
            if let (Some(key), Some(filename)) = (&ev.dfid_name_handle, &ev.dfid_name_filename) {
                let dir_path = if !filename.is_empty() {
                    ev.path.parent().map(|p| p.to_path_buf())
                } else {
                    Some(ev.path.clone())
                };
                if let Some(dp) = dir_path {
                    dir_cache.entry(key.clone()).or_insert(dp);
                }
            }
        }

        // Try to recover empty paths from cache (direct DashMap lookup, no copy)
        let mut made_progress = false;
        for ev in events.iter_mut() {
            if !ev.path.as_os_str().is_empty() { continue; }

            if let (Some(key), Some(filename)) = (&ev.dfid_name_handle, &ev.dfid_name_filename) {
                let dir_path = dir_cache.get(key).map(|p| p.clone()).or_else(|| {
                    // Cache miss: try direct handle resolution for first CREATE event
                    resolve_file_handle(mount_fds, key.as_slice())
                });
                if let Some(ref dp) = dir_path {
                    dir_cache.insert(key.clone(), dp.clone());
                    ev.path = if filename.is_empty() {
                        dp.clone()
                    } else {
                        dp.join(filename)
                    };
                    made_progress = true;
                }
            }

            if ev.path.as_os_str().is_empty()
                && let Some(ref key) = ev.self_handle
                && let Some(cached_path) = dir_cache.get(key)
            {
                ev.path = cached_path.clone();
                made_progress = true;
            }
        }

        if !made_progress { break; }
    }
    events
}

// ---- Constants ----

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
    let (uid, gid) = crate::config::resolve_uid_gid();
    let cpath = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains null"))?;
    match nix::unistd::chown(
        cpath.as_c_str(),
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    ) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::EPERM) | Err(nix::errno::Errno::EOPNOTSUPP) | Err(nix::errno::Errno::ENOSYS) => {
            // FS doesn't support ownership (vfat/exfat/NFS no_root_squash)
            Ok(false)
        }
        Err(e) => Err(std::io::Error::other(e)),
    }
}

// ---- Directory marking (used by inode mark fallback mode) ----

/// Mark a single directory. Strips FAN_FS_ERROR (only works with FS marks).
pub fn mark_directory(fan_fd: &OwnedFd, mask: u64, path: &Path) -> Result<()> {
    let safe_mask = mask & !FAN_FS_ERROR;
    fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path)
        .with_context(|| format!("fanotify_mark failed: {}", path.display()))
}

/// Recursively traverse and mark all subdirectories (ignore errors, e.g., permission denied).
/// Strips FAN_FS_ERROR (only works with FS marks).
pub fn mark_recursive(fan_fd: &OwnedFd, mask: u64, dir: &Path) {
    let safe_mask = mask & !FAN_FS_ERROR;
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path.as_path());
            mark_recursive(fan_fd, safe_mask, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventType;
    use fanotify_fid::consts::{
        FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE,
        FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR, FAN_MODIFY,
        FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC,
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
            assert_eq!(event_type_to_kernel_flag(event_type), *expected_flag,
                "mismatch for {:?}", event_type);
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
        let mask = FAN_ACCESS | FAN_MODIFY | FAN_CLOSE_WRITE | FAN_CLOSE_NOWRITE
            | FAN_OPEN | FAN_OPEN_EXEC | FAN_ATTRIB | FAN_CREATE | FAN_DELETE
            | FAN_DELETE_SELF | FAN_FS_ERROR | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF;
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
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
            cmd: None,
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
        assert!(mask & FAN_EVENT_ON_CHILD != 0, "should include FAN_EVENT_ON_CHILD");
        assert!(mask & FAN_ONDIR != 0, "should include FAN_ONDIR");
    }

    #[test]
    fn test_path_mask_from_options_default() {
        let opts = make_test_opts(None);
        let mask = path_mask_from_options(&opts);
        assert_eq!(mask, DEFAULT_EVENT_MASK, "should equal DEFAULT_EVENT_MASK");
        assert!(mask & FAN_CLOSE_WRITE != 0);
        assert!(mask & FAN_CREATE != 0);
        assert!(mask & FAN_ACCESS == 0, "default should NOT include FAN_ACCESS");
        assert!(mask & FAN_FS_ERROR == 0, "default should NOT include FAN_FS_ERROR");
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
    }

    // ---- constant values ----

    #[test]
    fn test_constants_are_positive() {
        assert!(FILE_SIZE_CACHE_CAP > 0, "FILE_SIZE_CACHE_CAP should be > 0");
        assert!(DEFAULT_EVENT_MASK > 0, "DEFAULT_EVENT_MASK should be > 0");
    }
}
