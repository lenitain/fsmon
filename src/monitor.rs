use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify::low_level::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_CLOSE_NOWRITE,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_MARK_ADD,
    FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO,
    FAN_NONBLOCK, FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_Q_OVERFLOW, FAN_REPORT_DIR_FID,
    FAN_REPORT_FID, FAN_REPORT_NAME, O_CLOEXEC, O_RDONLY, fanotify_init, fanotify_mark,
};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::signal::unix::{SignalKind, signal};

use crate::dir_cache;
use crate::fid_parser::{self, FAN_FS_ERROR};
use crate::proc_cache::{self, ProcCache};
use crate::socket::{SocketCmd, SocketResp};
use crate::store::PathEntry;
use crate::store::Store;
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

/// Chown a file or directory to the original user (daemon runs as root).
/// Resolves the original user from SUDO_UID/SUDO_GID env vars.
fn chown_to_user(path: &Path) -> std::io::Result<()> {
    let (uid, gid) = crate::config::resolve_uid_gid();
    let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains null"))?;
    let ret = unsafe { libc::chown(cpath.as_ptr(), uid, gid) };
    if ret != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
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
    | FAN_FS_ERROR
    | FAN_EVENT_ON_CHILD
    | FAN_ONDIR;

// ---- PathOptions ----

#[derive(Clone)]
pub struct PathOptions {
    pub min_size: Option<i64>,
    pub event_types: Option<Vec<EventType>>,
    pub exclude_regex: Option<regex::Regex>,
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
    store_path: Option<PathBuf>,
    proc_cache: Option<ProcCache>,
    file_size_cache: LruCache<PathBuf, u64>,
    buffer_size: usize,
    socket_listener: Option<tokio::net::UnixListener>,
    /// All fanotify fds (one per filesystem group)
    fan_fds: Vec<i32>,
    mount_fds: Vec<RawFd>,
    dir_cache: HashMap<fid_parser::HandleKey, PathBuf>,
    /// Shared state for spawning reader tasks during live-add (set in run())
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<fid_parser::FidEvent>>>,
    shared_dir_cache: Option<Arc<Mutex<HashMap<fid_parser::HandleKey, PathBuf>>>>,
}

impl Monitor {
    pub fn new(
        paths_and_options: Vec<(PathBuf, PathOptions)>,
        log_dir: Option<PathBuf>,
        store_path: Option<PathBuf>,
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
            store_path,
            proc_cache: None,
            file_size_cache: LruCache::new(NonZeroUsize::new(FILE_SIZE_CACHE_CAP).unwrap()),
            buffer_size,

            socket_listener,
            fan_fds: Vec::new(),
            mount_fds: Vec::new(),
            dir_cache: HashMap::new(),
            event_tx: None,
            shared_dir_cache: None,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        if unsafe { libc::geteuid() } != 0 {
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

        // Collect canonical paths
        for path in &self.paths {
            let canonical = if path.exists() {
                path.canonicalize().unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };
            self.canonical_paths.push(canonical);
        }

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
                (O_CLOEXEC | O_RDONLY) as u32,
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
                            unsafe {
                                libc::close(new_fd);
                            }
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    unsafe {
                        libc::close(new_fd);
                    }
                    continue;
                }
            };
            fan_groups.push(FanGroup { fd: new_fd });
        }

        if fan_groups.is_empty() {
            eprintln!("No paths configured. Waiting for socket commands (use 'fsmon add <path>').");
        } else {
            // Store fds for live-add reuse via socket commands
            for group in &fan_groups {
                self.fan_fds.push(group.fd);
            }

            // Open directory fds for open_by_handle_at to resolve file handles.
            // These are shared across all fan_fds (resolve_file_handle tries each).
            for canonical in &self.canonical_paths {
                if let Ok(c_path) = CString::new(canonical.to_string_lossy().as_bytes()) {
                    let mfd =
                        unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
                    if mfd >= 0 {
                        self.mount_fds.push(mfd);
                    }
                }
            }

            // Pre-cache directory handles (shared across fds)
            for (i, canonical) in self.canonical_paths.iter().enumerate() {
                if canonical.is_dir() {
                    let opts = self.paths.get(i).and_then(|p| self.path_options.get(p));
                    let recursive = opts.is_some_and(|o| o.recursive);
                    if recursive {
                        dir_cache::cache_recursive(&mut self.dir_cache, canonical);
                    } else {
                        dir_cache::cache_dir_handle(&mut self.dir_cache, canonical);
                    }
                }
            }
        }

        // Ensure log directory exists and is owned by the original user
        if let Some(ref dir) = self.log_dir {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create log directory {}", dir.display()))?;
            // Daemon runs as root; chown to the original user so they own their logs
            let _ = chown_to_user(dir);
        }

        println!("Starting file trace monitor...");
        if !self.canonical_paths.is_empty() {
            println!(
                "Monitoring paths: {}",
                self.canonical_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            println!("  FDs: {} file-descriptor(s)", fan_groups.len());
        }

        // Spawn one reader task per fan_fd. Events are sent through an
        // unbounded mpsc channel to the main loop for processing.
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<fid_parser::FidEvent>>();
        let mount_fds = Arc::new(self.mount_fds.clone());
        let dir_cache = Arc::new(Mutex::new(std::mem::take(&mut self.dir_cache)));
        let buf_size = self.buffer_size;

        // Store for live-add (add_path may need to spawn reader tasks)
        self.event_tx = Some(event_tx.clone());
        self.shared_dir_cache = Some(Arc::clone(&dir_cache));

        for group in &fan_groups {
            let fd = group.fd;
            let tx = event_tx.clone();
            let mfds = Arc::clone(&mount_fds);
            let dc = Arc::clone(&dir_cache);
            tokio::spawn(async move {
                let afd = match AsyncFd::new(FanFd(fd)) {
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
                        fid_parser::read_fid_events(fd, &mfds, &mut dc.lock().unwrap(), &mut buf);
                    if !events.is_empty() && tx.send(events).is_err() {
                        break; // receiver dropped (shutting down)
                    }
                    guard.clear_ready();
                }
                unsafe {
                    libc::close(fd);
                }
            });
        }

        let mut sigterm =
            signal(SignalKind::terminate()).context("failed to create SIGTERM signal handler")?;
        let mut sighup =
            signal(SignalKind::hangup()).context("failed to create SIGHUP signal handler")?;

        let socket_listener = self.socket_listener.take();

        loop {
            tokio::select! {
                Some(events) = event_rx.recv() => {
                    // Process events from any fan_fd reader task.
                    // Dynamic inode marking is not applied here — for
                    // cross-filesystem setups new directory marks are added
                    // at next daemon restart. This is a known limitation.
                    for raw in &events {
                        if raw.mask & FAN_Q_OVERFLOW != 0 {
                            eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                            continue;
                        }

                        let event_types = fid_parser::mask_to_event_types(raw.mask);

                        // Resolve monitored path once per event (used by build + write)
                        let matched_path = self.matching_path(&raw.path).cloned();

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

        // Cleanup: spawned tasks close their own fds on exit.
        // Close mount fds.
        for &mfd in &self.mount_fds {
            unsafe {
                libc::close(mfd);
            }
        }

        println!("\nStopping file trace monitor...");
        Ok(())
    }

    fn build_file_event(
        &mut self,
        raw: &fid_parser::FidEvent,
        event_type: EventType,
        matched_path: Option<&Path>,
    ) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

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
    fn spawn_fd_reader(&self, fd: i32) {
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
        let mfds = Arc::new(self.mount_fds.clone());
        let buf_size = self.buffer_size;
        tokio::spawn(async move {
            let afd = match AsyncFd::new(FanFd(fd)) {
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
                    fid_parser::read_fid_events(fd, &mfds, &mut dc.lock().unwrap(), &mut buf);
                if !events.is_empty() && tx.send(events).is_err() {
                    break;
                }
                guard.clear_ready();
            }
            unsafe {
                libc::close(fd);
            }
        });
    }

    pub fn add_path(&mut self, entry: &PathEntry) -> Result<()> {
        // Normalize path: expand tilde + resolve symlinks/../.
        // Store the shortest canonical form so all comparisons work consistently.
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

        let canonical = if path.exists() {
            path.canonicalize().unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };

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
        let recursive = entry.recursive.unwrap_or(false);
        let all_events = entry.all_events.unwrap_or(false);

        let opts = PathOptions {
            min_size,
            event_types,
            exclude_regex,
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
                        (O_CLOEXEC | O_RDONLY) as u32,
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
        if let Ok(c_path) = CString::new(canonical.to_string_lossy().as_bytes()) {
            let mfd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
            if mfd >= 0 {
                self.mount_fds.push(mfd);
            }
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
            let mut dc = cache.lock().unwrap();
            if recursive {
                dir_cache::cache_recursive(&mut dc, &canonical);
            } else {
                dir_cache::cache_dir_handle(&mut dc, &canonical);
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

        // Close and remove the matching mount fd
        if pos < self.mount_fds.len() {
            unsafe { libc::close(self.mount_fds[pos]) };
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
                // Normalize path for consistent comparison
                let path = resolve_recursion_check(&raw);
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
                    all_events: cmd.all_events,
                };
                match self.add_path(&entry) {
                    Ok(()) => {
                        let _ = self.persist_config();
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
                        let _ = self.persist_config();
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

    fn persist_config(&self) -> Result<()> {
        let entries: Vec<PathEntry> = self
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
                    exclude: opts
                        .and_then(|o| o.exclude_regex.as_ref().map(|r| r.as_str().to_string())),
                    all_events: opts.map(|o| o.all_events),
                }
            })
            .collect();
        // Persist to store.toml using the stored path
        if let Some(ref store_path) = self.store_path
            && let Ok(mut store) = Store::load(store_path)
        {
            store.entries = entries;
            let _ = store.save(store_path);
        }
        Ok(())
    }

    fn reload_config(&mut self) -> Result<()> {
        let store_path = self
            .store_path
            .as_ref()
            .context("No store path configured")?;
        let store = Store::load(store_path)?;
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
            let _ = chown_to_user(&log_path);
        }
        writeln!(file, "{}", event.to_toml_string())?;
        writeln!(file)?; // blank line separator
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
            all_events: None,
        };

        // add_path warns on bad fan_fd but still tracks the path
        let result = m.add_path(&entry);
        assert!(result.is_ok());
        assert!(m.path_options.contains_key(Path::new("/tmp/test_add")));

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
            (O_CLOEXEC | O_RDONLY) as u32,
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
            (O_CLOEXEC | O_RDONLY) as u32,
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
            (O_CLOEXEC | O_RDONLY) as u32,
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
                (O_CLOEXEC | O_RDONLY) as u32,
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
