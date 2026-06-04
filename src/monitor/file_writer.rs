use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::time::Duration;

use crate::FileEvent;
use crate::metrics::MetricsRegistry;

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
                    OpenOptions::new().create(true).append(true).open(&log_path)
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
                writeln!(file, "{}", self.jsonl_string(event))?;
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
            let log_path = self.log_dir.join(crate::utils::cmd_to_log_name(&cmd_name));
            let open_result = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        let _ = fs::create_dir_all(&self.log_dir);
                        let _ = crate::fid_parser::chown_to_user(&self.log_dir);
                        OpenOptions::new().create(true).append(true).open(&log_path)
                    } else {
                        Err(e)
                    }
                });
            match open_result {
                Ok(file) => {
                    let mut file = std::io::BufWriter::new(file);
                    use std::io::Write;
                    if writeln!(file, "{}", self.jsonl_string(&event)).is_err() {
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
