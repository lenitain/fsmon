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
mod proc_cache;

use monitor::Monitor;
use query::Query;
use daemon::{Daemon, DaemonStatus};
use utils::{parse_size, format_size, format_datetime};

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = "0.1.0")]
#[command(about = "轻量级、高性能的文件变更溯源工具", long_about = None)]
#[command(
    after_help = "使用 'fsmon <COMMAND> --help' 查看命令详细信息\n\n典型场景:\n  fsmon monitor /var/log                     # 基础监控\n  fsmon monitor /etc --types MODIFY         # 排查配置文件修改\n  fsmon monitor / --all-events               # 开启全部 14 种事件\n  fsmon monitor ~/project --recursive       # 递归监控项目\n  fsmon monitor / --daemon -o /var/log/fsmon-audit.log  # 守护进程模式\n  fsmon query --since 1h --cmd nginx         # 查询最近 1 小时 nginx 的操作\n  fsmon status                               # 查看守护进程状态"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "实时监控文件变更", long_about = LONG_ABOUT_MONITOR)]
    Monitor {
        /// 监控的目录/文件路径（支持多个）
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// 仅记录大小变化≥指定值的事件 (如：100MB, 1GB, 1048576)
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,

        /// 仅监控指定操作类型，逗号分隔
        /// (ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC,
        ///  ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, FS_ERROR)
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,

        /// 排除监控的路径（支持通配符，如："*.log", "/tmp/*"）
        #[arg(short, long, value_name = "PATTERN")]
        exclude: Option<String>,

        /// 捕获全部 14 种 fanotify 事件（默认只捕获 8 种变更事件）
        #[arg(long)]
        all_events: bool,

        /// 将监控日志写入指定文件（追加模式）
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// 输出格式 (human, json, csv)
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,

        /// 后台守护进程运行（必须与 --output 配合使用）
        #[arg(short, long)]
        daemon: bool,

        /// 递归监控所有子目录
        #[arg(short, long)]
        recursive: bool,
    },

    #[command(about = "查询历史监控日志", long_about = LONG_ABOUT_QUERY)]
    Query {
        /// 待查询的日志文件路径（默认：~/.fsmon/history.log）
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,

        /// 起始时间：相对时间 (1h, 30m, 7d) 或绝对时间 ("2024-05-01 10:00")
        #[arg(short = 'S', long, value_name = "TIME")]
        since: Option<String>,

        /// 结束时间：相对时间 (1h, 30m, 7d) 或绝对时间 ("2024-05-01 12:00")
        #[arg(short = 'U', long, value_name = "TIME")]
        until: Option<String>,

        /// 仅查询指定 PID 的事件（支持多个，逗号分隔：1234,5678）
        #[arg(short, long, value_name = "PIDS")]
        pid: Option<String>,

        /// 仅查询指定进程名的事件（支持*通配符：nginx*, python）
        #[arg(short, long, value_name = "PATTERN")]
        cmd: Option<String>,

        /// 仅查询指定用户的事件（支持多个，逗号分隔：root,admin）
        #[arg(short, long, value_name = "USERS")]
        user: Option<String>,

        /// 仅查询指定操作类型
        /// (ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC,
        ///  ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, FS_ERROR)
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,

        /// 仅查询大小变化≥指定值的事件 (如：100MB, 1GB)
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,

        /// 输出格式 (human, json, csv)
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,

        /// 排序方式 (time, size, pid)
        #[arg(short = 'r', long, value_enum, default_value = "time")]
        sort: SortBy,
    },

    #[command(about = "查看守护进程运行状态", long_about = LONG_ABOUT_STATUS)]
    Status {
        /// 输出格式 (human, json, csv)
        #[arg(short, long, value_enum, default_value = "human")]
        format: OutputFormat,
    },

    #[command(about = "停止后台守护进程", long_about = LONG_ABOUT_STOP)]
    Stop {
        /// 强制终止（发送 SIGKILL 信号，否则发送 SIGTERM）
        #[arg(long)]
        force: bool,
    },

    #[command(about = "清理历史日志", long_about = LONG_ABOUT_CLEAN)]
    Clean {
        /// 待清理的日志文件路径（默认：~/.fsmon/history.log）
        #[arg(short, long, value_name = "FILE")]
        log_file: Option<PathBuf>,

        /// 保留最近 N 天的日志（默认：30 天）
        #[arg(short, long, value_name = "DAYS", default_value = "30")]
        keep_days: u32,

        /// 日志文件最大大小（如：100MB, 1GB）
        #[arg(short, long, value_name = "SIZE")]
        max_size: Option<String>,

        /// 模拟清理，显示将删除的内容但不实际删除
        #[arg(short, long)]
        dry_run: bool,
    },
}

const LONG_ABOUT_MONITOR: &str = r#"监控指定路径的文件系统事件，实时输出 fanotify 原始事件。

【事件类型】
  默认：8 种核心变更事件 (CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF)
  --all-events: 开启全部 14 种 fanotify 事件 (包含 ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR)

【守护进程模式】
  --daemon 后台运行，必须与 --output 配合使用
  fsmon status/stop 查看状态和停止

【典型场景】
  fsmon monitor /etc --types MODIFY          # 排查配置文件修改
  fsmon monitor / --all-events               # 开启全部 14 种事件审计
  fsmon monitor ~/project --recursive        # 递归监控项目目录
  fsmon monitor /tmp --min-size 100MB        # 追踪大文件创建
  fsmon monitor /var/log --format json       # JSON 格式输出
  fsmon monitor / --daemon -o /var/log/fsmon-audit.log  # 守护进程长期监控"#;

const LONG_ABOUT_QUERY: &str = r#"从日志文件中查询历史文件变更事件，支持多种过滤条件和排序方式。

【时间过滤】
  --since   起始时间：相对时间 (1h, 30m, 7d) 或绝对时间 ("2024-05-01 10:00")
  --until   结束时间
  
【进程过滤】
  --pid     按进程 ID 过滤（支持多个：1234,5678）
  --cmd     按进程名过滤（支持*通配符：nginx*, python）
  --user    按用户名过滤（支持多个：root,admin）

【事件过滤】
  --types     按事件类型过滤 (ACCESS,MODIFY,CREATE,DELETE,...)
  --min-size  按大小变化过滤 (如：100MB, 1GB)

【典型场景】
  fsmon query                              # 查询默认日志 (~/.fsmon/history.log)
  fsmon query --since 1h                   # 最近 1 小时
  fsmon query --cmd nginx                  # 只查看 nginx 的操作
  fsmon query --since 1h --cmd java --types MODIFY --min-size 100MB  # 组合条件
  fsmon query --format json --sort size    # JSON 输出，按大小排序"#;

const LONG_ABOUT_STATUS: &str = r#"查看 fsmon 守护进程的运行状态。

【输出内容】
  - 运行状态（Running/Stopped）
  - 进程 ID（PID）
  - 监控的路径
  - 日志文件路径
  - 启动时间
  - 事件数量
  - 内存使用

【典型场景】
  fsmon status                 # 人类可读格式
  fsmon status --format json  # JSON 格式（便于集成监控系统）"#;

const LONG_ABOUT_STOP: &str = r#"停止 fsmon 守护进程。

【停止方式】
  默认：发送 SIGTERM 信号，优雅停止
  --force: 发送 SIGKILL 信号，强制停止

【典型场景】
  fsmon stop        # 优雅停止
  fsmon stop --force  # 强制停止（仅在无响应时使用）"#;

const LONG_ABOUT_CLEAN: &str = r#"清理历史日志文件，按时间或大小保留。

【清理策略】
  --keep-days   保留最近 N 天的日志（默认：30 天）
  --max-size    限制日志文件最大大小（如：100MB, 1GB）
  --dry-run     预览模式，不实际删除

【典型场景】
  fsmon clean --keep-days 7           # 保留 7 天日志
  fsmon clean --max-size 100MB        # 限制日志≤100MB
  fsmon clean --keep-days 7 --dry-run # 预览不删除"#;

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
