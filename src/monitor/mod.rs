use anyhow::{Context, Result, bail};
use std::collections::HashMap;
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
use crate::fid_parser::FsGroup;
use crate::filters::{self, PathOptions};
use crate::metrics::MetricsRegistry;
use crate::monitored::PathEntry;
use crate::proc_cache::{self, PidTree, ProcCache};
use crate::watchdog::Watchdog;
use serde_json;

// ---- Submodules ----

mod channel;
mod events;
mod file_writer;
mod filtering;
mod init;
mod live_path;
mod reader;
mod socket_handler;

pub(crate) use channel::{EventReceiver, EventSender};
pub(crate) use events::PendingEvent;
pub(crate) use file_writer::FileLogWriter;
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
    /// Watchdog manager for systemd integration.
    pub(crate) watchdog: Option<Watchdog>,
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
        watchdog_interval: Option<u64>,
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
            watchdog: Some(Watchdog::new(watchdog_interval)),
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
        self.check_root()?;

        // Initialize process cache and pid tree
        let proc_conn = self.init_process_cache();

        // Initialize fanotify: masks, fs_groups, pending paths, inotify
        let fan_group_count = self.init_fanotify()?;

        // Initialize logging: log dir, chown, disk check
        self.init_logging()?;

        // Print startup status and metrics
        self.print_startup_status(fan_group_count);

        // Spawn reader tasks and file writer
        let (mut event_rx, dir_cache) = self.spawn_tasks();

        // --- Signal handlers ---
        let mut sigterm =
            signal(SignalKind::terminate()).context("failed to create SIGTERM signal handler")?;
        let mut sighup =
            signal(SignalKind::hangup()).context("failed to create SIGHUP signal handler")?;

        let socket_listener = self.socket_listener.take();

        // Build inotify AsyncFd for tokio event loop
        let inotify_async = self.inotify.as_ref().map(|ino| {
            let fd = ino.as_raw_fd();
            AsyncFd::new(crate::fid_parser::FanFd(fd)).expect("inotify AsyncFd")
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

        // Clone caches for event loop use
        let proc_cache = self.proc_cache.clone().unwrap();
        let pid_tree = self.pid_tree.clone().unwrap();

        // Move the reader death receiver out of self so tokio::select! can use it.
        let mut reader_death_rx = std::mem::replace(
            &mut self.reader_death_rx,
            tokio::sync::mpsc::unbounded_channel::<usize>().1,
        );

        // Notify systemd: READY=1
        if let Err(e) = crate::watchdog::sd_notify(libsystemd::daemon::NotifyState::Ready) {
            eprintln!("[WARNING] systemd notify READY failed: {}", e);
        }

        // Start watchdog if enabled
        let _watchdog_handle = self.watchdog.as_ref().map(|wd| {
            if self.debug {
                eprintln!(
                    "[DEBUG] systemd watchdog enabled (interval: {}s)",
                    wd.interval().as_secs()
                );
            }
            wd.clone().start()
        });

        let mut metrics_tick: Option<tokio::time::Interval> =
            self.metrics_interval.map(tokio::time::interval);

        // --- Main event loop ---
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
                    // 6. Retry pending paths
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
                        "[metrics] uptime={}s rss={:.1}MB caches(d/p/t/f)={}/{}/{}/{} readers={}/{}/{} subs={} paths={} pending={} disk_buf={}",
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
                            let cmd = match serde_json::from_str::<crate::socket::SocketCmd>(&cmd_str) {
                                Ok(c) => c,
                                Err(e) => {
                                    let resp: Result<crate::socket::SocketResponse, crate::socket::SocketError> = Err(crate::socket::SocketError::Transient(format!("Invalid command: {e}")));
                                    if let Ok(json_str) = serde_json::to_string(&resp) {
                                        let _ = tokio_io_oneshot(
                                            writer,
                                            &format!("{json_str}\n"),
                                        ).await;
                                    }
                                    continue;
                                }
                            };
                            if let crate::socket::SocketCmd::Subscribe { .. } = cmd {
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
mod tests {
    use super::*;
    use crate::fid_parser::mask_to_event_types;
    use crate::filters::PathOptions;
    use crate::monitored::PathEntry;
    use crate::utils::{SizeFilter, SizeOp};
    use crate::{EventType, FileEvent};
    use fanotify_fid::consts::{
        FAN_CREATE, FAN_DELETE, FAN_EVENT_ON_CHILD, FAN_MARK_ADD, FAN_MARK_FILESYSTEM, FAN_MODIFY,
        FAN_ONDIR,
    };
    use fanotify_fid::prelude::*;
    use fanotify_fid::{fanotify_init, fanotify_mark};
    use std::path::Path;
    use std::path::PathBuf;
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
            false,
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
            false,
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
            false,
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
            false,
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
            false,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_add_path_and_remove_path() {
        let mut m = Monitor::new(
            vec![],
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        )
        .unwrap();

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

    // ---- DELETE_SELF canonical root ordering test ----

    #[test]
    fn test_delete_self_canonical_root_is_recorded() {
        use fanotify_fid::types::FidEvent;

        let mut m = Monitor::new(
            vec![(
                std::path::PathBuf::from("/tmp/fsmon_test_delete_self"),
                PathOptions {
                    size_filter: None,
                    event_types: None,
                    recursive: true,
                    cmd: None,
                },
            )],
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        )
        .unwrap();

        // simulate what run() does: canonicalize the path
        m.canonical_paths = vec![std::path::PathBuf::from("/tmp/fsmon_test_delete_self")];

        // Synthetic DELETE_SELF FidEvent matching the canonical root
        let event = FidEvent {
            mask: fanotify_fid::consts::FAN_DELETE_SELF,
            pid: 1234,
            path: std::path::PathBuf::from("/tmp/fsmon_test_delete_self"),
            dfid_name_handle: None,
            dfid_name_filename: None,
            self_handle: None,
        };

        let pending = m.process_event_batch(&[event]);

        // DELETE_SELF event should be recorded (not silently dropped)
        assert!(
            pending
                .iter()
                .any(|pe| pe.event.event_type == crate::EventType::DeleteSelf),
            "DELETE_SELF for canonical root should be recorded"
        );

        // Path should be moved to pending_paths after cleanup
        assert!(
            m.pending_paths
                .iter()
                .any(|(p, _)| p == &std::path::PathBuf::from("/tmp/fsmon_test_delete_self")),
            "canonical root should move to pending_paths after DELETE_SELF"
        );

        // Path should be removed from active monitoring
        assert!(
            !m.monitored_entries
                .iter()
                .any(|(p, _)| p == &std::path::PathBuf::from("/tmp/fsmon_test_delete_self")),
            "canonical root should be removed from monitored_entries"
        );
    }

    fn make_event(path: &str, event_type: EventType, pid: u32, size: u64) -> FileEvent {
        FileEvent {
            time: chrono::Utc::now(),
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
        let mask = FAN_CREATE | FAN_DELETE;
        let bad_path = Path::new("/tmp/ok\0evil");
        let dev_null = std::fs::File::open("/dev/null").expect("/dev/null must exist on Linux");
        let dummy_fd: std::os::fd::OwnedFd = dev_null.into();
        let result = fanotify_mark(&dummy_fd, FAN_MARK_ADD, mask, AT_FDCWD, bad_path);

        match result {
            Err(FanotifyError::Mark(code)) => {
                assert_eq!(
                    code,
                    libc::EINVAL,
                    "null byte path should return EINVAL, got errno={}",
                    code
                );
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
        assert!(!chains_contain("bash → myapp-backup → fsmon", "myapp"));
    }

    #[tokio::test]
    async fn test_subscriber_task_receives_events() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let mut rx2 = tx.subscribe();
        let event = FileEvent {
            time: chrono::Utc::now(),
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
        assert!(chains_contain("bash → myapp", "myapp"));
        assert!(!chains_contain("bash → myapp", "other-app"));
    }

    #[tokio::test]
    async fn test_subscriber_task_filters_by_type() {
        let allowed = [EventType::Delete, EventType::CloseWrite];

        let create_event = FileEvent {
            time: chrono::Utc::now(),
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
            time: chrono::Utc::now(),
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
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);

        for i in 0..10 {
            let _ = tx.send(FileEvent {
                time: chrono::Utc::now(),
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

        let result = rx.recv().await;
        match result {
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0, "should lag with >0 dropped events, got {}", n);
            }
            Ok(event) => {
                assert!(
                    event.file_size >= 6,
                    "should be a recent event, got file_size={}",
                    event.file_size
                );
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }
}
