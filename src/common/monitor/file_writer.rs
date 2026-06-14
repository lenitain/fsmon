use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::common::FileEvent;
use crate::common::metrics::MetricsRegistry;

/// Maximum number of open file handles to keep cached.
const MAX_OPEN_HANDLES: usize = 64;
/// Flush + sync interval: flush BufWriter to OS buffer and fdatasync to disk every second.
/// Ensures data is visible and persisted during runtime, not just on shutdown.
const FLUSH_SYNC_INTERVAL: Duration = Duration::from_secs(1);

// ---- FileLogWriter: unified event stream consumer for disk persistence ----

/// Async file writer consuming events from the broadcast stream and writing to JSONL files.
/// Runs as a tokio task. Handles disk-full buffering, fdatasync, and ENOENT retry.
pub(crate) struct FileLogWriter {
    log_dir: PathBuf,
    disk_buf: VecDeque<(FileEvent, String)>,
    disk_healthy: bool,
    last_disk_check: std::time::Instant,
    dirty_logs: HashSet<PathBuf>,
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

        // Single timer: flush BufWriter → OS buffer + fdatasync → disk every second.
        let mut flush_sync_timer = interval(FLUSH_SYNC_INTERVAL);

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
                // Flush + sync: BufWriter → OS buffer → disk
                _ = flush_sync_timer.tick() => {
                    self.flush_and_sync_dirty_logs();
                }
            }
        }

        self.flush_and_sync_dirty_logs();
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
        Ok(self
            .handles
            .get_mut(log_path)
            .expect("handle should exist after insert or contains_key check"))
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

        // If the file was deleted externally (e.g. by user or log rotation),
        // the cached BufWriter holds a stale fd pointing to a deleted inode.
        // Writes would silently succeed but data would be lost.  Detect this
        // by checking whether the file still exists on disk; if not, drop the
        // stale handle so get_or_open creates a fresh file.
        if self.handles.contains_key(&log_path) && !log_path.exists() {
            self.handles.remove(&log_path);
        }

        match self.get_or_open(&log_path) {
            Ok(writer) => {
                writeln!(writer, "{}", line)?;
                self.dirty_logs.insert(log_path);
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

    /// Flush + sync all dirty log files.
    /// Flush moves data from BufWriter (user-space) to OS buffer,
    /// then fdatasync persists OS buffer to disk.
    fn flush_and_sync_dirty_logs(&mut self) {
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
/// Chowns the file to the original user immediately after creation using fchown
/// (fd-based) to avoid TOCTOU race: the fd is always valid even if the path is
/// deleted between open() and chown().
fn open_log_file(log_path: &PathBuf, log_dir: &PathBuf) -> std::io::Result<File> {
    let file = OpenOptions::new()
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
        })?;
    // Chown via fchown (fd-based) — no TOCTOU race.
    // The path-based chown_to_user would fail with ENOENT if the file is
    // deleted between open() and chown(); fchown operates on the fd directly
    // and always succeeds as long as the fd is open.
    fchown_to_user(&file);
    Ok(file)
}

/// fchown a file descriptor to the original user (SUDO_UID/SUDO_GID).
/// Best-effort: errors are logged but not propagated — the file is still usable.
fn fchown_to_user(file: &File) {
    let (uid, gid) = crate::common::config::resolve_uid_gid();
    use std::os::fd::AsRawFd;
    let ret = unsafe { libc::fchown(file.as_raw_fd(), uid, gid) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        // EPERM / EOPNOTSUPP / ENOSYS are expected on vfat/exfat/NFS — skip warning.
        let errno = err.raw_os_error().unwrap_or(0);
        if errno != libc::EPERM && errno != libc::EOPNOTSUPP && errno != libc::ENOSYS {
            eprintln!(
                "[WARNING] fchown failed for log file (fd {}): {}",
                file.as_raw_fd(),
                err
            );
        }
    }
}
