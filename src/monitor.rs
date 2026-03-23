use anyhow::{bail, Context, Result};
use chrono::Utc;
use fanotify::low_level::{
    fanotify_init, fanotify_mark,
    FAN_CLOEXEC, FAN_CLASS_NOTIF, FAN_NONBLOCK,
    FAN_REPORT_FID, FAN_REPORT_DIR_FID, FAN_REPORT_NAME,
    FAN_MARK_ADD, AT_FDCWD,
    FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_MOVED_FROM, FAN_MOVED_TO,
    FAN_EVENT_ON_CHILD, FAN_ONDIR,
    O_CLOEXEC, O_RDONLY,
};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{FileEvent, OutputFormat};
use crate::utils::get_process_info_by_pid;

// ---- FID 事件解析所需的内核结构体和常量 ----

/// fanotify_event_info_header.info_type
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;

/// fanotify_event_metadata（与内核结构体一致）
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

/// 从 FID 缓冲区解析出的事件
struct FidEvent {
    mask: u64,
    pid: i32,
    path: PathBuf,
    /// DFID_NAME 中的目录句柄键（fsid + file_handle），用于缓存查找
    dfid_name_handle: Option<Vec<u8>>,
    /// DFID_NAME 中的文件名
    dfid_name_filename: Option<String>,
    /// DFID/FID 记录中的自身句柄键（fsid + file_handle），用于缓存构建
    self_handle: Option<Vec<u8>>,
}

// ---- Monitor ----

pub struct Monitor {
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<String>>,
    exclude: Option<String>,
    output: Option<PathBuf>,
    format: OutputFormat,
    recursive: bool,
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
    ) -> Self {
        Self { paths, min_size, event_types, exclude, output, format, recursive }
    }

    pub async fn run(self) -> Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            bail!("fanotify 需要 root 权限，请使用 sudo 运行");
        }

        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            r.store(false, Ordering::SeqCst);
        });

        // 初始化 fanotify，启用 FID 模式以支持全部目录条目事件
        let fan_fd = fanotify_init(
            FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF
                | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
            (O_CLOEXEC | O_RDONLY) as u32,
        ).context("fanotify_init 失败（需要 Linux 5.9+ 内核）")?;

        // 完整事件掩码
        let mask = FAN_CLOSE_WRITE
            | FAN_CREATE | FAN_DELETE
            | FAN_MOVED_FROM | FAN_MOVED_TO
            | FAN_EVENT_ON_CHILD | FAN_ONDIR;

        let mut mount_fds = Vec::new();

        for path in &self.paths {
            let canonical = if path.exists() {
                path.canonicalize().unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };

            // 标记根目录
            mark_directory(fan_fd, mask, &canonical)?;

            // 递归标记所有子目录
            if self.recursive && canonical.is_dir() {
                mark_recursive(fan_fd, mask, &canonical);
            }

            // 打开目录 fd，供 open_by_handle_at 解析文件句柄
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
            self.paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("Press Ctrl+C to stop\n");

        // 持久目录句柄缓存：handle_key → dir_path
        // 在启动时预缓存所有已标记目录，用于恢复已删除目录的子文件路径
        let mut dir_cache: HashMap<Vec<u8>, PathBuf> = HashMap::new();
        for path in &self.paths {
            let canonical = if path.exists() {
                path.canonicalize().unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };
            if canonical.is_dir() {
                cache_recursive(&mut dir_cache, &canonical);
            }
        }

        while running.load(Ordering::SeqCst) {
            let events = read_fid_events(fan_fd, &mount_fds, &mut dir_cache);

            // 同批事件预处理：
            // 1. CREATE + CLOSE_WRITE 去重（同一 open(O_CREAT) + close）
            // 2. MOVED_FROM + MOVED_TO 配对合并为单条 MOVE
            let mut created_in_batch = HashSet::new();
            let mut moved_from_by_pid: HashMap<i32, (usize, PathBuf)> = HashMap::new();

            for (i, raw) in events.iter().enumerate() {
                if raw.mask & FAN_CREATE != 0 {
                    created_in_batch.insert(raw.path.clone());
                }
                if raw.mask & FAN_MOVED_FROM != 0 {
                    moved_from_by_pid.insert(raw.pid, (i, raw.path.clone()));
                }
            }

            // 找出有配对 MOVED_TO 的 MOVED_FROM 索引（这些由 MOVED_TO 合并输出）
            let mut paired_from_indices = HashSet::new();
            for raw in &events {
                if raw.mask & FAN_MOVED_TO != 0 {
                    if let Some(&(from_idx, _)) = moved_from_by_pid.get(&raw.pid) {
                        paired_from_indices.insert(from_idx);
                    }
                }
            }

            // 递归模式：新建子目录或子目录移入时，动态添加 mark 并更新句柄缓存
            if self.recursive {
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

            for (i, raw) in events.iter().enumerate() {
                // 跳过已配对的 MOVED_FROM（由对应 MOVED_TO 合并输出）
                if paired_from_indices.contains(&i) {
                    continue;
                }

                // 跳过 CREATE 后紧跟的纯 CLOSE_WRITE 事件
                // 注意：内核可能将 CREATE+CLOSE_WRITE 合并为单个事件（mask 同时含两者），
                // 此时不能跳过，否则 CREATE 也被吞掉
                if raw.mask & FAN_CLOSE_WRITE != 0
                    && raw.mask & FAN_CREATE == 0
                    && created_in_batch.contains(&raw.path)
                {
                    continue;
                }

                // MOVED_TO：附上配对的 MOVED_FROM 源路径
                let move_from = if raw.mask & FAN_MOVED_TO != 0 {
                    moved_from_by_pid.get(&raw.pid).map(|(_, path)| path.clone())
                } else {
                    None
                };

                let event = self.build_file_event(raw, move_from);
                if self.should_output(&event) {
                    self.output_event(&event, &mut output_file)?;
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
        let pid_file = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join("fsmon.pid");

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
        let config_file = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join("fsmon.json");
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

    fn build_file_event(&self, raw: &FidEvent, move_from: Option<PathBuf>) -> FileEvent {
        let pid = raw.pid.unsigned_abs();
        let (cmd, user) = get_process_info_by_pid(pid, &raw.path);

        let mut event_type = mask_to_event_type(raw.mask);

        // 同目录下的 MOVE 视为 RENAME
        if event_type == "MOVE" {
            if let Some(ref from) = move_from {
                if from.parent() == raw.path.parent() {
                    event_type = "RENAME".to_string();
                }
            }
        }

        let size_change = fs::metadata(&raw.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path.clone(),
            move_from,
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

        if let Some(ref exclude) = self.exclude {
            if let Ok(pattern) = regex::Regex::new(&exclude.replace("*", ".*")) {
                if pattern.is_match(&event.path.to_string_lossy()) {
                    return false;
                }
            }
        }

        true
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

// ---- 事件类型映射 ----

fn mask_to_event_type(mask: u64) -> String {
    // MOVE 优先于 DELETE：rename 覆盖已有文件时 mask 同时含 FAN_DELETE | FAN_MOVED_TO
    if mask & (FAN_MOVED_FROM | FAN_MOVED_TO) != 0 { return "MOVE".to_string(); }
    if mask & FAN_CREATE != 0 { return "CREATE".to_string(); }
    if mask & FAN_DELETE != 0 { return "DELETE".to_string(); }
    if mask & FAN_CLOSE_WRITE != 0 { return "MODIFY".to_string(); }
    "UNKNOWN".to_string()
}

// ---- FID 事件读取与解析 ----

/// 从 fanotify fd 读取并解析 FID 格式事件
///
/// 使用两遍处理 + 持久缓存：
/// 1. 第一遍：解析所有事件，尝试解析文件句柄
/// 2. 第二遍：用持久缓存恢复因目录已删除而解析失败的子文件路径
/// 3. 将新解析的目录信息更新到持久缓存
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

    // ---- 第一遍：解析事件并提取句柄数据 ----

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

        // FID 模式下 fd 应为 -1，但防御性关闭
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

    // ---- 第二遍：用持久缓存恢复已删除目录的子文件路径 ----
    // 先从本批次成功解析的事件更新缓存，再用缓存恢复失败的事件
    // 迭代直到不再有新的路径被解析（处理多级嵌套删除）

    loop {
        // 从已成功解析的事件更新持久缓存
        for ev in events.iter() {
            if ev.path.as_os_str().is_empty() {
                continue;
            }

            // 缓存自身句柄 → 路径
            if let Some(ref key) = ev.self_handle {
                dir_cache.entry(key.clone()).or_insert_with(|| ev.path.clone());
            }

            // 缓存 DFID_NAME 中的目录句柄 → 目录路径
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

        // 尝试用缓存恢复空路径的事件
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

/// 解析 DFID_NAME info record：提取目录句柄键、文件名、尝试解析的路径
///
/// 返回 (handle_key, filename, resolved_path)
/// handle_key = fsid + file_handle 字节，唯一标识一个目录
/// 即使 open_by_handle_at 失败（目录已删除），也返回 handle_key 和 filename
///
/// 内存布局: InfoHeader(4) | fsid(8) | file_handle(8+N) | filename(null结尾,对齐填充)
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

    // 提取 null 结尾的文件名
    let name_bytes = &buf[name_off..record_end];
    let name = name_bytes.split(|&b| b == 0).next().unwrap_or(&[]);
    let filename = std::str::from_utf8(name).ok()?.to_string();

    // 缓存键：file_handle 字节（唯一标识该目录 inode，同一文件系统内）
    let key = buf[fh_off..fh_off + fh_total].to_vec();

    // 尝试解析目录句柄
    let dir_path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);
    let full_path = dir_path.map(|dp| {
        if filename.is_empty() { dp } else { dp.join(&filename) }
    });

    Some((key, filename, full_path))
}

/// 解析 FID/DFID info record：提取自身句柄键和尝试解析的路径
///
/// 返回 (handle_key, resolved_path)
///
/// 内存布局: InfoHeader(4) | fsid(8) | file_handle(8+N)
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

/// 通过 open_by_handle_at 将内核文件句柄解析为路径
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

// ---- 目录标记 ----

/// 标记单个目录
fn mark_directory(fan_fd: i32, mask: u64, path: &Path) -> Result<()> {
    fanotify_mark(fan_fd, FAN_MARK_ADD, mask, AT_FDCWD, path)
        .with_context(|| format!("fanotify_mark 失败: {}", path.display()))
}

/// 递归遍历并标记所有子目录（忽略错误，如权限不足的目录）
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

// ---- 目录句柄缓存 ----

/// 通过 name_to_handle_at 获取路径的文件句柄键
/// 返回的字节与 fanotify FID 事件中的 file_handle 格式相同
fn path_to_handle_key(path: &Path) -> Option<Vec<u8>> {
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut mount_id: libc::c_int = 0;
    let mut buf = vec![0u8; 128]; // 足够容纳任何文件句柄

    // 设置 handle_bytes 字段为缓冲区容量减去头部
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

/// 将目录路径的句柄键加入缓存
fn cache_dir_handle(cache: &mut HashMap<Vec<u8>, PathBuf>, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// 递归缓存目录及其所有子目录的句柄
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
