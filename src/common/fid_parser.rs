use crate::common::EventType;
use crate::common::filters::PathOptions;
use crate::common::monitor::factory::{FanotifyFactory, MAX_DIRS_PER_MARK};
use anyhow::{Context, Result};
use fanotify_fid::consts::{
    FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE,
    FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR, FAN_MARK_ADD, FAN_MODIFY, FAN_MOVE_SELF,
    FAN_MOVED_FROM, FAN_MOVED_TO, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_RENAME,
};
use fanotify_fid::prelude::*;
use std::collections::VecDeque;
use std::ffi::CString;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::common::dir_cache::{self, DirCache, DirCacheStore};

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
/// One fanotify fd per filesystem, shared by all paths on it.
///
/// The key is `st_dev`, the userspace view of the superblock the kernel
/// actually compares when it decides whether two marks may share a group.
/// Do not replace it with `fsid`: one fsid can map to two superblocks, which
/// the kernel would reject.
pub struct FsGroup {
    pub dev_id: u64,
    pub fan_fd: OwnedFd,
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
        EventType::Rename => FAN_RENAME,
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
    const BITS: [(u64, EventType); 15] = [
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
        (FAN_RENAME, EventType::Rename),
    ];
    BITS.iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, t)| *t)
        .collect()
}

/// One decoded event: everything the processor needs, and nothing else.
///
/// The reader task decodes; the processor task decides.  A kernel event cannot
/// cross between them — a `FidEvent` borrows the read buffer — and making one
/// owned copies the handle and the name of **every** event, which is the wrong
/// half of the batch to copy: fsmon drops most events (no matching path, no
/// matching cmd, the daemon's own pid), and only a rename ever looks at a handle
/// again.  So the reader keeps the bytes while it has them and sends what a
/// decision needs:
///
/// * `path` is resolved, so the processor never touches a handle;
/// * `rename` carries two **paths**, not two handles, for the one event type
///   whose payload is a pair of *other* objects' names;
/// * `fs_error` carries the filesystem error code, which lives on the event and
///   nowhere else;
/// * `unparsed` counts the info records the library preserved but fsmon cannot
///   read, because "data was dropped" has to stay observable.
///
/// It is deliberately not `FidEvent` with fields taken out: an event whose handle
/// the cache does not know has no path, and such an event cannot match any
/// watched path, so the reader drops it there rather than sending a record that
/// every downstream stage would have to test for emptiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedEvent {
    /// The kernel's mask, verbatim: the event types are derived from it by
    /// [`mask_to_event_types`], which is a bit test and needs no decoding.
    pub mask: u64,
    pub pid: i32,
    /// The resolved path.  Always present: an event that could not be resolved
    /// was dropped by the reader.
    pub path: PathBuf,
    /// `(old, new)` for a `FAN_RENAME`, already resolved to full paths.  Either
    /// side is `None` when its parent directory was not in the cache.
    pub rename: Option<(Option<PathBuf>, Option<PathBuf>)>,
    /// `(negative errno, merged count)` from a `FAN_FS_ERROR` record.
    pub fs_error: Option<(i32, u32)>,
    /// How many info records the parser preserved that fsmon does not read.
    pub unparsed: usize,
}

/// Read one batch, resolve it against `dir_cache`, and decode it for the
/// processor.
///
/// # Why the reader decodes
///
/// A parsed event borrows the read buffer, so it cannot be sent anywhere and
/// cannot outlive the next read.  The reader task is the only place that holds
/// both the bytes and the cache, so it is the only place that can turn a kernel
/// event into a record with a path on it — and doing it here is also what keeps
/// the processor free of handle bookkeeping.
///
/// # Path recovery, and what a miss means
///
/// `dir_cache` is the only source of paths.  No mount descriptors are registered
/// and the resolver's syscall fallback is **off**: fsmon deliberately does not
/// request `CAP_DAC_READ_SEARCH`, so `open_by_handle_at` could never succeed, and
/// turning the fallback off makes that a zero-syscall immediate failure rather
/// than a failed privileged call per event.
///
/// An event the cache cannot place is **dropped**, and counted: `DirCache::misses`
/// is the `dir_cache_misses` metric.  It cannot match a watched path, so the only
/// thing reporting it could do is tell a reader how often path recovery fails —
/// which the miss counter already says.
///
/// The cache is normally complete because the marking walk seeds a handle for
/// every directory it descends into — see `cache_handle_from_fd`.
///
/// # The learning form
///
/// Resolution runs through `resolve_events_memo`, so a batch that resolves a
/// directory teaches the cache and the next batch about that directory needs no
/// work at all.  That is free here — the store is already in hand — and it is
/// what keeps a burst of events about one directory from re-resolving it.
pub fn read_fid_events_cached(
    fan_fd: &OwnedFd,
    dir_cache: &DirCache,
    buf: &mut Vec<u8>,
) -> Vec<DecodedEvent> {
    // `Fanotify` owns the descriptor it is given, so it is handed a duplicate:
    // the caller's descriptor is the group, and the group must outlive this
    // call.
    let Ok(dup) = fan_fd.try_clone() else {
        eprintln!(
            "[WARNING] cannot dup fanotify fd {} for reading",
            fan_fd.as_raw_fd()
        );
        return Vec::new();
    };
    // SAFETY: `dup` is a duplicate of `fan_fd`, which the caller created as a
    // fanotify group.  See `mark_directory_at` and `FsGroup`.
    let fan = unsafe { Fanotify::from_fd(dup) };

    let mut events = match fan.read_events(buf) {
        Ok(events) => events,
        // An empty queue on a non-blocking group is not a failure.
        Err(err) if err.is_would_block() => return Vec::new(),
        Err(err) => {
            eprintln!(
                "[WARNING] fanotify read error on fd {}: {err}",
                fan_fd.as_raw_fd()
            );
            return Vec::new();
        }
    };

    decode_events(&mut events, dir_cache)
}

/// Resolve and decode a parsed batch.  Split out so a test can drive it with
/// synthetic events instead of a real queue.
fn decode_events(events: &mut [FidEvent<'_>], dir_cache: &DirCache) -> Vec<DecodedEvent> {
    if events.is_empty() {
        return Vec::new();
    }

    // The store is the cache, and the fallback stays off: see the doc comment
    // above.  Both borrows are shared, which is what the resolver takes — the
    // cache has its own mutability, so one reference reads and records.
    let store = DirCacheStore(dir_cache.clone());
    let mounts = Mounts::new();
    let mut resolver = PathResolver::new(&store, &mounts);
    resolver.set_syscall_fallback(false);
    let _ = resolver.resolve_events_memo(&store, events);

    let mut decoded = Vec::with_capacity(events.len());
    for ev in events.iter() {
        // A rename's payload names two *other* objects — the old and the new
        // parent directory — so `resolve_events` speaks for the source side and
        // the target is this lookup.  Both are cache lookups: the marking walk
        // seeded every directory it walked into.
        let rename = (ev.mask() & FAN_RENAME != 0).then(|| {
            let target = resolver.resolve_rename_target(ev).and_then(|r| r.ok());
            (ev.path().map(Path::to_path_buf), target)
        });

        // No path means the cache could not place it, which means it cannot
        // match a watched path.  `DirCache::misses` is the record of it.
        let Some(path) = ev.path() else {
            continue;
        };

        decoded.push(DecodedEvent {
            mask: ev.mask(),
            pid: ev.pid(),
            path: path.to_path_buf(),
            rename,
            fs_error: ev.fs_error(),
            unparsed: ev.unknown_info_records().len(),
        });
    }
    decoded
}

// ---- Constants ----

/// Capacity for the directory handle cache (path→handle key reverse lookup).
/// 100k covers ~10s of thousands of directories with room to spare.
/// Expired or excess entries are evicted when the limit is reached.
pub const DIR_CACHE_CAP: u64 = 100_000;

/// TTL for directory handle cache entries.
/// After 1 hour of no access, entries are automatically evicted.
/// This prevents stale entries when directories are deleted/renamed.
pub const DIR_CACHE_TTL_SECS: u64 = 3600;

pub const FILE_SIZE_CACHE_CAP: usize = 10_000;

/// Default mask: the core events (FS_ERROR excluded — only works with FS marks).
///
/// `FAN_RENAME` replaces the `FAN_MOVED_FROM | FAN_MOVED_TO` pair: a rename
/// arrives as **one** event naming both the old and the new location, so the
/// two halves never have to be correlated and a rename that leaves the watched
/// subtree is still reported completely.  The kernel requires
/// `FAN_REPORT_NAME` for this bit, which [`GROUP_INIT_FLAGS`] already sets.
///
/// Use `--types all` for every non-rename type (FS_ERROR included, but only
/// effective on FS marks); `--types rename` selects the fused rename event
/// explicitly, and it cannot be combined with MOVED_FROM/MOVED_TO.
///
/// [`GROUP_INIT_FLAGS`]: crate::common::monitor::factory::GROUP_INIT_FLAGS
pub const DEFAULT_EVENT_MASK: u64 = FAN_CLOSE_WRITE
    | FAN_ATTRIB
    | FAN_CREATE
    | FAN_DELETE
    | FAN_DELETE_SELF
    | FAN_RENAME
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
/// Strips `FAN_FS_ERROR` (only works with FS marks).
///
/// The actual `fanotify_mark()` call is made by the privileged factory
/// subprocess; the parent only ever passes a `dirfd`.
pub(crate) fn mark_directory_at(
    factory: &FanotifyFactory,
    fan_fd: &OwnedFd,
    dir_fd: &OwnedFd,
    mask: u64,
) -> Result<()> {
    let safe_mask = mask & !FAN_FS_ERROR;
    factory
        .mark(fan_fd, &[dir_fd.as_fd()], FAN_MARK_ADD, safe_mask)
        .context("fanotify factory mark (fd-level) failed")
}

/// Remove the inode mark on `dir_fd` from `fan_fd`'s group.
pub(crate) fn unmark_directory_at(
    factory: &FanotifyFactory,
    fan_fd: &OwnedFd,
    dir_fd: &OwnedFd,
) -> Result<()> {
    factory
        .mark(fan_fd, &[dir_fd.as_fd()], FAN_MARK_REMOVE, 0)
        .context("fanotify factory mark removal failed")
}

/// Mark a single directory. Strips FAN_FS_ERROR (only works with FS marks).
///
/// Kept for callers that only have a path; opens the directory safely first.
pub(crate) fn mark_directory(
    factory: &FanotifyFactory,
    fan_fd: &OwnedFd,
    mask: u64,
    path: &Path,
) -> Result<()> {
    let dir_fd = open_dir_safe(path)?;
    mark_directory_at(factory, fan_fd, &dir_fd, mask)
}

/// Recursively traverse and mark all subdirectories using iterative BFS (F-021).
///
/// Uses fd-level operations (`open_dir_safe` + `mark_directory_at`) to avoid
/// TOCTOU races (F-017). Skips symlinks (F-008).
///
/// Returns a list of **newly discovered** subdirectories (excluding `dir` itself)
/// so the caller can generate synthetic CREATE events for them.
///
/// `max_depth = None` means unlimited depth (backward compatible).
/// `max_depth = Some(0)` means only mark the root directory itself.
pub(crate) fn mark_recursive_with_depth(
    factory: &FanotifyFactory,
    fan_fd: &OwnedFd,
    mask: u64,
    dir: &Path,
    max_depth: Option<u32>,
    cache: Option<&DirCache>,
) -> Vec<PathBuf> {
    let safe_mask = mask & !FAN_FS_ERROR;
    let mut discovered = Vec::new();
    // Directory fds waiting to be marked. One factory round-trip carries up to
    // MAX_DIRS_PER_MARK of them, so the walk fills batches instead of paying an
    // IPC round-trip per directory.
    let mut pending: Vec<OwnedFd> = Vec::with_capacity(MAX_DIRS_PER_MARK);
    // BFS queue: (path, depth)
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((dir.to_path_buf(), 0));
    // Depth of the directories currently sitting in `pending`. The queue is in
    // level order, so reaching a new depth means the previous level is complete
    // and its marks can go out. Without this a narrow deep tree would hold every
    // mark until the walk ended, leaving the deepest directories unmarked for
    // the whole traversal.
    let mut pending_depth = 0u32;

    while let Some((current, depth)) = queue.pop_front() {
        if depth != pending_depth {
            flush_pending_marks(factory, fan_fd, safe_mask, &mut pending);
            pending_depth = depth;
        }

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

        // Cache handle→path here, while the descriptor is open and the path is
        // known.  This is what makes every marked directory resolvable later:
        // rename records carry parent-directory handles and nothing else, so a
        // handle missing from the cache means a rename that cannot be
        // attributed.  Deriving the handle from the fd we already hold costs one
        // syscall and avoids re-walking the whole tree by path afterwards.
        if let Some(cache) = cache {
            dir_cache::cache_handle_from_fd(cache, &dir_fd, &current);
        }

        if depth > 0 {
            // depth=0 is the root dir (already marked by caller)
            pending.push(dir_fd);
            discovered.push(current.clone());
            if pending.len() >= MAX_DIRS_PER_MARK {
                flush_pending_marks(factory, fan_fd, safe_mask, &mut pending);
            }
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

    flush_pending_marks(factory, fan_fd, safe_mask, &mut pending);
    discovered
}

/// Mark every queued directory in a single factory round-trip.
///
/// Errors are discarded, matching the previous one-directory-at-a-time
/// behaviour: an unmarkable directory is skipped rather than aborting the walk.
fn flush_pending_marks(
    factory: &FanotifyFactory,
    fan_fd: &OwnedFd,
    mask: u64,
    pending: &mut Vec<OwnedFd>,
) {
    if pending.is_empty() {
        return;
    }
    let dir_fds: Vec<BorrowedFd<'_>> = pending.iter().map(|fd| fd.as_fd()).collect();
    let _ = factory.mark(fan_fd, &dir_fds, FAN_MARK_ADD, mask);
    pending.clear();
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

    // ---- decode_events: handles in, records out ----

    /// The step the processor depends on: a parsed event whose parent directory the
    /// cache knows becomes a record with a path, and one it does not know is
    /// dropped rather than forwarded pathless.
    #[test]
    fn decode_resolves_a_cached_parent_and_drops_an_unknown_one() {
        let dir = tempfile::tempdir().unwrap();
        let fsid = fanotify_fid::handle::fsid_of_path(dir.path()).unwrap();
        let handle =
            fanotify_fid::handle::handle_from_fd(std::fs::File::open(dir.path()).unwrap()).unwrap();

        let cache = DirCache::new(1024, std::time::Duration::from_secs(60));
        cache.insert((fsid, handle.clone()), dir.path().to_path_buf());

        let mut known = FidEvent::new();
        known.set_mask(FAN_CREATE).set_pid(77).set_fsid(fsid);
        known.set_dfid_name(handle, b"entry.txt".to_vec());
        // An unknown handle is the same event with a parent the cache has never
        // seen.
        let mut unknown = FidEvent::new();
        unknown.set_mask(FAN_CREATE).set_pid(78).set_fsid(fsid);
        unknown.set_dfid_name(vec![0xde, 0xad, 0xbe, 0xef], b"other.txt".to_vec());

        let mut events = vec![known, unknown];
        let decoded = decode_events(&mut events, &cache);

        assert_eq!(decoded.len(), 1, "the unplaceable event must be dropped");
        assert_eq!(decoded[0].path, dir.path().join("entry.txt"));
        assert_eq!(decoded[0].pid, 77);
        assert_eq!(decoded[0].mask, FAN_CREATE);
        assert!(decoded[0].rename.is_none());
        // Counted, which is how degraded path recovery is observed from outside.
        // The number is per lookup rather than per event: the resolver walks the
        // batch more than once when a pass resolves something, and a handle the
        // cache does not know is asked about on each pass.
        assert!(
            cache.misses() >= 1,
            "the dropped event must be counted, got {}",
            cache.misses()
        );
    }

    /// A rename is the one event whose payload names two *other* objects, so both
    /// halves are resolved here — while the handles are in hand — and what reaches
    /// the processor is two paths.
    #[test]
    fn decode_resolves_both_halves_of_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let old_dir = dir.path().join("old");
        let new_dir = dir.path().join("new");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();

        let fsid = fanotify_fid::handle::fsid_of_path(&old_dir).unwrap();
        let old_handle =
            fanotify_fid::handle::handle_from_fd(std::fs::File::open(&old_dir).unwrap()).unwrap();
        let new_handle =
            fanotify_fid::handle::handle_from_fd(std::fs::File::open(&new_dir).unwrap()).unwrap();

        let cache = DirCache::new(1024, std::time::Duration::from_secs(60));
        cache.insert((fsid, old_handle.clone()), old_dir.clone());
        cache.insert((fsid, new_handle.clone()), new_dir.clone());

        let mut rename = FidEvent::new();
        rename.set_mask(FAN_RENAME).set_pid(4242).set_fsid(fsid);
        rename.set_rename_source(RenameSide {
            handle: old_handle.into(),
            name: b"moved.txt".to_vec().into(),
        });
        rename.set_rename_target(RenameSide {
            handle: new_handle.into(),
            name: b"landed.txt".to_vec().into(),
        });

        let mut events = vec![rename];
        let decoded = decode_events(&mut events, &cache);

        assert_eq!(decoded.len(), 1);
        assert_eq!(
            decoded[0].path,
            old_dir.join("moved.txt"),
            "the event's own path is the source side: the one that says it is gone"
        );
        assert_eq!(
            decoded[0].rename,
            Some((
                Some(old_dir.join("moved.txt")),
                Some(new_dir.join("landed.txt")),
            )),
            "and the pair carries both resolved locations"
        );
    }

    /// A rename whose new parent the cache does not know still arrives: half a
    /// rename is a fact, and losing it would lose the move entirely.
    #[test]
    fn decode_keeps_a_rename_with_one_unknown_half() {
        let dir = tempfile::tempdir().unwrap();
        let fsid = fanotify_fid::handle::fsid_of_path(dir.path()).unwrap();
        let handle =
            fanotify_fid::handle::handle_from_fd(std::fs::File::open(dir.path()).unwrap()).unwrap();

        let cache = DirCache::new(1024, std::time::Duration::from_secs(60));
        cache.insert((fsid, handle.clone()), dir.path().to_path_buf());

        let mut rename = FidEvent::new();
        rename.set_mask(FAN_RENAME).set_pid(1).set_fsid(fsid);
        rename.set_rename_source(RenameSide {
            handle: handle.into(),
            name: b"gone.txt".to_vec().into(),
        });
        rename.set_rename_target(RenameSide {
            handle: vec![0xbe, 0xef].into(),
            name: b"elsewhere.txt".to_vec().into(),
        });

        let mut events = vec![rename];
        let decoded = decode_events(&mut events, &cache);

        assert_eq!(decoded.len(), 1);
        assert_eq!(
            decoded[0].rename,
            Some((Some(dir.path().join("gone.txt")), None)),
            "the known half survives, the unknown one is a None"
        );
    }

    /// The two fields that have no other home: the filesystem error code and the
    /// count of records the library preserved but fsmon cannot read.
    #[test]
    fn decode_carries_fs_error_and_the_unparsed_count() {
        let dir = tempfile::tempdir().unwrap();
        let fsid = fanotify_fid::handle::fsid_of_path(dir.path()).unwrap();
        let handle =
            fanotify_fid::handle::handle_from_fd(std::fs::File::open(dir.path()).unwrap()).unwrap();

        let cache = DirCache::new(1024, std::time::Duration::from_secs(60));
        cache.insert((fsid, handle.clone()), dir.path().to_path_buf());

        let mut event = FidEvent::new();
        event.set_mask(FAN_FS_ERROR).set_pid(9).set_fsid(fsid);
        event.set_dfid_name(handle, b"broken".to_vec());
        event.set_fs_error(-5, 3);
        event.push_unknown_info_record(6, vec![0u8; 8]);
        event.push_unknown_info_record(7, vec![0u8; 8]);

        let mut events = vec![event];
        let decoded = decode_events(&mut events, &cache);

        assert_eq!(decoded[0].fs_error, Some((-5, 3)));
        assert_eq!(decoded[0].unparsed, 2);
    }

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
            // FAN_RENAME replaces the MOVED_FROM/MOVED_TO pair: the kernel
            // emits one or the other, never both (see EventType::Rename).
            assert!(DEFAULT_EVENT_MASK & FAN_RENAME != 0);
            assert!(DEFAULT_EVENT_MASK & FAN_MOVED_FROM == 0);
            assert!(DEFAULT_EVENT_MASK & FAN_MOVED_TO == 0);
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
