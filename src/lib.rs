pub mod config;
pub mod dir_cache;
pub mod fid_parser;
pub mod help;
pub mod monitor;
pub mod proc_cache;
pub mod query;
pub mod socket;
pub mod managed;
pub mod utils;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const DEFAULT_KEEP_DAYS: u32 = 30;

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

/// Clean a single log file by age and size.
async fn clean_single_log(
    log_file: &Path,
    keep_days: u32,
    max_size: Option<i64>,
    dry_run: bool,
) -> Result<()> {
    if !log_file.exists() {
        println!("Log file not found: {}", log_file.display());
        return Ok(());
    }

    let cutoff_time = Utc::now() - chrono::Duration::days(keep_days as i64);
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

            let should_keep = if let Some(event) = parse_log_line_jsonl(trimmed) {
                event.time >= cutoff_time
            } else {
                true
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1; // +1 for newline
            } else {
                time_deleted += 1;
            }
        }
    }

    let max_bytes = max_size.unwrap_or(i64::MAX) as usize;
    let size_deleted = if kept_bytes > max_bytes {
        let trim_start = find_tail_offset(&temp_file, max_bytes)?;
        let dropped = count_lines(&temp_file, trim_start)?;
        truncate_from_start(&temp_file, trim_start)?;
        kept_bytes -= trim_start;
        dropped
    } else {
        0
    };

    let total_deleted = time_deleted + size_deleted as u64;

    if dry_run {
        let _ = fs::remove_file(&temp_file);
        println!("Dry run: Would delete {} entries", total_deleted);
        println!("No changes made (--dry-run enabled)");
    } else {
        fs::rename(&temp_file, log_file)?;
        println!("Cleaning {}...", log_file.display());
        println!(
            "Deleted {} entries (logs older than {} days)",
            total_deleted, keep_days
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
    keep_days: u32,
    max_size: Option<i64>,
    dry_run: bool,
) -> Result<()> {
    if !log_dir.exists() {
        println!("Log directory not found: {}", log_dir.display());
        return Ok(());
    }

    if let Some(paths) = paths {
        for path in paths {
            let log_file = log_dir.join(crate::utils::path_to_log_name(path));
            clean_single_log(&log_file, keep_days, max_size, dry_run).await?;
        }
    } else {
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                clean_single_log(&path, keep_days, max_size, dry_run).await?;
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

    let read_start = (file_len - max_bytes).saturating_sub(4096);
    f.seek(SeekFrom::Start(read_start as u64))?;

    let mut buf = vec![0u8; file_len - read_start];
    f.read_exact(&mut buf)?;

    let first_newline = buf
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    Ok(read_start + first_newline)
}

fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }

    let file_len = fs::metadata(path)?.len() as usize;
    if offset >= file_len {
        bail!("offset {} >= file size {}", offset, file_len);
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

        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, 30, None, false))
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

        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, 30, None, true))
            .unwrap();

        let after_content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original_content, after_content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_nonexistent_file() {
        let path = PathBuf::from("/tmp/fsmon_nonexistent_dir_clean_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(
            rt.block_on(clean_logs(&path, None, 30, None, false))
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
        rt.block_on(clean_logs(log_dir, None, 0, Some(500), false))
            .unwrap();

        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size);

        let _ = fs::remove_dir_all(&dir);
    }
}
