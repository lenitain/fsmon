use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process;

mod monitor;
mod query;
mod daemon;
mod utils;
mod proc_cache;

use monitor::Monitor;
use query::Query;
use daemon::{Daemon, DaemonStatus};
use utils::{parse_size, format_size, format_datetime};

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = "0.1.0")]
#[command(about = "Lightweight high-performance file change tracking tool", long_about = None)]
#[command(
    after_help = "Use 'fsmon <COMMAND> --help' for detailed command info\n\nExamples:\n  fsmon monitor /var/log                     # Basic monitoring\n  fsmon monitor /etc --types MODIFY         # Investigate config file changes\n  fsmon monitor / --all-events               # Enable all 14 event types\n  fsmon monitor ~/project --recursive       # Recursively monitor project\n  fsmon monitor / --daemon -o /var/log/fsmon-audit.log  # Daemon mode\n  fsmon query --since 1h --cmd nginx         # Query nginx operations in last hour\n  fsmon status                               # Check daemon status"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Real-time file change monitoring", long_about = LONG_ABOUT_MONITOR)]
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
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,

        /// Run as background daemon (must be used with --output)
        #[arg(short, long)]
        daemon: bool,

        /// Recursively monitor all subdirectories
        #[arg(short, long)]
        recursive: bool,
    },

    #[command(about = "Query historical monitoring logs", long_about = LONG_ABOUT_QUERY)]
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
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,

        /// Sort by (time, size, pid)
        #[arg(short = 'r', long, value_enum, default_value = "time")]
        sort: SortBy,
    },

    #[command(about = "Check daemon running status", long_about = LONG_ABOUT_STATUS)]
    Status {
        /// Output format (human, json, csv)
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,
    },

    #[command(about = "Stop background daemon", long_about = LONG_ABOUT_STOP)]
    Stop {
        /// Force terminate (send SIGKILL, otherwise send SIGTERM)
        #[arg(long)]
        force: bool,
    },

    #[command(about = "Clean historical logs", long_about = LONG_ABOUT_CLEAN)]
    Clean {
        /// Log file path to clean (default: ~/.fsmon/history.log)
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,

        /// Keep logs from last N days (default: 30 days)
        #[arg(short, long, value_name = "DAYS", default_value = "30")]
        keep_days: u32,

        /// Maximum log file size (e.g., 100MB, 1GB)
        #[arg(short, long, value_name = "SIZE")]
        max_size: Option<String>,

        /// Simulate cleanup, show what would be deleted without actually deleting
        #[arg(short, long)]
        dry_run: bool,
    },
}

const LONG_ABOUT_MONITOR: &str = r#"Monitor filesystem events on specified paths, output fanotify raw events in real-time.

[Event Types]
  Default: 8 core change events (CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF)
  --all-events: Enable all 14 fanotify events (includes ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR)

[Daemon Mode]
  --daemon runs in background, must be used with --output
  fsmon status/stop to check status and stop

[Examples]
  fsmon monitor /etc --types MODIFY          # Investigate config file changes
  fsmon monitor / --all-events               # Enable all 14 event types
  fsmon monitor ~/project --recursive        # Recursively monitor project directory
  fsmon monitor /tmp --min-size 100MB        # Track large file creation
  fsmon monitor /var/log --format json       # JSON format output
  fsmon monitor / --daemon -o /var/log/fsmon-audit.log  # Daemon long-term monitoring"#;

const LONG_ABOUT_QUERY: &str = r#"Query historical file change events from log files, supports multiple filter conditions and sorting.

[Time Filtering]
  --since   Start time: relative (1h, 30m, 7d) or absolute ("2024-05-01 10:00")
  --until   End time
  
[Process Filtering]
  --pid     Filter by process ID (multiple supported: 1234,5678)
  --cmd     Filter by process name (wildcard support: nginx*, python)
  --user    Filter by username (multiple supported: root,admin)

[Event Filtering]
  --types     Filter by event type (ACCESS,MODIFY,CREATE,DELETE,...)
  --min-size  Filter by size change (e.g., 100MB, 1GB)

[Examples]
  fsmon query                              # Query default log (~/.fsmon/history.log)
  fsmon query --since 1h                   # Last 1 hour
  fsmon query --cmd nginx                  # Only nginx operations
  fsmon query --since 1h --cmd java --types MODIFY --min-size 100MB  # Combined filters
  fsmon query --format json --sort size    # JSON output, sorted by size"#;

const LONG_ABOUT_STATUS: &str = r#"Check fsmon daemon running status.

[Output Content]
  - Running status (Running/Stopped)
  - Process ID (PID)
  - Monitored paths
  - Log file path
  - Start time
  - Event count
  - Memory usage

[Examples]
  fsmon status                 # Human-readable format
  fsmon status --format json  # JSON format (for monitoring system integration)"#;

const LONG_ABOUT_STOP: &str = r#"Stop fsmon daemon.

[Stop Method]
  Default: Send SIGTERM signal, graceful stop
  --force: Send SIGKILL signal, force stop

[Examples]
  fsmon stop        # Graceful stop
  fsmon stop --force  # Force stop (only when unresponsive)"#;

const LONG_ABOUT_CLEAN: &str = r#"Clean historical log files, retain by time or size.

[Cleanup Strategy]
  --keep-days   Keep logs from last N days (default: 30 days)
  --max-size    Limit maximum log file size (e.g., 100MB, 1GB)
  --dry-run     Preview mode, don't actually delete

[Examples]
  fsmon clean --keep-days 7           # Keep 7 days of logs
  fsmon clean --max-size 100MB        # Limit logs to 100MB
  fsmon clean --keep-days 7 --dry-run # Preview without deleting"#;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub time: DateTime<Utc>,
    pub event_type: String,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Monitor {
            paths,
            min_size,
            types,
            exclude,
            all_events,
            output,
            format,
            daemon,
            recursive,
        } => {
            if paths.is_empty() {
                eprintln!("Error: Please specify at least one monitor path");
                process::exit(1);
            }

            let min_size_bytes = min_size
                .map(|s| parse_size(&s))
                .transpose()?;

            let event_types = types.map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_uppercase())
                    .collect::<Vec<_>>()
            });

            let monitor = Monitor::new(
                paths,
                min_size_bytes,
                event_types,
                exclude,
                output,
                format,
                recursive,
                all_events,
            );

            if daemon {
                monitor.run_daemon().await?;
            } else {
                monitor.run().await?;
            }
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
            let log_file = log_file.unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(".fsmon").join("history.log"))
                    .unwrap_or_else(|| PathBuf::from("history.log"))
            });

            let min_size_bytes = min_size
                .map(|s| parse_size(&s))
                .transpose()?;

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

            let event_types = types.map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_uppercase())
                    .collect::<Vec<_>>()
            });

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
        Commands::Status { format } => {
            let daemon = Daemon::new();
            let status = daemon.status()?;

            match format {
                OutputFormat::Human => {
                    match status {
                        DaemonStatus::Running { pid, paths, log_file, start_time, event_count, memory_usage } => {
                            let paths_str = paths
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ");
                            println!("fsmon daemon status: Running (PID: {})", pid);
                            println!("Monitored paths: {}", paths_str);
                            println!("Log file: {}", log_file.display());
                            println!("Start time: {}", format_datetime(&start_time));
                            println!("Event count: {}", event_count);
                            println!("Memory usage: {:.1}MB", memory_usage as f64 / 1024.0 / 1024.0);
                        }
                        DaemonStatus::Stopped => {
                            println!("fsmon daemon status: Stopped");
                        }
                    }
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                OutputFormat::Csv => {
                    println!("status,pid,monitored_paths,log_file,start_time,event_count,memory_usage");
                    match status {
                        DaemonStatus::Running { pid, paths, log_file, start_time, event_count, memory_usage } => {
                            let paths_str = paths
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(";");
                            println!(
                                "running,{},\"{}\",\"{}\",\"{}\",{},{}",
                                pid,
                                paths_str,
                                log_file.display(),
                                start_time.to_rfc3339(),
                                event_count,
                                memory_usage
                            );
                        }
                        DaemonStatus::Stopped => {
                            println!("stopped,,,,,,");
                        }
                    }
                }
            }
        }
        Commands::Stop { force } => {
            let daemon = Daemon::new();
            daemon.stop(force)?;
        }
        Commands::Clean {
            log_file,
            keep_days,
            max_size,
            dry_run,
        } => {
            let log_file = log_file.unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(".fsmon").join("history.log"))
                    .unwrap_or_else(|| PathBuf::from("history.log"))
            });

            let max_size_bytes = max_size
                .map(|s| parse_size(&s))
                .transpose()?;

            clean_logs(&log_file, keep_days, max_size_bytes, dry_run).await?;
        }
    }

    Ok(())
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

            let should_keep = if let Ok(event) = serde_json::from_str::<FileEvent>(&line) {
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
        println!("Deleted {} lines (logs older than {} days)", total_deleted, keep_days);
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

    let first_newline = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
    Ok(read_start + first_newline)
}

/// Keep only bytes from offset to end
fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }

    let content = {
        let mut f = fs::File::open(path)?;
        f.seek(std::io::SeekFrom::Start(offset as u64))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        buf
    };

    let mut f = fs::File::create(path)?;
    f.write_all(&content)?;
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
