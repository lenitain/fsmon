use anyhow::{Context, Result, bail};
use fanotify_fid::consts::{
    FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_NONBLOCK, FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use lru::LruCache;
use tokio::io::AsyncBufReadExt;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{SignalKind, signal};

use moka::sync::Cache;

use crate::FileEvent;
use crate::config::ResolvedCacheConfig;
use crate::dir_cache;
use crate::fid_parser::{
    DIR_CACHE_CAP, FanFd, FsGroup, chown_to_user, mark_directory, mark_recursive,
    path_mask_from_options,
};
use crate::filters::{self, PathOptions};
use crate::metrics::MetricsRegistry;
use crate::monitored::PathEntry;
use crate::proc_cache::{
    self, PID_TREE_CAP, PROC_CACHE_CAP, PidTree, ProcCache, snapshot_process_tree,
};
use crate::socket::{SocketCmd, SocketError};
use crate::utils::format_size;
use serde_json;

// ---- Submodules ----

mod channel;
mod events;
mod file_writer;
mod filtering;
mod live_path;
mod reader;
mod socket_handler;

pub(crate) use channel::{EventReceiver, EventSender};
pub(crate) use events::PendingEvent;
pub(crate) use file_writer::{FileLogWriter, notify_sd_ready};
pub(crate) use reader::ReaderState;
#[cfg(test)]
pub(crate) use socket_handler::chains_contain;
pub(crate) use socket_handler::tokio_io_oneshot;

// ---- Monitor ----

pub struct Monitor {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) canonical_paths: Vec<PathBuf>,
    /// Full list of (path, PathOptions) preserving duplicates across cmd groups.
    /// This is the single source of truth for path options.
    pub(crate) monitored_entries: Vec<(PathBuf, PathOptions)>,
    pub(crate) log_dir: Option<PathBuf>,
    pub(crate) monitored_path: Option<PathBuf>,
    pub(crate) proc_cache: Option<ProcCache>,
    pub(crate) pid_tree: Option<PidTree>,
    pub(crate) file_size_cache: LruCache<PathBuf, u64>,
    pub(crate) buffer_size: usize,
    pub(crate) socket_listener: Option<tokio::net::UnixListener>,
    /// One `FsGroup` per unique filesystem (fan_fd + mount_fd dedup'd)
    pub(crate) fs_groups: Vec<FsGroup>,
    /// Maps monitored path → index in fs_groups for fast lookup in remove_path
    pub(crate) path_to_group: HashMap<PathBuf, usize>,
    pub(crate) dir_cache: Cache<fanotify_fid::types::HandleKey, PathBuf>,
    /// Shared state for spawning reader tasks during live-add (set in run())
    pub(crate) event_tx: Option<EventSender>,
    pub(crate) shared_dir_cache: Option<Cache<fanotify_fid::types::HandleKey, PathBuf>>,
    /// Paths that didn't exist at add/startup time, retried on directory creation
    pub(crate) pending_paths: Vec<(PathBuf, PathEntry)>,
    /// inotify instance watching parent dirs of pending paths
    pub(crate) inotify: Option<inotify::Inotify>,
    /// Watch descriptors kept alive so watches stay active
    /// (watched_path, watch_descriptor) — maps wd back to the directory we're watching.
    pub(crate) _inotify_watches: Vec<(PathBuf, inotify::WatchDescriptor)>,
    /// PID of the fsmon daemon itself — events from this PID (or its children)
    /// are discarded to prevent self-triggering feedback loops.
    pub(crate) daemon_pid: u32,
    /// Resolved cache configuration (capacity, TTL, buffer size).
    pub(crate) cache_config: ResolvedCacheConfig,
    /// Enable debug output
    pub(crate) debug: bool,
    /// Death notifications from reader tasks: each sends its group_idx on exit.
    pub(crate) reader_death_rx: tokio::sync::mpsc::UnboundedReceiver<usize>,
    /// Cloneable sender for reader tasks to signal death.
    pub(crate) reader_death_tx: tokio::sync::mpsc::UnboundedSender<usize>,
    /// Per-group restart tracking (index-aligned with fs_groups).
    pub(crate) reader_states: Vec<Option<ReaderState>>,
    /// Daemon start time, set in run() for uptime calculation.
    pub(crate) started_at: std::time::Instant,
    /// Raw disk-min-free threshold string (e.g. "10%", "5GB"). None = no check.
    disk_min_free: Option<String>,
    /// Log file sync interval. None = disabled.
    sync_interval: Option<std::time::Duration>,
    /// Metrics report interval. None = disabled.
    metrics_interval: Option<std::time::Duration>,

    /// Unified event stream: broadcast channel for all consumers.
    /// Carries (FileEvent, cmd_name) — cmd_name is for file routing.
    /// Subscribe tasks extract FileEvent, file writer uses both.
    pub(crate) event_stream_tx: Option<tokio::sync::broadcast::Sender<(FileEvent, String)>>,
    /// Use local time instead of UTC in timestamp serialization.
    pub(crate) local_time: bool,
    /// Atomic metrics counters (thread-safe, cloneable).
    pub(crate) metrics: MetricsRegistry,
    /// Temporary fanotify marks on parent directories of deleted-and-pending
    /// paths, so that events during the recreate window aren't lost.
    /// Maps: target_pending_path → (parent_path, group_idx in fs_groups)
    pub(crate) temp_parent_marks: HashMap<PathBuf, (PathBuf, usize)>,
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
        local_time: bool,
        metrics_interval: Option<u64>,
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
            // Reject cmd=fsmon
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

        let (reader_death_tx, reader_death_rx) = tokio::sync::mpsc::unbounded_channel::<usize>();

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
            _inotify_watches: Vec::new(), // (path, wd)
            daemon_pid: std::process::id(),
            reader_death_rx,
            reader_death_tx,
            reader_states: Vec::new(),
            started_at: std::time::Instant::now(),
            disk_min_free,
            sync_interval,
            metrics_interval: metrics_interval
                .filter(|&n| n > 0)
                .map(std::time::Duration::from_secs),
            event_stream_tx: {
                let cap = subscribe_buf.unwrap_or(4096).max(1);
                let (tx, _) = tokio::sync::broadcast::channel::<(FileEvent, String)>(cap);
                Some(tx)
            },
            local_time,
            metrics: MetricsRegistry::new(metrics_interval.is_some()),
            temp_parent_marks: HashMap::new(),
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
                let pending_opts: Vec<PathOptions> = self
                    .monitored_entries
                    .iter()
                    .filter(|(p, _)| p == &path)
                    .map(|(_, o)| o.clone())
                    .collect();
                self.monitored_entries.retain(|(p, _)| p != &path);
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

        // Initialize per-filesystem fanotify fds.
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
                // Same filesystem — just add inode mark
                let group = &self.fs_groups[gi];
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
                    let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
                    if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                        mark_recursive(fan_fd, path_mask, canonical);
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

            let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
            let recursive = opts.is_some_and(|o| o.recursive) && canonical.is_dir();
            if self
                .add_mark_upward(&new_fd, path_mask, canonical, recursive)
                .is_none()
            {
                drop(new_fd);
                continue;
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
        if let Some(ref threshold_str) = self.disk_min_free
            && let Some(ref dir) = self.log_dir
        {
            Self::check_disk_space(dir, threshold_str);
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

        // Spawn one reader task per FsGroup
        let (event_tx, mut event_rx) = match self.cache_config.channel_capacity {
            Some(cap) if cap > 0 => {
                let (tx, rx) = tokio::sync::mpsc::channel(cap);
                (EventSender::Bounded(tx), EventReceiver::Bounded(rx))
            }
            _ => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                (EventSender::Unbounded(tx), EventReceiver::Unbounded(rx))
            }
        };
        let dir_cache = self.dir_cache.clone();

        // Shared state for live-add
        self.event_tx = Some(event_tx);
        self.shared_dir_cache = Some(dir_cache.clone());

        for gi in 0..self.fs_groups.len() {
            self.spawn_fd_reader(gi);
        }

        // Spawn file writer task
        let fw_log_dir = self.log_dir.clone();
        let fw_sync = self.sync_interval;
        let fw_debug = self.debug;
        let fw_local = self.local_time;
        let fw_metrics = self.metrics.clone();
        if let Some(fw_log_dir) = fw_log_dir
            && let Some(ref tx) = self.event_stream_tx
        {
            let fw_rx = tx.subscribe();
            let fw = FileLogWriter::new(fw_log_dir, fw_sync, fw_debug, fw_local, fw_metrics);
            tokio::spawn(async move {
                fw.run(fw_rx).await;
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

        // Build proc connector AsyncFd for tokio event loop
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

        // Move the reader death receiver out of self so tokio::select! can use it.
        let mut reader_death_rx = std::mem::replace(
            &mut self.reader_death_rx,
            tokio::sync::mpsc::unbounded_channel::<usize>().1,
        );

        // Notify systemd
        notify_sd_ready();

        let mut metrics_tick: Option<tokio::time::Interval> =
            self.metrics_interval.map(tokio::time::interval);

        loop {
            tokio::select! {
                Some(events) = event_rx.recv() => {
                    // 1. Drain proc events before processing (existing behavior)
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
                    // 2. Build events (deferred send)
                    let mut pending = self.process_event_batch(&events);
                    // 3. Second drain: catch Exec events that arrived between step 1 and step 2.
                    //    This closes the race window for short-lived processes (rm, touch, etc.)
                    //    whose Exec event may arrive after the fanotify event.
                    if let Some(afd) = proc_afd.as_ref() {
                        let conn = afd.get_ref();
                        loop {
                            match conn.recv_raw(&mut proc_buf) {
                                Ok(n) => {
                                    proc_cache::handle_proc_events(&proc_cache, &pid_tree, &proc_buf, n);
                                }
                                Err(proc_connector::Error::WouldBlock) => break,
                                Err(proc_connector::Error::Interrupted) => continue,
                                _ => break,
                            }
                        }
                    }
                    // 4. Patch any "unknown" fields using now-populated caches
                    self.patch_pending_events(&mut pending);
                    // 5. Send to broadcast
                    self.send_pending_events(&pending);
                    // 6. Retry pending paths (inotify handles the primary
                    //    delete→pending transition via DELETE_SELF).
                    self.check_pending();
                }
                _ = tokio::signal::ctrl_c() => {
                    while let Ok(events) = event_rx.try_recv() {
                        let mut pending = self.process_event_batch(&events);
                        self.patch_pending_events(&mut pending);
                        self.send_pending_events(&pending);
                    }
                    break;
                }
                _ = sigterm.recv() => {
                    while let Ok(events) = event_rx.try_recv() {
                        let mut pending = self.process_event_batch(&events);
                        self.patch_pending_events(&mut pending);
                        self.send_pending_events(&pending);
                    }
                    break;
                }
                _ = sighup.recv() => {
                    if let Err(e) = self.reload_config() {
                        eprintln!("Config reload error: {e}");
                    }
                }

                _ = async {
                    match metrics_tick.as_mut() {
                        Some(t) => t.tick().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let report = self.collect_metrics(&dir_cache, &proc_cache, &pid_tree);
                    eprintln!(
                        "[metrics] uptime={}s rss={:.1}MB caches(d/p/t/f)={}/{}/{}/{} readers={}/{}/{} subs={} paths={} pending={} disk_buf={} events={}",
                        report.uptime_secs,
                        report.rss_mb,
                        report.dir_cache_entries,
                        report.proc_cache_entries,
                        report.pid_tree_entries,
                        report.file_size_cache_entries,
                        report.reader_groups_total,
                        report.reader_groups_alive,
                        report.reader_groups_gave_up,
                        report.subscribers,
                        report.monitored_paths,
                        report.pending_paths,
                        report.disk_buffer_events,
                        report.events_total.iter().map(|(k, v)| format!("{}:{}", k, v)).collect::<Vec<_>>().join(" "),
                    );
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
                    if self.debug {
                        eprintln!("[DEBUG] inotify fd became readable");
                    }
                    if let Ok(mut guard) = inotify_ready {
                        self.handle_inotify_events();
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
                            let cmd = match serde_json::from_str::<SocketCmd>(&cmd_str) {
                                Ok(c) => c,
                                Err(e) => {
                                    let resp: Result<crate::socket::SocketResponse, SocketError> = Err(SocketError::Transient(format!("Invalid command: {e}")));
                                    if let Ok(json_str) = serde_json::to_string(&resp) {
                                        let _ = tokio_io_oneshot(
                                            writer,
                                            &format!("{json_str}\n"),
                                        ).await;
                                    }
                                    continue;
                                }
                            };
                            if let SocketCmd::Subscribe { .. } = cmd {
                                self.handle_subscribe(writer, &cmd);
                            } else {
                                let result = self.handle_socket_cmd(cmd);
                                if let Ok(json_str) = serde_json::to_string(&result) {
                                    let resp_bytes = format!("{json_str}\n");
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
        drop(self.event_stream_tx.take());
        Ok(())
    }

    /// Collect runtime metrics for periodic reporting.
    pub(crate) fn collect_metrics(
        &self,
        dir_cache: &moka::sync::Cache<fanotify_fid::types::HandleKey, std::path::PathBuf>,
        proc_cache: &crate::proc_cache::ProcCache,
        pid_tree: &crate::proc_cache::PidTree,
    ) -> MetricsReport {
        let reader_groups_alive = self
            .reader_states
            .iter()
            .filter(|s| s.as_ref().is_some_and(|s| !s.gave_up))
            .count() as u64;
        let reader_groups_gave_up = self
            .reader_states
            .iter()
            .filter(|s| s.as_ref().is_some_and(|s| s.gave_up))
            .count() as u64;

        let events_total = self.metrics.events_total().gather()
            .into_iter()
            .map(|(labels, val)| {
                let key = labels.join(",");
                (key, val)
            })
            .collect();

        MetricsReport {
            uptime_secs: self.started_at.elapsed().as_secs(),
            rss_mb: get_rss_mb(),
            dir_cache_entries: dir_cache.entry_count(),
            proc_cache_entries: proc_cache.entry_count(),
            pid_tree_entries: pid_tree.entry_count(),
            file_size_cache_entries: self.file_size_cache.len() as u64,
            reader_groups_total: self.fs_groups.len() as u64,
            reader_groups_alive,
            reader_groups_gave_up,
            subscribers: self.metrics.subscribers() as u64,
            monitored_paths: self.metrics.monitored_paths() as u64,
            pending_paths: self.metrics.pending_paths() as u64,
            disk_buffer_events: self.metrics.disk_buffer_events() as u64,
            events_total,
        }
    }

    /// Publish pending events to the broadcast stream.
    fn send_pending_events(&self, pending: &[PendingEvent]) {
        if let Some(ref tx) = self.event_stream_tx {
            for pe in pending {
                let _ = tx.send((pe.event.clone(), pe.cmd_name.clone()));
            }
        }
    }
}

/// Snapshot of daemon runtime metrics for periodic reporting.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct MetricsReport {
    pub uptime_secs: u64,
    pub rss_mb: f64,
    pub dir_cache_entries: u64,
    pub proc_cache_entries: u64,
    pub pid_tree_entries: u64,
    pub file_size_cache_entries: u64,
    pub reader_groups_total: u64,
    pub reader_groups_alive: u64,
    pub reader_groups_gave_up: u64,
    pub subscribers: u64,
    pub monitored_paths: u64,
    pub pending_paths: u64,
    pub disk_buffer_events: u64,
    pub events_total: Vec<(String, u64)>,
}

/// Read current RSS in MB from /proc/self/statm.
fn get_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split_whitespace().collect();
            parts.get(1).and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| (pages * 4096) as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests;
