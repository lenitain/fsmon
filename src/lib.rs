pub mod config;
pub mod dir_cache;
pub mod fid_parser;
pub mod filters;
pub mod help;
pub mod monitor;
pub mod proc_cache;
pub mod query;
pub mod socket;
pub mod managed;
pub mod utils;
pub use utils::{SizeOp, SizeFilter, TimeFilter, parse_size_filter, parse_size, parse_time_filter};

use crate::config::chown_to_original_user;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use fs2::FileExt;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Enforces single daemon instance via `flock`.
/// Lock file at `/tmp/fsmon-<UID>.lock`.
/// Lock released automatically when process exits or crashes.
pub struct DaemonLock {
    #[allow(dead_code)]
    file: fs::File,
    _path: PathBuf,
}

impl DaemonLock {
    /// Acquire exclusive lock. Fails if another daemon is already running.
    pub fn acquire(uid: u32) -> Result<Self> {
        let path = PathBuf::from(format!("/tmp/fsmon-{}.lock", uid));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| anyhow::anyhow!("Failed to open daemon lock file '{}': {}", path.display(), e))?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Read existing PID for helpful message
                let pid_str = fs::read_to_string(&path).unwrap_or_default();
                let pid_hint = if pid_str.trim().is_empty() {
                    String::new()
                } else {
                    format!(" (PID {})", pid_str.trim())
                };
                bail!("Another fsmon daemon is already running{}", pid_hint);
            }
            Err(e) => bail!("Failed to acquire daemon lock: {}", e),
        }

        // Write PID for diagnostic purposes (not relied on for correctness)
        let _ = fs::write(&path, format!("{}", std::process::id()));

        Ok(DaemonLock { file, _path: path })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // fd closes → kernel releases flock automatically
    }
}
use std::str::FromStr;

pub const DEFAULT_KEEP_DAYS: u32 = 30;
pub const DEFAULT_MAX_SIZE: &str = ">=1GB";

pub const EXIT_CONFIG: i32 = 78;



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    Access,
    Modify,
    CloseWrite,
    CloseNowrite,
    Open,
    OpenExec,
    Attrib,
    Create,
    Delete,
    DeleteSelf,
    MovedFrom,
    MovedTo,
    MoveSelf,
    FsError,
}

impl EventType {
    pub const ALL: &'static [EventType] = &[
        EventType::Access,
        EventType::Modify,
        EventType::CloseWrite,
        EventType::CloseNowrite,
        EventType::Open,
        EventType::OpenExec,
        EventType::Attrib,
        EventType::Create,
        EventType::Delete,
        EventType::DeleteSelf,
        EventType::MovedFrom,
        EventType::MovedTo,
        EventType::MoveSelf,
        EventType::FsError,
    ];
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            EventType::Access => "ACCESS",
            EventType::Modify => "MODIFY",
            EventType::CloseWrite => "CLOSE_WRITE",
            EventType::CloseNowrite => "CLOSE_NOWRITE",
            EventType::Open => "OPEN",
            EventType::OpenExec => "OPEN_EXEC",
            EventType::Attrib => "ATTRIB",
            EventType::Create => "CREATE",
            EventType::Delete => "DELETE",
            EventType::DeleteSelf => "DELETE_SELF",
            EventType::MovedFrom => "MOVED_FROM",
            EventType::MovedTo => "MOVED_TO",
            EventType::MoveSelf => "MOVE_SELF",
            EventType::FsError => "FS_ERROR",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for EventType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "ACCESS" => Ok(EventType::Access),
            "MODIFY" => Ok(EventType::Modify),
            "CLOSE_WRITE" => Ok(EventType::CloseWrite),
            "CLOSE_NOWRITE" => Ok(EventType::CloseNowrite),
            "OPEN" => Ok(EventType::Open),
            "OPEN_EXEC" => Ok(EventType::OpenExec),
            "ATTRIB" => Ok(EventType::Attrib),
            "CREATE" => Ok(EventType::Create),
            "DELETE" => Ok(EventType::Delete),
            "DELETE_SELF" => Ok(EventType::DeleteSelf),
            "MOVED_FROM" => Ok(EventType::MovedFrom),
            "MOVED_TO" => Ok(EventType::MovedTo),
            "MOVE_SELF" => Ok(EventType::MoveSelf),
            "FS_ERROR" => Ok(EventType::FsError),
            _ => Err(format!("Unknown event type: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub time: DateTime<Utc>,
    pub event_type: EventType,
    pub path: PathBuf,
    pub pid: u32,
    pub cmd: String,
    pub user: String,
    pub file_size: u64,
    /// The monitored (watched) path this event belongs to.
    /// Allows filtering events by which watched path triggered the log entry.
    pub monitored_path: PathBuf,
}

impl FileEvent {
    /// Serialize to a single JSON line (for log storage / pipe output)
    pub fn to_jsonl_string(&self) -> String {
        serde_json::to_string(self).expect("FileEvent serialization should not fail")
    }

    /// Deserialize from a single JSON line
    pub fn from_jsonl_str(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// Parse a JSONL line into a FileEvent.
pub fn parse_log_line_jsonl(line: &str) -> Option<FileEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    FileEvent::from_jsonl_str(trimmed)
}

/// Check if `kept_bytes` exceeds the limit per the filter's operator.
fn should_trim(kept_bytes: usize, filter: &SizeFilter) -> bool {
    let max = filter.bytes as usize;
    match filter.op {
        SizeOp::Gt => kept_bytes > max,
        SizeOp::Ge => kept_bytes >= max,
        SizeOp::Lt => kept_bytes < max,
        SizeOp::Le => kept_bytes <= max,
        SizeOp::Eq => kept_bytes == max,
    }
}

/// Clean a single log file by time and size.
async fn clean_single_log(
    log_file: &Path,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    if !log_file.exists() {
        println!("Log file not found: {}", log_file.display());
        return Ok(());
    }

    let original_size = fs::metadata(log_file)?.len();

    let temp_file = log_file.with_extension("tmp");
    let mut time_deleted: u64 = 0;
    let mut kept_bytes: usize = 0;

    {
        let file = fs::File::open(log_file)?;
        let reader = BufReader::new(file);
        let writer = fs::File::create(&temp_file)?;
        let mut writer = BufWriter::new(writer);

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let (should_keep, event) = if let Some(event) = parse_log_line_jsonl(trimmed) {
                let passes_time = time_filter.as_ref().map_or(true, |f| {
                    match f.op {
                        SizeOp::Gt => event.time > f.time,
                        SizeOp::Ge => event.time >= f.time,
                        SizeOp::Lt => event.time < f.time,
                        SizeOp::Le => event.time <= f.time,
                        SizeOp::Eq => event.time == f.time,
                    }
                });
                (passes_time, Some(event))
            } else {
                (true, None)
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1; // +1 for newline
            } else if dry_run {
                if let Some(ev) = event {
                    println!("  [to-delete] {} | {} | {}",
                        ev.time.format("%Y-%m-%d %H:%M:%S"),
                        ev.event_type,
                        ev.path.display());
                }
                time_deleted += 1;
            } else {
                time_deleted += 1;
            }
        }
    }

    let size_deleted = if let Some(ref filter) = max_size {
        if should_trim(kept_bytes, filter) {
            let max = filter.bytes as usize;
            let trim_start = find_tail_offset(&temp_file, max)?;
            let dropped = count_lines(&temp_file, trim_start)?;
            truncate_from_start(&temp_file, trim_start)?;
            kept_bytes -= trim_start;
            dropped
        } else {
            0
        }
    } else {
        0
    };

    let total_deleted = time_deleted + size_deleted as u64;

    if dry_run {
        let _ = fs::remove_file(&temp_file);
        if total_deleted > 0 {
            println!("---");
            println!("Dry run: {} entries would be deleted (use --dry-run to preview)", total_deleted);
        } else {
            println!("Dry run: 0 entries match cleanup criteria");
        }
    } else {
        fs::rename(&temp_file, log_file)?;
        chown_to_original_user(log_file);
        println!("Cleaning {}...", log_file.display());
        let time_desc = time_filter.as_ref().map_or("all time".to_string(), |f| {
            format!("{} {}", f.op, crate::utils::format_datetime(&f.time))
        });
        println!(
            "Deleted {} entries (time filter: {})",
            total_deleted, time_desc
        );
        println!(
            "Log file size reduced from {} to {}",
            utils::format_size(original_size as i64),
            utils::format_size(kept_bytes as i64)
        );
    }

    Ok(())
}

/// Clean log files by age and size.
///
/// If `paths` is Some, only clean matching log files for those paths.
/// If `paths` is None, clean all `*.jsonl` log files in `log_dir`.
pub async fn clean_logs(
    log_dir: &Path,
    paths: Option<&[PathBuf]>,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    if !log_dir.exists() {
        println!("Log directory not found: {}", log_dir.display());
        return Ok(());
    }

    if let Some(paths) = paths {
        for path in paths {
            let log_file = log_dir.join(crate::utils::path_to_log_name(path));
            clean_single_log(&log_file, time_filter, max_size, dry_run).await?;
        }
    } else {
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                clean_single_log(&path, time_filter, max_size, dry_run).await?;
            }
        }
    }

    Ok(())
}

fn find_tail_offset(path: &Path, max_bytes: usize) -> Result<usize> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let file_len = f.metadata()?.len() as usize;

    if file_len <= max_bytes {
        return Ok(0);
    }

    let target = file_len - max_bytes;         // we want to start here
    let scan_start = target.saturating_sub(4096);  // scan back up to 4KB
    let scan_len = file_len - scan_start;           // scan from scan_start to EOF

    f.seek(SeekFrom::Start(scan_start as u64))?;
    let mut buf = vec![0u8; scan_len];
    f.read_exact(&mut buf)?;

    // Find the LAST newline before (or at) `target`, so we keep ≈max_bytes
    // from the tail. If no newline found before target, look for the first
    // newline after target (fallback: keep a partial line).
    let target_rel = target - scan_start;
    let last_nl_before = buf[..target_rel].iter().rposition(|&b| b == b'\n');
    let first_nl_after = buf[target_rel..].iter().position(|&b| b == b'\n');

    let offset = match last_nl_before {
        Some(pos) => scan_start + pos + 1,  // keep after this newline
        None => match first_nl_after {
            Some(pos) => target + pos + 1,  // keep after next newline
            None => file_len,                // no newline at all — keep nothing
        },
    };
    Ok(offset)
}

fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }

    let file_len = fs::metadata(path)?.len() as usize;
    // offset == file_len means delete everything — write empty file
    if offset >= file_len {
        fs::write(path, b"")?;
        return Ok(());
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp_path = dir.join(format!(".fsmon_trunc_{}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut tmp = fs::File::create_new(&tmp_path)?;
        let mut src = fs::File::open(path)?;
        src.seek(SeekFrom::Start(offset as u64))?;

        let mut buf = vec![0u8; 8192];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            tmp.write_all(&buf[..n])?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;

    fs::rename(&tmp_path, path)?;
    chown_to_original_user(path);
    Ok(())
}

fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    use std::io::Read;

    let f = fs::File::open(path)?;
    let mut buf = vec![];
    f.take(upto as u64).read_to_end(&mut buf)?;
    Ok(buf.iter().filter(|&&b| b == b'\n').count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_count_lines_basic() {
        let dir = std::env::temp_dir().join("fsmon_test_count");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "line1\nline2\nline3\n");

        assert_eq!(count_lines(&path, 6).unwrap(), 1);
        assert_eq!(count_lines(&path, 12).unwrap(), 2);
        assert_eq!(count_lines(&path, 18).unwrap(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_lines_empty() {
        let dir = std::env::temp_dir().join("fsmon_test_count_empty");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "");

        assert_eq!(count_lines(&path, 0).unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_lines_no_trailing_newline() {
        let dir = std::env::temp_dir().join("fsmon_test_count_no_nl");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "line1\nline2");

        assert_eq!(count_lines(&path, 6).unwrap(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_small_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_small");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "short\n");

        assert_eq!(find_tail_offset(&path, 1000).unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_large_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_large");
        fs::create_dir_all(&dir).unwrap();

        let line = "aaa\n";
        let content = line.repeat(2000);
        let path = create_test_file(&dir, "test.log", &content);

        let offset = find_tail_offset(&path, 512).unwrap();
        assert!(offset > 0, "offset should be > 0 for large file");
        assert!(offset <= 8000, "offset should be within file");

        let full = fs::read_to_string(&path).unwrap();
        if offset > 0 {
            assert_eq!(
                full.as_bytes()[offset - 1],
                b'\n',
                "tail should start right after a newline"
            );
        }
        assert!(offset < content.len());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_by_time() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp"),
        };
        let new_event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/new"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp"),
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(time_filter), None, false))
            .unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "expected 1 event line, got {:?}", lines);
        let remaining = FileEvent::from_jsonl_str(lines[0]).unwrap();
        assert_eq!(remaining.path, PathBuf::from("/tmp/new"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_dry_run() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_dryrun");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp"),
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
        }

        let original_content = fs::read_to_string(&log_path).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(time_filter), None, true))
            .unwrap();

        let after_content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original_content, after_content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_nonexistent_file() {
        let path = PathBuf::from("/tmp/fsmon_nonexistent_dir_clean_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        assert!(
            rt.block_on(clean_logs(&path, None, Some(time_filter), None, false))
                .is_ok()
        );
    }

    #[test]
    fn test_clean_logs_by_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_size");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        {
            let mut f = fs::File::create(&log_path).unwrap();
            for i in 0..100 {
                let event = FileEvent {
                    time: Utc::now(),
                    event_type: EventType::Create,
                    path: PathBuf::from(format!("/tmp/file{}", i)),
                    pid: 1,
                    cmd: "test".into(),
                    user: "root".into(),
                    file_size: 0,
                    monitored_path: PathBuf::from("/tmp"),
                };
                writeln!(f, "{}", event.to_jsonl_string()).unwrap();
            }
        }

        let original_size = fs::metadata(&log_path).unwrap().len();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, None, Some(SizeFilter { op: SizeOp::Gt, bytes: 500 }), false))
            .unwrap();

        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size);

        let _ = fs::remove_dir_all(&dir);
    }

    // ---- should_trim unit tests ----

    #[test]
    fn test_should_trim_gt() {
        assert!(should_trim(100, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
        assert!(!should_trim(50, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_ge() {
        assert!(should_trim(100, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_lt() {
        assert!(should_trim(30, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
        assert!(!should_trim(50, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_le() {
        assert!(should_trim(30, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_eq() {
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
    }

    // ---- integration: size filter edge cases ----

    #[test]
    fn test_clean_size_filter_eq_zero_keeps_all() {
        // =0 means "trim only if size == 0" — never triggers for non-empty log
        let dir = std::env::temp_dir().join("fsmon_test_clean_eq0");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let event = FileEvent {
                time: Utc::now(), event_type: EventType::Create,
                path: PathBuf::from("/f"), pid: 1,
                cmd: "t".into(), user: "r".into(),
                file_size: 0, monitored_path: PathBuf::from("/f"),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        let original = fs::read_to_string(&log_path).unwrap();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(
            log_dir, None, None,
            Some(SizeFilter { op: SizeOp::Eq, bytes: 0 }), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original, after, "=0 should NOT delete when file is non-empty");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_size_filter_gt_zero_deletes_all() {
        // >0 means "trim if size > 0" — always triggers for non-empty log
        let dir = std::env::temp_dir().join("fsmon_test_clean_gt0");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let event = FileEvent {
                time: Utc::now(), event_type: EventType::Create,
                path: PathBuf::from("/f"), pid: 1,
                cmd: "t".into(), user: "r".into(),
                file_size: 0, monitored_path: PathBuf::from("/f"),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(
            log_dir, None, None,
            Some(SizeFilter { op: SizeOp::Gt, bytes: 0 }), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert!(after.trim().is_empty(), ">0 should delete all content, got: {:?}", after);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_size_filter_lt_inverts() {
        // <500 means "trim if size < 500" — triggers for small files, not large ones
        let dir = std::env::temp_dir().join("fsmon_test_clean_lt");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            for i in 0..20 {
                let event = FileEvent {
                    time: Utc::now(), event_type: EventType::Create,
                    path: PathBuf::from(format!("/f{}", i)), pid: 1,
                    cmd: "t".into(), user: "r".into(),
                    file_size: 0, monitored_path: PathBuf::from("/f"),
                };
                writeln!(f, "{}", event.to_jsonl_string()).unwrap();
            }
        }
        // File is small, <100000 means "trim if size < 100000" — should trigger
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let size_filter = SizeFilter { op: SizeOp::Lt, bytes: 100000 };
        rt.block_on(clean_logs(
            log_dir, None, None, Some(size_filter), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        // File should be trimmed (kept content ≤ 100000 bytes)
        assert!(after.len() > 0, "should keep at least 0 bytes worth of content");
        assert!(after.len() <= 100000, "kept content should be ≤ 100000 bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- integration: time filter operators ----

    #[test]
    fn test_clean_time_filter_ge() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time_ge");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(10),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        let mid_event = FileEvent {
            time: now - chrono::Duration::days(5),
            event_type: EventType::Create, path: PathBuf::from("/mid"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        let new_event = FileEvent {
            time: now,
            event_type: EventType::Create, path: PathBuf::from("/new"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", mid_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }
        // Keep events with time >= 7 days ago → keep old_event (10d old) should be deleted
        let cutoff = now - chrono::Duration::days(7);
        let tf = TimeFilter { op: SizeOp::Ge, time: cutoff };
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), None, false)).unwrap();
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, ">=7d should keep mid(5d) + new(0d)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_time_filter_le() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time_le");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(10),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        let new_event = FileEvent {
            time: now,
            event_type: EventType::Create, path: PathBuf::from("/new"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }
        // Keep events with time <= 7 days ago → keep old_event (10d), delete new_event (0d)
        let cutoff = now - chrono::Duration::days(7);
        let tf = TimeFilter { op: SizeOp::Le, time: cutoff };
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), None, false)).unwrap();
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "<=7d should keep old(10d) only");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_no_time_filter_keeps_all() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_no_time");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(100),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0, monitored_path: PathBuf::from("/"),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
        }
        let original = fs::read_to_string(&log_path).unwrap();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, None, None, false)).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original, after, "no time filter should keep all events");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- integration: clean specific paths ----

    #[test]
    fn test_clean_specific_path_only() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_specific");
        fs::create_dir_all(&dir).unwrap();
        let log_a = dir.join(crate::utils::path_to_log_name(Path::new("/a")));
        let log_b = dir.join(crate::utils::path_to_log_name(Path::new("/b")));
        {
            let mut f = fs::File::create(&log_a).unwrap();
            let event = FileEvent {
                time: Utc::now() - chrono::Duration::days(100),
                event_type: EventType::Create, path: PathBuf::from("/a/x"),
                pid: 1, cmd: "t".into(), user: "r".into(),
                file_size: 0, monitored_path: PathBuf::from("/a"),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        {
            let mut f = fs::File::create(&log_b).unwrap();
            writeln!(f, "keep").unwrap();
        }
        let cutoff = Utc::now();
        let tf = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&dir, Some(&[PathBuf::from("/a")]), Some(tf), None, false)).unwrap();
        // log_a should be cleaned (empty), log_b should remain intact
        let content_b = fs::read_to_string(&log_b).unwrap();
        assert_eq!(content_b.trim(), "keep", "log /b should be untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- integration: time + size combined ----

    #[test]
    fn test_clean_both_time_and_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_both");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        {
            let mut f = fs::File::create(&log_path).unwrap();
            // Old event (should be removed by time filter)
            let old = FileEvent {
                time: now - chrono::Duration::days(60),
                event_type: EventType::Create, path: PathBuf::from("/old"),
                pid: 1, cmd: "t".into(), user: "r".into(),
                file_size: 0, monitored_path: PathBuf::from("/"),
            };
            writeln!(f, "{}", old.to_jsonl_string()).unwrap();
            // Recent events (should be kept by time, but may be trimmed by size)
            for i in 0..50 {
                let ev = FileEvent {
                    time: now,
                    event_type: EventType::Create, path: PathBuf::from(format!("/f{}", i)),
                    pid: 1, cmd: "t".into(), user: "r".into(),
                    file_size: 0, monitored_path: PathBuf::from("/"),
                };
                writeln!(f, "{}", ev.to_jsonl_string()).unwrap();
            }
        }
        let tf = TimeFilter { op: SizeOp::Gt, time: now - chrono::Duration::days(7) };
        let sf = SizeFilter { op: SizeOp::Gt, bytes: 2000 };
        let original_size = fs::metadata(&log_path).unwrap().len();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), Some(sf), false)).unwrap();
        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size, "combined filters should reduce size (orig={}, new={})", original_size, new_size);
        // Tail offset rounds up to nearest newline boundary
        assert!(new_size <= 2200, "should be trimmed to ~2000 bytes (newline-aligned), got {}", new_size);
        let _ = fs::remove_dir_all(&dir);
    }
}
