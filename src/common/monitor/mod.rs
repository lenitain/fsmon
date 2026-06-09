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

use crate::common::FileEvent;
use crate::common::config::ResolvedCacheConfig;
use crate::common::fid_parser::FsGroup;
use crate::common::filters::{self, PathOptions};
use crate::common::metrics::MetricsRegistry;
use crate::common::monitored::PathEntry;
use crate::common::proc_cache::{self, DefaultStore as ProcessStore};
use crate::common::watchdog::Watchdog;
use serde_json;
use slotmap::SlotMap;

/// Key type for FsGroup SlotMap lookups.
pub(crate) type FsGroupKey = slotmap::DefaultKey;

// ---- Debug logging macro ----
// Avoids format!() allocation when debug is disabled.
macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug { eprintln!("[DEBUG] {}", format!($($arg)*)); }
    };
}

// ---- Submodules ----

mod channel;
mod dir_watcher;
mod events;
mod file_writer;
mod filtering;
mod init;
mod live_path;
mod reader;
mod socket_handler;
mod temp_marks;

pub(crate) use channel::{EventReceiver, EventSender};
pub(crate) use events::PendingEvent;
pub(crate) use file_writer::FileLogWriter;
pub(crate) use reader::ReaderState;
#[cfg(test)]
pub(crate) use socket_handler::chains_contain;
pub(crate) use socket_handler::tokio_io_oneshot;

// ---- MonitorConfig ----

/// Configuration for creating a Monitor instance.
pub struct MonitorConfig {
    pub paths_and_options: Vec<(PathBuf, PathOptions)>,
    pub log_dir: Option<PathBuf>,
    pub monitored_path: Option<PathBuf>,
    pub buffer_size: Option<usize>,
    pub socket_listener: Option<tokio::net::UnixListener>,
    pub debug: bool,
    pub cache_config: Option<ResolvedCacheConfig>,
    pub disk_min_free: Option<String>,
    pub subscribe_buf: Option<usize>,
    pub local_time: bool,
    pub metrics_interval: Option<u64>,
    pub watchdog_interval: Option<u64>,
}

impl MonitorConfig {
    /// Create a default config for tests (all None/false).
    #[cfg(test)]
    pub fn default_for_test() -> Self {
        Self {
            paths_and_options: Vec::new(),
            log_dir: None,
            monitored_path: None,
            buffer_size: None,
            socket_listener: None,
            debug: false,
            cache_config: None,
            disk_min_free: None,
            subscribe_buf: None,
            local_time: false,
            metrics_interval: None,
            watchdog_interval: None,
        }
    }
}

// ---- Sub-structures for Monitor ----

/// Fanotify state: per-filesystem groups and directory handle cache.
pub(crate) struct FanotifyState {
    /// One `FsGroup` per unique filesystem (fan_fd + mount_fd dedup'd).
    /// Uses SlotMap for stable keys — removal doesn't invalidate other keys.
    pub groups: SlotMap<FsGroupKey, FsGroup>,
    /// Maps monitored path → key in groups for fast lookup in remove_path.
    pub path_to_group: HashMap<PathBuf, FsGroupKey>,
    /// Directory handle → path cache (shared with reader tasks).
    pub dir_cache: Cache<fanotify_fid::types::HandleKey, PathBuf>,
    /// Clone of dir_cache for spawning reader tasks during live-add.
    pub shared_dir_cache: Option<Cache<fanotify_fid::types::HandleKey, PathBuf>>,
}

/// Inotify state: watches for pending paths and new subdirectory detection.
pub(crate) struct InotifyState {
    /// inotify instance watching parent dirs of pending paths.
    pub inotify: Option<inotify::Inotify>,
    /// Watch descriptors kept alive so watches stay active.
    pub watches: Vec<(PathBuf, inotify::WatchDescriptor)>,
    /// Paths that didn't exist at add/startup time, retried on directory creation.
    pub pending_paths: Vec<(PathBuf, PathEntry)>,
    /// Temporary fanotify marks on parent directories of deleted-and-pending paths.
    pub temp_parent_marks: HashMap<PathBuf, (PathBuf, FsGroupKey)>,
}

/// Process tree state: proc connector cache and PID tree.
pub(crate) struct ProcessState {
    /// Process store (unified tree and cache).
    pub store: Option<ProcessStore>,
}

// ---- Monitor ----

pub struct Monitor {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) canonical_paths: Vec<PathBuf>,
    /// Full list of (path, PathOptions) preserving duplicates across cmd groups.
    /// This is the single source of truth for path options.
    pub(crate) monitored_entries: Vec<(PathBuf, PathOptions)>,
    pub(crate) log_dir: Option<PathBuf>,
    pub(crate) monitored_path: Option<PathBuf>,
    pub(crate) fanotify: FanotifyState,
    pub(crate) inotify_state: InotifyState,
    pub(crate) proc: ProcessState,
    pub(crate) file_size_cache: LruCache<PathBuf, u64>,
    pub(crate) buffer_size: usize,
    pub(crate) socket_listener: Option<tokio::net::UnixListener>,
    /// Shared state for spawning reader tasks during live-add (set in run())
    pub(crate) event_tx: Option<EventSender>,
    /// PID of the fsmon daemon itself — events from this PID (or its children)
    /// are discarded to prevent self-triggering feedback loops.
    pub(crate) daemon_pid: u32,
    /// Resolved cache configuration (capacity, TTL, buffer size).
    pub(crate) cache_config: ResolvedCacheConfig,
    /// Enable debug output
    pub(crate) debug: bool,
    /// Death notifications from reader tasks: each sends its group key on exit.
    pub(crate) reader_death_rx: tokio::sync::mpsc::UnboundedReceiver<FsGroupKey>,
    /// Cloneable sender for reader tasks to signal death.
    pub(crate) reader_death_tx: tokio::sync::mpsc::UnboundedSender<FsGroupKey>,
    /// Per-group restart tracking (keyed by FsGroupKey).
    pub(crate) reader_states: HashMap<FsGroupKey, ReaderState>,
    /// Daemon start time, set in run() for uptime calculation.
    pub(crate) started_at: std::time::Instant,
    /// Raw disk-min-free threshold string (e.g. "10%", "5GB"). None = no check.
    disk_min_free: Option<String>,
    /// Metrics report interval. None = disabled.
    metrics_interval: Option<std::time::Duration>,
    /// Unified event stream: broadcast channel for all consumers.
    pub(crate) event_stream_tx: Option<tokio::sync::broadcast::Sender<(FileEvent, String)>>,
    /// Use local time instead of UTC in timestamp serialization.
    pub(crate) local_time: bool,
    /// Atomic metrics counters (thread-safe, cloneable).
    pub(crate) metrics: MetricsRegistry,
    /// Watchdog manager for systemd integration.
    pub(crate) watchdog: Option<Watchdog>,
}

impl Monitor {
    pub fn new(cfg: MonitorConfig) -> Result<Self> {
        let cache_config = cfg.cache_config.unwrap_or_default();
        let buffer_size = cfg.buffer_size.unwrap_or(cache_config.buffer_size);

        if buffer_size < 4096 {
            bail!("buffer_size must be at least 4096 bytes (4KB)");
        }
        if buffer_size > 1024 * 1024 {
            bail!("buffer_size must not exceed 1048576 bytes (1MB)");
        }

        let mut paths = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut monitored_entries = Vec::new();
        let log_dir_canonical = cfg
            .log_dir
            .as_ref()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()));
        for (path, opts) in &cfg.paths_and_options {
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

        let (reader_death_tx, reader_death_rx) =
            tokio::sync::mpsc::unbounded_channel::<FsGroupKey>();

        let debug = cfg.debug;
        let paths_and_options_len = cfg.paths_and_options.len();
        let log_dir = cfg.log_dir;
        let metrics_interval_dur = cfg
            .metrics_interval
            .filter(|&n| n > 0)
            .map(std::time::Duration::from_secs);
        let subscribe_buf = cfg.subscribe_buf;

        let monitor = Self {
            paths,
            canonical_paths: Vec::new(),
            monitored_entries,
            log_dir,
            monitored_path: cfg.monitored_path,
            fanotify: FanotifyState {
                groups: SlotMap::new(),
                path_to_group: HashMap::new(),
                dir_cache: Cache::builder()
                    .max_capacity(cache_config.dir_capacity)
                    .time_to_live(Duration::from_secs(cache_config.dir_ttl_secs))
                    .build(),
                shared_dir_cache: None,
            },
            inotify_state: InotifyState {
                inotify: None,
                watches: Vec::new(),
                pending_paths: Vec::new(),
                temp_parent_marks: HashMap::new(),
            },
            proc: ProcessState {
                store: None,
            },
            file_size_cache: LruCache::new(
                NonZeroUsize::new(cache_config.file_size_capacity).unwrap(),
            ),
            buffer_size,
            cache_config,
            socket_listener: cfg.socket_listener,
            debug,
            event_tx: None,
            daemon_pid: std::process::id(),
            reader_death_rx,
            reader_death_tx,
            reader_states: HashMap::new(),
            started_at: std::time::Instant::now(),
            disk_min_free: cfg.disk_min_free,
            metrics_interval: metrics_interval_dur,
            event_stream_tx: {
                let cap = subscribe_buf.unwrap_or(4096).max(1);
                let (tx, _) = tokio::sync::broadcast::channel::<(FileEvent, String)>(cap);
                Some(tx)
            },
            local_time: cfg.local_time,
            metrics: MetricsRegistry::new(cfg.metrics_interval.is_some()),
            watchdog: Some(Watchdog::new(cfg.watchdog_interval)),
        };
        if debug {
            eprintln!(
                "[DEBUG] Monitor initialized with {} path entries:",
                paths_and_options_len
            );
            for (i, (p, o)) in cfg.paths_and_options.iter().enumerate() {
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
        let inotify_async = self.inotify_state.inotify.as_ref().map(|ino| {
            let fd = ino.as_raw_fd();
            AsyncFd::new(crate::common::fid_parser::FanFd(fd)).expect("inotify AsyncFd")
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

        // Clone store for event loop use
        let proc_store = self.proc.store.clone().unwrap();

        // Move the reader death receiver out of self so tokio::select! can use it.
        let mut reader_death_rx = std::mem::replace(
            &mut self.reader_death_rx,
            tokio::sync::mpsc::unbounded_channel::<FsGroupKey>().1,
        );

        // Notify systemd: READY=1
        if let Err(e) = crate::common::watchdog::sd_notify(libsystemd::daemon::NotifyState::Ready) {
            eprintln!("[WARNING] systemd notify READY failed: {}", e);
        }

        // Heartbeat tick: integrated into the main event loop (not a separate task).
        // If the main loop blocks, this tick won't fire → systemd times out → restart.
        let mut heartbeat_tick: Option<tokio::time::Interval> =
            self.watchdog.as_ref().and_then(|wd| {
                if !wd.is_enabled() {
                    return None;
                }
                if self.debug {
                    debug_log!(
                        self.debug,
                        "systemd watchdog enabled (interval: {}s, heartbeat in main loop)",
                        wd.interval().as_secs()
                    );
                }
                // First tick fires after one interval (not immediately).
                let start = tokio::time::Instant::now() + wd.interval();
                Some(tokio::time::interval_at(start, wd.interval()))
            });

        let mut metrics_tick: Option<tokio::time::Interval> =
            self.metrics_interval.map(tokio::time::interval);

        // --- Main event loop ---
        loop {
            tokio::select! {
                Some(events) = event_rx.recv() => {
                    let _guards = self.drain_proc_events(&proc_afd, &mut proc_buf, &proc_store);
                    let mut pending = self.process_event_batch(&events);
                    let _guards2 = self.drain_proc_events(&proc_afd, &mut proc_buf, &proc_store);
                    self.patch_pending_events(&mut pending);
                    self.send_pending_events(&pending);
                    // Guards automatically remove processes when dropped
                    drop(_guards);
                    drop(_guards2);
                    self.check_pending();
                }
                _ = tokio::signal::ctrl_c() => {
                    self.drain_remaining_events(&mut event_rx, &proc_afd, &mut proc_buf, &proc_store);
                    break;
                }
                _ = sigterm.recv() => {
                    self.drain_remaining_events(&mut event_rx, &proc_afd, &mut proc_buf, &proc_store);
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
                    let report = self.collect_metrics(&dir_cache, &proc_store);
                    eprintln!(
                        "[metrics] uptime={}s rss={:.1}MB caches(d/p/f)={}/{}/{} readers={}/{}/{} subs={} paths={} pending={} disk_buf={}",
                        report.uptime_secs,
                        report.rss_mb,
                        report.dir_cache_entries,
                        report.proc_store_entries,
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

                // Watchdog heartbeat: fires only when the main event loop is responsive.
                // If any synchronous operation blocks the loop, this tick won't fire
                // and systemd will restart the service after WatchdogSec timeout.
                _ = async {
                    match heartbeat_tick.as_mut() {
                        Some(t) => t.tick().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(ref wd) = self.watchdog
                        && let Err(e) = wd.send_heartbeat()
                    {
                        eprintln!("[ERROR] systemd watchdog notify failed: {}", e);
                    }
                }

                proc_readable = async {
                    match proc_afd.as_ref() {
                        Some(afd) => afd.readable().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok(mut guard) = proc_readable {
                        let _guards = self.drain_proc_conn(guard.get_inner(), &mut proc_buf, &proc_store);
                        // Guards automatically remove processes when dropped
                        drop(_guards);
                        guard.clear_ready();
                    }
                }
                inotify_ready = async {
                    match inotify_async.as_ref() {
                        Some(afd) => afd.readable().await,
                        None => std::future::pending().await,
                    }
                } => {
                    debug_log!(self.debug, "inotify fd became readable");
                    if let Ok(mut guard) = inotify_ready {
                        self.handle_inotify_events();
                        guard.clear_ready();
                    }
                }
                accept_result = async {
                    match socket_listener.as_ref() {
                        Some(listener) => Self::accept_socket_connection(listener).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_socket_accept(accept_result).await;
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
        proc_store: &ProcessStore,
    ) -> MetricsReport {
        let reader_groups_alive = self.reader_states.values().filter(|s| !s.gave_up).count() as u64;
        let reader_groups_gave_up =
            self.reader_states.values().filter(|s| s.gave_up).count() as u64;

        MetricsReport {
            uptime_secs: self.started_at.elapsed().as_secs(),
            rss_mb: get_rss_mb(),
            dir_cache_entries: dir_cache.entry_count(),
            proc_store_entries: proc_store.len() as u64,
            file_size_cache_entries: self.file_size_cache.len() as u64,
            reader_groups_total: self.fanotify.groups.len() as u64,
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

    /// Drain all pending proc connector events into the caches.
    fn drain_proc_conn(
        &self,
        conn: &proc_connector::ProcConnector,
        proc_buf: &mut [u8],
        proc_store: &ProcessStore,
    ) -> Vec<proc_tree::ExitedProcessGuard<ProcessStore>> {
        let mut guards = Vec::new();
        loop {
            match conn.recv_raw(proc_buf) {
                Ok(n) => {
                    guards.extend(proc_cache::handle_proc_events(proc_store, proc_buf, n));
                }
                Err(proc_connector::Error::WouldBlock) => break,
                Err(proc_connector::Error::Interrupted) => continue,
                Err(e) => {
                    eprintln!("proc connector error: {e}");
                    break;
                }
            }
        }
        guards
    }

    /// Convenience wrapper: drain from an optional AsyncFd.
    fn drain_proc_events(
        &self,
        proc_afd: &Option<AsyncFd<proc_connector::ProcConnector>>,
        proc_buf: &mut [u8],
        proc_store: &ProcessStore,
    ) -> Vec<proc_tree::ExitedProcessGuard<ProcessStore>> {
        if let Some(afd) = proc_afd.as_ref() {
            self.drain_proc_conn(afd.get_ref(), proc_buf, proc_store)
        } else {
            Vec::new()
        }
    }

    /// Drain remaining events from the channel and process them before shutdown.
    fn drain_remaining_events(
        &mut self,
        event_rx: &mut EventReceiver,
        proc_afd: &Option<AsyncFd<proc_connector::ProcConnector>>,
        proc_buf: &mut [u8],
        proc_store: &ProcessStore,
    ) {
        while let Ok(events) = event_rx.try_recv() {
            let _guards = self.drain_proc_events(proc_afd, proc_buf, proc_store);
            let mut pending = self.process_event_batch(&events);
            self.patch_pending_events(&mut pending);
            self.send_pending_events(&pending);
            // Guards automatically remove processes when dropped
            drop(_guards);
        }
    }

    /// Accept a socket connection and read the command message.
    async fn accept_socket_connection(
        listener: &tokio::net::UnixListener,
    ) -> Result<(tokio::net::unix::OwnedWriteHalf, String), std::io::Error> {
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
        Ok((writer, message.trim().to_string()))
    }

    /// Handle an accepted socket connection: parse command and dispatch.
    async fn handle_socket_accept(
        &mut self,
        result: Result<(tokio::net::unix::OwnedWriteHalf, String), std::io::Error>,
    ) {
        match result {
            Ok((writer, cmd_str)) => {
                let cmd = match serde_json::from_str::<crate::common::socket::SocketCmd>(&cmd_str) {
                    Ok(c) => c,
                    Err(e) => {
                        let resp: Result<
                            crate::common::socket::SocketResponse,
                            crate::common::socket::SocketError,
                        > = Err(crate::common::socket::SocketError::Transient(format!(
                            "Invalid command: {e}"
                        )));
                        if let Ok(json_str) = serde_json::to_string(&resp) {
                            let _ = tokio_io_oneshot(writer, &format!("{json_str}\n")).await;
                        }
                        return;
                    }
                };
                if let crate::common::socket::SocketCmd::Subscribe { .. } = cmd {
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
}

/// Snapshot of daemon runtime metrics for periodic reporting.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct MetricsReport {
    pub uptime_secs: u64,
    pub rss_mb: f64,
    pub dir_cache_entries: u64,
    pub proc_store_entries: u64,
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
#[path = "tests.rs"]
mod tests;
