use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify::low_level::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_CLOSE_NOWRITE,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_MARK_ADD,
    FAN_MARK_FILESYSTEM, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_NONBLOCK,
    FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_Q_OVERFLOW, FAN_REPORT_DIR_FID, FAN_REPORT_FID,
    FAN_REPORT_NAME, O_CLOEXEC, O_RDONLY, fanotify_init, fanotify_mark,
};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};

use lru::LruCache;
use tokio::io::unix::AsyncFd;

use crate::dir_cache;
use crate::fid_parser::{self, FAN_FS_ERROR, HandleKey};
use crate::output;
use crate::proc_cache::{self, ProcCache};
use crate::utils::get_process_info_by_pid;
use crate::{EventType, FileEvent, OutputFormat};

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

// ---- Monitor ----

const FILE_SIZE_CACHE_CAP: usize = 10_000;
const PROC_CONNECTOR_TIMEOUT_SECS: u64 = 2;

pub struct Monitor {
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<EventType>>,
    exclude_regex: Option<regex::Regex>,
    output: Option<PathBuf>,
    format: OutputFormat,
    recursive: bool,
    all_events: bool,
    proc_cache: Option<ProcCache>,
    file_size_cache: LruCache<PathBuf, u64>,
}

impl Monitor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        paths: Vec<PathBuf>,
        min_size: Option<i64>,
        event_types: Option<Vec<EventType>>,
        exclude: Option<String>,
        output: Option<PathBuf>,
        format: OutputFormat,
        recursive: bool,
        all_events: bool,
    ) -> Self {
        let exclude_regex = exclude.map(|p| {
            let escaped = regex::escape(&p);
            let pattern = escaped.replace("\\*", ".*");
            regex::Regex::new(&pattern).expect("invalid exclude pattern")
        });
        Self {
            paths,
            min_size,
            event_types,
            exclude_regex,
            output,
            format,
            recursive,
            all_events,
            proc_cache: None,
            file_size_cache: LruCache::new(NonZeroUsize::new(FILE_SIZE_CACHE_CAP).unwrap()),
        }
    }

    pub async fn run(mut self) -> Result<()> {
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
                bail!(
                    "proc connector subscription timed out after {}s",
                    PROC_CONNECTOR_TIMEOUT_SECS
                );
            }
            tokio::time::sleep(poll_interval).await;
            poll_interval = (poll_interval * 2).min(tokio::time::Duration::from_millis(50));
        }

        // Initialize fanotify, enable FID mode to support all directory entry events
        let fan_fd = fanotify_init(
            FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_CLASS_NOTIF
                | FAN_REPORT_FID
                | FAN_REPORT_DIR_FID
                | FAN_REPORT_NAME,
            (O_CLOEXEC | O_RDONLY) as u32,
        )
        .context("fanotify_init failed (requires Linux 5.9+ kernel)")?;

        // Event mask
        // Default: 8 core change events
        // --all-events: all 14 fanotify notification events
        let mask = if self.all_events {
            FAN_ACCESS
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
                | FAN_ONDIR
        } else {
            FAN_CLOSE_WRITE
                | FAN_ATTRIB
                | FAN_CREATE
                | FAN_DELETE
                | FAN_DELETE_SELF
                | FAN_MOVED_FROM
                | FAN_MOVED_TO
                | FAN_MOVE_SELF
                | FAN_EVENT_ON_CHILD
                | FAN_ONDIR
        };

        let mut mount_fds = Vec::new();
        let mut canonical_paths = Vec::new();

        // Collect canonical paths
        for path in &self.paths {
            let canonical = if path.exists() {
                path.canonicalize().unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };
            canonical_paths.push(canonical);
        }

        // Try filesystem mark first (covers entire filesystem, no race window)
        // Fall back to inode mark + dynamic marking on EXDEV (e.g., btrfs subvolumes)
        let use_fs_mark = {
            let mut ok = true;
            for canonical in &canonical_paths {
                match fanotify_mark(
                    fan_fd,
                    FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                    mask,
                    AT_FDCWD,
                    canonical,
                ) {
                    Ok(()) => {}
                    Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                        ok = false;
                        break;
                    }
                    Err(e) => {
                        bail!("fanotify_mark failed: {}: {}", canonical.display(), e);
                    }
                }
            }
            ok
        };

        if !use_fs_mark {
            // inode mark fallback: mark directories one by one
            for canonical in &canonical_paths {
                mark_directory(fan_fd, mask, canonical)?;
                if self.recursive && canonical.is_dir() {
                    mark_recursive(fan_fd, mask, canonical);
                }
            }
        }

        // Open directory fds for open_by_handle_at to resolve file handles
        for canonical in &canonical_paths {
            if let Ok(c_path) = CString::new(canonical.to_string_lossy().as_bytes()) {
                let mfd =
                    unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
                if mfd >= 0 {
                    mount_fds.push(mfd);
                }
            }
        }

        // Setup output file if specified
        let mut output_file = if let Some(ref path) = self.output {
            let parent = path.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(parent)?;
            Some(OpenOptions::new().create(true).append(true).open(path)?)
        } else {
            None
        };

        println!("Starting file trace monitor...");
        println!(
            "Monitoring paths: {}\n",
            canonical_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );

        // Persistent directory handle cache: handle_key → dir_path
        // Pre-cache directory handles at startup so DELETE/DELETE_SELF events
        // for pre-existing directories can recover paths via the cache.
        // In recursive mode, cache all subdirectories; otherwise only root dirs.
        let mut dir_cache: HashMap<HandleKey, PathBuf> = HashMap::new();
        for canonical in &canonical_paths {
            if canonical.is_dir() {
                if self.recursive {
                    dir_cache::cache_recursive(&mut dir_cache, canonical);
                } else {
                    dir_cache::cache_dir_handle(&mut dir_cache, canonical);
                }
            }
        }

        let mut buf = vec![0u8; 4096 * 8]; // 32KB, reused across loop iterations

        let async_fd =
            AsyncFd::new(FanFd(fan_fd)).context("failed to register fanotify fd with tokio")?;

        loop {
            tokio::select! {
                result = async_fd.readable() => {
                    let mut guard = result?;

                    let events = fid_parser::read_fid_events(fan_fd, &mount_fds, &mut dir_cache, &mut buf);

                    if events.is_empty() {
                        guard.clear_ready();
                        continue;
                    }

                    // inode mark mode: dynamically add marks and update handle cache
                    if !use_fs_mark && self.recursive {
                        for raw in &events {
                            let is_dir_create = raw.mask & FAN_CREATE != 0 && raw.mask & FAN_ONDIR != 0;
                            let is_dir_moved_to = raw.mask & FAN_MOVED_TO != 0 && raw.mask & FAN_ONDIR != 0;
                            if (is_dir_create || is_dir_moved_to) && raw.path.is_dir() {
                                let _ = mark_directory(fan_fd, mask, &raw.path);
                                mark_recursive(fan_fd, mask, &raw.path);
                                dir_cache::cache_recursive(&mut dir_cache, &raw.path);
                            }
                        }
                    }

                    for raw in &events {
                        if raw.mask & FAN_Q_OVERFLOW != 0 {
                            eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                            continue;
                        }

                        let event_types = fid_parser::mask_to_event_types(raw.mask);

                        for event_type in event_types {
                            let event = self.build_file_event(raw, event_type);

                            if use_fs_mark && !self.is_path_in_scope(&event.path, &canonical_paths) {
                                continue;
                            }

                            if self.should_output(&event) {
                                output::output_event(&event, self.format, &mut output_file)?;
                            }
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
            }
        }

        // Cleanup
        unsafe {
            libc::close(fan_fd);
        }
        for mfd in mount_fds {
            unsafe {
                libc::close(mfd);
            }
        }

        println!("\nStopping file trace monitor...");
        Ok(())
    }

    fn build_file_event(&mut self, raw: &fid_parser::FidEvent, event_type: EventType) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

        let size_change = match event_type {
            // For CREATE/MODIFY/CLOSE_WRITE: get actual size and cache it
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(&raw.path).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.put(raw.path.clone(), size);
                size as i64
            }
            // For DELETE/DELETE_SELF/MOVED_FROM: use cached size (file already gone)
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.pop(&raw.path).unwrap_or(0) as i64
            }
            // For other events (OPEN, ACCESS, ATTRIB, etc.): use cached size if available
            _ => self.file_size_cache.get(&raw.path).map_or(0, |&s| s as i64),
        };

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path.clone(),
            pid,
            cmd,
            user,
            size_change,
        }
    }

    fn should_output(&self, event: &FileEvent) -> bool {
        if let Some(ref types) = self.event_types
            && !types.contains(&event.event_type)
        {
            return false;
        }

        if let Some(min) = self.min_size
            && event.size_change.abs() < min
        {
            return false;
        }

        if let Some(ref regex) = self.exclude_regex
            && regex.is_match(&event.path.to_string_lossy())
        {
            return false;
        }

        true
    }

    /// Check if path is within monitoring scope
    /// recursive=true: path can have any monitored directory as prefix
    /// recursive=false: path's parent must be exactly the monitored directory (i.e., direct children only)
    fn is_path_in_scope(&self, path: &Path, canonical_paths: &[PathBuf]) -> bool {
        for watched in canonical_paths {
            if self.recursive {
                if path.starts_with(watched) {
                    return true;
                }
            } else {
                // Non-recursive: only match direct children or self
                if path == watched.as_path() {
                    return true;
                }
                if let Some(parent) = path.parent()
                    && parent == watched.as_path()
                {
                    return true;
                }
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

    #[test]
    fn test_should_output_no_filters() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            false,
            false,
        );
        let event = make_event("/tmp/test.txt", EventType::Create, 1000, 1024);
        assert!(m.should_output(&event));
    }

    #[test]
    fn test_should_output_type_filter_match() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            Some(vec![EventType::Create, EventType::Delete]),
            None,
            None,
            OutputFormat::Human,
            false,
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/a", EventType::Delete, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Modify, 1, 0)));
    }

    #[test]
    fn test_should_output_min_size_filter() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            Some(1000),
            None,
            None,
            None,
            OutputFormat::Human,
            false,
            false,
        );
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 2000)));
        assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, -2000)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, 500)));
        assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, -500)));
    }

    #[test]
    fn test_should_output_exclude_pattern() {
        // Pattern "*.tmp" becomes regex ".*.tmp" (dot is not escaped, matches any char)
        // This matches any path containing "tmp" as a substring in the right position
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            None,
            Some("*.tmp".into()),
            None,
            OutputFormat::Human,
            false,
            false,
        );
        // /tmp/test.tmp matches (ends with .tmp)
        assert!(!m.should_output(&make_event("/tmp/test.tmp", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/foo.tmp", EventType::Delete, 1, 0)));
        // Note: /tmp/test.txt also matches because regex ".*.tmp" matches "/tmp" substring
        // This is expected behavior - the pattern is a substring match, not a glob
    }

    #[test]
    fn test_should_output_exclude_exact_pattern() {
        // Pattern "test.tmp" should match literally, not regex "test.tmp"
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            None,
            Some("test.tmp".into()),
            None,
            OutputFormat::Human,
            false,
            false,
        );
        assert!(m.should_output(&make_event("/tmp/test.txt", EventType::Create, 1, 0)));
        assert!(!m.should_output(&make_event("/tmp/test.tmp", EventType::Create, 1, 0)));
        assert!(m.should_output(&make_event("/tmp/foo.tmp", EventType::Delete, 1, 0)));
        // Should not match "testXtmp" because dot is escaped
        assert!(m.should_output(&make_event("/tmp/testXtmp", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_combined_filters() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            Some(100),
            Some(vec![EventType::Create]),
            Some("*.log".into()),
            None,
            OutputFormat::Human,
            false,
            false,
        );
        // Passes all filters
        assert!(m.should_output(&make_event("/tmp/data", EventType::Create, 1, 200)));
        // Wrong type
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Delete, 1, 200)));
        // Too small
        assert!(!m.should_output(&make_event("/tmp/data", EventType::Create, 1, 50)));
        // Excluded pattern
        assert!(!m.should_output(&make_event("/tmp/app.log", EventType::Create, 1, 200)));
    }

    #[test]
    fn test_is_path_in_scope_recursive() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            true,
            false,
        );
        let watched = vec![PathBuf::from("/tmp")];
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub"), &watched));
        assert!(m.is_path_in_scope(Path::new("/tmp/sub/deep/file.txt"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
        assert!(!m.is_path_in_scope(Path::new("/tmpfile"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp")],
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            false,
            false,
        );
        let watched = vec![PathBuf::from("/tmp")];
        // Self matches
        assert!(m.is_path_in_scope(Path::new("/tmp"), &watched));
        // Direct children match
        assert!(m.is_path_in_scope(Path::new("/tmp/file.txt"), &watched));
        // Nested children don't match
        assert!(!m.is_path_in_scope(Path::new("/tmp/sub/file.txt"), &watched));
        // Unrelated paths don't match
        assert!(!m.is_path_in_scope(Path::new("/var/log"), &watched));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let m = Monitor::new(
            vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")],
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            true,
            false,
        );
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

        // Inserting a 4th entry evicts the least recently used (/a)
        cache.put(PathBuf::from("/d"), 400);
        assert_eq!(cache.len(), 3);
        assert!(cache.get(&PathBuf::from("/a")).is_none());
        assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
        assert_eq!(cache.get(&PathBuf::from("/d")), Some(&400));

        // Accessing /b promotes it; inserting /e evicts /c (now LRU)
        cache.get(&PathBuf::from("/b"));
        cache.put(PathBuf::from("/e"), 500);
        assert!(cache.get(&PathBuf::from("/c")).is_none());
        assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
    }

    fn make_event(path: &str, event_type: EventType, pid: u32, size: i64) -> FileEvent {
        FileEvent {
            time: Utc::now(),
            event_type,
            path: PathBuf::from(path),
            pid,
            cmd: "test".to_string(),
            user: "root".to_string(),
            size_change: size,
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

        // Run monitor in background for a short time
        let handle = rt.spawn(async move {
            // Initialize fanotify
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

            // Read events for 200ms
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

        // Give monitor time to start
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create some files to trigger events
        for i in 0..3 {
            let path = test_dir.join(format!("test_{}.txt", i));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "content {}", i).unwrap();
        }

        // Wait for monitor to finish
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
