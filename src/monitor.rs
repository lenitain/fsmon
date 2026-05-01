use anyhow::{Context, Result, bail};
use chrono::Utc;
use fanotify::low_level::{
    AT_FDCWD, FAN_ACCESS, FAN_ATTRIB, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_CLOSE_NOWRITE,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_DELETE_SELF, FAN_EVENT_ON_CHILD, FAN_MARK_ADD,
    FAN_MARK_FILESYSTEM, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_NONBLOCK,
    FAN_ONDIR, FAN_OPEN, FAN_OPEN_EXEC, FAN_Q_OVERFLOW, FAN_REPORT_DIR_FID, FAN_REPORT_FID,
    FAN_REPORT_NAME, O_CLOEXEC, O_RDONLY, fanotify_init, fanotify_mark,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::proc_cache::{self, ProcCache};
use crate::utils::get_process_info_by_pid;
use crate::{EventType, FileEvent, OutputFormat};

/// Handle key type: fsid + file_handle bytes, stack-allocated if ≤128 bytes
type HandleKey = SmallVec<[u8; 128]>;

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
const FSID_SIZE: usize = 8; // __kernel_fsid_t = { i32 val[2]; }
const FH_HDR_SIZE: usize = 8; // file_handle: handle_bytes(u32) + handle_type(i32)

/// Event parsed from FID buffer
struct FidEvent {
    mask: u64,
    pid: i32,
    path: PathBuf,
    /// DFID_NAME directory handle key (fsid + file_handle), for cache lookup
    dfid_name_handle: Option<HandleKey>,
    /// DFID_NAME filename
    dfid_name_filename: Option<String>,
    /// Self handle key from DFID/FID record (fsid + file_handle), for cache building
    self_handle: Option<HandleKey>,
}

// ---- Monitor ----

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
    file_size_cache: HashMap<PathBuf, u64>,
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
            file_size_cache: HashMap::new(),
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
                    cache_recursive(&mut dir_cache, canonical);
                } else {
                    cache_dir_handle(&mut dir_cache, canonical);
                }
            }
        }

        let mut buf = vec![0u8; 4096 * 8]; // 32KB, reused across loop iterations

        while running.load(Ordering::SeqCst) {
            let events = read_fid_events(fan_fd, &mount_fds, &mut dir_cache, &mut buf);

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

    fn build_file_event(&mut self, raw: &FidEvent, event_type: EventType) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

        let size_change = match event_type {
            // For CREATE/MODIFY/CLOSE_WRITE: get actual size and cache it
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(&raw.path).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.insert(raw.path.clone(), size);
                size as i64
            }
            // For DELETE/DELETE_SELF/MOVED_FROM: use cached size (file already gone)
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.remove(&raw.path).unwrap_or(0) as i64
            }
            // For other events: get actual size
            _ => fs::metadata(&raw.path).map(|m| m.len() as i64).unwrap_or(0),
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

    fn output_event(&self, event: &FileEvent, output_file: &mut Option<fs::File>) -> Result<()> {
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

const EVENT_BITS: [(u64, EventType); 14] = [
    (FAN_ACCESS, EventType::Access),
    (FAN_MODIFY, EventType::Modify),
    (FAN_CLOSE_WRITE, EventType::CloseWrite),
    (FAN_CLOSE_NOWRITE, EventType::CloseNowrite),
    (FAN_OPEN, EventType::Open),
    (FAN_OPEN_EXEC, EventType::OpenExec),
    (FAN_ATTRIB, EventType::Attrib),
    (FAN_CREATE, EventType::Create),
    (FAN_DELETE, EventType::Delete),
    (FAN_DELETE_SELF, EventType::DeleteSelf),
    (FAN_MOVED_FROM, EventType::MovedFrom),
    (FAN_MOVED_TO, EventType::MovedTo),
    (FAN_MOVE_SELF, EventType::MoveSelf),
    (FAN_FS_ERROR, EventType::FsError),
];

fn mask_to_event_types(mask: u64) -> SmallVec<[EventType; 8]> {
    EVENT_BITS
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, event_type)| *event_type)
        .collect()
}

// ---- FID event reading and parsing ----

/// Read and parse FID format events from fanotify fd
///
/// Uses two-pass processing + persistent cache:
/// 1. First pass: Parse all events, try to resolve file handles
/// 2. Second pass: Use persistent cache to recover child file paths for events that failed due to directory deletion
/// 3. Update newly resolved directory info to persistent cache
fn read_fid_events(
    fan_fd: i32,
    mount_fds: &[i32],
    dir_cache: &mut HashMap<HandleKey, PathBuf>,
    buf: &mut Vec<u8>,
) -> Vec<FidEvent> {
    let n = unsafe { libc::read(fan_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };

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
        let mut dfid_name_handle: Option<HandleKey> = None;
        let mut dfid_name_filename: Option<String> = None;
        let mut self_handle: Option<HandleKey> = None;

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
                    if let Some((key, filename, resolved)) =
                        extract_dfid_name(buf, info_off, info_len, mount_fds)
                    {
                        dfid_name_handle = Some(key);
                        dfid_name_filename = Some(filename);
                        if let Some(p) = resolved {
                            path = p;
                        }
                    }
                }
                FAN_EVENT_INFO_TYPE_FID | FAN_EVENT_INFO_TYPE_DFID => {
                    if let Some((key, resolved)) = extract_fid(buf, info_off, info_len, mount_fds) {
                        self_handle = Some(key);
                        if path.as_os_str().is_empty()
                            && let Some(p) = resolved
                        {
                            path = p;
                        }
                    }
                }
                _ => {}
            }

            info_off += info_len;
        }

        // In FID mode, fd should be -1, but defensively close it
        if meta.fd >= 0 {
            unsafe {
                libc::close(meta.fd);
            }
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
                dir_cache
                    .entry(key.clone())
                    .or_insert_with(|| ev.path.clone());
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
            if let (Some(key), Some(filename)) = (&ev.dfid_name_handle, &ev.dfid_name_filename)
                && let Some(dir_path) = dir_cache.get(key)
            {
                ev.path = if filename.is_empty() {
                    dir_path.clone()
                } else {
                    dir_path.join(filename)
                };
                made_progress = true;
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
fn extract_dfid_name(
    buf: &[u8],
    info_off: usize,
    info_len: usize,
    mount_fds: &[i32],
) -> Option<(HandleKey, String, Option<PathBuf>)> {
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[fh_off..fh_off + 4].try_into().ok()?) as usize;
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
    let key = HandleKey::from_slice(&buf[fh_off..fh_off + fh_total]);

    // Try to resolve directory handle
    let dir_path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);
    let full_path = dir_path.map(|dp| {
        if filename.is_empty() {
            dp
        } else {
            dp.join(&filename)
        }
    });

    Some((key, filename, full_path))
}

/// Parse FID/DFID info record: extract self handle key and try to resolve path
///
/// Returns (handle_key, resolved_path)
///
/// Memory layout: InfoHeader(4) | fsid(8) | file_handle(8+N)
fn extract_fid(
    buf: &[u8],
    info_off: usize,
    info_len: usize,
    mount_fds: &[i32],
) -> Option<(HandleKey, Option<PathBuf>)> {
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[fh_off..fh_off + 4].try_into().ok()?) as usize;
    let fh_total = FH_HDR_SIZE + handle_bytes;

    if fh_off + fh_total > record_end {
        return None;
    }

    let key = HandleKey::from_slice(&buf[fh_off..fh_off + fh_total]);
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
            unsafe {
                libc::close(fd);
            }
            if let Ok(p) = result {
                return Some(p);
            }
        }
    }

    None
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
fn path_to_handle_key(path: &Path) -> Option<HandleKey> {
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
    Some(HandleKey::from_slice(&buf[0..FH_HDR_SIZE + handle_bytes]))
}

/// Add directory path handle key to cache
fn cache_dir_handle(cache: &mut HashMap<HandleKey, PathBuf>, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// Recursively cache directory and all subdirectory handles
fn cache_recursive(cache: &mut HashMap<HandleKey, PathBuf>, dir: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            | FAN_MOVED_FROM
            | FAN_MOVED_TO
            | FAN_MOVE_SELF
            | FAN_FS_ERROR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 14);
    }

    #[test]
    fn test_mask_to_event_types_with_flags() {
        // FAN_EVENT_ON_CHILD and FAN_ONDIR are flags, not event types
        let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }

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

    #[test]
    #[ignore]
    fn test_resolve_file_handle() {
        let test_dir = std::env::temp_dir().join("fsmon_test_handle");
        let test_file = test_dir.join("test.txt");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(&test_file, "hello").unwrap();

        // Open directory fd for handle resolution
        let dir_c = std::ffi::CString::new(test_dir.to_string_lossy().as_bytes()).unwrap();
        let mfd = unsafe { libc::open(dir_c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        assert!(mfd >= 0, "Should be able to open directory");

        // Get file handle using name_to_handle_at
        let file_c = std::ffi::CString::new(test_file.to_string_lossy().as_bytes()).unwrap();
        let mut mount_id: libc::c_int = 0;
        let mut fh_buf = vec![0u8; 128];
        let capacity = (fh_buf.len() - 8) as u32;
        fh_buf[0..4].copy_from_slice(&capacity.to_ne_bytes());

        let ret = unsafe {
            libc::name_to_handle_at(
                libc::AT_FDCWD,
                file_c.as_ptr(),
                fh_buf.as_mut_ptr() as *mut libc::file_handle,
                &mut mount_id,
                0,
            )
        };
        assert_eq!(ret, 0, "name_to_handle_at should succeed");

        // Resolve handle back to path
        let resolved = resolve_file_handle(&[mfd], &fh_buf);
        assert!(resolved.is_some(), "Should resolve file handle to path");

        unsafe {
            libc::close(mfd);
        }
        let _ = std::fs::remove_dir_all(&test_dir);
    }
}
