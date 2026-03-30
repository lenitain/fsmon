use anyhow::{bail, Context, Result};
use chrono::Utc;
use fanotify::low_level::{
    fanotify_init, fanotify_mark,
    FAN_CLOEXEC, FAN_CLASS_NOTIF, FAN_NONBLOCK,
    FAN_REPORT_FID, FAN_REPORT_DIR_FID, FAN_REPORT_NAME,
    FAN_MARK_ADD, FAN_MARK_FILESYSTEM, AT_FDCWD,
    FAN_ACCESS, FAN_MODIFY, FAN_ATTRIB,
    FAN_CLOSE_WRITE, FAN_CLOSE_NOWRITE,
    FAN_OPEN, FAN_OPEN_EXEC,
    FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF,
    FAN_MOVED_FROM, FAN_MOVED_TO, FAN_MOVE_SELF,
    FAN_Q_OVERFLOW,
    FAN_EVENT_ON_CHILD, FAN_ONDIR,
    O_CLOEXEC, O_RDONLY,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{FileEvent, OutputFormat};
use crate::utils::get_process_info_by_pid;
use crate::proc_cache::{self, ProcCache};

fn get_runtime_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "fsmon", "fsmon")
        .and_then(|dirs| dirs.runtime_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

// ---- FID event parsing required kernel structures and constants ----

/// fanotify_event_info_header.info_type
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;

/// FAN_FS_ERROR: filesystem error notification (Linux 5.16+)
/// fanotify-rs 0.3.1 does not export this constant, manually define
const FAN_FS_ERROR: u64 = 0x0000_8000;

/// fanotify_event_metadata (matches kernel structure)
#[repr(C)]
struct FanMetadata {
    event_len: u32,
    vers: u8,
    reserved: u8,
    metadata_len: u16,
    mask: u64,
    fd: i32,
    pid: i32,
}

/// fanotify_event_info_header
#[repr(C)]
struct FanInfoHeader {
    info_type: u8,
    pad: u8,
    len: u16,
}

const META_SIZE: usize = std::mem::size_of::<FanMetadata>();
const INFO_HDR_SIZE: usize = std::mem::size_of::<FanInfoHeader>();
const FSID_SIZE: usize = 8;            // __kernel_fsid_t = { i32 val[2]; }
const FH_HDR_SIZE: usize = 8;          // file_handle: handle_bytes(u32) + handle_type(i32)

/// Event parsed from FID buffer
struct FidEvent {
    mask: u64,
    pid: i32,
    path: PathBuf,
    /// DFID_NAME directory handle key (fsid + file_handle), for cache lookup
    dfid_name_handle: Option<Vec<u8>>,
    /// DFID_NAME filename
    dfid_name_filename: Option<String>,
    /// Self handle key from DFID/FID record (fsid + file_handle), for cache building
    self_handle: Option<Vec<u8>>,
}

// ---- Monitor ----

pub struct Monitor {
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<String>>,
    exclude_regex: Option<regex::Regex>,
    output: Option<PathBuf>,
    format: OutputFormat,
    recursive: bool,
    all_events: bool,
    proc_cache: Option<ProcCache>,
}

impl Monitor {
    pub fn new(
        paths: Vec<PathBuf>,
        min_size: Option<i64>,
        event_types: Option<Vec<String>>,
        exclude: Option<String>,
        output: Option<PathBuf>,
        format: OutputFormat,
        recursive: bool,
        all_events: bool,
    ) -> Self {
        let exclude_regex = exclude
            .map(|p| regex::Regex::new(&p.replace("*", ".*")).expect("invalid exclude pattern"));
        Self { paths, min_size, event_types, exclude_regex, output, format, recursive, all_events, proc_cache: None }
    }

    pub async fn run(mut self) -> Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            bail!("fanotify requires root privileges, please run with sudo");
        }

        // Start proc connector listener thread, cache process exec info
        self.proc_cache = Some(proc_cache::start_proc_listener());
        // Brief wait for listener to complete subscription
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            r.store(false, Ordering::SeqCst);
        });

        // Initialize fanotify, enable FID mode to support all directory entry events
        let fan_fd = fanotify_init(
            FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF
                | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
            (O_CLOEXEC | O_RDONLY) as u32,
        ).context("fanotify_init failed (requires Linux 5.9+ kernel)")?;

        // Event mask
        // Default: 8 core change events
        // --all-events: all 14 fanotify notification events
        let mask = if self.all_events {
            FAN_ACCESS | FAN_MODIFY | FAN_ATTRIB
                | FAN_CLOSE_WRITE | FAN_CLOSE_NOWRITE
                | FAN_OPEN | FAN_OPEN_EXEC
                | FAN_CREATE | FAN_DELETE | FAN_DELETE_SELF
                | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF
                | FAN_FS_ERROR
                | FAN_EVENT_ON_CHILD | FAN_ONDIR
        } else {
            FAN_CLOSE_WRITE | FAN_ATTRIB
                | FAN_CREATE | FAN_DELETE | FAN_DELETE_SELF
                | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF
                | FAN_EVENT_ON_CHILD | FAN_ONDIR
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
                    fan_fd, FAN_MARK_ADD | FAN_MARK_FILESYSTEM, mask, AT_FDCWD, canonical,
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
                let mfd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
                if mfd >= 0 {
                    mount_fds.push(mfd);
                }
            }
        }

        // Setup output file if specified
        let mut output_file = if let Some(ref path) = self.output {
            let parent = path.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(parent)?;
            Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            )
        } else {
            None
        };

        println!("Starting file trace monitor...");
        println!(
            "Monitoring paths: {}",
            canonical_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("Press Ctrl+C to stop\n");

        // Persistent directory handle cache: handle_key → dir_path
        // Pre-cache monitored directories at startup, for recovering deleted directory child file paths
        let mut dir_cache: HashMap<Vec<u8>, PathBuf> = HashMap::new();
        for canonical in &canonical_paths {
            if canonical.is_dir() {
                cache_recursive(&mut dir_cache, canonical);
            }
        }

        while running.load(Ordering::SeqCst) {
            let events = read_fid_events(fan_fd, &mount_fds, &mut dir_cache);

            // inode mark mode: dynamically add marks and update handle cache when new subdirectories are created or moved in
            if !use_fs_mark && self.recursive {
                for raw in &events {
                    let is_dir_create = raw.mask & FAN_CREATE != 0 && raw.mask & FAN_ONDIR != 0;
                    let is_dir_moved_to = raw.mask & FAN_MOVED_TO != 0 && raw.mask & FAN_ONDIR != 0;
                    if (is_dir_create || is_dir_moved_to) && raw.path.is_dir() {
                        let _ = mark_directory(fan_fd, mask, &raw.path);
                        mark_recursive(fan_fd, mask, &raw.path);
                        cache_recursive(&mut dir_cache, &raw.path);
                    }
                }
            }

            for raw in &events {
                // FAN_Q_OVERFLOW: queue overflow warning (auto-delivered by kernel, not in subscription mask)
                if raw.mask & FAN_Q_OVERFLOW != 0 {
                    eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                    continue;
                }

                let event_types = mask_to_event_types(raw.mask);

                for event_type in event_types {
                    let event = self.build_file_event(raw, event_type);

                    // filesystem mark mode: userspace path prefix filtering
                    if use_fs_mark && !self.is_path_in_scope(&event.path, &canonical_paths) {
                        continue;
                    }

                    if self.should_output(&event) {
                        self.output_event(&event, &mut output_file)?;
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        // Cleanup
        unsafe { libc::close(fan_fd); }
        for mfd in mount_fds {
            unsafe { libc::close(mfd); }
        }

        println!("\nStopping file trace monitor...");
        Ok(())
    }

    pub async fn run_daemon(self) -> Result<()> {
        // Create PID file
        let pid_file = get_runtime_dir().join("fsmon.pid");

        if pid_file.exists() {
            let pid_str = fs::read_to_string(&pid_file)?;
            let pid: u32 = pid_str.trim().parse()?;
            if process_exists(pid) {
                println!("fsmon daemon already running (PID: {})", pid);
                return Ok(());
            }
        }

        // Write PID file
        fs::write(&pid_file, process::id().to_string())?;

        // Create log directory
        let log_file = self.output.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h: PathBuf| h.join(".fsmon").join("history.log"))
                .unwrap_or_else(|| PathBuf::from("history.log"))
        });

        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // Save daemon config
        let config_file = get_runtime_dir().join("fsmon.json");
        let config = serde_json::json!({
            "paths": self.paths,
            "log_file": log_file,
            "start_time": Utc::now().to_rfc3339(),
        });
        fs::write(&config_file, serde_json::to_string_pretty(&config)?)?;

        println!(
            "fsmon daemon started (PID: {}), log file: {}",
            process::id(),
            log_file.display()
        );

        self.run().await?;

        // Cleanup
        let _ = fs::remove_file(&pid_file);
        let _ = fs::remove_file(&config_file);

        Ok(())
    }

    fn build_file_event(&self, raw: &FidEvent, event_type: &str) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

        let size_change = fs::metadata(&raw.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        FileEvent {
            time: Utc::now(),
            event_type: event_type.to_string(),
            path: raw.path.clone(),
            pid,
            cmd,
            user,
            size_change,
        }
    }

    fn should_output(&self, event: &FileEvent) -> bool {
        if let Some(ref types) = self.event_types {
            if !types.contains(&event.event_type) {
                return false;
            }
        }

        if let Some(min) = self.min_size {
            if event.size_change.abs() < min {
                return false;
            }
        }

        if let Some(ref regex) = self.exclude_regex {
            if regex.is_match(&event.path.to_string_lossy()) {
                return false;
            }
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
                if let Some(parent) = path.parent() {
                    if parent == watched.as_path() {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn output_event(
        &self,
        event: &FileEvent,
        output_file: &mut Option<fs::File>,
    ) -> Result<()> {
        match self.format {
            OutputFormat::Human => {
                let output = event.to_human_string();
                println!("{}", output);
                if let Some(file) = output_file {
                    writeln!(file, "{}", serde_json::to_string(event)?)?;
                }
            }
            OutputFormat::Json => {
                let json = serde_json::to_string(event)?;
                println!("{}", json);
                if let Some(file) = output_file {
                    writeln!(file, "{}", json)?;
                }
            }
            OutputFormat::Csv => {
                let csv = format!(
                    "{},{},{},{},{},{},{}",
                    event.time.to_rfc3339(),
                    event.event_type,
                    event.path.display(),
                    event.pid,
                    event.cmd,
                    event.user,
                    event.size_change
                );
                println!("{}", csv);
                if let Some(file) = output_file {
                    writeln!(file, "{}", serde_json::to_string(event)?)?;
                }
            }
        }
        Ok(())
    }
}

// ---- Event type mapping (1:1 with fanotify event bits) ----

const EVENT_BITS: [(u64, &str); 14] = [
    (FAN_ACCESS,        "ACCESS"),
    (FAN_MODIFY,        "MODIFY"),
    (FAN_CLOSE_WRITE,   "CLOSE_WRITE"),
    (FAN_CLOSE_NOWRITE, "CLOSE_NOWRITE"),
    (FAN_OPEN,          "OPEN"),
    (FAN_OPEN_EXEC,     "OPEN_EXEC"),
    (FAN_ATTRIB,        "ATTRIB"),
    (FAN_CREATE,        "CREATE"),
    (FAN_DELETE,        "DELETE"),
    (FAN_DELETE_SELF,   "DELETE_SELF"),
    (FAN_MOVED_FROM,   "MOVED_FROM"),
    (FAN_MOVED_TO,     "MOVED_TO"),
    (FAN_MOVE_SELF,    "MOVE_SELF"),
    (FAN_FS_ERROR,     "FS_ERROR"),
];

fn mask_to_event_types(mask: u64) -> SmallVec<[&'static str; 8]> {
    EVENT_BITS.iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

// ---- FID event reading and parsing ----

/// Read and parse FID format events from fanotify fd
///
/// Uses two-pass processing + persistent cache:
/// 1. First pass: Parse all events, try to resolve file handles
/// 2. Second pass: Use persistent cache to recover child file paths for events that failed due to directory deletion
/// 3. Update newly resolved directory info to persistent cache
fn read_fid_events(fan_fd: i32, mount_fds: &[i32], dir_cache: &mut HashMap<Vec<u8>, PathBuf>) -> Vec<FidEvent> {
    let mut buf = vec![0u8; 4096 * 8]; // 32KB
    let n = unsafe {
        libc::read(fan_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
    };

    if n <= 0 {
        return vec![];
    }

    let n = n as usize;
    let mut events = Vec::new();
    let mut offset = 0;

    // ---- First pass: Parse events and extract handle data ----

    while offset + META_SIZE <= n {
        let meta = unsafe { &*(buf.as_ptr().add(offset) as *const FanMetadata) };
        let event_len = meta.event_len as usize;

        if event_len < META_SIZE || offset + event_len > n {
            break;
        }

        let mut path = PathBuf::new();
        let mut dfid_name_handle: Option<Vec<u8>> = None;
        let mut dfid_name_filename: Option<String> = None;
        let mut self_handle: Option<Vec<u8>> = None;

        let mut info_off = offset + meta.metadata_len as usize;
        let event_end = offset + event_len;

        while info_off + INFO_HDR_SIZE <= event_end {
            let hdr = unsafe { &*(buf.as_ptr().add(info_off) as *const FanInfoHeader) };
            let info_len = hdr.len as usize;

            if info_len < INFO_HDR_SIZE || info_off + info_len > event_end {
                break;
            }

            match hdr.info_type {
                FAN_EVENT_INFO_TYPE_DFID_NAME => {
                    if let Some((key, filename, resolved)) = extract_dfid_name(&buf, info_off, info_len, mount_fds) {
                        dfid_name_handle = Some(key);
                        dfid_name_filename = Some(filename);
                        if let Some(p) = resolved {
                            path = p;
                        }
                    }
                }
                FAN_EVENT_INFO_TYPE_FID | FAN_EVENT_INFO_TYPE_DFID => {
                    if let Some((key, resolved)) = extract_fid(&buf, info_off, info_len, mount_fds) {
                        self_handle = Some(key);
                        if path.as_os_str().is_empty() {
                            if let Some(p) = resolved {
                                path = p;
                            }
                        }
                    }
                }
                _ => {}
            }

            info_off += info_len;
        }

        // In FID mode, fd should be -1, but defensively close it
        if meta.fd >= 0 {
            unsafe { libc::close(meta.fd); }
        }

        events.push(FidEvent {
            mask: meta.mask,
            pid: meta.pid,
            path,
            dfid_name_handle,
            dfid_name_filename,
            self_handle,
        });

        offset += event_len;
    }

    // ---- Second pass: Use persistent cache to recover child file paths for deleted directories ----
    // First update cache from successfully resolved events in this batch, then use cache to recover failed events
    // Iterate until no new paths are resolved (handles multi-level nested deletion)

    loop {
        // Update persistent cache from successfully resolved events
        for ev in events.iter() {
            if ev.path.as_os_str().is_empty() {
                continue;
            }

            // Cache self handle → path
            if let Some(ref key) = ev.self_handle {
                dir_cache.entry(key.clone()).or_insert_with(|| ev.path.clone());
            }

            // Cache DFID_NAME directory handle → directory path
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

        // Try to recover empty path events using cache
        let mut made_progress = false;
        for ev in events.iter_mut() {
            if !ev.path.as_os_str().is_empty() {
                continue;
            }
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
        }

        if !made_progress {
            break;
        }
    }

    events
}

/// Parse DFID_NAME info record: extract directory handle key, filename, and try to resolve path
///
/// Returns (handle_key, filename, resolved_path)
/// handle_key = fsid + file_handle bytes, uniquely identifies a directory
/// Even if open_by_handle_at fails (directory deleted), still returns handle_key and filename
///
/// Memory layout: InfoHeader(4) | fsid(8) | file_handle(8+N) | filename(null-terminated, padded)
fn extract_dfid_name(buf: &[u8], info_off: usize, info_len: usize, mount_fds: &[i32])
    -> Option<(Vec<u8>, String, Option<PathBuf>)>
{
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(
        buf[fh_off..fh_off + 4].try_into().ok()?
    ) as usize;
    let fh_total = FH_HDR_SIZE + handle_bytes;
    let name_off = fh_off + fh_total;

    if name_off > record_end {
        return None;
    }

    // Extract null-terminated filename
    let name_bytes = &buf[name_off..record_end];
    let name = name_bytes.split(|&b| b == 0).next().unwrap_or(&[]);
    let filename = std::str::from_utf8(name).ok()?.to_string();

    // Cache key: file_handle bytes (uniquely identifies the directory inode within the same filesystem)
    let key = buf[fh_off..fh_off + fh_total].to_vec();

    // Try to resolve directory handle
    let dir_path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);
    let full_path = dir_path.map(|dp| {
        if filename.is_empty() { dp } else { dp.join(&filename) }
    });

    Some((key, filename, full_path))
}

/// Parse FID/DFID info record: extract self handle key and try to resolve path
///
/// Returns (handle_key, resolved_path)
///
/// Memory layout: InfoHeader(4) | fsid(8) | file_handle(8+N)
fn extract_fid(buf: &[u8], info_off: usize, info_len: usize, mount_fds: &[i32])
    -> Option<(Vec<u8>, Option<PathBuf>)>
{
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(
        buf[fh_off..fh_off + 4].try_into().ok()?
    ) as usize;
    let fh_total = FH_HDR_SIZE + handle_bytes;

    if fh_off + fh_total > record_end {
        return None;
    }

    let key = buf[fh_off..fh_off + fh_total].to_vec();
    let path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);

    Some((key, path))
}

/// Resolve kernel file handle to path via open_by_handle_at
fn resolve_file_handle(mount_fds: &[i32], fh_data: &[u8]) -> Option<PathBuf> {
    if fh_data.len() < FH_HDR_SIZE {
        return None;
    }

    for &mfd in mount_fds {
        let fd = unsafe {
            libc::open_by_handle_at(
                mfd,
                fh_data.as_ptr() as *mut libc::file_handle,
                libc::O_PATH,
            )
        };

        if fd >= 0 {
            let result = fs::read_link(format!("/proc/self/fd/{}", fd));
            unsafe { libc::close(fd); }
            if let Ok(p) = result {
                return Some(p);
            }
        }
    }

    None
}

fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
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

// ---- Directory Handle Cache ----

/// Get handle key for a path via name_to_handle_at
/// Returns bytes matching the file_handle format in fanotify FID events
fn path_to_handle_key(path: &Path) -> Option<Vec<u8>> {
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut mount_id: libc::c_int = 0;
    let mut buf = vec![0u8; 128];

    let capacity = (buf.len() - FH_HDR_SIZE) as u32;
    buf[0..4].copy_from_slice(&capacity.to_ne_bytes());

    let ret = unsafe {
        libc::name_to_handle_at(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            buf.as_mut_ptr() as *mut libc::file_handle,
            &mut mount_id,
            0,
        )
    };

    if ret != 0 {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
    Some(buf[0..FH_HDR_SIZE + handle_bytes].to_vec())
}

/// Add directory path handle key to cache
fn cache_dir_handle(cache: &mut HashMap<Vec<u8>, PathBuf>, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// Recursively cache directory and all subdirectory handles
fn cache_recursive(cache: &mut HashMap<Vec<u8>, PathBuf>, dir: &Path) {
    cache_dir_handle(cache, dir);
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            cache_recursive(cache, &path);
        }
    }
}
