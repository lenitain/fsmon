use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;

mod config;
mod dir_cache;
mod fid_parser;
mod help;
mod monitor;
mod output;
mod proc_cache;
mod query;
mod systemd;
mod utils;

use help::HelpTopic;

const DEFAULT_LOG_PATH: &str = ".fsmon/history.log";
const DEFAULT_KEEP_DAYS: u32 = 30;

use config::Config;
use monitor::Monitor;
use query::Query;
use utils::{format_datetime, format_size, parse_size};

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = help::about(HelpTopic::Root), long_about = None)]
#[command(after_help = help::after_help())]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = help::about(HelpTopic::Monitor), long_about = help::long_about(HelpTopic::Monitor))]
    Monitor {
        /// Directory/file path to monitor (supports multiple)
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Only record events with size change >= specified value (e.g., 100MB, 1GB, 1048576)
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,

        /// Only monitor specified operation types, comma-separated
        /// (ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC,
        ///  ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, FS_ERROR)
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,

        /// Paths to exclude from monitoring (supports wildcards, e.g., "*.log", "/tmp/*")
        #[arg(short, long, value_name = "PATTERN")]
        exclude: Option<String>,

        /// Capture all 14 fanotify events (default only captures 8 change events)
        #[arg(long)]
        all_events: bool,

        /// Write monitoring log to specified file (append mode)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Output format (human, json, csv)
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,

        /// Recursively monitor all subdirectories
        #[arg(short, long)]
        recursive: bool,
    },

    #[command(about = help::about(HelpTopic::Query), long_about = help::long_about(HelpTopic::Query))]
    Query {
        /// Log file path to query (default: ~/.fsmon/history.log)
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,

        /// Start time: relative (1h, 30m, 7d) or absolute ("2024-05-01 10:00")
        #[arg(short = 'S', long, value_name = "TIME")]
        since: Option<String>,

        /// End time: relative (1h, 30m, 7d) or absolute ("2024-05-01 12:00")
        #[arg(short = 'U', long, value_name = "TIME")]
        until: Option<String>,

        /// Only query events for specified PIDs (multiple supported, comma-separated: 1234,5678)
        #[arg(short, long, value_name = "PIDS")]
        pid: Option<String>,

        /// Only query events for specified process name (supports wildcards: nginx*, python)
        #[arg(short, long, value_name = "PATTERN")]
        cmd: Option<String>,

        /// Only query events for specified users (multiple supported, comma-separated: root,admin)
        #[arg(short, long, value_name = "USERS")]
        user: Option<String>,

        /// Only query specified operation types
        /// (ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC,
        ///  ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, FS_ERROR)
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,

        /// Only query events with size change >= specified value (e.g., 100MB, 1GB)
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,

        /// Output format (human, json, csv)
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,

        /// Sort by (time, size, pid)
        #[arg(short = 'r', long, value_enum)]
        sort: Option<SortBy>,
    },

    #[command(about = help::about(HelpTopic::Status), long_about = help::long_about(HelpTopic::Status))]
    Status,

    #[command(about = help::about(HelpTopic::Stop), long_about = help::long_about(HelpTopic::Stop))]
    Stop,

    #[command(about = help::about(HelpTopic::Start), long_about = help::long_about(HelpTopic::Start))]
    Start,

    #[command(about = help::about(HelpTopic::Install), long_about = help::long_about(HelpTopic::Install))]
    Install {
        /// Directory/file path to monitor (supports multiple)
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Write monitoring log to specified file
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Force overwrite existing service file
        #[arg(short, long)]
        force: bool,
    },

    #[command(about = help::about(HelpTopic::Uninstall), long_about = help::long_about(HelpTopic::Uninstall))]
    Uninstall,

    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean {
        /// Log file path to clean (default: ~/.fsmon/history.log)
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,

        /// Keep logs from last N days (default: 30 days)
        #[arg(short, long, value_name = "DAYS")]
        keep_days: Option<u32>,

        /// Maximum log file size (e.g., 100MB, 1GB)
        #[arg(short, long, value_name = "SIZE")]
        max_size: Option<String>,

        /// Simulate cleanup, show what would be deleted without actually deleting
        #[arg(short, long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
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
        let time_str = format_datetime(&self.time);
        let size_str = format_size(self.size_change);
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

pub fn parse_log_line(line: &str) -> Option<FileEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') {
        serde_json::from_str(trimmed).ok()
    } else {
        FileEvent::from_csv_str(trimmed)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        Commands::Monitor {
            paths,
            min_size,
            types,
            exclude,
            all_events,
            output,
            format,
            recursive,
        } => {
            let config = config.monitor.unwrap_or_default();

            let paths = if paths.is_empty() {
                config.paths.unwrap_or_default()
            } else {
                paths
            };

            if paths.is_empty() {
                eprintln!("Error: Please specify at least one monitor path");
                process::exit(1);
            }

            let min_size = min_size.or(config.min_size);
            let types = types.or(config.types);
            let exclude = exclude.or(config.exclude);
            let all_events = all_events || config.all_events.unwrap_or(false);
            let output = output.or(config.output);
            let recursive = recursive || config.recursive.unwrap_or(false);
            let buffer_size = config.buffer_size;
            let format = format
                .or(config.format.as_deref().and_then(parse_output_format))
                .unwrap_or(OutputFormat::Human);

            let min_size_bytes = min_size.map(|s| parse_size(&s)).transpose()?;

            let event_types = types
                .map(|t| {
                    t.split(',')
                        .map(|s| {
                            s.trim()
                                .parse::<EventType>()
                                .map_err(|e| anyhow::anyhow!(e))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;

            let monitor = Monitor::new(
                paths,
                min_size_bytes,
                event_types,
                exclude,
                output,
                format,
                recursive,
                all_events,
                buffer_size,
            );

            monitor.run().await?;
        }
        Commands::Query {
            log_file,
            since,
            until,
            pid,
            cmd,
            user,
            types,
            min_size,
            format,
            sort,
        } => {
            let config = config.query.unwrap_or_default();

            let log_file = log_file.or(config.log_file).unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(DEFAULT_LOG_PATH))
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_LOG_PATH))
            });

            let since = since.or(config.since);
            let until = until.or(config.until);
            let pid = pid.or(config.pid);
            let cmd = cmd.or(config.cmd);
            let user = user.or(config.user);
            let types = types.or(config.types);
            let min_size = min_size.or(config.min_size);
            let format = format
                .or(config.format.as_deref().and_then(parse_output_format))
                .unwrap_or(OutputFormat::Human);
            let sort = sort
                .or(config.sort.as_deref().and_then(parse_sort_by))
                .unwrap_or(SortBy::Time);

            let min_size_bytes = min_size.map(|s| parse_size(&s)).transpose()?;

            let pids = pid.map(|p| {
                p.split(',')
                    .filter_map(|s| s.trim().parse::<u32>().ok())
                    .collect::<Vec<_>>()
            });

            let users = user.map(|u| {
                u.split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });

            let event_types = types
                .map(|t| {
                    t.split(',')
                        .map(|s| {
                            s.trim()
                                .parse::<EventType>()
                                .map_err(|e| anyhow::anyhow!(e))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;

            let query = Query::new(
                log_file,
                since,
                until,
                pids,
                cmd,
                users,
                event_types,
                min_size_bytes,
                format,
                sort,
            );

            query.execute().await?;
        }
        Commands::Status => {
            let status = systemd::status()?;
            println!("fsmon service: {}", status);
        }
        Commands::Stop => {
            systemd::stop()?;
        }
        Commands::Start => {
            systemd::start()?;
        }
        Commands::Install {
            paths,
            output,
            force,
        } => {
            if paths.is_empty() {
                eprintln!("Error: Please specify at least one monitor path");
                process::exit(1);
            }
            systemd::install(&paths, output.as_ref(), force)?;
        }
        Commands::Uninstall => {
            systemd::uninstall()?;
        }
        Commands::Clean {
            log_file,
            keep_days,
            max_size,
            dry_run,
        } => {
            let config = config.clean.unwrap_or_default();

            let log_file = log_file.or(config.log_file).unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(DEFAULT_LOG_PATH))
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_LOG_PATH))
            });

            let keep_days = keep_days.or(config.keep_days).unwrap_or(DEFAULT_KEEP_DAYS);

            let max_size = max_size.or(config.max_size);

            let max_size_bytes = max_size.map(|s| parse_size(&s)).transpose()?;

            clean_logs(&log_file, keep_days, max_size_bytes, dry_run).await?;
        }
    }

    Ok(())
}

fn parse_output_format(s: &str) -> Option<OutputFormat> {
    match s.to_lowercase().as_str() {
        "human" => Some(OutputFormat::Human),
        "json" => Some(OutputFormat::Json),
        "csv" => Some(OutputFormat::Csv),
        _ => None,
    }
}

fn parse_sort_by(s: &str) -> Option<SortBy> {
    match s.to_lowercase().as_str() {
        "time" => Some(SortBy::Time),
        "size" => Some(SortBy::Size),
        "pid" => Some(SortBy::Pid),
        _ => None,
    }
}

async fn clean_logs(
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

    // Pass 1: Stream filter by time, write to temp file
    let temp_file = log_file.with_extension("tmp");
    let mut time_deleted = 0;
    let mut kept_bytes: usize = 0;

    {
        let file = fs::File::open(log_file)?;
        let reader = BufReader::new(file);
        let writer = fs::File::create(&temp_file)?;
        let mut writer = BufWriter::new(writer);

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let should_keep = if let Some(event) = parse_log_line(&line) {
                event.time >= cutoff_time
            } else {
                true
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1;
            } else {
                time_deleted += 1;
            }
        }
    }

    // Pass 2: Truncate from tail if exceeds max_size
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
        println!("Dry run: Would delete {} lines", total_deleted);
        println!("No changes made (--dry-run enabled)");
    } else {
        fs::rename(&temp_file, log_file)?;
        println!("Cleaning {}...", log_file.display());
        println!(
            "Deleted {} lines (logs older than {} days)",
            total_deleted, keep_days
        );
        println!(
            "Log file size reduced from {} to {}",
            format_size(original_size as i64),
            format_size(kept_bytes as i64)
        );
    }

    Ok(())
}

/// Find byte offset from file end that contains at most max_bytes
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

/// Keep only bytes from offset to end, streaming via temp file
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

/// Count lines in first `upto` bytes of file
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

        assert_eq!(count_lines(&path, 6).unwrap(), 1); // "line1\n"
        assert_eq!(count_lines(&path, 12).unwrap(), 2); // "line1\nline2\n"
        assert_eq!(count_lines(&path, 18).unwrap(), 3); // all

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

        // Read only the first part (line1\n) - 6 bytes
        assert_eq!(count_lines(&path, 6).unwrap(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_small_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_small");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "short\n");

        // File smaller than max_bytes, offset should be 0
        assert_eq!(find_tail_offset(&path, 1000).unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_large_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_large");
        fs::create_dir_all(&dir).unwrap();

        // Create a file with known content - make it large enough
        // so that the 4096 subtraction doesn't bring read_start to 0
        let line = "aaa\n";
        let content = line.repeat(2000); // 8000 bytes
        let path = create_test_file(&dir, "test.log", &content);

        // max_bytes = 512, file is 8000 bytes
        // read_start = (8000 - 512).saturating_sub(4096) = 3392
        // Should find a newline at or after offset 3392
        let offset = find_tail_offset(&path, 512).unwrap();
        assert!(offset > 0, "offset should be > 0 for large file");
        assert!(offset <= 8000, "offset should be within file");

        // Verify the tail starts at a newline boundary
        let full = fs::read_to_string(&path).unwrap();
        if offset > 0 {
            assert_eq!(
                full.as_bytes()[offset - 1],
                b'\n',
                "tail should start right after a newline"
            );
        }
        // Verify tail is non-empty and within expected bounds
        assert!(offset < content.len());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_by_time() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        // Create events: one old, one new
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
            writeln!(f, "{}", serde_json::to_string(&old_event).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&new_event).unwrap()).unwrap();
        }

        // Clean: keep 30 days
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&log_path, 30, None, false)).unwrap();

        // Verify only new event remains
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1);
        let remaining: FileEvent = serde_json::from_str(lines[0]).unwrap();
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
            writeln!(f, "{}", serde_json::to_string(&old_event).unwrap()).unwrap();
        }

        let original_content = fs::read_to_string(&log_path).unwrap();

        // Dry run should not modify file
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
        // Should not error on missing file
        assert!(rt.block_on(clean_logs(&path, 30, None, false)).is_ok());
    }

    #[test]
    fn test_clean_logs_by_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_size");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        // Create multiple events to exceed size limit
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
                writeln!(f, "{}", serde_json::to_string(&event).unwrap()).unwrap();
            }
        }

        let original_size = fs::metadata(&log_path).unwrap().len();

        // Clean with small max_size to force truncation
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&log_path, 0, Some(500), false))
            .unwrap();

        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size);

        let _ = fs::remove_dir_all(&dir);
    }
}
