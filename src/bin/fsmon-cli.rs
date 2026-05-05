use anyhow::Result;
use clap::{Parser, Subcommand};
use fsmon::config::{self, Config};
use fsmon::help::{self, HelpTopic};
use fsmon::monitor::Monitor;
use fsmon::query::Query;
use fsmon::utils::parse_size;
use fsmon::{
    DEFAULT_KEEP_DAYS, DEFAULT_LOG_PATH, EventType, OutputFormat, SortBy, clean_logs,
    parse_output_format, parse_sort_by,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "fsmon-cli")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "fsmon CLI — monitor, query, clean, and generate configuration")]
#[command(after_help = help::cli_after_help())]
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

        /// Output format (human, json, csv) — affects stdout only; log file is always JSON
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,

        /// Recursively monitor all subdirectories
        #[arg(short, long)]
        recursive: bool,

        /// Instance name (for systemd template mode, reads /etc/fsmon/fsmon-{name}.toml)
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
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
        #[arg(short, long, value_name = "TYPES")]
        types: Option<String>,

        /// Only query events with size change >= specified value (e.g., 100MB, 1GB)
        #[arg(short, long, value_name = "SIZE")]
        min_size: Option<String>,

        /// Output format (human, json, csv) — affects stdout only
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,

        /// Sort by (time, size, pid)
        #[arg(short = 'r', long, value_enum)]
        sort: Option<SortBy>,
    },

    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean {
        /// Log file path to clean (default: ~/.config/fsmon/history.log)
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

    #[command(about = help::about(HelpTopic::Generate), long_about = help::long_about(HelpTopic::Generate))]
    Generate {
        /// Force overwrite existing config file
        #[arg(short, long)]
        force: bool,
    },
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
            instance,
        } => {
            let user_config = config.monitor.unwrap_or_default();

            let instance_config = match instance {
                Some(ref name) => match Config::load_instance(name) {
                    Ok(Some(cfg)) => cfg,
                    Ok(None) => {
                        eprintln!(
                            "========================================\n\
                             ERROR: Instance config not found for '{}'.\n\
                             \n\
                             Create /etc/fsmon/fsmon-{}.toml first:\n\
                               sudo fsmon generate --instance {}\n\
                             \n\
                             Then edit the config file to set monitored paths.\n\
                             ========================================",
                            name, name, name
                        );
                        std::process::exit(fsmon::EXIT_CONFIG);
                    }
                    Err(e) => {
                        eprintln!(
                            "========================================\n\
                             ERROR: Invalid instance config for '{}':\n\
                             \n\
                             {}\n\
                             \n\
                             Fix the file at /etc/fsmon/fsmon-{}.toml\n\
                             or regenerate with: fsmon generate --instance {} --force\n\
                             ========================================",
                            name, e, name, name
                        );
                        std::process::exit(fsmon::EXIT_CONFIG);
                    }
                },
                None => config::InstanceConfig {
                    paths: vec![],
                    output: None,
                    min_size: None,
                    types: None,
                    exclude: None,
                    all_events: None,
                    recursive: None,
                },
            };

            let base = if instance.is_some() {
                instance_config
            } else {
                config::InstanceConfig {
                    paths: user_config.paths.unwrap_or_default(),
                    output: user_config.output,
                    min_size: user_config.min_size,
                    types: user_config.types,
                    exclude: user_config.exclude,
                    all_events: user_config.all_events,
                    recursive: user_config.recursive,
                }
            };

            let paths = if !paths.is_empty() { paths } else { base.paths };
            if paths.is_empty() {
                eprintln!("Error: Please specify at least one monitor path");
                std::process::exit(1);
            }

            let nonexistent: Vec<_> = paths.iter().filter(|p| !p.exists()).collect();
            if !nonexistent.is_empty() {
                eprintln!(
                    "========================================\n\
                     ERROR: The following monitored paths do not exist:\n"
                );
                for p in &nonexistent {
                    eprintln!("         {}", p.display());
                }
                eprintln!(
                    "\n\
                     Please verify the paths in your configuration.\n\
                     ========================================"
                );
                std::process::exit(fsmon::EXIT_CONFIG);
            }

            let min_size = min_size.or(base.min_size);
            let types = types.or(base.types);
            let exclude = exclude.or(base.exclude);
            let all_events = all_events || base.all_events.unwrap_or(false);
            let output = output.or(base.output);
            if let Some(ref name) = instance
                && output.is_none()
            {
                eprintln!(
                    "[WARNING] No output file configured for instance '{}'.\n\
                     Events will only be written to stdout (journald) and will NOT be persisted.\n\
                     Set output = \"/var/log/fsmon/{}.log\" in /etc/fsmon/fsmon-{}.toml\n\
                     to enable persistent event logging for query and cleanup.",
                    name, name, name
                );
            }
            let recursive = recursive || base.recursive.unwrap_or(false);
            let buffer_size = if instance.is_some() {
                None
            } else {
                user_config.buffer_size
            };
            let format = if instance.is_some() {
                OutputFormat::Human
            } else {
                format
                    .or(user_config.format.as_deref().and_then(parse_output_format))
                    .unwrap_or(OutputFormat::Human)
            };

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
                instance,
            )?;

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
            let query_cfg = config.query.unwrap_or_default();

            let log_file = log_file.or(query_cfg.log_file).unwrap_or_else(|| {
                dirs::config_dir()
                    .map(|h: PathBuf| h.join("fsmon").join(DEFAULT_LOG_PATH))
                    .unwrap_or_else(|| PathBuf::from("fsmon").join(DEFAULT_LOG_PATH))
            });

            let since = since.or(query_cfg.since);
            let until = until.or(query_cfg.until);
            let pid = pid.or(query_cfg.pid);
            let cmd = cmd.or(query_cfg.cmd);
            let user = user.or(query_cfg.user);
            let types = types.or(query_cfg.types);
            let min_size = min_size.or(query_cfg.min_size);
            let format = format
                .or(query_cfg.format.as_deref().and_then(parse_output_format))
                .unwrap_or(OutputFormat::Human);
            let sort = sort
                .or(query_cfg.sort.as_deref().and_then(parse_sort_by))
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
        Commands::Clean {
            log_file,
            keep_days,
            max_size,
            dry_run,
        } => {
            let clean_cfg = config.clean.unwrap_or_default();

            let log_file = log_file.or(clean_cfg.log_file).unwrap_or_else(|| {
                dirs::config_dir()
                    .map(|h: PathBuf| h.join("fsmon").join(DEFAULT_LOG_PATH))
                    .unwrap_or_else(|| PathBuf::from("fsmon").join(DEFAULT_LOG_PATH))
            });

            let keep_days = keep_days
                .or(clean_cfg.keep_days)
                .unwrap_or(DEFAULT_KEEP_DAYS);

            let max_size = max_size.or(clean_cfg.max_size);

            let max_size_bytes = max_size.map(|s| parse_size(&s)).transpose()?;

            clean_logs(&log_file, keep_days, max_size_bytes, dry_run).await?;
        }
        Commands::Generate { force } => {
            Config::generate(force)?;
        }
    }

    Ok(())
}
