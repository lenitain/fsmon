use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process;

mod monitor;
mod query;
mod daemon;
mod utils;

use monitor::Monitor;
use query::Query;
use daemon::{Daemon, DaemonStatus};
use utils::{parse_size, format_size, format_datetime};

#[derive(Parser)]
#[command(name = "ftrace")]
#[command(about = "轻量级、高性能的文件变更溯源工具")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 实时监控文件变更
    Monitor {
        /// 监控的目录/文件路径（支持多个）
        paths: Vec<PathBuf>,
        /// 仅记录大小变化≥指定值的事件
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,
        /// 仅监控指定操作类型（逗号分隔）
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,
        /// 排除监控的路径（支持通配符）
        #[arg(short, long, value_name = "PATTERN")]
        exclude: Option<String>,
        /// 将监控日志写入指定文件
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// 输出格式
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,
        /// 后台守护进程运行
        #[arg(short, long)]
        daemon: bool,
    },
    /// 查询历史监控日志
    Query {
        /// 待查询的日志文件路径
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,
        /// 起始时间
        #[arg(short = 'S', long, value_name = "TIME")]
        since: Option<String>,
        /// 结束时间
        #[arg(short = 'U', long, value_name = "TIME")]
        until: Option<String>,
        /// 仅查询指定 PID 的事件
        #[arg(short, long, value_name = "PIDS")]
        pid: Option<String>,
        /// 仅查询指定进程名的事件
        #[arg(short, long, value_name = "PATTERN")]
        cmd: Option<String>,
        /// 仅查询指定用户的事件
        #[arg(short, long, value_name = "USERS")]
        user: Option<String>,
        /// 仅查询指定操作类型
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,
        /// 仅查询大小变化≥指定值的事件
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,
        /// 输出格式
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,
        /// 排序方式
        #[arg(short = 'r', long, value_enum, default_value = "time")]
        sort: SortBy,
    },
    /// 查看监控状态
    Status {
        /// 输出格式
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,
    },
    /// 停止后台监控
    Stop {
        /// 强制终止
        #[arg(long)]
        force: bool,
    },
    /// 清理历史日志
    Clean {
        /// 待清理的日志文件路径
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,
        /// 保留最近 N 天的日志
        #[arg(short, long, value_name = "DAYS", default_value = "30")]
        keep_days: u32,
        /// 日志文件最大大小
        #[arg(short, long, value_name = "SIZE")]
        max_size: Option<String>,
        /// 模拟清理，不实际删除
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
            "[{}] [{}] {} (PID: {}, CMD: {}, USER: {}, SIZE_CHANGE: {}{})",
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
            output,
            format,
            daemon,
        } => {
            if paths.is_empty() {
                eprintln!("错误: 请指定至少一个监控路径");
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
                    .map(|h: PathBuf| h.join(".ftrace").join("history.log"))
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
            let status = daemon.status().await?;

            match format {
                OutputFormat::Human => {
                    match status {
                        DaemonStatus::Running { pid, paths, log_file, start_time, event_count, memory_usage } => {
                            let paths_str = paths
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ");
                            println!("ftrace daemon status: Running (PID: {})", pid);
                            println!("Monitored paths: {}", paths_str);
                            println!("Log file: {}", log_file.display());
                            println!("Start time: {}", format_datetime(&start_time));
                            println!("Event count: {}", event_count);
                            println!("Memory usage: {:.1}MB", memory_usage as f64 / 1024.0 / 1024.0);
                        }
                        DaemonStatus::Stopped => {
                            println!("ftrace daemon status: Stopped");
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
            daemon.stop(force).await?;
        }
        Commands::Clean {
            log_file,
            keep_days,
            max_size,
            dry_run,
        } => {
            let log_file = log_file.unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(".ftrace").join("history.log"))
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
    let file = fs::File::open(log_file)?;
    let reader = BufReader::new(file);

    let mut kept_lines = Vec::new();
    let mut deleted_count = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // Try to parse as JSON to get timestamp
        let should_keep = if let Ok(event) = serde_json::from_str::<FileEvent>(&line) {
            event.time >= cutoff_time
        } else {
            // If can't parse, keep the line
            true
        };

        if should_keep {
            kept_lines.push(line);
        } else {
            deleted_count += 1;
        }
    }

    // Apply max_size limit if specified
    let mut final_lines = kept_lines;
    if let Some(max_bytes) = max_size {
        let total_size: usize = final_lines.iter().map(|l| l.len() + 1).sum();
        if total_size > max_bytes as usize {
            let mut current_size = 0;
            let mut keep_count = 0;
            for (i, line) in final_lines.iter().enumerate().rev() {
                current_size += line.len() + 1;
                if current_size > max_bytes as usize {
                    keep_count = final_lines.len() - i - 1;
                    break;
                }
            }
            deleted_count += final_lines.len() - keep_count;
            final_lines = final_lines.split_off(final_lines.len() - keep_count);
        }
    }

    let original_size = fs::metadata(log_file)?.len();

    if dry_run {
        println!("Dry run: Would delete {} lines", deleted_count);
        println!("No changes made (--dry-run enabled)");
    } else {
        let temp_file = log_file.with_extension("tmp");
        let mut file = fs::File::create(&temp_file)?;
        for line in &final_lines {
            writeln!(file, "{}", line)?;
        }
        fs::rename(&temp_file, log_file)?;

        let new_size = fs::metadata(log_file)?.len();
        println!("Cleaning {}...", log_file.display());
        println!("Deleted {} lines (logs older than {} days)", deleted_count, keep_days);
        println!(
            "Log file size reduced from {} to {}",
            format_size(original_size as i64),
            format_size(new_size as i64)
        );
    }

    Ok(())
}
