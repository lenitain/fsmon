use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify_fid::{fanotify_init, fanotify_mark};
use fanotify_fid::types::{FidEvent, HandleKey};
use fanotify_fid::consts::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_CLOSE_NOWRITE,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_MARK_ADD,
    FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO,
    FAN_NONBLOCK, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_Q_OVERFLOW, FAN_REPORT_DIR_FID,
    FAN_REPORT_FID, FAN_REPORT_NAME,
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

// ---- FID event helpers ----

/// Convert a fanotify event mask to fsmon's EventType enum.
fn mask_to_event_types(mask: u64) -> smallvec::SmallVec<[EventType; 8]> {
    const BITS: [(u64, EventType); 13] = [
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
    ];
    BITS.iter().filter(|(bit, _)| mask & bit != 0).map(|(_, t)| *t).collect()
}

/// Read and parse FID events, using a `DashMap`-based cache for path recovery.
fn read_fid_events_dashmap(
    fan_fd: i32,
    mount_fds: &[i32],
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
                if let Some(dir_path) = dir_cache.get(key) {
                    ev.path = if filename.is_empty() {
                        dir_path.clone()
                    } else {
                        dir_path.join(filename)
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

const ALL_EVENT_MASK: u64 = FAN_ACCESS
    | FAN_MODIFY
    | FAN_ATTRIB
    | FAN_CLOSE_WRITE
    | FAN_CLOSE_NOWRITE
    | FAN_OPEN
    | FAN_OPEN_EXEC
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
    pub exclude_cmd_regex: Option<regex::Regex>,
    pub only_cmd_regex: Option<regex::Regex>,
    pub recursive: bool,
    pub all_events: bool,
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
    /// All fanotify fds (one per filesystem group)
    fan_fds: Vec<i32>,
    mount_fds: Vec<OwnedFd>,
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
            fan_fds: Vec::new(),
            mount_fds: Vec::new(),
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

        // Compute combined event mask from all monitored paths
        let combined_mask = if self.path_options.values().any(|o| o.all_events) {
            ALL_EVENT_MASK
        } else {
            DEFAULT_EVENT_MASK
        };

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
                    exclude: opts.exclude_regex.as_ref().map(|r| r.as_str().to_string()),
                    exclude_cmd: None,
                    only_cmd: None,
                    all_events: Some(opts.all_events),
                }));
            }
        }
        self.paths = keep_paths;
        self.path_options = keep_opts;
        // Initialize inotify for watching parent dirs of pending paths
        self.inotify = Some(inotify::Inotify::init().context("inotify_init")?);
        self.setup_inotify_watches();

        // Initialize per-filesystem fanotify fds. The kernel does not allow
        // marks on different filesystems to coexist on a single fanotify fd
        // (even inode marks — all return EXDEV). So we create one fd per
        // filesystem and spawn a reader task for each.
        //
        // Strategy: try to add each path to an existing fd's group (same
        // filesystem), probing with FAN_MARK_ADD|FAN_MARK_FILESYSTEM first.
        // If EXDEV, the path belongs to a different filesystem — create a
        // new fd for it. If FS-mark also fails with a non-EXDEV error, fall
        // back to inode mark on the new fd.

        struct FanGroup {
            fd: i32,
        }

        let mut fan_groups: Vec<FanGroup> = Vec::new();
        for canonical in &self.canonical_paths {
            let path_mask = combined_mask;

            // Try to add this path to an existing fd (same filesystem)
            let mut matched = false;
            for group in &fan_groups {
                match fanotify_mark(
                    group.fd,
                    FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                    path_mask,
                    AT_FDCWD,
                    canonical,
                ) {
                    Ok(()) => {
                        matched = true;
                        eprintln!(
                            "[INFO] Monitoring {} (fs mark) on existing fd {}",
                            canonical.display(),
                            group.fd
                        );
                        break;
                    }
                    Err(ref e) if e.raw_os_error() == Some(libc::EXDEV) => {
                        continue; // different filesystem, try next fd
                    }
                    Err(e) => {
                        eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    }
                }
            }
            if matched {
                continue;
            }

            // Create a fresh fd for this filesystem
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

            let _use_fs = match fanotify_mark(
                new_fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                path_mask,
                AT_FDCWD,
                canonical,
            ) {
                Ok(()) => {
                    eprintln!(
                        "[INFO] Monitoring {} (filesystem mark) on fd {}",
                        canonical.display(),
                        new_fd
                    );
                    true
                }
                Err(ref e) if e.raw_os_error() == Some(libc::EXDEV) => {
                    // Filesystem mark EXDEV shouldn't happen on a fresh fd,
                    // but fall back to inode mark just in case.
                    match mark_directory(new_fd, path_mask, canonical) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Monitoring {} (inode mark) on fd {}",
                                canonical.display(),
                                new_fd
                            );
                            // mark subdirectories recursively
                            let opts = self
                                .paths
                                .iter()
                                .position(|p| {
                                    canonical == p
                                        || canonical
                                            == &p.canonicalize().unwrap_or_else(|_| p.clone())
                                })
                                .and_then(|i| self.path_options.get(&self.paths[i]));
                            if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                                mark_recursive(new_fd, path_mask, canonical);
                            }
                            false
                        }
                        Err(e) => {
                            eprintln!(
                                "[WARNING] Cannot monitor {} (inode mark): {:#}",
                                canonical.display(),
                                e
                            );
                            let _ = nix::unistd::close(new_fd);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    let _ = nix::unistd::close(new_fd);
                    continue;
                }
            };
            fan_groups.push(FanGroup { fd: new_fd });
        }

        if !fan_groups.is_empty() {
            // Managed fds for live-add reuse via socket commands
            for group in &fan_groups {
                self.fan_fds.push(group.fd);
            }

            // Open directory fds for open_by_handle_at to resolve file handles.
            for canonical in &self.canonical_paths {
                if let Ok(raw) = nix::fcntl::open(
                    canonical,
                    nix::fcntl::OFlag::O_DIRECTORY,
                    nix::sys::stat::Mode::empty(),
                ) {
                    // SAFETY: raw fd just opened successfully, OwnedFd takes ownership
                    self.mount_fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
                }
            }

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
            println!("  FDs: {} file-descriptor(s)", fan_groups.len());
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

        // Spawn one reader task per fan_fd. Events are sent through an
        // unbounded mpsc channel to the main loop for processing.
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<FidEvent>>();
        let mount_fds: Vec<i32> = self.mount_fds.iter().map(|fd| fd.as_raw_fd()).collect();
        let mount_fds = Arc::new(mount_fds);
        let dir_cache = Arc::new(std::mem::take(&mut self.dir_cache));
        let buf_size = self.buffer_size;

        // Managed for live-add (add_path may need to spawn reader tasks)
        self.event_tx = Some(event_tx.clone());
        self.shared_dir_cache = Some(Arc::clone(&dir_cache));

        for group in &fan_groups {
            let fd = group.fd;
            let tx = event_tx.clone();
            let mfds = Arc::clone(&mount_fds);
            let dc = Arc::clone(&dir_cache);
            tokio::spawn(async move {
                // SAFETY: fd is a fanotify fd owned by this task from now on
                let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
                let afd = match AsyncFd::new(owned_fd) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("[ERROR] AsyncFd for fd {}: {}", fd, e);
                        return;
                    }
                };
                let mut buf = vec![0u8; buf_size];
                loop {
                    let result = afd.readable().await;
                    let mut guard = match result {
                        Ok(g) => g,
                        Err(e) => {
                            eprintln!("[ERROR] fd {} readable: {}", fd, e);
                            break;
                        }
                    };
                    let events =
                        read_fid_events_dashmap(fd, &mfds, dc.as_ref(), &mut buf);
                    if !events.is_empty() && tx.send(events).is_err() {
                        break; // receiver dropped (shutting down)
                    }
                    guard.clear_ready();
                }
                // OwnedFd dropped here → fd auto-closed
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
                                    exclude: opts.and_then(|o| o.exclude_regex.as_ref().map(|r| r.as_str().to_string())),
                                    exclude_cmd: None,
                                    only_cmd: None,
                                    all_events: opts.map(|o| o.all_events),
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

    /// Try to add a path to an existing fan_fd via FAN_MARK_FILESYSTEM.
    /// Returns Ok(fd) on success, Err(()) if no fd accepts this path.
    fn try_mark_on_existing(fds: &[i32], mask: u64, path: &Path) -> std::result::Result<i32, ()> {
        for &fd in fds {
            match fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_FILESYSTEM, mask, AT_FDCWD, path) {
                Ok(()) => return Ok(fd),
                Err(ref e) if e.raw_os_error() == Some(libc::EXDEV) => continue,
                Err(e) => {
                    eprintln!(
                        "[WARNING] Cannot monitor {} on fd {}: {:#}",
                        path.display(),
                        fd,
                        e
                    );
                }
            }
        }
        Err(())
    }

    /// Spawn a tokio reader task for a newly created fanotify fd.
    fn spawn_fd_reader(&mut self, fd: i32) {
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
        let mfds: Vec<i32> = self.mount_fds.iter().map(|fd| fd.as_raw_fd()).collect();
        let mfds = Arc::new(mfds);
        let buf_size = self.buffer_size;
        tokio::spawn(async move {
            // SAFETY: fd is a fanotify fd owned by this task from now on
            let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let afd = match AsyncFd::new(owned_fd) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[ERROR] AsyncFd for fd {}: {}", fd, e);
                    return;
                }
            };
            let mut buf = vec![0u8; buf_size];
            loop {
                let result = afd.readable().await;
                let mut guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("[ERROR] fd {} readable: {}", fd, e);
                        break;
                    }
                };
                let events =
                    read_fid_events_dashmap(fd, &mfds, dc.as_ref(), &mut buf);
                if !events.is_empty() && tx.send(events).is_err() {
                    break;
                }
                guard.clear_ready();
            }
            // OwnedFd dropped here → fd auto-closed
        });
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
        let exclude_regex = entry
            .exclude
            .as_ref()
            .map(|p| {
                let escaped = regex::escape(p);
                let pattern = escaped.replace("\\*", ".*");
                regex::Regex::new(&pattern).with_context(|| "invalid exclude pattern")
            })
            .transpose()?;
        let exclude_cmd_regex = entry
            .exclude_cmd
            .as_ref()
            .map(|p| {
                let pattern = p.replace("*", ".*");
                regex::Regex::new(&pattern).with_context(|| "invalid --exclude-cmd pattern")
            })
            .transpose()?;
        let only_cmd_regex = entry
            .only_cmd
            .as_ref()
            .map(|p| {
                let pattern = p.replace("*", ".*");
                regex::Regex::new(&pattern).with_context(|| "invalid --only-cmd pattern")
            })
            .transpose()?;
        let recursive = entry.recursive.unwrap_or(false);
        let all_events = entry.all_events.unwrap_or(false);

        let opts = PathOptions {
            min_size,
            event_types,
            exclude_regex,
            exclude_cmd_regex,
            only_cmd_regex,
            recursive,
            all_events,
        };

        let path_mask = if all_events {
            ALL_EVENT_MASK
        } else {
            DEFAULT_EVENT_MASK
        };

        // Find or create a fanotify fd for this path's filesystem.
        // Try each existing fd first (same filesystem → OK, EXDEV → try next).
        let (fan_fd, is_new_fd) =
            match Self::try_mark_on_existing(&self.fan_fds, path_mask, &canonical) {
                Ok(fd) => (fd, false),
                Err(()) => {
                    // No existing fd works — create a new one
                    let fd = fanotify_init(
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
                    self.fan_fds.push(fd);

                    // Try filesystem mark on the new fd
                    match fanotify_mark(
                        fd,
                        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                        path_mask,
                        AT_FDCWD,
                        &canonical,
                    ) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Monitoring {} (fs mark) on new fd {}",
                                canonical.display(),
                                fd
                            );
                        }
                        Err(ref e) if e.raw_os_error() == Some(libc::EXDEV) => {
                            // Fall back to inode mark
                            if let Err(e) = mark_directory(fd, path_mask, &canonical) {
                                eprintln!(
                                    "[WARNING] Cannot monitor {} (inode mark): {:#}",
                                    canonical.display(),
                                    e
                                );
                            } else {
                                eprintln!(
                                    "[INFO] Monitoring {} (inode mark) on new fd {}",
                                    canonical.display(),
                                    fd
                                );
                                if recursive && canonical.is_dir() {
                                    mark_recursive(fd, path_mask, &canonical);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                        }
                    }
                    (fd, true)
                }
            };

        // Open directory fd for handle resolution BEFORE spawning reader,
        // so the new reader task can resolve file handles for this path.
        if let Ok(raw) = nix::fcntl::open(
            canonical.as_path(),
            nix::fcntl::OFlag::O_DIRECTORY,
            nix::sys::stat::Mode::empty(),
        ) {
            // SAFETY: raw fd just opened successfully, OwnedFd takes ownership
            self.mount_fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
        }

        // Update path tracking
        self.paths.push(path.clone());
        self.canonical_paths.push(canonical.clone());
        self.path_options.insert(path.clone(), opts);

        // Pre-cache directory handles in the shared cache (used by all reader tasks)
        // before spawning the reader, so second-pass path recovery works.
        if canonical.is_dir()
            && let Some(ref cache) = self.shared_dir_cache
        {
            if recursive {
                dir_cache::cache_recursive(cache.as_ref(), &canonical);
            } else {
                dir_cache::cache_dir_handle(cache.as_ref(), &canonical);
            }
        }

        // Spawn reader task + confirm monitoring (after mount_fd + cache are ready)
        if is_new_fd {
            self.spawn_fd_reader(fan_fd);
        } else {
            eprintln!(
                "[INFO] Monitoring {} on existing fd {}",
                canonical.display(),
                fan_fd
            );
        }

        println!(
            "Added path: {} (recursive={}, all_events={})",
            path.display(),
            recursive,
            all_events
        );
        Ok(())
    }

    pub fn remove_path(&mut self, path: &Path) -> Result<()> {
        let pos = self
            .paths
            .iter()
            .position(|p| p == path)
            .ok_or_else(|| anyhow::anyhow!("Path not being monitored: {}", path.display()))?;

        let canonical = &self.canonical_paths[pos];
        let fan_fd = if let Some(&fd) = self.fan_fds.first() {
            fd
        } else {
            bail!("Monitor not running (no fanotify fd)");
        };
        let opts = self
            .path_options
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("No options for path: {}", path.display()))?;
        let path_mask = if opts.all_events {
            ALL_EVENT_MASK
        } else {
            DEFAULT_EVENT_MASK
        };

        // Try to remove filesystem mark
        match fanotify_mark(
            fan_fd,
            FAN_MARK_REMOVE | FAN_MARK_FILESYSTEM,
            path_mask,
            AT_FDCWD,
            canonical,
        ) {
            Ok(()) => {}
            Err(ref e)
                if e.raw_os_error() == Some(libc::EXDEV)
                    || e.raw_os_error() == Some(libc::EINVAL) =>
            {
                // In inode mark mode, marks are per-directory and hard to remove individually.
                // They'll be cleaned up on shutdown. That's acceptable.
            }
            Err(e) => {
                eprintln!(
                    "Warning: fanotify_mark remove failed for {}: {}",
                    canonical.display(),
                    e
                );
            }
        }

        self.paths.remove(pos);
        self.canonical_paths.remove(pos);
        self.path_options.remove(path);

        // Remove the matching mount fd (OwnedFd dropped → auto-closed)
        if pos < self.mount_fds.len() {
            self.mount_fds.remove(pos);
        }

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
                // Remove first if already monitored, then add with new options
                if self.path_options.contains_key(&path) {
                    let _ = self.remove_path(&path);
                }
                let entry = PathEntry {
                    path,
                    recursive: cmd.recursive,
                    types: cmd.types.clone(),
                    min_size: cmd.min_size.clone(),
                    exclude: cmd.exclude.clone(),
                    exclude_cmd: cmd.exclude_cmd.clone(),
                    only_cmd: cmd.only_cmd.clone(),
                    all_events: cmd.all_events,
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
                                o.exclude_regex.as_ref().map(|r| r.as_str().to_string())
                            }),
                            exclude_cmd: None,
                            only_cmd: None,
                            all_events: opts.map(|o| o.all_events),
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

        if let Some(ref regex) = opts.exclude_regex
            && regex.is_match(&event.path.to_string_lossy())
        {
            return false;
        }

        if let Some(ref regex) = opts.exclude_cmd_regex
            && regex.is_match(&event.cmd)
        {
            return false;
        }

        if let Some(ref regex) = opts.only_cmd_regex
            && !regex.is_match(&event.cmd)
        {
            return false;
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

/// Mark a single directory
fn mark_directory(fan_fd: i32, mask: u64, path: &Path) -> Result<()> {
    fanotify_mark(fan_fd, FAN_MARK_ADD, mask, AT_FDCWD, path)
        .with_context(|| format!("fanotify_mark failed: {}", path.display()))
}

/// Recursively traverse and mark all subdirectories (ignore errors, e.g., permission denied)
fn mark_recursive(fan_fd: i32, mask: u64, dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = fanotify_mark(fan_fd, FAN_MARK_ADD, mask, AT_FDCWD, path.as_path());
            mark_recursive(fan_fd, mask, &path);
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
            FAN_DELETE_SELF, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO,
            FAN_OPEN, FAN_OPEN_EXEC,
        };
        let mask = FAN_ACCESS | FAN_MODIFY | FAN_CLOSE_WRITE | FAN_CLOSE_NOWRITE
            | FAN_OPEN | FAN_OPEN_EXEC | FAN_ATTRIB | FAN_CREATE | FAN_DELETE
            | FAN_DELETE_SELF | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 13);
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
        all_events: bool,
    ) -> PathOptions {
        let exclude_regex = exclude.map(|p| {
            let escaped = regex::escape(p);
            let pattern = escaped.replace("\\*", ".*");
            regex::Regex::new(&pattern).expect("invalid exclude pattern")
        });
        PathOptions {
            min_size,
            event_types,
            exclude_regex,
            exclude_cmd_regex: None,
            only_cmd_regex: None,
            recursive,
            all_events,
        }
    }

    fn make_monitor(
        paths: Vec<&str>,
        min_size: Option<i64>,
        event_types: Option<Vec<EventType>>,
        exclude: Option<&str>,
        recursive: bool,
        all_events: bool,
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
                            all_events,
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
        let m = make_monitor(vec!["/tmp"], None, None, None, false, false);
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
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/a", EventType::Delete, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Modify, 1, 0)));
    }

    #[test]
    fn test_should_output_min_size_filter() {
        let m = make_monitor(vec!["/tmp"], Some(1000), None, None, false, false);
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 2000)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, 500)));
    }

    #[test]
    fn test_should_output_exclude_pattern() {
        let m = make_monitor(vec!["/tmp"], None, None, Some("*.tmp"), false, false);
        assert!(!m.should_output(&make_event("/tmp/test.tmp", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/foo.tmp", EventType::Delete, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_exact_pattern() {
        let m = make_monitor(vec!["/tmp"], None, None, Some("test.tmp"), false, false);
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
            false,
        );
        assert!(m.should_output(&make_event("/tmp/data", EventType::Create, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Delete, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Create, 1, 50)));
        assert!(!m.should_output(&make_event("/tmp/app.log", EventType::Create, 1, 200)));
    }

    #[test]
    fn test_is_path_in_scope_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, None, true, false);
        let watched = vec![PathBuf::from("/tmp")];
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub/deep/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/tmpfile"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, None, false, false);
        let watched = vec![PathBuf::from("/tmp")];
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/tmp/sub/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let m = make_monitor(vec!["/tmp", "/var/log"], None, None, None, true, false);
        let watched = vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")];
        assert!(m.is_path_in_scope(Path::new("/tmp/file"), &watched));
        assert!(m.is_path_in_scope(Path::new("/var/log/syslog"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/etc/passwd"), &watched));
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
        let opts = options(None, None, None, false, false);

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
        m.fan_fds.push(-1); // dummy fd for tests

        let entry = PathEntry {
            path: PathBuf::from("/tmp/test_add"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
            exclude_cmd: None,
            only_cmd: None,
            all_events: None,
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
        unsafe {
            libc::close(fd.unwrap());
        }
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
            fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            AT_FDCWD,
            &test_dir,
        );
        assert!(
            result.is_ok(),
            "fanotify_mark should succeed on existing directory"
        );

        unsafe {
            libc::close(fd);
        }
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
            fd,
            FAN_MARK_ADD,
            mask,
            AT_FDCWD,
            Path::new("/nonexistent_path_12345"),
        );
        assert!(
            result.is_err(),
            "fanotify_mark should fail on nonexistent path"
        );

        unsafe {
            libc::close(fd);
        }
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
                fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                mask,
                AT_FDCWD,
                &test_dir_clone,
            )
            .unwrap();

            let mut buf = vec![0u8; 4096];
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(200) {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n > 0 {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            unsafe {
                libc::close(fd);
            }
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
}
