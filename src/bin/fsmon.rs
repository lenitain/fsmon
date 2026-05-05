use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fsmon::config::{PathEntry, UserConfig};
use fsmon::help::{self, HelpTopic};
use fsmon::monitor::{Monitor, PathOptions};
use fsmon::query::Query;
use fsmon::socket::{self, SocketCmd};
use fsmon::utils::parse_size;
use fsmon::{DEFAULT_KEEP_DAYS, EventType, OutputFormat, SortBy, clean_logs};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = help::about(HelpTopic::Root))]
#[command(after_help = help::after_help())]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = help::about(HelpTopic::Daemon), long_about = help::long_about(HelpTopic::Daemon))]
    Daemon,

    #[command(about = help::about(HelpTopic::Add), long_about = help::long_about(HelpTopic::Add))]
    Add(AddArgs),

    #[command(about = help::about(HelpTopic::Remove), long_about = help::long_about(HelpTopic::Remove))]
    Remove { path: PathBuf },

    #[command(about = help::about(HelpTopic::Managed), long_about = help::long_about(HelpTopic::Managed))]
    Managed,

    #[command(about = help::about(HelpTopic::Query), long_about = help::long_about(HelpTopic::Query))]
    Query(QueryArgs),

    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean(CleanArgs),

    #[command(about = help::about(HelpTopic::Install), long_about = help::long_about(HelpTopic::Install))]
    Install {
        #[arg(short, long)]
        force: bool,
    },

    #[command(about = help::about(HelpTopic::Uninstall), long_about = help::long_about(HelpTopic::Uninstall))]
    Uninstall,
}

#[derive(Parser)]
struct AddArgs {
    /// Path to monitor
    path: PathBuf,

    /// Watch subdirectories recursively
    #[arg(short)]
    recursive: bool,

    /// Only monitor specified operation types, comma-separated
    #[arg(short, long, value_name = "TYPES")]
    types: Option<String>,

    /// Only record events with size change >= specified value
    #[arg(short = 'm', long, value_name = "SIZE")]
    min_size: Option<String>,

    /// Paths to exclude from monitoring (wildcards)
    #[arg(short = 'e', long, value_name = "PATTERN")]
    exclude: Option<String>,

    /// Capture all 14 fanotify events
    #[arg(long)]
    all_events: bool,
}

#[derive(Parser)]
struct QueryArgs {
    #[arg(short, long)]
    log_file: Option<PathBuf>,
    #[arg(short = 'S', long)]
    since: Option<String>,
    #[arg(short = 'U', long)]
    until: Option<String>,
    #[arg(short, long)]
    pid: Option<String>,
    #[arg(short, long)]
    cmd: Option<String>,
    #[arg(short, long)]
    user: Option<String>,
    #[arg(short, long)]
    types: Option<String>,
    #[arg(short = 'm', long)]
    min_size: Option<String>,
    #[arg(short, long, value_enum)]
    format: Option<OutputFormat>,
    #[arg(short = 'r', long, value_enum)]
    sort: Option<SortBy>,
}

#[derive(Parser)]
struct CleanArgs {
    #[arg(short, long)]
    log_file: Option<PathBuf>,
    #[arg(short, long)]
    keep_days: Option<u32>,
    #[arg(short = 'm', long)]
    max_size: Option<String>,
    #[arg(short, long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => cmd_daemon().await?,
        Commands::Add(args) => cmd_add(args)?,
        Commands::Remove { path } => cmd_remove(&path)?,
        Commands::Managed => cmd_managed()?,
        Commands::Query(args) => cmd_query(args).await?,
        Commands::Clean(args) => cmd_clean(args).await?,
        Commands::Install { force } => cmd_install(force)?,
        Commands::Uninstall => cmd_uninstall()?,
    }

    Ok(())
}

async fn cmd_daemon() -> Result<()> {
    // Migrate paths from old /etc/fsmon/fsmon.toml if user config doesn't exist yet
    UserConfig::migrate_from_etc()?;

    let user_cfg = UserConfig::load()?;

    if user_cfg.paths.is_empty() {
        eprintln!("Warning: No paths configured. Use 'fsmon add <path>' to add paths.");
    }

    let socket_path = UserConfig::default_socket_path();
    let log_file = UserConfig::default_log_file();

    for p in [socket_path.parent(), log_file.parent()]
        .into_iter()
        .flatten()
    {
        fs::create_dir_all(p)?;
    }

    if socket_path.exists() {
        fs::remove_file(&socket_path)?;
    }

    let socket_listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind socket at {}", socket_path.display()))?;

    let paths_and_options = parse_path_entries(&user_cfg.paths)?;

    let mut monitor = Monitor::new(
        paths_and_options,
        Some(log_file),
        OutputFormat::Json,
        None,
        None,
        Some(socket_listener),
    )?;

    monitor.run().await?;
    Ok(())
}

fn cmd_add(args: AddArgs) -> Result<()> {
    let path = args.path.clone();
    let types: Option<Vec<String>> = args
        .types
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());
    let min_size = args.min_size.clone();
    let exclude = args.exclude.clone();
    let recursive = if args.recursive { Some(true) } else { None };
    let all_events = if args.all_events { Some(true) } else { None };

    // Always persist to user config first
    UserConfig::add_path(PathEntry {
        path: path.clone(),
        recursive,
        types: types.clone(),
        min_size: min_size.clone(),
        exclude: exclude.clone(),
        all_events,
    })?;
    println!("Path added to config: {}", path.display());

    // Try live update via socket (non-fatal if fails)
    let socket_path = UserConfig::default_socket_path();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd::Add {
            path,
            recursive,
            types,
            min_size,
            exclude,
            all_events,
        },
    ) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            eprintln!("Path will be monitored after daemon restart");
        }
        Err(e) => {
            eprintln!("Daemon not reachable: {}", e);
            eprintln!("Path will be monitored after daemon restart");
        }
    }
    Ok(())
}

fn cmd_remove(path: &Path) -> Result<()> {
    // Always persist to user config first
    UserConfig::remove_path(path)?;
    println!("Path removed from config: {}", path.display());

    // Try live update via socket (non-fatal if fails)
    let socket_path = UserConfig::default_socket_path();
    match socket::send_cmd(&socket_path, &SocketCmd::Remove { path: path.into() }) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            eprintln!("Change will apply after daemon restart");
        }
        Err(e) => {
            eprintln!("Daemon not reachable: {}", e);
            eprintln!("Change will apply after daemon restart");
        }
    }
    Ok(())
}

fn cmd_managed() -> Result<()> {
    let socket_path = UserConfig::default_socket_path();
    // Try live list first, fall back to user config
    let entries = match socket::send_cmd(&socket_path, &SocketCmd::List) {
        Ok(resp) if resp.ok => resp.paths.unwrap_or_default(),
        _ => {
            UserConfig::load()
                .unwrap_or(UserConfig { paths: vec![] })
                .paths
        }
    };

    for entry in &entries {
        let types_str = entry
            .types
            .as_ref()
            .map(|v| v.join(","))
            .unwrap_or_else(|| "-".to_string());
        let recursive_str = if entry.recursive.unwrap_or(false) {
            "recursive"
        } else {
            "non-recursive"
        };
        let min_size_str = entry.min_size.as_deref().unwrap_or("-");
        let exclude_str = entry.exclude.as_deref().unwrap_or("-");
        let all_events_str = if entry.all_events.unwrap_or(false) {
            "all"
        } else {
            "filtered"
        };

        println!(
            "{} | types={} | {} | min_size={} | exclude={} | events={}",
            entry.path.display(),
            types_str,
            recursive_str,
            min_size_str,
            exclude_str,
            all_events_str,
        );
    }

    Ok(())
}

async fn cmd_query(args: QueryArgs) -> Result<()> {
    let log_file = resolve_log_file(args.log_file);

    let min_size_bytes = args.min_size.map(|s| parse_size(&s)).transpose()?;

    let pids = args.pid.map(|p| {
        p.split(',')
            .filter_map(|s| s.trim().parse::<u32>().ok())
            .collect()
    });

    let users = args
        .user
        .map(|u| u.split(',').map(|s| s.trim().to_string()).collect());

    let event_types = args
        .types
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

    let format = args.format.unwrap_or(OutputFormat::Human);
    let sort = args.sort.unwrap_or(SortBy::Time);

    let query = Query::new(
        log_file,
        args.since,
        args.until,
        pids,
        args.cmd,
        users,
        event_types,
        min_size_bytes,
        format,
        sort,
    );

    query.execute().await?;
    Ok(())
}

async fn cmd_clean(args: CleanArgs) -> Result<()> {
    let log_file = resolve_log_file(args.log_file);
    let keep_days = args.keep_days.unwrap_or(DEFAULT_KEEP_DAYS);
    let max_size_bytes = args.max_size.map(|s| parse_size(&s)).transpose()?;
    clean_logs(&log_file, keep_days, max_size_bytes, args.dry_run).await?;
    Ok(())
}

fn cmd_install(force: bool) -> Result<()> {
    fsmon::systemd::install(force)?;
    Ok(())
}

fn cmd_uninstall() -> Result<()> {
    fsmon::systemd::uninstall()?;
    Ok(())
}

fn resolve_log_file(cli_log_file: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli_log_file {
        return path;
    }
    UserConfig::default_log_file()
}

fn parse_path_entries(entries: &[PathEntry]) -> Result<Vec<(PathBuf, PathOptions)>> {
    let mut result = Vec::new();
    for entry in entries {
        let opts = parse_path_options(entry)?;
        result.push((entry.path.clone(), opts));
    }
    Ok(result)
}

fn parse_path_options(entry: &PathEntry) -> Result<PathOptions> {
    let min_size = entry.min_size.as_ref().map(|s| parse_size(s)).transpose()?;
    let event_types = entry
        .types
        .as_ref()
        .map(|v| {
            v.iter()
                .map(|s| s.parse::<EventType>())
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let exclude_regex = entry
        .exclude
        .as_ref()
        .map(|p| {
            let escaped = regex::escape(p);
            let pattern = escaped.replace("\\*", ".*");
            regex::Regex::new(&pattern).with_context(|| "invalid exclude pattern")
        })
        .transpose()?;
    Ok(PathOptions {
        min_size,
        event_types,
        exclude_regex,
        recursive: entry.recursive.unwrap_or(false),
        all_events: entry.all_events.unwrap_or(false),
    })
}
