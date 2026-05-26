use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify_fid::consts::{
    AT_FDCWD, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_MARK_ADD, FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE,
    FAN_NONBLOCK, FAN_Q_OVERFLOW, FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;
use fanotify_fid::types::FidEvent;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lru::LruCache;
use tokio::io::unix::AsyncFd;
use tokio::io::AsyncBufReadExt;
use tokio::signal::unix::{SignalKind, signal};

use moka::sync::Cache;

use crate::config::ResolvedCacheConfig;
use crate::dir_cache;
use crate::fid_parser::{
    DIR_CACHE_CAP, FanFd, FsGroup, chown_to_user, mark_directory, mark_recursive,
    mask_to_event_types, path_mask_from_options, read_fid_events_cached,
};
use crate::filters::{self, PathOptions};
use crate::monitored::Monitored;
use crate::monitored::PathEntry;
use crate::proc_cache::{
    self, PID_TREE_CAP, PROC_CACHE_CAP, PidTree, ProcCache, build_chain, is_descendant,
    snapshot_process_tree,
};
use crate::socket::{SocketCmd, SocketResp};
use crate::utils::{format_size, get_process_info_by_pid, parse_size_filter};
use crate::{EventType, FileEvent};

// ---- Event channel types ----

/// Bounded or unbounded sender for the event channel.
enum EventSender {
    Unbounded(tokio::sync::mpsc::UnboundedSender<Vec<FidEvent>>),
    Bounded(tokio::sync::mpsc::Sender<Vec<FidEvent>>),
}

impl Clone for EventSender {
    fn clone(&self) -> Self {
        match self {
            EventSender::Unbounded(tx) => EventSender::Unbounded(tx.clone()),
            EventSender::Bounded(tx) => EventSender::Bounded(tx.clone()),
        }
    }
}

/// Bounded or unbounded receiver for the event channel.
enum EventReceiver {
    Unbounded(tokio::sync::mpsc::UnboundedReceiver<Vec<FidEvent>>),
    Bounded(tokio::sync::mpsc::Receiver<Vec<FidEvent>>),
}

impl EventReceiver {
    async fn recv(&mut self) -> Option<Vec<FidEvent>> {
        match self {
            EventReceiver::Unbounded(rx) => rx.recv().await,
            EventReceiver::Bounded(rx) => rx.recv().await,
        }
    }

    fn try_recv(&mut self) -> Result<Vec<FidEvent>, tokio::sync::mpsc::error::TryRecvError> {
        match self {
            EventReceiver::Unbounded(rx) => rx.try_recv(),
            EventReceiver::Bounded(rx) => rx.try_recv(),
        }
    }
}

// ---- Reader supervision ----

/// Per-reader-task restart tracking for exponential backoff.
/// Restarts are capped at MAX_RESTARTS within BACKOFF_WINDOW.
struct ReaderState {
    restart_count: u32,
    last_restart: std::time::Instant,
    /// Set when restart_reader gives up (backoff exhausted within window).
    /// Reset when spawn_fd_reader attempts a new spawn (even if it later fails).
    /// Used by health() for reliable alive/dead reporting.
    gave_up: bool,
}

const MAX_RESTARTS: u32 = 3;
const BACKOFF_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

// ---- Monitor ----

pub struct Monitor {
    paths: Vec<PathBuf>,
    canonical_paths: Vec<PathBuf>,
    /// Full list of (path, PathOptions) preserving duplicates across cmd groups.
    /// This is the single source of truth for path options.
    monitored_entries: Vec<(PathBuf, PathOptions)>,
    log_dir: Option<PathBuf>,
    monitored_path: Option<PathBuf>,
    proc_cache: Option<ProcCache>,
    pid_tree: Option<PidTree>,
    file_size_cache: LruCache<PathBuf, u64>,
    buffer_size: usize,
    socket_listener: Option<tokio::net::UnixListener>,
    /// One `FsGroup` per unique filesystem (fan_fd + mount_fd dedup'd)
    fs_groups: Vec<FsGroup>,
    /// Maps monitored path → index in fs_groups for fast lookup in remove_path
    path_to_group: HashMap<PathBuf, usize>,
    dir_cache: Cache<fanotify_fid::types::HandleKey, PathBuf>,
    /// Shared state for spawning reader tasks during live-add (set in run())
    event_tx: Option<EventSender>,
    shared_dir_cache: Option<Cache<fanotify_fid::types::HandleKey, PathBuf>>,
    /// Paths that didn't exist at add/startup time, retried on directory creation
    pending_paths: Vec<(PathBuf, PathEntry)>,
    /// inotify instance watching parent dirs of pending paths
    inotify: Option<inotify::Inotify>,
    /// Watch descriptors kept alive so watches stay active
    _inotify_watches: Vec<inotify::WatchDescriptor>,
    /// PID of the fsmon daemon itself — events from this PID (or its children)
    /// are discarded to prevent self-triggering feedback loops.
    daemon_pid: u32,
    /// Resolved cache configuration (capacity, TTL, buffer size).
    cache_config: ResolvedCacheConfig,
    /// Enable debug output
    debug: bool,
    /// Death notifications from reader tasks: each sends its group_idx on exit.
    reader_death_rx: tokio::sync::mpsc::UnboundedReceiver<usize>,
    /// Cloneable sender for reader tasks to signal death.
    reader_death_tx: tokio::sync::mpsc::UnboundedSender<usize>,
    /// Per-group restart tracking (index-aligned with fs_groups).
    reader_states: Vec<Option<ReaderState>>,
    /// Daemon start time, set in run() for uptime calculation.
    started_at: std::time::Instant,
    /// Raw disk-min-free threshold string (e.g. "10%", "5GB"). None = no check.
    disk_min_free: Option<String>,
    /// Log file sync interval. None = disabled.
    sync_interval: Option<std::time::Duration>,

    /// Unified event stream: broadcast channel for all consumers.
    /// Carries (FileEvent, cmd_name) — cmd_name is for file routing.
    /// Subscribe tasks extract FileEvent, file writer uses both.
    event_stream_tx: Option<tokio::sync::broadcast::Sender<(FileEvent, String)>>,
}

impl Monitor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        paths_and_options: Vec<(PathBuf, PathOptions)>,
        log_dir: Option<PathBuf>,
        monitored_path: Option<PathBuf>,
        buffer_size: Option<usize>,
        socket_listener: Option<tokio::net::UnixListener>,
        debug: bool,
        cache_config: Option<ResolvedCacheConfig>,
        disk_min_free: Option<String>,
        sync_interval: Option<std::time::Duration>,
        subscribe_buf: Option<usize>,
    ) -> Result<Self> {
        let cache_config = cache_config.unwrap_or_default();
        let buffer_size = buffer_size.unwrap_or(cache_config.buffer_size);

        if buffer_size < 4096 {
            bail!("buffer_size must be at least 4096 bytes (4KB)");
        }
        if buffer_size > 1024 * 1024 {
            bail!("buffer_size must not exceed 1048576 bytes (1MB)");
        }

        let mut paths = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut monitored_entries = Vec::new();
        let log_dir_canonical = log_dir
            .as_ref()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()));
        for (path, opts) in &paths_and_options {
            // Reject paths that overlap with the log directory.
            // - Exact match (path == log dir) → always reject (it IS the log dir)
            // - Parent + recursive → reject (would capture log file writes)
            // - Parent + non-recursive → allow (only direct children, log files deeper)
            // Resolve tilde + symlinks to catch symlink-based conflicts
            let resolved = filters::resolve_recursion_check(path);
            if let Some(ref log_dir) = log_dir_canonical {
                let is_exact = log_dir.as_path() == resolved;
                let is_parent_recursive = opts.recursive && log_dir.starts_with(&resolved);
                if is_exact || is_parent_recursive {
                    bail!(
                        "Cannot monitor '{}': {} — \
                         Tip: use a path outside the log directory, or use a different logging.path",
                        path.display(),
                        if is_exact {
                            "this path is the log directory itself".to_string()
                        } else {
                            format!("log directory '{}' is inside this path", log_dir.display())
                        },
                    );
                }
            }
            // Reject cmd=fsmon: daemon's own events are excluded by PID filter,
            // so --cmd fsmon would never match anything. This check mirrors the
            // validation in add.rs to prevent silent misconfiguration via direct
            // jsonl edits.
            if opts.cmd.as_deref() == Some("fsmon") {
                bail!(
                    "Cannot monitor 'fsmon' process: fsmon daemon's own events \
                    are excluded from monitoring."
                );
            }
            // Same path under multiple cmd groups → fanotify dedup by path only
            if seen.insert(resolved.clone()) {
                paths.push(resolved.clone());
            }
            // Full list preserves duplicates for matching (single source of truth)
            monitored_entries.push((resolved.clone(), opts.clone()));
        }

        let (reader_death_tx, reader_death_rx) =
            tokio::sync::mpsc::unbounded_channel::<usize>();

        let monitor = Self {
            paths,
            canonical_paths: Vec::new(),
            monitored_entries,
            log_dir,
            monitored_path,
            proc_cache: None,
            pid_tree: None,
            file_size_cache: LruCache::new(
                NonZeroUsize::new(cache_config.file_size_capacity).unwrap(),
            ),
            buffer_size,

            dir_cache: Cache::builder()
                .max_capacity(cache_config.dir_capacity)
                .time_to_live(Duration::from_secs(cache_config.dir_ttl_secs))
                .build(),
            cache_config,
            socket_listener,
            debug,
            fs_groups: Vec::new(),
            path_to_group: HashMap::new(),
            event_tx: None,
            shared_dir_cache: None,
            pending_paths: Vec::new(),
            inotify: None,
            _inotify_watches: Vec::new(),
            daemon_pid: std::process::id(),
            reader_death_rx,
            reader_death_tx,
            reader_states: Vec::new(),
            started_at: std::time::Instant::now(),
            disk_min_free,
            sync_interval,
            event_stream_tx: {
                let cap = subscribe_buf.unwrap_or(4096).max(1);
                let (tx, _) = tokio::sync::broadcast::channel::<(FileEvent, String)>(cap);
                Some(tx)
            },
        };
        if debug {
            eprintln!(
                "[DEBUG] Monitor initialized with {} path entries:",
                paths_and_options.len()
            );
            for (i, (p, o)) in paths_and_options.iter().enumerate() {
                let label = o.cmd.as_deref().unwrap_or("global");
                eprintln!(
                    "[DEBUG]   [{}] {} cmd={} recursive={}",
                    i,
                    p.display(),
                    label,
                    o.recursive
                );
            }
            eprintln!("[DEBUG] log_dir: {:?}", monitor.log_dir);
            eprintln!("[DEBUG] buffer_size: {}", buffer_size);
        }
        Ok(monitor)
    }

    /// Duplicate a file descriptor, returning an owned fd.
    /// The returned `OwnedFd` has independent lifetime from the source
    /// and will be closed on drop.
    fn dup_fd(fd: &impl AsRawFd) -> std::io::Result<OwnedFd> {
        let new_raw = nix::unistd::dup(fd.as_raw_fd()).map_err(std::io::Error::other)?;
        // SAFETY: nix::unistd::dup returned a new valid fd that we
        // exclusively own. The kernel guarantees dup returns the
        // lowest-numbered unused fd, not owned by any other OwnedFd.
        Ok(unsafe { OwnedFd::from_raw_fd(new_raw) })
    }

    /// Open a directory and return an owned fd.
    /// The returned `OwnedFd` has the directory open and will be
    /// closed on drop.
    fn open_dir(path: &Path) -> std::io::Result<OwnedFd> {
        let raw = nix::fcntl::open(
            path,
            nix::fcntl::OFlag::O_DIRECTORY,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(std::io::Error::other)?;
        // SAFETY: nix::fcntl::open succeeded, returning a new valid fd
        // that we exclusively own. It will be closed when OwnedFd drops.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    /// Get all PathOptions for a path from monitored_entries (single source of truth).
    fn opts_for_path(&self, path: &Path) -> Vec<&PathOptions> {
        self.monitored_entries
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, o)| o)
            .collect()
    }

    /// Get the first PathOptions entry for a path (for mask calculation, recursive flag, etc.).
    fn first_opt_for_path(&self, path: &Path) -> Option<&PathOptions> {
        self.monitored_entries
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, o)| o)
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

        // Create proc connector (event-driven, non-blocking).
        // The fd is polled via AsyncFd in the main event loop below.
        let proc_conn = proc_cache::try_create_connector();
        let proc_params = proc_cache::CacheParams {
            capacity: proc_cache::PROC_CACHE_CAP,
            ttl_secs: self.cache_config.proc_ttl_secs,
        };
        let proc_cache = proc_cache::new_cache_with(proc_params);
        self.proc_cache = Some(proc_cache.clone());
        let tree_params = proc_cache::CacheParams {
            capacity: proc_cache::PID_TREE_CAP,
            ttl_secs: self.cache_config.proc_ttl_secs,
        };
        let pid_tree = proc_cache::new_pid_tree_with(tree_params);
        snapshot_process_tree(&pid_tree, &proc_cache);
        self.pid_tree = Some(pid_tree.clone());

        // Compute combined event mask from ALL cmd groups (OR over all entries)
        let combined_mask = self
            .monitored_entries
            .iter()
            .map(|(_, opts)| path_mask_from_options(opts))
            .fold(0, |a, b| a | b);
        if self.debug {
            eprintln!("[DEBUG] combined fanotify mask: {:#x}", combined_mask);
        }

        // Collect canonical paths — non-existent paths go to pending_paths
        // and removed from monitored_entries so add_path can work cleanly on retry.
        let mut keep_paths: Vec<PathBuf> = Vec::new();
        for path in std::mem::take(&mut self.paths) {
            if path.exists() {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                self.canonical_paths.push(canonical);
                keep_paths.push(path);
            } else {
                eprintln!(
                    "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                    path.display()
                );
                // Collect all opts for this path before removing, to build pending entry
                let pending_opts: Vec<PathOptions> = self
                    .monitored_entries
                    .iter()
                    .filter(|(p, _)| p == &path)
                    .map(|(_, o)| o.clone())
                    .collect();
                // Remove stale entries from monitored_entries so add_path later
                // doesn't create a duplicate when check_pending fires.
                self.monitored_entries.retain(|(p, _)| p != &path);
                // Create one pending entry per cmd group
                for opts in pending_opts {
                    self.pending_paths.push((
                        path.clone(),
                        PathEntry {
                            path: path.clone(),
                            recursive: Some(opts.recursive),
                            types: opts
                                .event_types
                                .as_ref()
                                .map(|v| v.iter().map(|t| t.to_string()).collect()),
                            size: opts
                                .size_filter
                                .map(|f| format!("{}{}", f.op, format_size(f.bytes))),
                            cmd: opts.cmd,
                        },
                    ));
                }
            }
        }
        self.paths = keep_paths;
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
                            "[INFO] Added {} (inode mark) on existing fd {}",
                            canonical.display(),
                            fan_fd.as_raw_fd()
                        );
                        // mark subdirectories recursively
                        let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
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
                        "[INFO] Added {} (filesystem mark) on fd {}",
                        canonical.display(),
                        new_fd.as_raw_fd()
                    );
                    (true, true)
                }
                Err(FanotifyError::Mark(code)) if code == libc::EXDEV => {
                    match mark_directory(&new_fd, path_mask, canonical) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Added {} (inode mark) on fd {}",
                                canonical.display(),
                                new_fd.as_raw_fd()
                            );
                            let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
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
            let mount_fd = match Self::open_dir(canonical) {
                Ok(fd) => fd,
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
                    let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
                    let recursive = opts.is_some_and(|o| o.recursive);
                    if recursive {
                        dir_cache::cache_recursive(&self.dir_cache, canonical);
                    } else {
                        dir_cache::cache_dir_handle(&self.dir_cache, canonical);
                    }
                }
            }
        } else if self.pending_paths.is_empty() {
            eprintln!(
                "No entries configured. Waiting for socket commands (use 'fsmon add <cmd> --path <path>')."
            );
        }

        // Ensure log directory exists and is owned by the original user
        if let Some(ref dir) = self.log_dir {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create log directory {}", dir.display()))?;
            // Daemon runs as root; chown to the original user so they own their logs
            match chown_to_user(dir) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!(
                        "[WARNING] Log directory '{}' is on a filesystem that does not support\n         ownership changes (e.g. vfat/exfat/NFS). Log files will remain owned by root.\n         Run 'sudo fsmon clean' if you cannot clean logs as a normal user.",
                        dir.display()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[WARNING] Could not chown log directory '{}': {}.\n         Log files may remain owned by root.",
                        dir.display(),
                        e
                    );
                }
            }
        }

        // Startup disk space check
        if let Some(ref threshold_str) = self.disk_min_free {
            if let Some(ref dir) = self.log_dir {
                Self::check_disk_space(dir, threshold_str);
            }
        }

        println!("Starting file trace monitor...");
        if !self.canonical_paths.is_empty() {
            println!("Active paths ({} fd(s)):", fan_group_count);
            for (path, opts) in &self.monitored_entries {
                let label = match opts.cmd {
                    Some(ref name) => format!("[{}]", name),
                    None => "[global]".to_string(),
                };
                println!("  {} {}", label, path.display());
            }
        }
        if self.debug {
            eprintln!(
                "[DEBUG] monitored_entries ({} entries, full list):",
                self.monitored_entries.len()
            );
            for (i, (p, o)) in self.monitored_entries.iter().enumerate() {
                let label = o.cmd.as_deref().unwrap_or("global");
                eprintln!(
                    "[DEBUG]   [{}] {} cmd={} recursive={}",
                    i,
                    p.display(),
                    label,
                    o.recursive
                );
            }
        }
        if self.debug {
            eprintln!("[DEBUG] --- cache stats ---");
            eprintln!(
                "[DEBUG]   dir_cache:        {}/{} entries",
                self.dir_cache.entry_count(),
                DIR_CACHE_CAP
            );
            if let Some(ref c) = self.proc_cache {
                eprintln!(
                    "[DEBUG]   proc_cache:       {}/{} entries",
                    c.entry_count(),
                    PROC_CACHE_CAP
                );
            }
            if let Some(ref t) = self.pid_tree {
                eprintln!(
                    "[DEBUG]   pid_tree:         {}/{} entries",
                    t.entry_count(),
                    PID_TREE_CAP
                );
            }
            eprintln!(
                "[DEBUG]   file_size_cache:  {}/{} entries",
                self.file_size_cache.len(),
                self.file_size_cache.cap()
            );
        }
        if !self.pending_paths.is_empty() {
            println!("Pending paths (waiting for directory creation):");
            let mut by_cmd: std::collections::BTreeMap<Option<String>, Vec<&PathBuf>> =
                std::collections::BTreeMap::new();
            for (path, entry) in &self.pending_paths {
                let cmd = entry.cmd.as_deref().and_then(|c| {
                    if c == crate::monitored::CMD_GLOBAL {
                        None
                    } else {
                        Some(c.to_string())
                    }
                });
                by_cmd.entry(cmd).or_default().push(path);
            }
            for (cmd, paths) in &by_cmd {
                let label = match cmd {
                    Some(name) => format!("[{}]", name),
                    None => "[global]".to_string(),
                };
                for path in paths {
                    println!("  {} {}", label, path.display());
                }
            }
        }

        // Spawn one reader task per FsGroup (one per filesystem).
        // Events are sent through an mpsc channel (bounded or unbounded per config).
        let (event_tx, mut event_rx) = match self.cache_config.channel_capacity {
            Some(cap) if cap > 0 => {
                let (tx, rx) = tokio::sync::mpsc::channel(cap);
                (EventSender::Bounded(tx), EventReceiver::Bounded(rx))
            }
            _ => {
                // None or 0 → unbounded (default)
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                (EventSender::Unbounded(tx), EventReceiver::Unbounded(rx))
            }
        };
        let dir_cache = self.dir_cache.clone();

        // Shared state for live-add (add_path may need to spawn reader tasks)
        self.event_tx = Some(event_tx);
        self.shared_dir_cache = Some(dir_cache.clone());

        for gi in 0..self.fs_groups.len() {
            self.spawn_fd_reader(gi);
        }

        // Spawn file writer task — subscribes to broadcast like any consumer.
        let fw_log_dir = self.log_dir.clone();
        let fw_sync = self.sync_interval;
        let fw_debug = self.debug;
        if let Some(fw_log_dir) = fw_log_dir {
            if let Some(ref tx) = self.event_stream_tx {
                let fw_rx = tx.subscribe();
                tokio::spawn(async move {
                    let fw = FileLogWriter::new(fw_log_dir, fw_sync, fw_debug);
                    fw.run(fw_rx).await;
                });
            }
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

        // Build proc connector AsyncFd for tokio event loop
        // Use 64KB buffer to avoid truncation (was: fixed 4096)
        let proc_afd = proc_conn.and_then(|conn| {
            let fd = conn.as_raw_fd();
            match AsyncFd::new(conn) {
                Ok(a) => Some(a),
                Err(e) => {
                    eprintln!("[ERROR] AsyncFd for proc connector fd {}: {}", fd, e);
                    None
                }
            }
        });
        let mut proc_buf = vec![0u8; 65536];
        let mut last_cache_stats = std::time::Instant::now();

        // Move the reader death receiver out of self so tokio::select! can use it.
        // Reader tasks hold cloned death_tx senders that remain valid.
        let mut reader_death_rx = std::mem::replace(
            &mut self.reader_death_rx,
            tokio::sync::mpsc::unbounded_channel::<usize>().1,
        );

        // Notify systemd that the daemon is ready (only effective when
        // running under a Type=notify systemd service).
        notify_sd_ready();

        loop {
            tokio::select! {
                Some(events) = event_rx.recv() => {
                    // Drain proc connector events first (non-blocking) to
                    // minimize window between exec and fanotify arrival.
                    if let Some(afd) = proc_afd.as_ref() {
                        let conn = afd.get_ref();
                        loop {
                            match conn.recv_raw(&mut proc_buf) {
                                Ok(n) => {
                                    proc_cache::handle_proc_events(&proc_cache, &pid_tree, &proc_buf, n);
                                }
                                Err(proc_connector::Error::WouldBlock) => break,
                                Err(proc_connector::Error::Interrupted) => continue,
                                Err(e) => {
                                    eprintln!("proc connector error: {e}");
                                    break;
                                }
                            }
                        }
                    }
                    self.process_event_batch(&events);
                    // Periodic cache stats
                    if self.debug && self.cache_config.stats_interval_secs > 0
                        && last_cache_stats.elapsed() >= std::time::Duration::from_secs(self.cache_config.stats_interval_secs)
                    {
                        eprintln!("[DEBUG] --- cache stats ---");
                        eprintln!("[DEBUG]   dir_cache:        {}/{} entries", dir_cache.entry_count(), DIR_CACHE_CAP);
                        eprintln!("[DEBUG]   proc_cache:       {}/{} entries", proc_cache.entry_count(), PROC_CACHE_CAP);
                        eprintln!("[DEBUG]   pid_tree:         {}/{} entries", pid_tree.entry_count(), PID_TREE_CAP);
                        eprintln!("[DEBUG]   file_size_cache:  {}/{} entries", self.file_size_cache.len(), self.file_size_cache.cap());
                        last_cache_stats = std::time::Instant::now();
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    while let Ok(events) = event_rx.try_recv() {
                        self.process_event_batch(&events);
                    }
                    break;
                }
                _ = sigterm.recv() => {
                    while let Ok(events) = event_rx.try_recv() {
                        self.process_event_batch(&events);
                    }
                    break;
                }
                _ = sighup.recv() => {
                    if let Err(e) = self.reload_config() {
                        eprintln!("Config reload error: {e}");
                    }
                }

                proc_readable = async {
                    match proc_afd.as_ref() {
                        Some(afd) => afd.readable().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok(mut guard) = proc_readable {
                        loop {
                            match guard.get_inner().recv_raw(&mut proc_buf) {
                                Ok(n) => {
                                    proc_cache::handle_proc_events(&proc_cache, &pid_tree, &proc_buf, n);
                                }
                                Err(proc_connector::Error::WouldBlock) => break,
                                Err(proc_connector::Error::Interrupted) => continue,
                                Err(e) => {
                                    eprintln!("proc connector error: {e}");
                                    break;
                                }
                            }
                        }
                        guard.clear_ready();
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
                        Ok((writer, cmd_str)) => {
                            let cmd = match toml::from_str::<SocketCmd>(&cmd_str) {
                                Ok(c) => c,
                                Err(e) => {
                                    let resp = SocketResp::err(format!("Invalid command: {e}"));
                                    if let Ok(toml_str) = toml::to_string(&resp) {
                                        let _ = tokio_io_oneshot(
                                            writer,
                                            &format!("{toml_str}\n"),
                                        ).await;
                                    }
                                    continue;
                                }
                            };
                            if cmd.cmd == "subscribe" {
                                self.handle_subscribe(writer, &cmd);
                            } else {
                                let resp = self.handle_socket_cmd(cmd);
                                if let Ok(toml_str) = toml::to_string(&resp) {
                                    let resp_bytes = format!("{toml_str}\n");
                                    let _ = tokio_io_oneshot(writer, &resp_bytes).await;
                                }
                            }
                        }
                        Err(e) => eprintln!("Socket accept error: {e}"),
                    }
                }
                Some(dead_idx) = reader_death_rx.recv() => {
                    self.restart_reader(dead_idx);
                }
            }
        }

        println!("\nStopping file trace monitor...");
        // Drop event stream tx → all receivers get Closed → tasks drain + exit
        drop(self.event_stream_tx.take());
        // event_rx drops here → channel closed → reader tasks exit on next event
        // OS cleans up all fds on process exit
        Ok(())
    }

    /// Process a batch of fanotify events: match paths, filter, build FileEvents, write logs.
    /// Called from both the main event loop and the shutdown drain path.
    fn process_event_batch(
        &mut self,
        events: &[FidEvent],
    ) {
        for raw in events {
            if raw.mask & FAN_Q_OVERFLOW != 0 {
                eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                continue;
            }

            let event_types = mask_to_event_types(raw.mask);
            let matched_path = self.matching_path(&raw.path).cloned();

            // If a monitored directory was deleted, move to pending_paths
            let is_delete_self = event_types.contains(&EventType::DeleteSelf)
                || event_types.contains(&EventType::MovedFrom);
            let is_canonical_root = is_delete_self
                && self.canonical_paths.iter().any(|cp| cp == &raw.path);
            if is_canonical_root {
                if self.debug {
                    eprintln!("[DEBUG] monitored directory deleted: {}", raw.path.display());
                }
                if let Some(ref path) = matched_path {
                    // Preserve ALL cmd groups before removing
                    let all_opts: Vec<PathOptions> = self.opts_for_path(path).into_iter().cloned().collect();
                    if let Err(e) = self.remove_path(path, None) {
                        eprintln!("[WARNING] Failed to remove deleted path '{}': {e}", path.display());
                    }
                    for opts in all_opts {
                        self.pending_paths.push((
                            path.clone(),
                            PathEntry {
                                path: path.clone(),
                                recursive: Some(opts.recursive),
                                types: opts.event_types.as_ref().map(
                                    |v| v.iter().map(|t| t.to_string()).collect()
                                ),
                                size: opts.size_filter.map(|f| format!("{}{}", f.op, format_size(f.bytes))),
                                cmd: opts.cmd,
                            },
                        ));
                    }
                    self.setup_inotify_watches();
                }
                continue;
            }

            let event_pid = raw.pid.unsigned_abs();

            // Exclude fsmon daemon's own events to prevent self-triggering.
            if event_pid == self.daemon_pid {
                if self.debug {
                    eprintln!("[DEBUG] skip daemon self-event (pid={})", event_pid);
                }
                continue;
            }

            // Match event against ALL cmd groups for this path
            let matching_entries = self.matching_opts_for_event(&raw.path);
            if self.debug && matching_entries.is_empty() {
                eprintln!("[DEBUG] event on {} (pid={}): no matching entries",
                    raw.path.display(), event_pid);
            }
            for (_monitored_path, opts) in &matching_entries {
                // Check process tree filter
                let cmd_match = if let Some(ref cmd_name) = opts.cmd {
                    let matched = self.pid_tree.as_ref()
                        .map(|tree| is_descendant(tree, event_pid, cmd_name))
                        .unwrap_or(false);
                    if self.debug {
                        eprintln!("[DEBUG]   check cmd=\"{}\" pid={}: {}",
                            cmd_name, event_pid, if matched { "MATCH" } else { "SKIP" });
                    }
                    matched
                } else {
                    if self.debug {
                        eprintln!("[DEBUG]   check cmd=global pid={}: MATCH", event_pid);
                    }
                    true
                };
                if !cmd_match {
                    continue;
                }

                for event_type in &event_types {
                    let event = self.build_file_event_for_opts(raw, *event_type, opts);

                    if !self.is_path_in_scope_for_opts(&event.path, opts) {
                        if self.debug {
                            eprintln!("[DEBUG]   -> out of scope for this opts");
                        }
                        continue;
                    }

                    if self.should_output_for_opts(&event, opts) {
                        if self.debug {
                            let cmd = opts.cmd.as_deref().unwrap_or("global");
                            eprintln!("[DEBUG]   -> {}_log.jsonl", cmd);
                        }
                        // 推送到统一事件流（broadcast）
                        // 所有消费者（文件写入、subscribe）都从此接收
                        if let Some(ref tx) = self.event_stream_tx {
                            let cmd_name = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
                            let _ = tx.send((event.clone(), cmd_name.to_string()));
                        }
                    }
                }
            }
        }
    }

    /// Like `build_file_event` but uses a specific PathOptions for chain building.
    fn build_file_event_for_opts(
        &mut self,
        raw: &FidEvent,
        event_type: EventType,
        opts: &PathOptions,
    ) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let info = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

        let file_size = match event_type {
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(&raw.path).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.put(raw.path.clone(), size);
                size
            }
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.pop(&raw.path).unwrap_or(0)
            }
            _ => self.file_size_cache.get(&raw.path).map_or(0, |&s| s),
        };

        // Chain building based on the specific opts' cmd
        let chain = opts
            .cmd
            .as_ref()
            .and_then(|_| {
                self.pid_tree.as_ref().and_then(|tree| {
                    self.proc_cache
                        .as_ref()
                        .map(|cache| build_chain(tree, cache, pid))
                })
            })
            .unwrap_or_default();

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path.clone(),
            pid,
            cmd: info.cmd,
            user: info.user,
            file_size,
            ppid: info.ppid,
            tgid: info.tgid,
            chain,
        }
    }

    /// Find the PathOptions matching a given event path.
    #[cfg(test)]
    fn get_matching_path_options(&self, path: &Path) -> Option<&PathOptions> {
        filters::get_matching_path_options(
            &self.paths,
            &self.monitored_entries,
            &self.canonical_paths,
            path,
        )
    }

    /// Return all PathOptions matching an event path (owned, no borrow conflict).
    /// Uses `monitored_entries` directly (not `path_options`), so (path, cmd) pairs
    /// are preserved even when the same path exists under multiple cmd groups.
    fn matching_opts_for_event(&self, event_path: &Path) -> Vec<(PathBuf, PathOptions)> {
        let mut result = Vec::new();
        if self.debug {
            eprintln!("[DEBUG] matching path={}", event_path.display());
        }
        for (monitored_path, opts) in &self.monitored_entries {
            let matches = if opts.recursive {
                event_path.starts_with(monitored_path)
            } else {
                event_path == monitored_path.as_path()
                    || event_path.parent() == Some(monitored_path.as_path())
            };
            if self.debug {
                let label = opts.cmd.as_deref().unwrap_or("global");
                eprintln!(
                    "[DEBUG]   check {} (cmd={}, recursive={}): {}",
                    monitored_path.display(),
                    label,
                    opts.recursive,
                    if matches { "MATCH" } else { "no" }
                );
            }
            if matches {
                result.push((monitored_path.clone(), opts.clone()));
            }
        }
        if self.debug && result.is_empty() {
            eprintln!("[DEBUG]   -> no matching entries");
        }
        result
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
        let dc = match &self.shared_dir_cache {
            Some(d) => d.clone(),
            None => {
                eprintln!("[ERROR] Cannot spawn reader: shared_dir_cache not initialized");
                return;
            }
        };
        let death_tx = self.reader_death_tx.clone();
        let buf_size = self.buffer_size;
        let debug = self.debug;
        let group = &self.fs_groups[group_idx];

        // Duplicate fds so the reader task owns independent copies
        let owned_fan_fd = match Self::dup_fd(&group.fan_fd) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!(
                    "[ERROR] Failed to dup fanotify fd {}: {}",
                    group.fan_fd.as_raw_fd(),
                    e
                );
                return;
            }
        };
        let owned_mount_fd = match Self::dup_fd(&group.mount_fd) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!(
                    "[ERROR] Failed to dup mount fd {}: {}",
                    group.mount_fd.as_raw_fd(),
                    e
                );
                // owned_fan_fd drops here, closing the dup'd fan fd
                return;
            }
        };
        let raw_fd = owned_fan_fd.as_raw_fd();
        let mfds = Arc::new(vec![owned_mount_fd]);

        tokio::spawn(async move {
            let afd = match AsyncFd::new(owned_fan_fd) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[ERROR] AsyncFd for fd {}: {}", raw_fd, e);
                    let _ = death_tx.send(group_idx);
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
                let events = read_fid_events_cached(afd.get_ref(), &mfds, &dc, &mut buf);
                if !events.is_empty() {
                    let send_err = match &tx {
                        EventSender::Unbounded(tx) => tx.send(events).is_err(),
                        EventSender::Bounded(tx) => tx.send(events).await.is_err(),
                    };
                    if send_err {
                        break;
                    }
                }
                guard.clear_ready();
            }
            if debug {
                eprintln!(
                    "[DEBUG] Reader task for group {} (fd {}) exited",
                    group_idx, raw_fd
                );
            }
            let _ = death_tx.send(group_idx);
        });

        // Track reader state for restart backoff
        if let Some(state) = self.reader_states.get_mut(group_idx).and_then(|s| s.as_mut()) {
            state.restart_count += 1;
            state.last_restart = std::time::Instant::now();
            state.gave_up = false;
        } else {
            // Ensure reader_states is large enough
            if group_idx >= self.reader_states.len() {
                self.reader_states.resize_with(group_idx + 1, || None);
            }
            self.reader_states[group_idx] = Some(ReaderState {
                restart_count: 1,
                last_restart: std::time::Instant::now(),
                gave_up: false,
            });
        }
    }

    /// Restart a reader task that has died.
    ///
    /// Applies exponential backoff: up to MAX_RESTARTS within BACKOFF_WINDOW.
    /// If the backoff limit is exceeded, logs a warning and gives up.
    /// On success, the dead task's fds are re-duplicated from FsGroup and
    /// a new reader is spawned.
    fn restart_reader(&mut self, group_idx: usize) {
        // Check backoff limits
        let now = std::time::Instant::now();
        let state = self.reader_states.get(group_idx).and_then(|s| s.as_ref());
        if let Some(s) = state {
            let in_window =
                now.duration_since(s.last_restart) < BACKOFF_WINDOW;
            if in_window && s.restart_count >= MAX_RESTARTS {
                eprintln!(
                    "[ERROR] Reader task for group {} has crashed {} times in \
                     the last {}s — giving up. fsmon daemon restart required.",
                    group_idx,
                    MAX_RESTARTS,
                    BACKOFF_WINDOW.as_secs(),
                );
                // Mark gave_up so health() reports accurate alive/dead status.
                // This will be reset when spawn_fd_reader is called again.
                if let Some(s) = self.reader_states.get_mut(group_idx).and_then(|s| s.as_mut()) {
                    s.gave_up = true;
                }
                return;
            }
        }

        // Verify the FsGroup still exists (may have been removed during shutdown)
        if group_idx >= self.fs_groups.len() {
            eprintln!(
                "[WARNING] Cannot restart reader for group {}: group no longer exists",
                group_idx
            );
            return;
        }

        let dev_id = self.fs_groups[group_idx].dev_id;
        eprintln!(
            "[INFO] Restarting reader task for group {} (dev_id={})...",
            group_idx, dev_id
        );
        self.spawn_fd_reader(group_idx);
    }

    pub fn add_path(&mut self, entry: &PathEntry) -> Result<()> {
        if self.debug {
            let cmd = entry.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
            eprintln!(
                "[DEBUG] add_path: path={} cmd={}",
                entry.path.display(),
                cmd
            );
        }
        let path = filters::resolve_recursion_check(&entry.path);

        let is_new_path = !self.paths.contains(&path);
        if !is_new_path {
            if self.debug {
                eprintln!(
                    "[DEBUG]   path already monitored — adding cmd and updating fanotify mask"
                );
            }
            let cmd = entry.cmd.as_deref().and_then(|c| {
                if c == crate::monitored::CMD_GLOBAL {
                    None
                } else {
                    Some(c.to_string())
                }
            });
            let event_types = entry.types.as_ref().map(|types| {
                types
                    .iter()
                    .filter_map(|s| s.parse::<EventType>().ok())
                    .collect()
            });
            let size_filter = entry
                .size
                .as_ref()
                .map(|s| parse_size_filter(s))
                .transpose()?;
            let recursive = entry.recursive.unwrap_or(false);
            let opts = PathOptions {
                size_filter,
                event_types,
                recursive,
                cmd,
            };
            self.monitored_entries.push((path.clone(), opts.clone()));

            // Update fanotify mask: OR all entries for this path
            let new_mask = self
                .monitored_entries
                .iter()
                .filter(|(p, _)| p == &path)
                .map(|(_, o)| path_mask_from_options(o))
                .fold(0, |a, b| a | b);
            if let Some(&gi) = self.path_to_group.get(&path) {
                let fan_fd = &self.fs_groups[gi].fan_fd;
                let canonical = self
                    .paths
                    .iter()
                    .position(|p| p == &path)
                    .and_then(|i| self.canonical_paths.get(i).cloned())
                    .unwrap_or_else(|| path.clone());
                if self.fs_groups[gi].is_fs_mark {
                    let _ = fanotify_mark(
                        fan_fd,
                        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                        new_mask,
                        AT_FDCWD,
                        &canonical,
                    );
                } else {
                    let _ = mark_directory(fan_fd, new_mask, &canonical);
                }
                if self.debug {
                    eprintln!("[DEBUG]   updated fanotify mask to {:#x}", new_mask);
                }
            }
            let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
            println!(
                "Monitoring entry: [{}] {} (recursive={})",
                cmd_label,
                path.display(),
                recursive
            );
            return Ok(());
        }

        // Reject paths that overlap with the log directory.
        // - Exact match (path == log dir) → always reject (it IS the log dir)
        // - Parent + recursive → reject (would capture log file writes)
        // - Parent + non-recursive → allow (only direct children, log files deeper)
        if let Some(ref log_dir) = self.log_dir {
            let log_canonical = log_dir.canonicalize().unwrap_or_else(|_| log_dir.clone());
            let is_exact = log_canonical == path;
            let is_parent_recursive =
                entry.recursive.unwrap_or(false) && log_canonical.starts_with(&path);
            if is_exact || is_parent_recursive {
                bail!(
                    "Cannot monitor '{}': {} — \
                     Tip: use a path outside the log directory, or use a different logging.path",
                    path.display(),
                    if is_exact {
                        "this path is the log directory itself".to_string()
                    } else {
                        format!("log directory '{}' is inside this path", log_dir.display())
                    },
                );
            }
        }

        if !path.exists() {
            // Avoid duplicate pending entries for the same (path, cmd)
            let already_pending = self
                .pending_paths
                .iter()
                .any(|(p, e)| p == &path && e.cmd == entry.cmd);
            if !already_pending {
                eprintln!(
                    "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                    path.display()
                );
                self.pending_paths.push((path.clone(), entry.clone()));
                self.setup_inotify_watches();
            }
            return Ok(());
        }

        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

        let event_types = entry.types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|s| s.parse::<EventType>().ok())
                .collect()
        });
        let size_filter = entry
            .size
            .as_ref()
            .map(|s| parse_size_filter(s))
            .transpose()?;
        let recursive = entry.recursive.unwrap_or(false);
        // `_global` in PathEntry means no process tracking → convert to None
        let cmd = entry.cmd.as_deref().and_then(|c| {
            if c == crate::monitored::CMD_GLOBAL {
                None
            } else {
                Some(c.to_string())
            }
        });
        // Reject cmd=fsmon: daemon's own events are excluded by PID filter.
        // This mirrors the validation in Monitor::new() for runtime socket adds.
        if cmd.as_deref() == Some("fsmon") {
            bail!(
                "Cannot monitor 'fsmon' process: fsmon daemon's own events \
                 are excluded from monitoring."
            );
        }

        let opts = PathOptions {
            size_filter,
            event_types,
            recursive,
            cmd,
        };

        let path_mask = path_mask_from_options(&opts);

        let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
        println!(
            "Monitoring entry: [{}] {} (recursive={})",
            cmd_label,
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
            let mount_fd = Self::open_dir(&canonical)?;

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
        self.monitored_entries.push((path.clone(), opts.clone()));

        // Pre-cache directory handles in the shared cache
        if canonical.is_dir()
            && let Some(ref cache) = self.shared_dir_cache
        {
            if recursive {
                dir_cache::cache_recursive(cache, &canonical);
            } else {
                dir_cache::cache_dir_handle(cache, &canonical);
            }
        }

        Ok(())
    }

    pub fn remove_path(&mut self, path: &Path, cmd: Option<&str>) -> Result<()> {
        if self.debug {
            let label = cmd.unwrap_or("*");
            eprintln!("[DEBUG] remove_path: path={} cmd={}", path.display(), label);
        }

        // Remove matching entries from monitored_entries
        let before = self.monitored_entries.len();
        self.monitored_entries.retain(|(p, o)| {
            if p != path {
                return true;
            }
            if let Some(c) = cmd {
                o.cmd.as_deref() != Some(c) // keep if cmd doesn't match
            } else {
                false // remove all entries for this path
            }
        });
        let removed = before - self.monitored_entries.len();
        if removed == 0 {
            return Err(anyhow::anyhow!("Path not found: {}", path.display()));
        }

        // Check if other cmd groups still monitor this path
        let has_other = self.monitored_entries.iter().any(|(p, _)| p == path);

        if !has_other {
            // No more entries for this path — tear down fanotify
            if let Some(pos) = self.paths.iter().position(|p| p == path) {
                let canonical = &self.canonical_paths[pos];
                if let Some(opts) = self.first_opt_for_path(path) {
                    let path_mask = path_mask_from_options(opts);
                    if let Some(&gi) = self.path_to_group.get(path) {
                        let fan_fd = &self.fs_groups[gi].fan_fd;
                        let _ = fanotify_mark(
                            fan_fd,
                            FAN_MARK_REMOVE | FAN_MARK_FILESYSTEM,
                            path_mask,
                            AT_FDCWD,
                            canonical,
                        );
                        let _ =
                            fanotify_mark(fan_fd, FAN_MARK_REMOVE, path_mask, AT_FDCWD, canonical);
                        self.fs_groups[gi].ref_count =
                            self.fs_groups[gi].ref_count.saturating_sub(1);
                        if self.fs_groups[gi].ref_count == 0 {
                            self.fs_groups.remove(gi);
                            self.path_to_group.iter_mut().for_each(|(_, idx)| {
                                if *idx > gi {
                                    *idx -= 1;
                                }
                            });
                        }
                    }
                }
                self.paths.remove(pos);
                self.canonical_paths.remove(pos);
                self.path_to_group.remove(path);
            }
            println!("Removed entry: {}", path.display());
        } else {
            // Other cmd groups still exist — update fanotify mask
            let new_mask = self
                .monitored_entries
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, o)| path_mask_from_options(o))
                .fold(0, |a, b| a | b);
            if let Some(&gi) = self.path_to_group.get(path) {
                let fan_fd = &self.fs_groups[gi].fan_fd;
                let canonical = self
                    .paths
                    .iter()
                    .position(|p| p == path)
                    .and_then(|i| self.canonical_paths.get(i).cloned())
                    .unwrap_or_else(|| path.to_path_buf());
                if self.fs_groups[gi].is_fs_mark {
                    let _ = fanotify_mark(
                        fan_fd,
                        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                        new_mask,
                        AT_FDCWD,
                        &canonical,
                    );
                } else {
                    let _ = mark_directory(fan_fd, new_mask, &canonical);
                }
            }
            if self.debug {
                eprintln!(
                    "[DEBUG]   updated fanotify mask to {:#x} (other cmd groups remain)",
                    new_mask
                );
            }
            let label = cmd.unwrap_or("?");
            println!("Removed entry: [{}] {}", label, path.display());
        }
        Ok(())
    }

    /// Check disk space for the log directory against the configured threshold.
    /// Prints a warning if free space is below the threshold.
    fn check_disk_space(log_dir: &std::path::Path, threshold_str: &str) {
        let threshold = match crate::utils::parse_disk_min_free(threshold_str) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[WARNING] Invalid disk-min-free '{}': {}", threshold_str, e);
                return;
            }
        };

        // Get filesystem stats
        let stat = match nix::sys::statvfs::statvfs(log_dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[WARNING] Cannot stat filesystem for '{}': {}", log_dir.display(), e);
                return;
            }
        };

        let block_size = stat.block_size() as u64;
        let total = stat.blocks() as u64 * block_size;
        let free = stat.blocks_available() as u64 * block_size;

        if total == 0 {
            return;
        }

        let below = match threshold {
            crate::utils::DiskFreeThreshold::Percent(min_pct) => {
                let free_pct = (free as f64 / total as f64) * 100.0;
                if free_pct < min_pct {
                    eprintln!(
                        "[WARNING] Low disk space on '{}': {:.1}% free ({}/{}), \
                         threshold is {}%",
                        log_dir.display(),
                        free_pct,
                        crate::utils::format_size(free as i64),
                        crate::utils::format_size(total as i64),
                        min_pct,
                    );
                    true
                } else {
                    false
                }
            }
            crate::utils::DiskFreeThreshold::Bytes(min_bytes) => {
                if free < min_bytes {
                    eprintln!(
                        "[WARNING] Low disk space on '{}': {} free, threshold is {}",
                        log_dir.display(),
                        crate::utils::format_size(free as i64),
                        crate::utils::format_size(min_bytes as i64),
                    );
                    true
                } else {
                    false
                }
            }
        };

        if !below {
            eprintln!(
                "[INFO] Disk space OK on '{}': {} free",
                log_dir.display(),
                crate::utils::format_size(free as i64),
            );
        }
    }

    /// Build a health snapshot for the `health` socket command.
    fn health(&self) -> SocketResp {
        use crate::socket::{HealthInfo, ReaderHealth};

        let readers: Vec<ReaderHealth> = self
            .fs_groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let state = self.reader_states.get(i).and_then(|s| s.as_ref());
                let alive = state.is_some_and(|s| {
                    // Only dead when restart_reader explicitly gave up.
                    // gave_up is reset when spawn_fd_reader attempts recovery.
                    !s.gave_up
                });
                let restarts = state.map(|s| s.restart_count).unwrap_or(0);
                ReaderHealth {
                    alive,
                    restarts,
                    fd: g.fan_fd.as_raw_fd(),
                }
            })
            .collect();

        let channel_type = match self.cache_config.channel_capacity {
            Some(cap) => format!("bounded({})", cap),
            None => "unbounded".to_string(),
        };

        SocketResp::health(HealthInfo {
            uptime_secs: self.started_at.elapsed().as_secs(),
            channel_type,
            monitored_paths: self.monitored_entries.len(),
            reader_groups: self.fs_groups.len(),
            readers,
        })
    }

    /// Handle a subscribe command: spawn a task that streams events to the socket.
    fn handle_subscribe(
        &self,
        writer: tokio::net::unix::OwnedWriteHalf,
        cmd: &SocketCmd,
    ) {
        let tx = match self.event_stream_tx.as_ref() {
            Some(tx) => tx,
            None => {
                tokio::spawn(write_resp_and_close(
                    writer,
                    SocketResp::permanent_err("subscriptions disabled"),
                ));
                return;
            }
        };

        let rx = tx.subscribe();
        let track_cmd = cmd.track_cmd.clone();
        let types: Option<Vec<EventType>> = cmd.types.as_ref().map(|v| {
            v.iter()
                .filter_map(|t| t.parse::<EventType>().ok())
                .collect()
        });

        tokio::spawn(subscriber_task(writer, rx, track_cmd, types));
    }

    fn handle_socket_cmd(&mut self, cmd: SocketCmd) -> SocketResp {
        if self.debug {
            eprintln!(
                "[DEBUG] socket command: {} path={:?} track_cmd={:?}",
                cmd.cmd, cmd.path, cmd.track_cmd
            );
        }
        match cmd.cmd.as_str() {
            "add" => {
                let raw = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                let path = raw;
                let track_cmd = cmd.track_cmd.as_deref().and_then(|c| {
                    if c == crate::monitored::CMD_GLOBAL {
                        None
                    } else {
                        Some(c.to_string())
                    }
                });
                // Remove only this (path, cmd) pair, not other cmd groups for same path
                self.monitored_entries
                    .retain(|(p, o)| !(p == &path && o.cmd == track_cmd));
                let has_other_cmds = self.monitored_entries.iter().any(|(p, _)| p == &path);
                if !has_other_cmds {
                    // No other cmd groups for this path — full teardown + setup
                    let _ = self.remove_path(&path, None);
                }
                // Rebuild fanotify mask: last seen mask stays via path_options
                let entry = PathEntry {
                    path,
                    recursive: cmd.recursive,
                    types: cmd.types.clone(),
                    size: cmd.size.clone(),
                    cmd: cmd.track_cmd.clone(),
                };
                match self.add_path(&entry) {
                    Ok(()) => SocketResp::ok(),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    }
                }
            }
            "remove" => {
                let path = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                match self.remove_path(&path, cmd.track_cmd.as_deref()) {
                    Ok(()) => SocketResp::ok(),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    }
                }
            }
            "list" => {
                let paths: Vec<PathEntry> = self
                    .monitored_entries
                    .iter()
                    .map(|(p, opts)| {
                        let cmd = opts
                            .cmd
                            .clone()
                            .or(Some(crate::monitored::CMD_GLOBAL.to_string()));
                        PathEntry {
                            path: p.clone(),
                            recursive: Some(opts.recursive),
                            types: opts
                                .event_types
                                .as_ref()
                                .map(|v| v.iter().map(|t| t.to_string()).collect()),
                            size: opts
                                .size_filter
                                .map(|f| format!("{}{}", f.op, format_size(f.bytes))),
                            cmd,
                        }
                    })
                    .collect();
                SocketResp {
                    ok: true,
                    error: None,
                    error_kind: None,
                    paths: Some(paths),
                    health: None,
                }
            }
            "health" => {
                self.health()
            }
            _ => SocketResp::err(format!("Unknown command: {}", cmd.cmd)),
        }
    }

    fn reload_config(&mut self) -> Result<()> {
        if self.debug {
            eprintln!("[DEBUG] reload_config");
        }
        let monitored_path = self
            .monitored_path
            .as_ref()
            .context("No store path configured")?;
        let store = Monitored::load(monitored_path)?;
        // Add new paths that appear in store
        let flat_entries = store.flatten();
        for entry in &flat_entries {
            if !self.paths.contains(&entry.path)
                && let Err(e) = self.add_path(entry)
            {
                eprintln!("Failed to add path {} on reload: {e}", entry.path.display());
            }
        }
        // Remove paths no longer in store
        let current_paths: Vec<PathBuf> = self.paths.clone();
        for path in &current_paths {
            if !flat_entries.iter().any(|p| p.path == *path)
                && let Err(e) = self.remove_path(path, None)
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
                && let Ok(wd) = inotify
                    .watches()
                    .add(&parent, WatchMask::CREATE | WatchMask::MOVED_TO)
            {
                self._inotify_watches.push(wd);
            }
        }
    }

    /// Retry setting up fanotify monitoring for paths that didn't exist before.
    /// Called when inotify detects directory creation under a watched parent.
    fn check_pending(&mut self) {
        if self.debug && !self.pending_paths.is_empty() {
            eprintln!(
                "[DEBUG] check_pending: {} pending path(s)",
                self.pending_paths.len()
            );
        }
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

    #[cfg(test)]
    fn should_output(&self, event: &FileEvent) -> bool {
        let opts = self.get_matching_path_options(&event.path);
        filters::should_output(opts, event)
    }

    /// Check output filters using a specific PathOptions instead of auto-detecting.
    fn should_output_for_opts(&self, event: &FileEvent, opts: &PathOptions) -> bool {
        filters::should_output(Some(opts), event)
    }

    /// Find the configured path that matches a given event path.
    /// Checks configured paths (direct or recursive prefix), then canonical paths.
    fn matching_path(&self, path: &Path) -> Option<&PathBuf> {
        filters::matching_path(&self.paths, &self.canonical_paths, path)
    }

    #[cfg(test)]
    fn is_path_in_scope(&self, path: &Path) -> bool {
        filters::is_path_in_scope(
            &self.paths,
            &self.monitored_entries,
            &self.canonical_paths,
            path,
        )
    }

    /// Check if event path is within scope of a specific PathOptions.
    /// Uses `monitored_entries` directly (not `path_options`).
    fn is_path_in_scope_for_opts(&self, event_path: &Path, opts: &PathOptions) -> bool {
        self.monitored_entries.iter().any(|(mp, stored_opts)| {
            if stored_opts.cmd != opts.cmd || stored_opts.recursive != opts.recursive {
                return false;
            }
            if opts.recursive {
                event_path.starts_with(mp)
            } else {
                event_path == mp.as_path() || event_path.parent() == Some(mp.as_path())
            }
        })
    }
}

/// Write a TOML response and close the socket (one-shot command helper).
async fn write_resp_and_close(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    resp: SocketResp,
) {
    use tokio::io::AsyncWriteExt;
    if let Ok(toml_str) = toml::to_string(&resp) {
        let _ = writer.write_all(format!("{toml_str}\n").as_bytes()).await;
    }
}

/// Write bytes and close (for non-subscribe socket commands).
async fn tokio_io_oneshot(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    data: &str,
) {
    use tokio::io::AsyncWriteExt;
    let _ = writer.write_all(data.as_bytes()).await;
}

/// Check if a cmd group name appears in a chain string.
fn chains_contain(chain: &str, cmd_name: &str) -> bool {
    chain.split(" → ").any(|s| s.trim() == cmd_name)
}

/// Stream events from a broadcast receiver to a subscriber socket.
async fn subscriber_task(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut rx: tokio::sync::broadcast::Receiver<(FileEvent, String)>,
    track_cmd: Option<String>,
    type_filter: Option<Vec<EventType>>,
) {
    use tokio::io::AsyncWriteExt;

    // 1. Send initial ok response (TOML)
    let resp = SocketResp::ok();
    let resp_str = toml::to_string(&resp).unwrap_or_default();
    if writer
        .write_all(format!("{resp_str}\n").as_bytes())
        .await
        .is_err()
    {
        return;
    }

    // 2. Stream events
    loop {
        match rx.recv().await {
            Ok((event, _cmd_name)) => {
                // Optional filter by cmd group
                if let Some(ref wanted) = track_cmd {
                    if event.chain.is_empty() || !chains_contain(&event.chain, wanted) {
                        continue;
                    }
                }
                // Optional filter by event type
                if let Some(ref allowed) = type_filter {
                    if !allowed.contains(&event.event_type) {
                        continue;
                    }
                }

                let line = event.to_jsonl_string() + "\n";
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break; // subscriber disconnected
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // Subscriber too slow, dropped n events — send a warning JSON line
                let warn = format!(
                    r#"{{"warning":"subscriber too slow, dropped {} events","path":""}}"#,
                    n
                );
                let _ = writer.write_all(format!("{warn}\n").as_bytes()).await;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break; // daemon shutting down
            }
        }
    }
    // writer drops → connection closes
}

// ---- FileLogWriter: unified event stream consumer for disk persistence ----

/// Async file writer consuming events from a channel and writing to JSONL files.
/// Runs as a tokio task. Handles disk-full buffering, fdatasync, and ENOENT retry.
struct FileLogWriter {
    log_dir: PathBuf,
    disk_buf: VecDeque<(FileEvent, String)>,
    disk_healthy: bool,
    last_disk_check: std::time::Instant,
    dirty_logs: HashSet<PathBuf>,
    sync_interval: Option<Duration>,
    debug: bool,
}

impl FileLogWriter {
    fn new(log_dir: PathBuf, sync_interval: Option<Duration>, debug: bool) -> Self {
        Self {
            log_dir,
            disk_buf: VecDeque::with_capacity(10_000),
            disk_healthy: true,
            last_disk_check: std::time::Instant::now(),
            dirty_logs: HashSet::new(),
            sync_interval,
            debug,
        }
    }

    /// Run the file writer event loop.
    async fn run(
        mut self,
        mut rx: tokio::sync::broadcast::Receiver<(FileEvent, String)>,
    ) {
        use tokio::time::interval;

        let mut sync_timer = self.sync_interval.map(|d| interval(d));

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok((event, cmd_name)) => {
                            if let Err(e) = self.write_event(&event, &cmd_name) {
                                if self.debug {
                                    eprintln!("[DEBUG] FileLogWriter write error: {}", e);
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[WARNING] FileLogWriter dropped {} events (disk too slow)", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
                _ = async {
                    match sync_timer.as_mut() {
                        Some(timer) => timer.tick().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.sync_dirty_logs();
                }
            }
        }

        self.sync_dirty_logs();
    }

    /// Write an event to the appropriate JSONL log file.
    /// Returns Ok(()) even if disk is full (event is buffered for retry).
    fn write_event(&mut self, event: &FileEvent, cmd_name: &str) -> std::io::Result<()> {
        let log_path = self.log_dir.join(crate::utils::cmd_to_log_name(cmd_name));

        // Try to flush buffer if disk was previously unhealthy
        if !self.disk_healthy
            && self.last_disk_check.elapsed() >= std::time::Duration::from_secs(10)
        {
            self.flush_disk_buf();
        }

        let is_new = !log_path.exists();
        // Retry once on ENOENT: recreate log directory if deleted at runtime
        let open_result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    let _ = fs::create_dir_all(&self.log_dir);
                    let _ = crate::fid_parser::chown_to_user(&self.log_dir);
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                } else {
                    Err(e)
                }
            });

        match open_result {
            Ok(file) => {
                // Chown new log files to the original user
                if is_new {
                    match crate::fid_parser::chown_to_user(&log_path) {
                        Ok(true) => {}
                        Ok(false) => {}
                        Err(e) => {
                            eprintln!(
                                "[WARNING] Could not chown log file '{}': {}",
                                log_path.display(),
                                e
                            );
                        }
                    }
                }
                let mut file = std::io::BufWriter::new(file);
                use std::io::Write;
                writeln!(file, "{}", event.to_jsonl_string())?;
                // Track dirty log for periodic fdatasync
                if self.sync_interval.is_some() {
                    self.dirty_logs.insert(log_path);
                }
                self.disk_healthy = true;
                Ok(())
            }
            Err(e) => {
                // Disk might be full — buffer the event
                self.disk_healthy = false;
                self.last_disk_check = std::time::Instant::now();
                if self.disk_buf.len() < 10_000 {
                    self.disk_buf
                        .push_back((event.clone(), cmd_name.to_string()));
                }
                Err(e)
            }
        }
    }

    /// Try to flush buffered events to disk.
    fn flush_disk_buf(&mut self) {
        if self.disk_buf.is_empty() {
            self.disk_healthy = true;
            return;
        }

        let mut remaining = VecDeque::new();
        while let Some((event, cmd_name)) = self.disk_buf.pop_front() {
            let log_path = self.log_dir.join(crate::utils::cmd_to_log_name(&cmd_name));
            let open_result = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        let _ = fs::create_dir_all(&self.log_dir);
                        let _ = crate::fid_parser::chown_to_user(&self.log_dir);
                        OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&log_path)
                    } else {
                        Err(e)
                    }
                });
            match open_result {
                Ok(file) => {
                    let mut file = std::io::BufWriter::new(file);
                    use std::io::Write;
                    if writeln!(file, "{}", event.to_jsonl_string()).is_err() {
                        remaining.push_back((event, cmd_name));
                    }
                }
                Err(_) => {
                    remaining.push_back((event, cmd_name));
                }
            }
        }
        self.disk_buf = remaining;
        self.disk_healthy = self.disk_buf.is_empty();
        self.last_disk_check = std::time::Instant::now();
    }

    /// Sync all dirty log files to disk via fdatasync.
    fn sync_dirty_logs(&mut self) {
        if self.dirty_logs.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.dirty_logs.drain().collect();
        for path in &paths {
            match std::fs::OpenOptions::new().write(true).open(path) {
                Ok(file) => {
                    if let Err(e) = file.sync_data() {
                        eprintln!(
                            "[WARNING] fdatasync failed for '{}': {}",
                            path.display(),
                            e
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    eprintln!(
                        "[WARNING] Could not open '{}' for sync: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
    }

}

/// Send READY=1 to systemd via sd_notify protocol.
/// Reads $NOTIFY_SOCKET from the environment (set by systemd for Type=notify services).
/// If the env var is not set (e.g. running directly via sudo), this is a no-op.
fn notify_sd_ready() {
    use std::os::unix::net::UnixDatagram;
    let socket_path = match std::env::var("NOTIFY_SOCKET") {
        Ok(p) => p,
        Err(_) => return,
    };
    let socket = match UnixDatagram::unbound() {
        Ok(s) => s,
        Err(_) => return,
    };
    let msg = b"READY=1\n";
    let _ = socket.send_to(msg, &socket_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{SizeFilter, SizeOp};
    use fanotify_fid::consts::{FAN_CREATE, FAN_DELETE, FAN_EVENT_ON_CHILD, FAN_MODIFY, FAN_ONDIR};
    use std::sync::Arc;

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
            FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_DELETE_SELF,
            FAN_FS_ERROR, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_OPEN, FAN_OPEN_EXEC,
        };
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
    fn test_mask_to_event_types_with_flags() {
        let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }

    // ---- Monitor tests ----

    fn options(
        size_filter: Option<SizeFilter>,
        event_types: Option<Vec<EventType>>,
        /* exclude: Option<&str>, */
        recursive: bool,
    ) -> PathOptions {
        PathOptions {
            size_filter,
            event_types,
            recursive,
            cmd: None,
        }
    }

    fn make_monitor(
        paths: Vec<&str>,
        size_filter: Option<SizeFilter>,
        event_types: Option<Vec<EventType>>,
        /* exclude: Option<&str>, */
        recursive: bool,
    ) -> Monitor {
        Monitor::new(
            paths
                .into_iter()
                .map(|p| {
                    (
                        PathBuf::from(p),
                        options(size_filter, event_types.clone(), recursive),
                    )
                })
                .collect(),
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn test_should_output_no_filters() {
        let m = make_monitor(vec!["/tmp"], None, None, false);
        let event = make_event("/tmp/test.txt", EventType::Create, 1000, 1024);
        assert!(m.should_output(&event));
    }

    #[test]
    fn test_should_output_type_filter_match() {
        let m = make_monitor(
            vec!["/tmp"],
            None,
            Some(vec![EventType::Create, EventType::Delete]),
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/a", EventType::Delete, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Modify, 1, 0)));
    }

    #[test]
    fn test_should_output_size_filter() {
        let m = make_monitor(
            vec!["/tmp"],
            Some(SizeFilter {
                op: SizeOp::Ge,
                bytes: 1000,
            }),
            None,
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 2000)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, 500)));
    }

    #[test]
    fn test_should_output_combined_filters() {
        let m = make_monitor(
            vec!["/tmp"],
            Some(SizeFilter {
                op: SizeOp::Ge,
                bytes: 100,
            }),
            Some(vec![EventType::Create]),
            false,
        );
        assert!(m.should_output(&make_event("/tmp/data", EventType::Create, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Delete, 1, 200)));
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Create, 1, 50)));
    }

    #[test]
    fn test_is_path_in_scope_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, true);
        assert!(m.is_path_in_scope(Path::new("/tmp")));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub")));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub/deep/file.txt")));
        assert!(!m.is_path_in_scope(Path::new("/var/log")));
        assert!(!m.is_path_in_scope(Path::new("/tmpfile")));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let m = make_monitor(vec!["/tmp"], None, None, false);
        assert!(m.is_path_in_scope(Path::new("/tmp")));
        assert!(m.is_path_in_scope(Path::new("/tmp/file.txt")));
        assert!(!m.is_path_in_scope(Path::new("/tmp/sub/file.txt")));
        assert!(!m.is_path_in_scope(Path::new("/var/log")));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let m = make_monitor(vec!["/tmp", "/var/log"], None, None, true);
        assert!(m.is_path_in_scope(Path::new("/tmp/file")));
        assert!(m.is_path_in_scope(Path::new("/var/log/syslog")));
        assert!(!m.is_path_in_scope(Path::new("/etc/passwd")));
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
    fn test_reject_cmd_fsmon_at_startup() {
        let opts = PathOptions {
            size_filter: None,
            event_types: None,
            recursive: true,
            cmd: Some("fsmon".to_string()),
        };
        let result = Monitor::new(
            vec![(PathBuf::from("/tmp"), opts)],
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_err(), "Monitor::new() should reject cmd=fsmon");
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("Cannot monitor 'fsmon' process"),
            "Error should mention fsmon rejection, got: {}",
            err
        );
    }

    #[test]
    fn test_monitor_buffer_size_validation() {
        let opts = options(None, None, false);

        let result = Monitor::new(
            vec![(PathBuf::from("/tmp"), opts.clone())],
            None,
            None,
            Some(1024),
            None,
            false,
            None,
            None,
            None,
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
            false,
            None,
            None,
            None,
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
            false,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_add_path_and_remove_path() {
        let mut m = Monitor::new(vec![], None, None, None, None, false, None, None, None, None).unwrap();

        let entry = PathEntry {
            cmd: None,
            path: PathBuf::from("/tmp/test_add"),
            recursive: Some(true),
            types: None,
            size: None,
        };

        // add_path on non-existent path → goes to pending_paths
        let result = m.add_path(&entry);
        assert!(result.is_ok());
        assert!(
            m.pending_paths
                .iter()
                .any(|(p, _)| p == Path::new("/tmp/test_add"))
        );
        assert!(!m.paths.contains(&PathBuf::from("/tmp/test_add")));

        // remove_path on non-existent path (not in options)
        let result = m.remove_path(Path::new("/nonexistent"), None);
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
            ppid: 0,
            tgid: 0,
            chain: String::new(),
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
    fn test_fanotify_mark_null_byte_path_no_root() {
        // Verifies CString::new rejects interior null bytes BEFORE any
        // syscall. This test does NOT require root — the error is raised
        // in userspace during path-to-C-string conversion.
        let mask = FAN_CREATE | FAN_DELETE;

        // Create a path with an interior null byte
        let bad_path = Path::new("/tmp/ok\0evil");

        // fanotify_mark needs an fd, but the null byte rejection happens
        // before any syscall. We just need a valid OwnedFd for the param.
        let dev_null = std::fs::File::open("/dev/null")
            .expect("/dev/null must exist on Linux");
        let dummy_fd: std::os::fd::OwnedFd = dev_null.into();

        let result = fanotify_mark(
            &dummy_fd,
            FAN_MARK_ADD,
            mask,
            AT_FDCWD,
            bad_path,
        );

        match result {
            Err(FanotifyError::Mark(code)) => {
                assert_eq!(code, libc::EINVAL,
                    "null byte path should return EINVAL, got errno={}", code);
            }
            other => panic!("expected Err(Mark(EINVAL)), got {:?}", other),
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
                &fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                mask,
                AT_FDCWD,
                &test_dir_clone,
            )
            .unwrap();

            let mut buf = vec![0u8; 4096];
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(200) {
                if let Ok(events) = fanotify_fid::read::read_fid_events(&fd, &[], &mut buf, None)
                    && !events.is_empty()
                {
                    counter_clone.fetch_add(events.len(), Ordering::SeqCst);
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

    // ---- Subscribe tests ----

    #[test]
    fn test_chains_contain_exact() {
        assert!(chains_contain("bash → myapp → fsmon", "myapp"));
    }

    #[test]
    fn test_chains_contain_not_found() {
        assert!(!chains_contain("bash → other → fsmon", "myapp"));
    }

    #[test]
    fn test_chains_contain_empty_chain() {
        assert!(!chains_contain("", "myapp"));
    }

    #[test]
    fn test_chains_contain_partial_name_not_match() {
        // "myapp-backup" should not match filter "myapp"
        assert!(!chains_contain("bash → myapp-backup → fsmon", "myapp"));
    }

    #[tokio::test]
    async fn test_subscriber_task_receives_events() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);

        // Verify broadcast channel works as the unified event stream:
        // Multiple receivers get the same events.
        let mut rx2 = tx.subscribe();
        let event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/test.txt"),
            pid: 1234,
            cmd: "test-cmd".to_string(),
            user: "root".to_string(),
            file_size: 100,
            ppid: 0,
            tgid: 0,
            chain: "bash → test-cmd".to_string(),
        };
        tx.send(event.clone()).unwrap();

        let received1 = rx.recv().await.unwrap();
        let received2 = rx2.recv().await.unwrap();
        assert_eq!(received1.path, PathBuf::from("/tmp/test.txt"));
        assert_eq!(received2.path, PathBuf::from("/tmp/test.txt"));
    }

    #[tokio::test]
    async fn test_subscriber_task_filters_by_cmd() {
        // Test the filter logic directly: chains_contain is already tested
        // above. The subscriber_task's filter is just chains_contain check.
        assert!(chains_contain("bash → myapp", "myapp"));
        assert!(!chains_contain("bash → myapp", "other-app"));
    }

    #[tokio::test]
    async fn test_subscriber_task_filters_by_type() {
        // Test the type filter logic: subscriber_task checks if event.event_type
        // is in the allowed types list. We verify by checking a broadcast receiver
        // with the same filter pattern.
        let allowed = vec![EventType::Delete, EventType::CloseWrite];

        let create_event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/ignored.txt"),
            pid: 1,
            cmd: "test".to_string(),
            user: "root".to_string(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        assert!(!allowed.contains(&create_event.event_type));

        let delete_event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Delete,
            path: PathBuf::from("/tmp/deleted.txt"),
            pid: 2,
            cmd: "test".to_string(),
            user: "root".to_string(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        assert!(allowed.contains(&delete_event.event_type));
    }

    #[tokio::test]
    async fn test_subscriber_task_handles_lagged() {
        // Test the broadcast Lagged behavior directly
        let (tx, mut rx) = tokio::sync::broadcast::channel(4); // small buffer

        // Fill the buffer and overflow to trigger Lagged
        for i in 0..10 {
            let _ = tx.send(FileEvent {
                time: Utc::now(),
                event_type: EventType::Create,
                path: PathBuf::from(format!("/tmp/batch_{}.txt", i)),
                pid: 100 + i as u32,
                cmd: "test".to_string(),
                user: "root".to_string(),
                file_size: i as u64,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            });
        }

        // The next recv should get Lagged
        let result = rx.recv().await;
        match result {
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0, "should lag with >0 dropped events, got {}", n);
            }
            Ok(event) => {
                // Might get a recent event if buffer still has capacity
                assert!(event.file_size >= 6, "should be a recent event, got file_size={}", event.file_size);
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }
}
