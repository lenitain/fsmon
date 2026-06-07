use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::common::FileEvent;
use crate::common::metrics::MetricsRegistry;

/// Maximum number of open file handles to keep cached.
const MAX_OPEN_HANDLES: usize = 64;

// ---- FileLogWriter: unified event stream consumer for disk persistence ----

/// Async file writer consuming events from the broadcast stream and writing to JSONL files.
/// Runs as a tokio task. Handles disk-full buffering, fdatasync, and ENOENT retry.
pub(crate) struct FileLogWriter {
    log_dir: PathBuf,
    disk_buf: VecDeque<(FileEvent, String)>,
    disk_healthy: bool,
    last_disk_check: std::time::Instant,
    dirty_logs: HashSet<PathBuf>,
    sync_interval: Option<Duration>,
    debug: bool,
    local_time: bool,
    metrics: MetricsRegistry,
    /// Cached open file handles, keyed by log path.
    /// Avoids open+close per event for high-frequency writes.
    handles: HashMap<PathBuf, BufWriter<File>>,
}

impl FileLogWriter {
    pub(crate) fn new(
        log_dir: PathBuf,
        sync_interval: Option<Duration>,
        debug: bool,
        local_time: bool,
        metrics: MetricsRegistry,
    ) -> Self {
        Self {
            log_dir,
            disk_buf: VecDeque::with_capacity(10_000),
            disk_healthy: true,
            last_disk_check: std::time::Instant::now(),
            dirty_logs: HashSet::new(),
            sync_interval,
            metrics,
            debug,
            local_time,
            handles: HashMap::new(),
        }
    }

    /// Select UTC or local time serialization based on config.
    fn jsonl_string(&self, event: &FileEvent) -> String {
        if self.local_time {
            event.to_jsonl_string_local()
        } else {
            event.to_jsonl_string()
        }
    }

    /// Run the file writer event loop.
    pub(crate) async fn run(
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
                            if let Err(e) = self.write_event(&event, &cmd_name)
                                && self.debug {
                                    eprintln!("[DEBUG] FileLogWriter write error: {}", e);
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
        self.flush_all();
    }

    /// Get or open a BufWriter for the given log path.
    /// Returns None if the open fails (caller should buffer the event).
    fn get_or_open(&mut self, log_path: &PathBuf) -> std::io::Result<&mut BufWriter<File>> {
        if !self.handles.contains_key(log_path) {
            // Evict oldest handle if at capacity
            if self.handles.len() >= MAX_OPEN_HANDLES
                && let Some(evict_path) = self.handles.keys().next().cloned()
            {
                self.handles.remove(&evict_path);
            }
            let file = open_log_file(log_path, &self.log_dir)?;
            self.handles.insert(log_path.clone(), BufWriter::new(file));
        }
        Ok(self.handles.get_mut(log_path).unwrap())
    }

    /// Write an event to the appropriate JSONL log file.
    /// Returns Ok(()) even if disk is full (event is buffered for retry).
    fn write_event(&mut self, event: &FileEvent, cmd_name: &str) -> std::io::Result<()> {
        let log_path = self
            .log_dir
            .join(crate::common::utils::cmd_to_log_name(cmd_name));

        // Try to flush buffer if disk was previously unhealthy
        if !self.disk_healthy
            && self.last_disk_check.elapsed() >= std::time::Duration::from_secs(10)
        {
            self.flush_disk_buf();
        }

        // Serialize before borrowing self.handles
        let line = self.jsonl_string(event);
        let is_new = !log_path.exists();

        match self.get_or_open(&log_path) {
            Ok(writer) => {
                if is_new && let Err(e) = crate::common::fid_parser::chown_to_user(&log_path) {
                    eprintln!(
                        "[WARNING] Could not chown log file '{}': {}",
                        log_path.display(),
                        e
                    );
                }
                writeln!(writer, "{}", line)?;
                if self.sync_interval.is_some() {
                    self.dirty_logs.insert(log_path);
                }
                self.disk_healthy = true;
                Ok(())
            }
            Err(e) => {
                // Disk might be full — buffer the event
                self.handles.remove(&log_path);
                self.disk_healthy = false;
                self.last_disk_check = std::time::Instant::now();
                if self.disk_buf.len() < 10_000 {
                    self.disk_buf
                        .push_back((event.clone(), cmd_name.to_string()));
                }
                self.metrics
                    .set_disk_buffer_events(self.disk_buf.len() as i64);
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
            let log_path = self
                .log_dir
                .join(crate::common::utils::cmd_to_log_name(&cmd_name));
            let line = self.jsonl_string(&event);
            match self.get_or_open(&log_path) {
                Ok(writer) => {
                    if writeln!(writer, "{}", line).is_err() {
                        self.handles.remove(&log_path);
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
        self.metrics
            .set_disk_buffer_events(self.disk_buf.len() as i64);
    }

    /// Flush all open BufWriters to OS buffers.
    /// Called on shutdown to ensure no data is lost in user-space buffers.
    fn flush_all(&mut self) {
        for (path, writer) in &mut self.handles {
            if let Err(e) = writer.flush() {
                eprintln!("[WARNING] flush failed for '{}': {}", path.display(), e);
            }
        }
    }

    /// Sync all dirty log files to disk via flush + fdatasync.
    /// Flush moves data from BufWriter (user-space) to OS buffer,
    /// then fdatasync persists OS buffer to disk.
    fn sync_dirty_logs(&mut self) {
        if self.dirty_logs.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.dirty_logs.drain().collect();
        for path in &paths {
            // Try cached handle first
            if let Some(writer) = self.handles.get_mut(path) {
                // Flush BufWriter's user-space buffer to OS
                if let Err(e) = writer.flush() {
                    eprintln!("[WARNING] flush failed for '{}': {}", path.display(), e);
                    continue;
                }
                // Then sync OS buffer to disk
                if let Err(e) = writer.get_ref().sync_data() {
                    eprintln!("[WARNING] fdatasync failed for '{}': {}", path.display(), e);
                }
            } else {
                // Fallback: open for sync (handle was evicted)
                match File::options().write(true).open(path) {
                    Ok(file) => {
                        if let Err(e) = file.sync_data() {
                            eprintln!("[WARNING] fdatasync failed for '{}': {}", path.display(), e);
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
}

/// Open a log file with create+append, retrying once on ENOENT.
fn open_log_file(log_path: &PathBuf, log_dir: &PathBuf) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                let _ = fs::create_dir_all(log_dir);
                let _ = crate::common::fid_parser::chown_to_user(log_dir);
                OpenOptions::new().create(true).append(true).open(log_path)
            } else {
                Err(e)
            }
        })
}
