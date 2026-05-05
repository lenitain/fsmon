pub mod config;
pub mod dir_cache;
pub mod fid_parser;
pub mod help;
pub mod monitor;
pub mod output;
pub mod proc_cache;
pub mod query;
pub mod socket;
pub mod store;
pub mod systemd;
pub mod utils;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const DEFAULT_KEEP_DAYS: u32 = 30;
pub const TOML_SEPARATOR: &str = "\n\n";
pub const EXIT_CONFIG: i32 = 78;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Human,
    /// Alias for TOML output format
    #[clap(name = "json")]
    Toml,
    Csv,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SortBy {
    Time,
    Size,
    Pid,
}

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
    pub size_change: i64,
}

impl FileEvent {
    pub fn to_human_string(&self) -> String {
        let time_str = utils::format_datetime(&self.time);
        let size_str = utils::format_size(self.size_change);
        let size_prefix = if self.size_change >= 0 { "+" } else { "" };
        format!(
            "[{}] [{}] {} (PID: {}, CMD: {}, USER: {}, SIZE: {}{})",
            time_str,
            self.event_type,
            self.path.display(),
            self.pid,
            self.cmd,
            self.user,
            size_prefix,
            size_str
        )
    }

    pub fn to_toml_string(&self) -> String {
        format!(
            r#"time = "{}"
event_type = "{}"
path = "{}"
pid = {}
cmd = "{}"
user = "{}"
size_change = {}
"#,
            self.time.to_rfc3339(),
            self.event_type,
            self.path
                .display()
                .to_string()
                .replace('\\', "\\\\")
                .replace('"', "\\\""),
            self.pid,
            self.cmd.replace('\\', "\\\\").replace('"', "\\\""),
            self.user.replace('\\', "\\\\").replace('"', "\\\""),
            self.size_change,
        )
    }

    pub fn from_toml_str(s: &str) -> Option<Self> {
        // Parse a TOML document into a FileEvent.
        // Accepts both inline and multi-line TOML.
        let value: toml::Value = s.parse().ok()?;
        let table = value.as_table()?;

        let time_str = table.get("time")?.as_str()?;
        let time = DateTime::parse_from_rfc3339(time_str)
            .ok()?
            .with_timezone(&Utc);

        let event_type_str = table.get("event_type")?.as_str()?;
        let event_type: EventType = event_type_str.parse().ok()?;

        let path_str = table.get("path")?.as_str()?;
        let path = PathBuf::from(path_str);

        let pid = table.get("pid")?.as_integer()? as u32;
        let cmd = table.get("cmd")?.as_str()?.to_string();
        let user = table.get("user")?.as_str()?.to_string();
        let size_change = table.get("size_change")?.as_integer()?;

        Some(FileEvent {
            time,
            event_type,
            path,
            pid,
            cmd,
            user,
            size_change,
        })
    }

    pub fn to_csv_string(&self) -> String {
        use csv::WriterBuilder;
        let mut wtr = WriterBuilder::new().has_headers(false).from_writer(vec![]);
        wtr.write_record([
            self.time.to_rfc3339(),
            self.event_type.to_string(),
            self.path.display().to_string(),
            self.pid.to_string(),
            self.cmd.clone(),
            self.user.clone(),
            self.size_change.to_string(),
        ])
        .expect("csv write failed");
        String::from_utf8(wtr.into_inner().expect("csv flush failed"))
            .expect("csv not utf8")
            .trim()
            .to_string()
    }

    pub fn from_csv_str(s: &str) -> Option<Self> {
        use csv::ReaderBuilder;
        let mut rdr = ReaderBuilder::new()
            .has_headers(false)
            .from_reader(s.as_bytes());
        let record = rdr.records().next()?.ok()?;
        if record.len() < 7 {
            return None;
        }
        let time = DateTime::parse_from_rfc3339(&record[0])
            .ok()?
            .with_timezone(&Utc);
        let event_type: EventType = record[1].parse().ok()?;
        let path = PathBuf::from(&record[2]);
        let pid: u32 = record[3].parse().ok()?;
        let cmd = record[4].to_string();
        let user = record[5].to_string();
        let size_change: i64 = record[6].parse().ok()?;
        Some(FileEvent {
            time,
            event_type,
            path,
            pid,
            cmd,
            user,
            size_change,
        })
    }
}

/// Parse a log line (or multi-line block) into a FileEvent.
/// Supports TOML format (multi-line, separated by blank lines) and CSV (single-line).
pub fn parse_log_line(line: &str) -> Option<FileEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Try TOML first, fall back to CSV
    FileEvent::from_toml_str(trimmed).or_else(|| FileEvent::from_csv_str(trimmed))
}

/// Read the next TOML event block from the reader.
/// Each event is a TOML document (multi-line) separated by blank lines.
/// Returns the block content (trimmed) or None at EOF.
fn read_toml_block(reader: &mut BufReader<fs::File>) -> Result<Option<String>> {
    let mut block = String::new();
    let mut found_start = false;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            // EOF
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank line = end of current block
            if found_start {
                break;
            }
            // Skip leading blank lines
            continue;
        }

        found_start = true;
        block.push_str(&line);
    }

    if block.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(block))
    }
}

pub async fn clean_logs(
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
    let mut time_deleted = 0;
    let mut kept_bytes: usize = 0;

    {
        let file = fs::File::open(log_file)?;
        let mut reader = BufReader::new(file);
        let writer = fs::File::create(&temp_file)?;
        let mut writer = BufWriter::new(writer);

        loop {
            let block = read_toml_block(&mut reader)?;
            match block {
                Some(content) => {
                    let should_keep = if let Some(event) = parse_log_line(&content) {
                        event.time >= cutoff_time
                    } else {
                        true
                    };

                    if should_keep {
                        write!(writer, "{}", content)?;
                        if !content.ends_with('\n') {
                            writeln!(writer)?;
                        }
                        // Blank line separator between events
                        writeln!(writer)?;
                        kept_bytes += content.len() + 2; // +2 for blank line
                    } else {
                        time_deleted += 1;
                    }
                }
                None => break,
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

    let total_deleted = time_deleted + size_deleted;

    if dry_run {
        let _ = fs::remove_file(temp_file);
        println!("Dry run: Would delete {} blocks", total_deleted);
        println!("No changes made (--dry-run enabled)");
    } else {
        fs::rename(&temp_file, log_file)?;
        println!("Cleaning {}...", log_file.display());
        println!(
            "Deleted {} blocks (logs older than {} days)",
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
    let tmp_path = dir.join(".fsmon_trunc_tmp");

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

    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; upto];
    f.read_exact(&mut buf)?;
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
        let log_path = dir.join("test.log");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let new_event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/new"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            write!(f, "{}", old_event.to_toml_string()).unwrap();
            writeln!(f).unwrap(); // blank line separator
            write!(f, "{}", new_event.to_toml_string()).unwrap();
            writeln!(f).unwrap(); // trailing blank line
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&log_path, 30, None, false)).unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        // Parse by TOML blocks, expect one event
        let blocks: Vec<&str> = content
            .split("\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(blocks.len(), 1, "expected 1 event block, got {:?}", blocks);
        let remaining = FileEvent::from_toml_str(blocks[0]).unwrap();
        assert_eq!(remaining.path, PathBuf::from("/tmp/new"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_dry_run() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_dryrun");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            write!(f, "{}", old_event.to_toml_string()).unwrap();
            writeln!(f).unwrap(); // trailing blank line
        }

        let original_content = fs::read_to_string(&log_path).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&log_path, 30, None, true)).unwrap();

        let after_content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original_content, after_content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_nonexistent_file() {
        let path = PathBuf::from("/tmp/fsmon_nonexistent_test.log");
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(clean_logs(&path, 30, None, false)).is_ok());
    }

    #[test]
    fn test_clean_logs_by_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_size");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

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
                    size_change: 0,
                };
                write!(f, "{}", event.to_toml_string()).unwrap();
                writeln!(f).unwrap(); // blank line separator
            }
        }

        let original_size = fs::metadata(&log_path).unwrap().len();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&log_path, 0, Some(500), false))
            .unwrap();

        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size);

        let _ = fs::remove_dir_all(&dir);
    }
}
