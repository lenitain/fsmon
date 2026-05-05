use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fsmon::config::Config;
use fsmon::help::{self, HelpTopic};
use fsmon::monitor::{Monitor, PathOptions};
use fsmon::query::Query;
use fsmon::socket::{self, SocketCmd};
use fsmon::store::{PathEntry, Store};
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
    /// Run the fsmon daemon (requires sudo for fanotify)
    #[command(about = help::about(HelpTopic::Daemon), long_about = help::long_about(HelpTopic::Daemon))]
    Daemon,

    /// Add a path to the monitoring list
    #[command(about = help::about(HelpTopic::Add), long_about = help::long_about(HelpTopic::Add))]
    Add(AddArgs),

    /// Remove a path from the monitoring list by numeric ID
    #[command(about = help::about(HelpTopic::Remove), long_about = help::long_about(HelpTopic::Remove))]
    Remove { id: u64 },

    /// List all monitored paths with their configuration
    #[command(about = help::about(HelpTopic::Managed), long_about = help::long_about(HelpTopic::Managed))]
    Managed,

    /// Query historical file change events
    #[command(about = help::about(HelpTopic::Query), long_about = help::long_about(HelpTopic::Query))]
    Query(QueryArgs),

    /// Clean historical log files
    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean(CleanArgs),

    /// Generate a default configuration file
    #[command(about = help::about(HelpTopic::Generate), long_about = help::long_about(HelpTopic::Generate))]
    Generate {
        /// Overwrite existing configuration file if it exists
        #[arg(short, long)]
        force: bool,
    },
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
    /// Entry ID(s) to query. Comma-separated and/or ranges. Repeatable. Default: all.
    #[arg(short, long, value_name = "IDS")]
    id: Vec<String>,
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
    /// Entry ID(s) to clean. Comma-separated and/or ranges. Repeatable. Default: all.
    #[arg(short, long, value_name = "IDS")]
    id: Vec<String>,
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
        Commands::Remove { id } => cmd_remove(id)?,
        Commands::Managed => cmd_managed()?,
        Commands::Query(args) => cmd_query(args).await?,
        Commands::Clean(args) => cmd_clean(args).await?,
        Commands::Generate { force } => cmd_generate(force)?,
    }

    Ok(())
}

async fn cmd_daemon() -> Result<()> {
    // Auto-generate config if it doesn't exist
    let config_path = Config::path();
    if !config_path.exists() {
        eprintln!(
            "Config not found at {}, generating default config...",
            config_path.display()
        );
        Config::generate_default()?;
        eprintln!("Default config generated at {}", config_path.display());
    }

    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    eprintln!("Config loaded:");
    eprintln!("  Store file:  {}", cfg.store.file.display());
    eprintln!("  Log dir:     {}", cfg.logging.dir.display());
    eprintln!("  Socket:      {}", cfg.socket.path.display());

    let store = Store::load(&cfg.store.file)?;

    if store.entries.is_empty() {
        eprintln!("Warning: No paths configured. Use 'fsmon add <path>' to add paths.");
    }

    let socket_path = cfg.socket.path.clone();

    // Create parent directories for socket
    if socket_path.exists() {
        fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let socket_listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind socket at {}", socket_path.display()))?;

    // Set socket permissions to 0666 so any user can send commands
    set_socket_permissions(&socket_path)?;

    eprintln!("Monitored paths ({}):", store.entries.len());
    for entry in &store.entries {
        eprintln!("  [{}] {}", entry.id, entry.path.display());
    }

    let paths_and_options = parse_path_entries(&store.entries)?;
    let path_ids: std::collections::HashMap<_, _> = store
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.id))
        .collect();

    let store_path = cfg.store.file.clone();
    let mut monitor = Monitor::new(
        paths_and_options,
        path_ids,
        Some(cfg.logging.dir.clone()),
        Some(store_path),
        None,
        Some(socket_listener),
    )?;

    monitor.run().await?;
    Ok(())
}

/// Set socket permissions to 0666 so non-root users can communicate with the daemon.
fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = fs::Permissions::from_mode(0o666);
    fs::set_permissions(path, perm)
        .with_context(|| format!("Failed to set socket permissions on {}", path.display()))?;
    Ok(())
}

fn cmd_add(args: AddArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Store::load(&cfg.store.file)?;

    let path = args.path.clone();
    let types: Option<Vec<String>> = args
        .types
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());
    let min_size = args.min_size.clone();
    let exclude = args.exclude.clone();
    let recursive = if args.recursive { Some(true) } else { None };
    let all_events = if args.all_events { Some(true) } else { None };

    let id = store.add_entry(PathEntry {
        id: 0,
        path: path.clone(),
        recursive,
        types: types.clone(),
        min_size: min_size.clone(),
        exclude: exclude.clone(),
        all_events,
    });

    store.save(&cfg.store.file)?;
    println!("Path added (ID: {}): {}", id, path.display());

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "add".to_string(),
            path: Some(path),
            recursive,
            types,
            min_size,
            exclude,
            all_events,
            id: None,
        },
    ) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            eprintln!("Path will be monitored after daemon restart");
        }
        Err(_) => {
            // daemon not running — store already saved, change applies on restart
        }
    }
    Ok(())
}

fn cmd_remove(id: u64) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Store::load(&cfg.store.file)?;

    if !store.remove_entry(id) {
        eprintln!("No monitored path with ID {}", id);
        std::process::exit(1);
    }

    store.save(&cfg.store.file)?;
    println!("Path removed from config (ID: {})", id);

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "remove".to_string(),
            path: None,
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
            id: Some(id),
        },
    ) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            eprintln!("Change will apply after daemon restart");
        }
        Err(_) => {
            // daemon not running — store already saved, change applies on restart
        }
    }
    Ok(())
}

fn cmd_managed() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let socket_path = cfg.socket.path.clone();

    // Try live list first, fall back to store file
    let entries = match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "list".to_string(),
            path: None,
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
            id: None,
        },
    ) {
        Ok(resp) if resp.ok => resp.paths.unwrap_or_default(),
        _ => {
            if let Ok(store) = Store::load(&cfg.store.file) {
                store.entries
            } else {
                vec![]
            }
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
            "{} | id={} | types={} | {} | min_size={} | exclude={} | events={}",
            entry.path.display(),
            entry.id,
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
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let ids = if args.id.is_empty() {
        None
    } else {
        Some(parse_query_ids(&args.id)?)
    };

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
        cfg.logging.dir,
        ids,
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

fn cmd_generate(force: bool) -> Result<()> {
    let config_path = Config::path();
    if config_path.exists() && !force {
        eprintln!("Config already exists at {}", config_path.display());
        eprintln!("Use -f or --force to overwrite");
        std::process::exit(1);
    }
    Config::generate_default()?;
    println!("Default config generated at {}", config_path.display());
    Ok(())
}

async fn cmd_clean(args: CleanArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let ids = if args.id.is_empty() {
        None
    } else {
        Some(parse_query_ids(&args.id)?)
    };
    let keep_days = args.keep_days.unwrap_or(DEFAULT_KEEP_DAYS);
    let max_size_bytes = args.max_size.map(|s| parse_size(&s)).transpose()?;
    clean_logs(
        &cfg.logging.dir,
        ids.as_deref(),
        keep_days,
        max_size_bytes,
        args.dry_run,
    )
    .await?;
    Ok(())
}

/// Parse --id argument: comma-separated IDs and/or ranges, e.g. "1,3,5-8"
/// Also handles repeated: --id 1 --id 3
fn parse_query_ids(raw: &[String]) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for part in raw {
        for segment in part.split(',') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            if let Some((start, end)) = segment.split_once('-') {
                let s: u64 = start
                    .trim()
                    .parse()
                    .with_context(|| format!("Invalid ID range start: {}", start))?;
                let e: u64 = end
                    .trim()
                    .parse()
                    .with_context(|| format!("Invalid ID range end: {}", end))?;
                ids.extend(s..=e);
            } else {
                let v: u64 = segment
                    .parse()
                    .with_context(|| format!("Invalid ID: {}", segment))?;
                ids.push(v);
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
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
