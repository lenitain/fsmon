use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify_fid::prelude::*;
use fanotify_fid::types::{FidEvent, HandleKey};
use fanotify_fid::handle::resolve_file_handle;
use fanotify_fid::consts::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_CLOSE_NOWRITE,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_FS_ERROR,
    FAN_MARK_ADD, FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM,
    FAN_MOVED_TO, FAN_NONBLOCK, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_Q_OVERFLOW,
    FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use dashmap::DashMap;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lru::LruCache;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::signal::unix::{SignalKind, signal};

use crate::dir_cache;
use crate::proc_cache::{self, ProcCache, ProcInfo};
use crate::socket::{SocketCmd, SocketResp};
use crate::managed::PathEntry;
use crate::managed::Managed;
use crate::utils::{get_process_info_by_pid, parse_size};
use crate::{EventType, FileEvent};

// ---- FanFd wrapper for AsyncFd ----

/// Newtype wrapper around a raw fanotify file descriptor.
/// Implements `AsRawFd` and `AsFd` so it can be used with `AsyncFd`.
struct FanFd(RawFd);

impl AsRawFd for FanFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl AsFd for FanFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: the fd is valid for the lifetime of the Monitor
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

// ---- FsGroup: one per unique filesystem ----

/// A group of fds for a single filesystem.
/// One fanotify fd + one directory fd per filesystem, shared by all paths on it.
struct FsGroup {
    dev_id: u64,
    is_fs_mark: bool,
    fan_fd: OwnedFd,
    mount_fd: OwnedFd,
    ref_count: usize,
}

/// Convert an EventType to its fanotify kernel flag.
fn event_type_to_kernel_flag(t: &EventType) -> u64 {
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
fn path_mask_from_options(opts: &PathOptions) -> u64 {
    match &opts.event_types {
        Some(types) if !types.is_empty() => {
            types.iter()
                .fold(FAN_EVENT_ON_CHILD | FAN_ONDIR, |m, t| m | event_type_to_kernel_flag(t))
        }
        _ => DEFAULT_EVENT_MASK,
    }
}

// ---- FID event helpers ----

/// Convert a fanotify event mask to fsmon's EventType enum.
fn mask_to_event_types(mask: u64) -> smallvec::SmallVec<[EventType; 8]> {
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
fn read_fid_events_dashmap(
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

/// Chown a file or directory to the original user (daemon runs as root).
/// Resolves the original user from SUDO_UID/SUDO_GID env vars.
///
/// Returns `Ok(true)` if chown succeeded, `Ok(false)` if the filesystem
/// does not support ownership changes (vfat/exfat/NFS no_root_squash, etc.),
/// and `Err` for genuine errors (bad path, IO failure).
fn chown_to_user(path: &Path) -> std::io::Result<bool> {
    let (uid, gid) = crate::config::resolve_uid_gid();
    let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes())
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

// ---- Constants ----

const FILE_SIZE_CACHE_CAP: usize = 10_000;
const PROC_CONNECTOR_TIMEOUT_SECS: u64 = 2;

/// Default mask: 8 core events (FS_ERROR excluded — only works with FS marks).
/// Use --types all to get all 14 (FS_ERROR included, but only effective on FS marks).
const DEFAULT_EVENT_MASK: u64 = FAN_CLOSE_WRITE
    | FAN_ATTRIB
    | FAN_CREATE
    | FAN_DELETE
    | FAN_DELETE_SELF
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_MOVE_SELF
    | FAN_EVENT_ON_CHILD
    | FAN_ONDIR;


// ---- PathOptions ----

#[derive(Clone)]
pub struct PathOptions {
    pub min_size: Option<i64>,
    pub event_types: Option<Vec<EventType>>,
    pub exclude_regex: Option<regex::Regex>,
    pub exclude_invert: bool,
    pub exclude_cmd_regex: Option<regex::Regex>,
    pub exclude_cmd_invert: bool,
    pub recursive: bool,
}


/// Resolve a path for recursion check: expand tilde, then canonicalize if the path exists
/// (follows symlinks). Falls back to tilde-expanded path if can't canonicalize.
fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = crate::config::expand_tilde(path, &home);
    expanded.canonicalize().unwrap_or(expanded)
}
// ---- Monitor ----

pub struct Monitor {
    paths: Vec<PathBuf>,
    canonical_paths: Vec<PathBuf>,
    path_options: HashMap<PathBuf, PathOptions>,
    log_dir: Option<PathBuf>,
    managed_path: Option<PathBuf>,
    proc_cache: Option<ProcCache>,
    file_size_cache: LruCache<PathBuf, u64>,
    pid_cache: LruCache<u32, ProcInfo>,
    buffer_size: usize,
    socket_listener: Option<tokio::net::UnixListener>,
    /// One `FsGroup` per unique filesystem (fan_fd + mount_fd dedup'd)
    fs_groups: Vec<FsGroup>,
    /// Maps monitored path → index in fs_groups for fast lookup in remove_path
    path_to_group: HashMap<PathBuf, usize>,
    dir_cache: DashMap<HandleKey, PathBuf>,
    /// Shared state for spawning reader tasks during live-add (set in run())
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<FidEvent>>>,
    shared_dir_cache: Option<Arc<DashMap<HandleKey, PathBuf>>>,
    /// Paths that didn't exist at add/startup time, retried on directory creation
    pending_paths: Vec<(PathBuf, PathEntry)>,
    /// inotify instance watching parent dirs of pending paths
    inotify: Option<inotify::Inotify>,
    /// Watch descriptors kept alive so watches stay active
    _inotify_watches: Vec<inotify::WatchDescriptor>,
}

impl Monitor {
    pub fn new(
        paths_and_options: Vec<(PathBuf, PathOptions)>,
        log_dir: Option<PathBuf>,
        managed_path: Option<PathBuf>,
        buffer_size: Option<usize>,
        socket_listener: Option<tokio::net::UnixListener>,
    ) -> Result<Self> {
        let buffer_size = buffer_size.unwrap_or(4096 * 8); // Default 32KB

        if buffer_size < 4096 {
            bail!("buffer_size must be at least 4096 bytes (4KB)");
        }
        if buffer_size > 1024 * 1024 {
            bail!("buffer_size must not exceed 1048576 bytes (1MB)");
        }

        let mut paths = Vec::new();
        let mut path_options = HashMap::new();
        let log_dir_canonical = log_dir.as_ref().map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()));
        for (path, opts) in &paths_and_options {
            // Reject paths that would cause infinite recursion
            // Resolve tilde + symlinks to catch symlink-based conflicts
            let resolved = resolve_recursion_check(path);
            if let Some(ref log_dir) = log_dir_canonical
                && log_dir.starts_with(&resolved)
            {
                bail!(
                    "Cannot monitor '{}': log directory '{}' is inside this path — \
                     would cause infinite recursion on every log write.\n\
                     Tip: fsmon remove {} or exclude the log directory with --exclude \
                     or use a different logging.dir",
                    path.display(),
                    log_dir_canonical.as_ref().unwrap().display(),
                    path.display()
                );
            }
            path_options.insert(resolved.clone(), opts.clone());
            paths.push(resolved.clone());
        }

        Ok(Self {
            paths,
            canonical_paths: Vec::new(),
            path_options,
            log_dir,
            managed_path,
            proc_cache: None,
            file_size_cache: LruCache::new(NonZeroUsize::new(FILE_SIZE_CACHE_CAP).unwrap()),
            pid_cache: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            buffer_size,

            socket_listener,
            fs_groups: Vec::new(),
            path_to_group: HashMap::new(),
            dir_cache: DashMap::new(),
            event_tx: None,
            shared_dir_cache: None,
            pending_paths: Vec::new(),
            inotify: None,
            _inotify_watches: Vec::new(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        if nix::unistd::geteuid().as_raw() != 0 {
            let hint = if let Ok(exe) = std::env::current_exe() {
                if exe.to_string_lossy().contains(".cargo/bin") {
                    "\n\nHint: It looks like fsmon was installed via cargo install (~/.cargo/bin).\n\
                    sudo cannot find it because ~/.cargo/bin is not in sudo's secure_path.\n\
                    Please either:\n\
                      1. Copy to system path: sudo cp ~/.cargo/bin/fsmon /usr/local/bin/\n\
                      2. Or use full path: sudo ~/.cargo/bin/fsmon monitor ..."
                } else {
                    ""
                }
            } else {
                ""
            };

            bail!(
                "fanotify requires root privileges, please run with sudo{}",
                hint
            );
        }

        // Start proc connector listener thread, cache process exec info
        let (proc_cache, proc_ready) = proc_cache::start_proc_listener();
        self.proc_cache = Some(proc_cache);

        // Wait for proc connector subscription to complete (poll with backoff)
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_secs(PROC_CONNECTOR_TIMEOUT_SECS);
        let mut poll_interval = tokio::time::Duration::from_millis(1);
        loop {
            if proc_ready.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "[WARNING] proc connector subscription timed out after {}s. \
                     Process name attribution may be incomplete. Monitoring continues.",
                    PROC_CONNECTOR_TIMEOUT_SECS
                );
                self.proc_cache = None;
                break;
            }
            tokio::time::sleep(poll_interval).await;
            poll_interval = (poll_interval * 2).min(tokio::time::Duration::from_millis(50));
        }

        // Compute combined event mask (OR of all per-path masks)
        let combined_mask = self.path_options.values()
            .map(path_mask_from_options)
            .fold(0, |a, b| a | b);

        // Collect canonical paths — non-existent paths go to pending_paths
        // and are removed from paths/path_options so add_path can work on retry
        let mut keep_paths: Vec<PathBuf> = Vec::new();
        let mut keep_opts = HashMap::new();
        for path in std::mem::take(&mut self.paths) {
            let opts = self.path_options.remove(&path)
                .expect("path in paths but not in path_options");
            if path.exists() {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                self.canonical_paths.push(canonical);
                keep_paths.push(path.clone());
                keep_opts.insert(path, opts);
            } else {
                eprintln!(
                    "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                    path.display()
                );
                let entry_path = path.clone();
                self.pending_paths.push((path, PathEntry {
                    path: entry_path,
                    recursive: Some(opts.recursive),
                    types: opts.event_types.as_ref().map(
                        |v| v.iter().map(|t| t.to_string()).collect()
                    ),
                    min_size: opts.min_size.map(|s| s.to_string()),
                    exclude: opts.exclude_regex.as_ref().map(|r| vec![r.as_str().to_string()]),
                    exclude_cmd: None,
                }));
            }
        }
        self.paths = keep_paths;
        self.path_options = keep_opts;
        // Initialize inotify for watching parent dirs of pending paths
        self.inotify = Some(inotify::Inotify::init().context("inotify_init")?);
        self.setup_inotify_watches();

        // Initialize per-filesystem fanotify fds. One FsGroup per unique
        // filesystem (grouped by st_dev). All paths on the same filesystem
        // share one fanotify fd + one directory mount fd.
        //
        // Strategy: try FAN_MARK_FILESYSTEM first. If it succeeds, the FS mark
        // covers all paths on that superblock. If EXDEV, fall back to per-path
        // inode marks (plus recursive marking for directories).

        let mut fs_group_devs: Vec<u64> = Vec::new();
        for (i, canonical) in self.canonical_paths.iter().enumerate() {
            let path_mask = combined_mask;

            // Determine filesystem via st_dev
            let dev_id = std::fs::metadata(canonical)
                .ok()
                .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
                .unwrap_or(0);

            // Try to reuse an existing FsGroup on the same filesystem
            let mut reuse_idx = None;
            for (gi, gdev) in fs_group_devs.iter().enumerate() {
                if *gdev == dev_id {
                    reuse_idx = Some(gi);
                    break;
                }
            }

            if let Some(gi) = reuse_idx {
                // Same filesystem — just add mark (inode) if group uses inode marks
                let group = &self.fs_groups[gi];
                if !group.is_fs_mark {
                    let fan_fd = &group.fan_fd;
                    if let Err(e) = mark_directory(fan_fd, path_mask, canonical) {
                        eprintln!(
                            "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                            canonical.display(),
                            fan_fd.as_raw_fd(),
                            e
                        );
                    } else {
                        eprintln!(
                            "[INFO] Monitoring {} (inode mark) on existing fd {}",
                            canonical.display(),
                            fan_fd.as_raw_fd()
                        );
                        // mark subdirectories recursively
                        let opts = self.paths.get(i).and_then(|p| self.path_options.get(p));
                        if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                            mark_recursive(fan_fd, path_mask, canonical);
                        }
                    }
                }
                self.fs_groups[gi].ref_count += 1;
                self.path_to_group.insert(self.paths[i].clone(), gi);
                continue;
            }

            // New filesystem — create fanotify fd + mount fd
            let new_fd = fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            )
            .with_context(|| {
                format!(
                    "fanotify_init failed for {} (requires Linux 5.9+ kernel)",
                    canonical.display()
                )
            })?;

            let (is_fs_mark, _) = match fanotify_mark(
                &new_fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                path_mask,
                AT_FDCWD,
                canonical,
            ) {
                Ok(()) => {
                    eprintln!(
                        "[INFO] Monitoring {} (filesystem mark) on fd {}",
                        canonical.display(),
                        new_fd.as_raw_fd()
                    );
                    (true, true)
                }
                Err(FanotifyError::Mark(code)) if code == libc::EXDEV => {
                    match mark_directory(&new_fd, path_mask, canonical) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Monitoring {} (inode mark) on fd {}",
                                canonical.display(),
                                new_fd.as_raw_fd()
                            );
                            let opts = self.paths.get(i).and_then(|p| self.path_options.get(p));
                            if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                                mark_recursive(&new_fd, path_mask, canonical);
                            }
                            (false, true)
                        }
                        Err(e) => {
                            eprintln!(
                                "[WARNING] Cannot monitor {} (inode mark): {:#}",
                                canonical.display(),
                                e
                            );
                            drop(new_fd);
                            // Skip this path, continue to next
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    drop(new_fd);
                    continue;
                }
            };

            if !is_fs_mark {
                // Need to check if this path should have been set up fine above
                // (inode mark branch handles it)
            }

            // Open directory fd for open_by_handle_at
            let mount_fd_raw = nix::fcntl::open(
                canonical,
                nix::fcntl::OFlag::O_DIRECTORY,
                nix::sys::stat::Mode::empty(),
            );
            let mount_fd = match mount_fd_raw {
                Ok(raw) => unsafe { OwnedFd::from_raw_fd(raw) },
                Err(e) => {
                    eprintln!(
                        "[WARNING] Could not open directory fd for {}: {}",
                        canonical.display(),
                        e
                    );
                    drop(new_fd);
                    continue;
                }
            };

            let gi = self.fs_groups.len();
            self.fs_groups.push(FsGroup {
                dev_id,
                is_fs_mark,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });
            fs_group_devs.push(dev_id);
            self.path_to_group.insert(self.paths[i].clone(), gi);
        }

        let fan_group_count = self.fs_groups.len();

        if fan_group_count > 0 {
            // Pre-cache directory handles (shared across fds)
            for (i, canonical) in self.canonical_paths.iter().enumerate() {
                if canonical.is_dir() {
                    let opts = self.paths.get(i).and_then(|p| self.path_options.get(p));
                    let recursive = opts.is_some_and(|o| o.recursive);
                    if recursive {
                        dir_cache::cache_recursive(&self.dir_cache, canonical);
                    } else {
                        dir_cache::cache_dir_handle(&self.dir_cache, canonical);
                    }
                }
            }
        } else if self.pending_paths.is_empty() {
            eprintln!("No paths configured. Waiting for socket commands (use 'fsmon add <path>').");
        }

        // Ensure log directory exists and is owned by the original user
        if let Some(ref dir) = self.log_dir {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create log directory {}", dir.display()))?;
            // Daemon runs as root; chown to the original user so they own their logs
            match chown_to_user(dir) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!("[WARNING] Log directory '{}' is on a filesystem that does not support\n         ownership changes (e.g. vfat/exfat/NFS). Log files will remain owned by root.\n         Run 'sudo fsmon clean' if you cannot clean logs as a normal user.", dir.display());
                }
                Err(e) => {
                    eprintln!("[WARNING] Could not chown log directory '{}': {}.\n         Log files may remain owned by root.", dir.display(), e);
                }
            }
        }

        println!("Starting file trace monitor...");
        if !self.canonical_paths.is_empty() {
            println!(
                "Active paths: {}",
                self.canonical_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            println!("  FDs: {} file-descriptor(s)", fan_group_count);
        }
        if !self.pending_paths.is_empty() {
            println!(
                "Pending paths (waiting for directory creation): {}",
                self.pending_paths
                    .iter()
                    .map(|(p, _)| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }

        // Spawn one reader task per FsGroup (one per filesystem).
        // Events are sent through an unbounded mpsc channel to the main loop.
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<FidEvent>>();
        let dir_cache = Arc::new(std::mem::take(&mut self.dir_cache));
        let buf_size = self.buffer_size;

        // Shared state for live-add (add_path may need to spawn reader tasks)
        self.event_tx = Some(event_tx.clone());
        self.shared_dir_cache = Some(Arc::clone(&dir_cache));

        for gi in 0..self.fs_groups.len() {
            // Duplicate both fds so reader task owns independent copies
            let dup_fan_raw = unsafe { libc::dup(self.fs_groups[gi].fan_fd.as_raw_fd()) };
            if dup_fan_raw < 0 {
                eprintln!(
                    "[ERROR] Failed to dup fanotify fd {}: {}",
                    self.fs_groups[gi].fan_fd.as_raw_fd(),
                    std::io::Error::last_os_error()
                );
                continue;
            }
            let dup_mount_raw = unsafe { libc::dup(self.fs_groups[gi].mount_fd.as_raw_fd()) };
            if dup_mount_raw < 0 {
                eprintln!(
                    "[ERROR] Failed to dup mount fd {}: {}",
                    self.fs_groups[gi].mount_fd.as_raw_fd(),
                    std::io::Error::last_os_error()
                );
                unsafe { libc::close(dup_fan_raw); }
                continue;
            }
            // SAFETY: dup returned valid new fds, wrap them in OwnedFd
            let owned_fan_fd = unsafe { OwnedFd::from_raw_fd(dup_fan_raw) };
            let owned_mount_fd = unsafe { OwnedFd::from_raw_fd(dup_mount_raw) };
            let mfds = Arc::new(vec![owned_mount_fd]);
            let tx = event_tx.clone();
            let dc = Arc::clone(&dir_cache);
            let raw_fd = dup_fan_raw;
            tokio::spawn(async move {
                let afd = match AsyncFd::new(owned_fan_fd) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("[ERROR] AsyncFd for fd {}: {}", raw_fd, e);
                        return;
                    }
                };
                let mut buf = vec![0u8; buf_size];
                loop {
                    let result = afd.readable().await;
                    let mut guard = match result {
                        Ok(g) => g,
                        Err(e) => {
                            eprintln!("[ERROR] fd {} readable: {}", raw_fd, e);
                            break;
                        }
                    };
                    let events =
                        read_fid_events_dashmap(afd.get_ref(), &mfds, dc.as_ref(), &mut buf);
                    if !events.is_empty() && tx.send(events).is_err() {
                        break;
                    }
                    guard.clear_ready();
                }
            });
        }

        let mut sigterm =
            signal(SignalKind::terminate()).context("failed to create SIGTERM signal handler")?;
        let mut sighup =
            signal(SignalKind::hangup()).context("failed to create SIGHUP signal handler")?;

        let socket_listener = self.socket_listener.take();

        // Build inotify AsyncFd for tokio event loop
        let inotify_async = self.inotify.as_ref().map(|ino| {
            let fd = ino.as_raw_fd();
            AsyncFd::new(FanFd(fd)).expect("inotify AsyncFd")
        });

        loop {
            tokio::select! {
                Some(events) = event_rx.recv() => {
                    for raw in &events {
                        if raw.mask & FAN_Q_OVERFLOW != 0 {
                            eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                            continue;
                        }

                        let event_types = mask_to_event_types(raw.mask);
                        let matched_path = self.matching_path(&raw.path).cloned();

                        // If a managed directory was deleted, move to pending_paths
                        let is_delete_self = event_types.contains(&EventType::DeleteSelf)
                            || event_types.contains(&EventType::MovedFrom);
                        let is_canonical_root = is_delete_self
                            && self.canonical_paths.iter().any(|cp| cp == &raw.path);
                        if is_canonical_root {
                            if let Some(ref path) = matched_path {
                                // Preserve options before removing
                                let opts = self.path_options.get(path);
                                let pending_entry = PathEntry {
                                    path: path.clone(),
                                    recursive: opts.map(|o| o.recursive),
                                    types: opts.and_then(|o| o.event_types.as_ref().map(
                                        |v| v.iter().map(|t| t.to_string()).collect()
                                    )),
                                    min_size: opts.and_then(|o| o.min_size.map(|s| s.to_string())),
                                    exclude: opts.and_then(|o| o.exclude_regex.as_ref().map(|r| vec![r.as_str().to_string()])),
                                    exclude_cmd: None,
                                };
                                if let Err(e) = self.remove_path(path) {
                                    eprintln!("[WARNING] Failed to remove deleted path '{}': {e}", path.display());
                                }
                                self.pending_paths.push((path.clone(), pending_entry));
                                self.setup_inotify_watches();
                            }
                            continue;
                        }

                        for event_type in event_types {
                            let event = self.build_file_event(raw, event_type, matched_path.as_deref());

                            if !self.is_path_in_scope(&event.path, &self.canonical_paths) {
                                continue;
                            }

                            if self.should_output(&event)
                                && let Err(e) = self.write_event(&event)
                            {
                                eprintln!("[ERROR] Failed to write event: {}", e);
                            }
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                _ = sigterm.recv() => {
                    break;
                }
                _ = sighup.recv() => {
                    if let Err(e) = self.reload_config() {
                        eprintln!("Config reload error: {e}");
                    }
                }
                inotify_ready = async {
                    match inotify_async.as_ref() {
                        Some(afd) => afd.readable().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok(mut guard) = inotify_ready {
                        if let Some(ref mut inotify) = self.inotify {
                            // Drain all pending inotify events
                            let mut buf = [0u8; 4096];
                            let _ = inotify.read_events(&mut buf);
                            // inotify doesn't tell us which pending path was created,
                            // so just check all of them
                            self.check_pending();
                        }
                        guard.clear_ready();
                    }
                }
                accept_result = async {
                    match socket_listener.as_ref() {
                        Some(listener) => {
                            let (stream, _) = listener.accept().await?;
                            let (reader, writer) = stream.into_split();
                            let mut buf_reader = tokio::io::BufReader::new(reader);
                            let mut message = String::new();
                            loop {
                                let mut line = String::new();
                                let bytes = buf_reader.read_line(&mut line).await?;
                                if bytes == 0 {
                                    break;
                                }
                                if line.trim().is_empty() && !message.is_empty() {
                                    break;
                                }
                                message.push_str(&line);
                            }
                            Ok::<(tokio::net::unix::OwnedWriteHalf, String), std::io::Error>((writer, message.trim().to_string()))
                        }
                        None => std::future::pending().await,
                    }
                } => {
                    match accept_result {
                        Ok((mut writer, cmd_str)) => {
                            let resp = match toml::from_str::<SocketCmd>(&cmd_str) {
                                Ok(cmd) => self.handle_socket_cmd(cmd),
                                Err(e) => SocketResp::err(format!("Invalid command: {e}")),
                            };
                            if let Ok(toml_str) = toml::to_string(&resp) {
                                let resp_bytes = format!("{toml_str}\n");
                                let _ = writer.write_all(resp_bytes.as_bytes()).await;
                            }
                        }
                        Err(e) => eprintln!("Socket accept error: {e}"),
                    }
                }
            }
        }

        println!("\nStopping file trace monitor...");
        // event_rx drops here → channel closed → reader tasks exit on next event
        // OS cleans up all fds on process exit
        Ok(())
    }

    fn build_file_event(
        &mut self,
        raw: &FidEvent,
        event_type: EventType,
        matched_path: Option<&Path>,
    ) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = if let Some(info) = self.pid_cache.get(&pid) {
            (info.cmd.clone(), info.user.clone())
        } else {
            let (cmd, user) =
                get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());
            // Cache successfully resolved info for reuse
            if cmd != "unknown" || user != "unknown" {
                self.pid_cache.put(
                    pid,
                    ProcInfo {
                        cmd: cmd.clone(),
                        user: user.clone(),
                    },
                );
            }
            (cmd, user)
        };

        let file_size = match event_type {
            // For CREATE/MODIFY/CLOSE_WRITE: get actual size and cache it
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(&raw.path).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.put(raw.path.clone(), size);
                size
            }
            // For DELETE/DELETE_SELF/MOVED_FROM: use cached size (file already gone)
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.pop(&raw.path).unwrap_or(0)
            }
            // For other events (OPEN, ACCESS, ATTRIB, etc.): use cached size if available
            _ => self.file_size_cache.get(&raw.path).map_or(0, |&s| s ),
        };

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path.clone(),
            pid,
            cmd,
            user,
            file_size,
            monitored_path: matched_path.map_or(PathBuf::new(), |p| p.to_path_buf()),
        }
    }

    fn get_matching_path_options(&self, path: &Path) -> Option<&PathOptions> {
        for watched in &self.paths {
            if let Some(opts) = self.path_options.get(watched) {
                if opts.recursive {
                    if path.starts_with(watched) {
                        return Some(opts);
                    }
                } else if path == watched.as_path() || path.parent() == Some(watched.as_path()) {
                    return Some(opts);
                }
            }
        }
        // Fallback: match against canonical paths (handles symlinks/bind-mounts)
        for (i, canonical) in self.canonical_paths.iter().enumerate() {
            if let Some(orig) = self.paths.get(i)
                && let Some(opts) = self.path_options.get(orig)
            {
                if opts.recursive {
                    if path.starts_with(canonical) {
                        return Some(opts);
                    }
                } else if path == canonical.as_path() || path.parent() == Some(canonical.as_path())
                {
                    return Some(opts);
                }
            }
        }
        None
    }



    /// Spawn a tokio reader task for `group_idx` in `fs_groups`.
    /// Both the fanotify fd and mount fd are duplicated so the reader task
    /// owns independent copies, avoiding double-close with Monitor's OwnedFd.
    fn spawn_fd_reader(&mut self, group_idx: usize) {
        let tx = match self.event_tx.as_ref() {
            Some(t) => t.clone(),
            None => {
                eprintln!("[ERROR] Cannot spawn reader: event_tx not initialized");
                return;
            }
        };
        let dc = match self.shared_dir_cache.as_ref() {
            Some(d) => Arc::clone(d),
            None => {
                eprintln!("[ERROR] Cannot spawn reader: shared_dir_cache not initialized");
                return;
            }
        };
        let buf_size = self.buffer_size;
        let group = &self.fs_groups[group_idx];

        // Duplicate fds so the reader task owns independent copies
        let dup_fan_raw = unsafe { libc::dup(group.fan_fd.as_raw_fd()) };
        if dup_fan_raw < 0 {
            eprintln!(
                "[ERROR] Failed to dup fanotify fd {}: {}",
                group.fan_fd.as_raw_fd(),
                std::io::Error::last_os_error()
            );
            return;
        }
        let dup_mount_raw = unsafe { libc::dup(group.mount_fd.as_raw_fd()) };
        if dup_mount_raw < 0 {
            eprintln!(
                "[ERROR] Failed to dup mount fd {}: {}",
                group.mount_fd.as_raw_fd(),
                std::io::Error::last_os_error()
            );
            unsafe { libc::close(dup_fan_raw); }
            return;
        }

        // SAFETY: dup returned valid new fds, wrap in OwnedFd
        let owned_fan_fd = unsafe { OwnedFd::from_raw_fd(dup_fan_raw) };
        let owned_mount_fd = unsafe { OwnedFd::from_raw_fd(dup_mount_raw) };
        let mfds = Arc::new(vec![owned_mount_fd]);
        let raw_fd = dup_fan_raw;

        tokio::spawn(async move {
            let afd = match AsyncFd::new(owned_fan_fd) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[ERROR] AsyncFd for fd {}: {}", raw_fd, e);
                    return;
                }
            };
            let mut buf = vec![0u8; buf_size];
            loop {
                let result = afd.readable().await;
                let mut guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("[ERROR] fd {} readable: {}", raw_fd, e);
                        break;
                    }
                };
                let events =
                    read_fid_events_dashmap(afd.get_ref(), &mfds, dc.as_ref(), &mut buf);
                if !events.is_empty() && tx.send(events).is_err() {
                    break;
                }
                guard.clear_ready();
            }
        });
    }

    /// Build a combined regex from a list of patterns.
    fn build_exclude_regex(patterns: Option<&[String]>, label: &str) -> Result<(Option<regex::Regex>, bool)> {
        let Some(patterns) = patterns else { return Ok((None, false)); };
        if patterns.is_empty() { return Ok((None, false)); }
        let invert = patterns[0].starts_with('!');
        let parts: Vec<String> = patterns.iter().map(|p| {
            let raw = p.strip_prefix('!').unwrap_or(p);
            if label == "--exclude-cmd" {
                raw.replace("*", ".*")
            } else {
                regex::escape(raw).replace("\\*", ".*")
            }
        }).collect();
        let regex = regex::Regex::new(&parts.join("|"))
            .with_context(|| format!("invalid {} pattern", label))?;
        Ok((Some(regex), invert))
    }

    pub fn add_path(&mut self, entry: &PathEntry) -> Result<()> {
        // Normalize path: expand tilde + resolve symlinks/../.
        // Managed the shortest canonical form so all comparisons work consistently.
        let path = resolve_recursion_check(&entry.path);

        if self.path_options.contains_key(&path) {
            bail!("Path already being monitored: {}", path.display());
        }

        // Reject paths that would cause infinite recursion (log dir inside monitored path)
        if let Some(ref log_dir) = self.log_dir
            && log_dir.canonicalize().unwrap_or_else(|_| log_dir.clone()).starts_with(&path)
        {
            bail!(
                "Cannot monitor '{}': log directory '{}' is inside this path — \
                 every log write would trigger a new event, causing infinite recursion.\n\
                 Tip: exclude the log directory with --exclude or use a different logging.dir",
                path.display(),
                log_dir.display()
            );
        }

        if !path.exists() {
            eprintln!(
                "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                path.display()
            );
            self.pending_paths.push((path.clone(), entry.clone()));
            self.setup_inotify_watches();
            return Ok(());
        }

        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

        let event_types = entry.types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|s| s.parse::<EventType>().ok())
                .collect()
        });
        let min_size = entry.min_size.as_ref().map(|s| parse_size(s)).transpose()?;
        let (exclude_regex, exclude_invert) = Self::build_exclude_regex(entry.exclude.as_deref(), "exclude")?;
        let (exclude_cmd_regex, exclude_cmd_invert) = Self::build_exclude_regex(entry.exclude_cmd.as_deref(), "--exclude-cmd")?;
        let recursive = entry.recursive.unwrap_or(false);
        let opts = PathOptions {
            min_size,
            event_types,
            exclude_regex,
            exclude_invert,
            exclude_cmd_regex,
            exclude_cmd_invert,
            recursive,
        };

        let path_mask = path_mask_from_options(&opts);

        println!(
            "Added path: {} (recursive={})",
            path.display(),
            recursive,
        );

        // Determine filesystem device ID for dedup lookup
        let dev_id = std::fs::metadata(&canonical)
            .ok()
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
            .unwrap_or(0);

        // Find existing FsGroup for this filesystem
        let existing_idx = self.fs_groups.iter().position(|g| g.dev_id == dev_id);

        let group_idx = if let Some(idx) = existing_idx {
            // Reuse existing group — just add inode mark if needed
            if !self.fs_groups[idx].is_fs_mark {
                let fan_fd = &self.fs_groups[idx].fan_fd;
                if let Err(e) = mark_directory(fan_fd, path_mask, &canonical) {
                    eprintln!(
                        "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                        canonical.display(),
                        fan_fd.as_raw_fd(),
                        e
                    );
                } else {
                    if recursive && canonical.is_dir() {
                        mark_recursive(fan_fd, path_mask, &canonical);
                    }
                }
            }
            self.fs_groups[idx].ref_count += 1;
            eprintln!(
                "[INFO] Monitoring {} on existing fd {}",
                canonical.display(),
                self.fs_groups[idx].fan_fd.as_raw_fd()
            );
            idx
        } else {
            // New filesystem — create fanotify fd + mount fd
            let new_fd = fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            )
            .with_context(|| {
                format!(
                    "fanotify_init failed for {} (requires Linux 5.9+ kernel)",
                    canonical.display()
                )
            })?;

            let is_fs_mark = match fanotify_mark(
                &new_fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                path_mask,
                AT_FDCWD,
                &canonical,
            ) {
                Ok(()) => {
                    eprintln!(
                        "[INFO] Monitoring {} (fs mark) on new fd {}",
                        canonical.display(),
                        new_fd.as_raw_fd()
                    );
                    true
                }
                Err(FanotifyError::Mark(code)) if code == libc::EXDEV => {
                    // Fall back to inode mark
                    match mark_directory(&new_fd, path_mask, &canonical) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Monitoring {} (inode mark) on new fd {}",
                                canonical.display(),
                                new_fd.as_raw_fd()
                            );
                            if recursive && canonical.is_dir() {
                                mark_recursive(&new_fd, path_mask, &canonical);
                            }
                            false
                        }
                        Err(e) => {
                            eprintln!(
                                "[WARNING] Cannot monitor {} (inode mark): {:#}",
                                canonical.display(),
                                e
                            );
                            drop(new_fd);
                            bail!("Failed to mark {}: {:#}", canonical.display(), e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    drop(new_fd);
                    bail!("Failed to mark {}: {:#}", canonical.display(), e);
                }
            };

            // Open directory fd for handle resolution
            let mount_fd_raw = nix::fcntl::open(
                &canonical,
                nix::fcntl::OFlag::O_DIRECTORY,
                nix::sys::stat::Mode::empty(),
            )?;
            let mount_fd = unsafe { OwnedFd::from_raw_fd(mount_fd_raw) };

            let idx = self.fs_groups.len();
            self.fs_groups.push(FsGroup {
                dev_id,
                is_fs_mark,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });

            // Spawn reader for this new group
            self.spawn_fd_reader(idx);
            idx
        };

        // Update path tracking
        self.path_to_group.insert(path.clone(), group_idx);
        self.paths.push(path.clone());
        self.canonical_paths.push(canonical.clone());
        self.path_options.insert(path.clone(), opts);

        // Pre-cache directory handles in the shared cache
        if canonical.is_dir()
            && let Some(ref cache) = self.shared_dir_cache
        {
            if recursive {
                dir_cache::cache_recursive(cache.as_ref(), &canonical);
            } else {
                dir_cache::cache_dir_handle(cache.as_ref(), &canonical);
            }
        }

        Ok(())
    }

    pub fn remove_path(&mut self, path: &Path) -> Result<()> {
        let pos = self
            .paths
            .iter()
            .position(|p| p == path)
            .ok_or_else(|| anyhow::anyhow!("Path not being monitored: {}", path.display()))?;

        let canonical = &self.canonical_paths[pos];
        let opts = self
            .path_options
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("No options for path: {}", path.display()))?;
        let path_mask = path_mask_from_options(opts);

        // Look up which FsGroup this path belongs to
        if let Some(&gi) = self.path_to_group.get(path) {
            // Remove fanotify mark
            let fan_fd = &self.fs_groups[gi].fan_fd;
            let _ = fanotify_mark(
                fan_fd,
                FAN_MARK_REMOVE | FAN_MARK_FILESYSTEM,
                path_mask,
                AT_FDCWD,
                canonical,
            );
            let _ = fanotify_mark(fan_fd, FAN_MARK_REMOVE, path_mask, AT_FDCWD, canonical);

            // Decrement ref_count; if zero, drop the entire FsGroup (close both fds)
            self.fs_groups[gi].ref_count = self.fs_groups[gi].ref_count.saturating_sub(1);
            if self.fs_groups[gi].ref_count == 0 {
                self.fs_groups.remove(gi);
                // Shift indices in path_to_group for groups after the removed one
                self.path_to_group.iter_mut().for_each(|(_, idx)| {
                    if *idx > gi {
                        *idx -= 1;
                    }
                });
            }
        }

        self.paths.remove(pos);
        self.canonical_paths.remove(pos);
        self.path_options.remove(path);
        self.path_to_group.remove(path);

        println!("Removed path: {}", path.display());
        Ok(())
    }

    fn handle_socket_cmd(&mut self, cmd: SocketCmd) -> SocketResp {
        match cmd.cmd.as_str() {
            "add" => {
                let raw = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                let path = raw;
                // Always remove first (no-op if not monitored), then add.
                // This keeps the state machine simple: add_path always creates
                // fresh fanotify marks and caches regardless of prior state.
                let _ = self.remove_path(&path);
                let entry = PathEntry {
                    path,
                    recursive: cmd.recursive,
                    types: cmd.types.clone(),
                    min_size: cmd.min_size.clone(),
                    exclude: cmd.exclude.clone(),
                    exclude_cmd: cmd.exclude_cmd.clone(),
                };
                match self.add_path(&entry) {
                    Ok(()) => {
                        SocketResp::ok()
                    }
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    },
                }
            }
            "remove" => {
                let path = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                match self.remove_path(&path) {
                    Ok(()) => {
                        SocketResp::ok()
                    }
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    },
                }
            }
            "list" => {
                let paths: Vec<PathEntry> = self
                    .paths
                    .iter()
                    .map(|p| {
                        let opts = self.path_options.get(p);
                        PathEntry {
                            path: p.clone(),
                            recursive: opts.map(|o| o.recursive),
                            types: opts.and_then(|o| {
                                o.event_types
                                    .as_ref()
                                    .map(|v| v.iter().map(|t| t.to_string()).collect())
                            }),
                            min_size: opts.and_then(|o| o.min_size.map(|s| s.to_string())),
                            exclude: opts.and_then(|o| {
                                o.exclude_regex.as_ref().map(|r| vec![r.as_str().to_string()])
                            }),
                            exclude_cmd: None,
                        }
                    })
                    .collect();
                SocketResp {
                    ok: true,
                    error: None,
                    error_kind: None,
                    paths: Some(paths),
                }
            }
            _ => SocketResp::err(format!("Unknown command: {}", cmd.cmd)),
        }
    }

    fn reload_config(&mut self) -> Result<()> {
        let managed_path = self
            .managed_path
            .as_ref()
            .context("No store path configured")?;
        let store = Managed::load(managed_path)?;
        // Add new paths that appear in store
        for entry in &store.entries {
            if !self.path_options.contains_key(&entry.path)
                && let Err(e) = self.add_path(entry)
            {
                eprintln!("Failed to add path {} on reload: {e}", entry.path.display());
            }
        }
        // Remove paths no longer in store
        let current_paths: Vec<PathBuf> = self.paths.clone();
        for path in &current_paths {
            if !store.entries.iter().any(|p| p.path == *path)
                && let Err(e) = self.remove_path(path)
            {
                eprintln!("Failed to remove path {} on reload: {e}", path.display());
            }
        }

        Ok(())
    }

    /// Find the deepest existing ancestor directory of a path.
    /// Walks up until it finds a directory that exists, or returns None.
    fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
        let mut p = path.to_path_buf();
        loop {
            if p.is_dir() {
                return Some(p);
            }
            if !p.pop() {
                return None;
            }
        }
    }

    /// Set up inotify watches on parent directories of all pending paths.
    /// Removes stale watches first.
    fn setup_inotify_watches(&mut self) {
        use inotify::WatchMask;

        // Drop old watches
        self._inotify_watches.clear();

        let inotify = match self.inotify.as_ref() {
            Some(ino) => ino,
            None => return,
        };

        for (path, _) in &self.pending_paths {
            if let Some(parent) = Self::nearest_existing_ancestor(path)
                && let Ok(wd) = inotify.watches().add(
                    &parent,
                    WatchMask::CREATE | WatchMask::MOVED_TO,
                )
            {
                self._inotify_watches.push(wd);
            }
        }
    }

    /// Retry setting up fanotify monitoring for paths that didn't exist before.
    /// Called when inotify detects directory creation under a watched parent.
    fn check_pending(&mut self) {
        let mut i = 0;
        while i < self.pending_paths.len() {
            let (path, _) = &self.pending_paths[i];
            if path.exists() {
                let entry = self.pending_paths.swap_remove(i);
                match self.add_path(&entry.1) {
                    Ok(()) => {
                        eprintln!(
                            "[INFO] Path '{}' now exists — monitoring started.",
                            entry.0.display()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[WARNING] Path '{}' exists but monitoring setup failed: {e}",
                            entry.0.display()
                        );
                        i += 1;
                    }
                }
            } else {
                i += 1;
            }
        }

        // Refresh inotify watches for remaining pending paths
        self.setup_inotify_watches();
    }

    fn should_output(&self, event: &FileEvent) -> bool {
        let opts = match self.get_matching_path_options(&event.path) {
            Some(o) => o,
            None => return true,
        };

        if let Some(ref types) = opts.event_types
            && !types.contains(&event.event_type)
        {
            return false;
        }

        if let Some(min) = opts.min_size
            && event.file_size < min as u64
        {
            return false;
        }

        if let Some(ref regex) = opts.exclude_regex {
            let matched = regex.is_match(&event.path.to_string_lossy());
            if opts.exclude_invert {
                if !matched { return false; }
            } else if matched {
                return false;
            }
        }

        if let Some(ref regex) = opts.exclude_cmd_regex {
            let matched = regex.is_match(&event.cmd);
            if opts.exclude_cmd_invert {
                if !matched { return false; }
            } else if matched {
                return false;
            }
        }

        true
    }

    /// Find the configured path that matches a given event path.
    /// Checks configured paths (direct or recursive prefix), then canonical paths.
    fn matching_path(&self, path: &Path) -> Option<&PathBuf> {
        // Direct match first: find the configured PathBuf that matches this path
        for watched in &self.paths {
            if watched == path && self.path_options.contains_key(watched) {
                return Some(watched);
            }
        }
        // Recursive match: find watched path that is a prefix of event path
        for watched in self.path_options.keys() {
            if path.starts_with(watched) {
                return Some(watched);
            }
        }
        // Fallback: match against canonical paths (handles symlinks/bind-mounts)
        for (i, canonical) in self.canonical_paths.iter().enumerate() {
            if (path == canonical.as_path() || path.starts_with(canonical))
                && let Some(orig) = self.paths.get(i)
            {
                return Some(orig);
            }
        }
        None
    }

    /// Write an event to its path-based log file.
    fn write_event(&self, event: &FileEvent) -> std::io::Result<()> {
        let log_dir = match self.log_dir.as_ref() {
            Some(d) => d,
            None => return Ok(()),
        };
        let matched_path = &event.monitored_path;
        let log_path = log_dir.join(crate::utils::path_to_log_name(matched_path));
        let is_new = !log_path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        // Chown new log files to the original user so they own everything in their ~
        if is_new {
            match chown_to_user(&log_path) {
                Ok(true) => {}
                Ok(false) => {
                    // one-time warning already emitted in run() for the log directory
                }
                Err(e) => {
                    eprintln!("[WARNING] Could not chown log file '{}': {}",
                        log_path.display(), e);
                }
            }
        }
        writeln!(file, "{}", event.to_jsonl_string())?;
        Ok(())
    }

    /// Check if path is within monitoring scope
    /// Uses per-path recursive setting from path_options
    fn is_path_in_scope(&self, path: &Path, canonical_paths: &[PathBuf]) -> bool {
        for (i, watched) in canonical_paths.iter().enumerate() {
            let recursive = self
                .paths
                .get(i)
                .and_then(|p| self.path_options.get(p))
                .map(|o| o.recursive)
                .unwrap_or(false);
            if recursive {
                if path.starts_with(watched) {
                    return true;
                }
            } else if path == watched.as_path() || path.parent() == Some(watched.as_path()) {
                return true;
            }
        }
        false
    }
}

// ---- Directory marking (used by inode mark fallback mode) ----

/// Mark a single directory. Strips FAN_FS_ERROR (only works with FS marks).
fn mark_directory(fan_fd: &OwnedFd, mask: u64, path: &Path) -> Result<()> {
    let safe_mask = mask & !FAN_FS_ERROR;
    fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path)
        .with_context(|| format!("fanotify_mark failed: {}", path.display()))
}

/// Recursively traverse and mark all subdirectories (ignore errors, e.g., permission denied).
/// Strips FAN_FS_ERROR (only works with FS marks).
fn mark_recursive(fan_fd: &OwnedFd, mask: u64, dir: &Path) {
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
    use std::sync::Arc;
    use fanotify_fid::consts::{FAN_CREATE, FAN_DELETE, FAN_EVENT_ON_CHILD, FAN_MODIFY, FAN_ONDIR};

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
        use fanotify_fid::consts::{
            FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE,
            FAN_DELETE_SELF, FAN_FS_ERROR, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO,
            FAN_OPEN, FAN_OPEN_EXEC,
        };
        let mask = FAN_ACCESS | FAN_MODIFY | FAN_CLOSE_WRITE | FAN_CLOSE_NOWRITE
            | FAN_OPEN | FAN_OPEN_EXEC | FAN_ATTRIB | FAN_CREATE | FAN_DELETE
            | FAN_DELETE_SELF | FAN_FS_ERROR | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 14);
    }

    #[test]
    fn test_mask_to_event_types_with_flags() {
        let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }

    // ---- Monitor tests ----

    fn options(
        min_size: Option<i64>,
        event_types: Option<Vec<EventType>>,
        exclude: Option<&str>,
        recursive: bool,
    ) -> PathOptions {
        let exclude_regex = exclude.map(|p| {
            let escaped = regex::escape(p);
            let pattern = escaped.replace("\\*", ".*").replace("\\|", "|");
            regex::Regex::new(&pattern).expect("invalid exclude pattern")
        });
        PathOptions {
            min_size,
            event_types,
            exclude_regex,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive,
        }
    }

    fn make_monitor(
        paths: Vec<&str>,
        min_size: Option<i64>,
        event_types: Option<Vec<EventType>>,
        exclude: Option<&str>,
        recursive: bool,
    ) -> Monitor {
        Monitor::new(
            paths
                .into_iter()
                .map(|p| {
                    (
                        PathBuf::from(p),
                        options(
                            min_size,
                            event_types.clone(),
                            exclude,
                            recursive,
                        ),
                    )
                })
                .collect(),
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn test_should_output_no_filters() {
        let m = make_monitor(vec!["/tmp"], None, None, None, false);
        let event = make_event("/tmp/test.txt", EventType::Create, 1000, 1024);
        assert!(m.should_output(&event));
    }

    #[test]
    fn test_should_output_type_filter_match() {
        let m = make_monitor(
            vec!["/tmp"],
            None,
            Some(vec![EventType::Create, EventType::Delete]),
            None,
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/a", EventType::Delete, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Modify, 1, 0)));
    }

    #[test]
    fn test_should_output_min_size_filter() {
        let m = make_monitor(vec!["/tmp"], Some(1000), None, None, false);
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 2000)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, 500)));
    }

    #[test]
    fn test_should_output_exclude_pattern() {
        let m = make_monitor(vec!["/tmp"], None, None, Some("*.tmp"), false);
        assert!(!m.should_output(&make_event("/tmp/test.tmp", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/foo.tmp", EventType::Delete, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_exact_pattern() {
        let m = make_monitor(vec!["/tmp"], None, None, Some("test.tmp"), false);
        assert!(m.should_output(&make_event("/tmp/test.txt", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/test.tmp", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/foo.tmp", EventType::Delete, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/testXtmp", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_combined_filters() {
        let m = make_monitor(
            vec!["/tmp"],
            Some(100),
            Some(vec![EventType::Create]),
            Some("*.log"),
            false,
        );
        assert!(m.should_output(&make_event("/tmp/data", EventType::Create, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Delete, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Create, 1, 50)));
        assert!(!m.should_output(&make_event("/tmp/app.log", EventType::Create, 1, 200)));
    }

    #[test]
    fn test_is_path_in_scope_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, None, true);
        let watched = vec![PathBuf::from("/tmp")];
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub/deep/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/tmpfile"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, None, false);
        let watched = vec![PathBuf::from("/tmp")];
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/tmp/sub/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let m = make_monitor(vec!["/tmp", "/var/log"], None, None, None, true);
        let watched = vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")];
        assert!(m.is_path_in_scope(Path::new("/tmp/file"), &watched));
        assert!(m.is_path_in_scope(Path::new("/var/log/syslog"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/etc/passwd"), &watched));
    }

    #[test]
    fn test_should_output_exclude_pipe_multiple() {
        // --exclude "*.tmp|*.log" → excludes both .tmp and .log
        let m = make_monitor_exclude(Some("*.tmp|*.log"), None, false, false);
        assert!(!m.should_output(&make_event("/tmp/a.tmp", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a.log", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/a.txt", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_invert() {
        // --exclude "!*.py" → only .py files pass
        let m = make_monitor_exclude(Some("!*.py"), None, false, false);
        assert!(m.should_output(&make_event("/tmp/main.py", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/main.rs", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a.txt", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_cmd_basic() {
        // --exclude-cmd "rsync" → excludes rsync
        let m = make_monitor_exclude(None, Some("rsync"), false, false);
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 1, 0, "rsync")));
        assert!(m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 2, 0, "nginx")));
    }

    #[test]
    fn test_should_output_exclude_cmd_pipe() {
        // --exclude-cmd "rsync|apt" → excludes both
        let m = make_monitor_exclude(None, Some("rsync|apt"), false, false);
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 1, 0, "rsync")));
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 2, 0, "apt")));
        assert!(m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 3, 0, "nginx")));
    }

    #[test]
    fn test_should_output_exclude_cmd_invert() {
        // --exclude-cmd "!nginx" → only nginx passes
        let m = make_monitor_exclude(None, Some("!nginx"), false, false);
        assert!(m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 1, 0, "nginx")));
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 2, 0, "rsync")));
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 3, 0, "apt")));
    }

    #[test]
    fn test_should_output_exclude_cmd_invert_multi() {
        // --exclude-cmd "!nginx|python" → only nginx and python pass
        let m = make_monitor_exclude(None, Some("!nginx|python"), false, false);
        assert!(m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 1, 0, "nginx")));
        assert!(m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 2, 0, "python")));
        assert!(!m.should_output(&make_event_cmd("/tmp/a", EventType::Create, 3, 0, "rsync")));
    }

    #[test]
    fn test_should_output_exclude_and_exclude_cmd() {
        // --exclude "*.tmp" --exclude-cmd "rsync" → both filters
        let m = make_monitor_exclude(Some("*.tmp"), Some("rsync"), false, false);
        assert!(!m.should_output(&make_event_cmd("/tmp/a.tmp", EventType::Create, 1, 0, "vim")));
        assert!(!m.should_output(&make_event_cmd("/tmp/a.txt", EventType::Create, 1, 0, "rsync")));
        assert!(m.should_output(&make_event_cmd("/tmp/a.txt", EventType::Create, 2, 0, "vim")));
    }

    #[test]
    fn test_file_size_cache_eviction() {
        use lru::LruCache;
        use std::num::NonZeroUsize;

        let mut cache = LruCache::new(NonZeroUsize::new(3).unwrap());

        cache.put(PathBuf::from("/a"), 100);
        cache.put(PathBuf::from("/b"), 200);
        cache.put(PathBuf::from("/c"), 300);
        assert_eq!(cache.len(), 3);

        cache.put(PathBuf::from("/d"), 400);
        assert_eq!(cache.len(), 3);
        assert!(cache.get(&PathBuf::from("/a")).is_none());
        assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
        assert_eq!(cache.get(&PathBuf::from("/d")), Some(&400));

        cache.get(&PathBuf::from("/b"));
        cache.put(PathBuf::from("/e"), 500);
        assert!(cache.get(&PathBuf::from("/c")).is_none());
        assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
    }

    #[test]
    fn test_monitor_buffer_size_validation() {
        let opts = options(None, None, None, false);

        let result = Monitor::new(
            vec![(PathBuf::from("/tmp"), opts.clone())],
            None,
            None,
            Some(1024),
            None,
        );
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("at least 4096"));

        let result = Monitor::new(
            vec![(PathBuf::from("/tmp"), opts.clone())],
            None,
            None,
            Some(2 * 1024 * 1024),
            None,
        );
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("not exceed"));

        let result = Monitor::new(
            vec![(PathBuf::from("/tmp"), opts.clone())],
            None,
            None,
            Some(65536),
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_add_path_and_remove_path() {
        let mut m = Monitor::new(vec![], None, None, None, None).unwrap();

        let entry = PathEntry {
            path: PathBuf::from("/tmp/test_add"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
            exclude_cmd: None,
        };

        // add_path on non-existent path → goes to pending_paths
        let result = m.add_path(&entry);
        assert!(result.is_ok());
        assert!(m.pending_paths.iter().any(|(p, _)| p == Path::new("/tmp/test_add")));
        assert!(!m.path_options.contains_key(Path::new("/tmp/test_add")));

        // remove_path on non-existent path (not in options)
        let result = m.remove_path(Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    /// Build a Monitor with custom exclude/exclude_cmd patterns for testing.
    fn make_monitor_exclude(
        exclude: Option<&str>,
        exclude_cmd: Option<&str>,
        _exclude_invert: bool,
        _exclude_cmd_invert: bool,
    ) -> Monitor {
        let (exclude_regex, exclude_invert) = match exclude {
            Some(p) => {
                let raw = p.strip_prefix('!').unwrap_or(p);
                let escaped = regex::escape(raw);
                let pattern = escaped.replace("\\*", ".*").replace("\\|", "|");
                (Some(regex::Regex::new(&pattern).expect("invalid exclude pattern")), p.starts_with('!'))
            }
            None => (None, false),
        };
        let (exclude_cmd_regex, exclude_cmd_invert) = match exclude_cmd {
            Some(p) => {
                let raw = p.strip_prefix('!').unwrap_or(p);
                let pattern = raw.replace("*", ".*");
                (Some(regex::Regex::new(&pattern).expect("invalid exclude-cmd pattern")), p.starts_with('!'))
            }
            None => (None, false),
        };
        Monitor::new(
            vec![(
                PathBuf::from("/tmp"),
                PathOptions {
                    min_size: None,
                    event_types: None,
                    exclude_regex,
                    exclude_invert,
                    exclude_cmd_regex,
                    exclude_cmd_invert,
                    recursive: false,
                },
            )],
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn make_event(path: &str, event_type: EventType, pid: u32, size: u64) -> FileEvent {
        FileEvent {
            time: Utc::now(),
            event_type,
            path: PathBuf::from(path),
            pid,
            cmd: "test".to_string(),
            user: "root".to_string(),
            file_size: size,
            monitored_path: PathBuf::from("/watched"),
        }
    }

    fn make_event_cmd(path: &str, event_type: EventType, pid: u32, size: u64, cmd: &str) -> FileEvent {
        FileEvent {
            time: Utc::now(),
            event_type,
            path: PathBuf::from(path),
            pid,
            cmd: cmd.to_string(),
            user: "root".to_string(),
            file_size: size,
            monitored_path: PathBuf::from("/watched"),
        }
    }

    // ---- Integration tests (require sudo) ----

    #[test]
    #[ignore]
    fn test_fanotify_init() {
        let fd = fanotify_init(
            FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_CLASS_NOTIF
                | FAN_REPORT_FID
                | FAN_REPORT_DIR_FID
                | FAN_REPORT_NAME,
            (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
        );
        assert!(fd.is_ok(), "fanotify_init should succeed with root");
        // OwnedFd is closed on drop — no explicit close needed
    }

    #[test]
    #[ignore]
    fn test_fanotify_mark_directory() {
        let test_dir = std::env::temp_dir().join("fsmon_test_mark");
        std::fs::create_dir_all(&test_dir).unwrap();

        let fd = fanotify_init(
            FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_CLASS_NOTIF
                | FAN_REPORT_FID
                | FAN_REPORT_DIR_FID
                | FAN_REPORT_NAME,
            (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
        )
        .unwrap();

        let mask = FAN_CREATE | FAN_DELETE | FAN_CLOSE_WRITE;
        let result = fanotify_mark(
            &fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            AT_FDCWD,
            &test_dir,
        );
        assert!(
            result.is_ok(),
            "fanotify_mark should succeed on existing directory"
        );

        drop(fd);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    #[ignore]
    fn test_fanotify_mark_nonexistent_path() {
        let fd = fanotify_init(
            FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_CLASS_NOTIF
                | FAN_REPORT_FID
                | FAN_REPORT_DIR_FID
                | FAN_REPORT_NAME,
            (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
        )
        .unwrap();

        let mask = FAN_CREATE;
        let result = fanotify_mark(
            &fd,
            FAN_MARK_ADD,
            mask,
            AT_FDCWD,
            Path::new("/nonexistent_path_12345"),
        );
        assert!(
            result.is_err(),
            "fanotify_mark should fail on nonexistent path"
        );

        drop(fd);
    }

    #[test]
    #[ignore]
    fn test_monitor_run_captures_events() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let test_dir = std::env::temp_dir().join("fsmon_test_events");
        std::fs::create_dir_all(&test_dir).unwrap();
        let test_dir_for_cleanup = test_dir.clone();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let test_dir_clone = test_dir.clone();

        let handle = rt.spawn(async move {
            let fd = fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            )
            .unwrap();

            let mask = FAN_CREATE | FAN_CLOSE_WRITE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
            fanotify_mark(
                &fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                mask,
                AT_FDCWD,
                &test_dir_clone,
            )
            .unwrap();

            let mut buf = vec![0u8; 4096];
            let raw_fd = fd.as_raw_fd();
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(200) {
                let n = unsafe { libc::read(raw_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n > 0 {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            drop(fd);
        });

        std::thread::sleep(std::time::Duration::from_millis(50));

        for i in 0..3 {
            let path = test_dir.join(format!("test_{}.txt", i));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "content {}", i).unwrap();
        }

        rt.block_on(handle).unwrap();

        let events_captured = counter.load(Ordering::SeqCst);
        assert!(
            events_captured > 0,
            "Should capture at least some events, got {}",
            events_captured
        );

        let _ = std::fs::remove_dir_all(&test_dir_for_cleanup);
    }

    // ---- build_exclude_regex ----

    #[test]
    fn test_build_exclude_regex_none() {
        let (re, inv) = Monitor::build_exclude_regex(None, "exclude").unwrap();
        assert!(re.is_none());
        assert!(!inv);
    }

    #[test]
    fn test_build_exclude_regex_empty() {
        let (re, inv) = Monitor::build_exclude_regex(Some(&[]), "exclude").unwrap();
        assert!(re.is_none());
        assert!(!inv);
    }

    #[test]
    fn test_build_exclude_regex_single_pattern() {
        let patterns = vec!["*.tmp".to_string()];
        let (re, inv) = Monitor::build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("foo.tmp"));
        assert!(!re.as_ref().unwrap().is_match("foo.txt"));
    }

    #[test]
    fn test_build_exclude_regex_multiple_patterns() {
        let patterns = vec!["*.tmp".to_string(), "*.log".to_string()];
        let (re, inv) = Monitor::build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("foo.tmp"));
        assert!(re.as_ref().unwrap().is_match("bar.log"));
        assert!(!re.as_ref().unwrap().is_match("foo.txt"));
    }

    #[test]
    fn test_build_exclude_regex_invert() {
        let patterns = vec!["!*.py".to_string()];
        let (re, inv) = Monitor::build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(inv);
        assert!(re.as_ref().unwrap().is_match("foo.py"));
        assert!(!re.as_ref().unwrap().is_match("foo.tmp"));
    }

    #[test]
    fn test_build_exclude_regex_cmd() {
        let patterns = vec!["rsync".to_string(), "apt".to_string()];
        let (re, inv) = Monitor::build_exclude_regex(Some(&patterns), "--exclude-cmd").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("rsync"));
        assert!(re.as_ref().unwrap().is_match("apt"));
        assert!(!re.as_ref().unwrap().is_match("nginx"));
    }

    #[test]
    fn test_build_exclude_regex_cmd_wildcard() {
        let patterns = vec!["nginx*".to_string()];
        let (re, inv) = Monitor::build_exclude_regex(Some(&patterns), "--exclude-cmd").unwrap();
        assert!(re.is_some());
        assert!(re.as_ref().unwrap().is_match("nginx"));
        assert!(re.as_ref().unwrap().is_match("nginx-worker"));
        assert!(!re.as_ref().unwrap().is_match("apache"));
    }
}
