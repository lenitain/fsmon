use anyhow::{Context, Result, bail};
use path_clean::PathClean;
use clap::{Parser, Subcommand};
use fsmon::config::Config;
use fsmon::help::{self, HelpTopic};
use fsmon::monitor::{Monitor, PathOptions};
use fsmon::query::Query;
use fsmon::socket::{self, SocketCmd};
use fsmon::managed::{PathEntry, Managed};
use fsmon::utils::parse_size;
use fsmon::{DEFAULT_KEEP_DAYS, EventType, clean_logs};
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

    /// Remove one or more paths from the monitoring list
    #[command(about = help::about(HelpTopic::Remove), long_about = help::long_about(HelpTopic::Remove))]
    Remove {
        /// Path(s) to remove. Multiple paths can be specified.
        paths: Vec<PathBuf>,
    },

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

    /// List managed paths (one per line, for shell completion use)
    #[command(hide = true)]
    ListManagedPaths,
}

#[derive(Parser)]
struct AddArgs {
    /// Path to monitor
    path: PathBuf,

    /// Watch subdirectories recursively
    #[arg(short)]
    recursive: bool,

    /// Event types to monitor (repeatable; use "all" for all 14 types). Controls kernel mask.
    #[arg(short, long, value_name = "TYPE")]
    types: Vec<String>,

    /// Only record events with size change >= specified value
    #[arg(short = 'm', long, value_name = "SIZE")]
    min_size: Option<String>,

    /// Path glob patterns to exclude (repeatable, prefix ! to invert)
    #[arg(short = 'e', long, value_name = "PATTERN")]
    exclude: Vec<String>,

    /// Process names to exclude (glob, repeatable, prefix ! to invert)
    #[arg(long, value_name = "PATTERN")]
    exclude_cmd: Vec<String>,


}

#[derive(Parser)]
struct QueryArgs {
    /// Path(s) to query. Repeatable. Default: all.
    #[arg(short, long, value_name = "PATH")]
    path: Vec<PathBuf>,
    #[arg(short = 'S', long)]
    since: Option<String>,
    #[arg(short = 'U', long)]
    until: Option<String>,
}

#[derive(Parser)]
struct CleanArgs {
    /// Path(s) to clean. Repeatable. Default: all.
    #[arg(short, long, value_name = "PATH")]
    path: Vec<PathBuf>,
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
        Commands::Remove { paths } => {
            for path in paths {
                eprintln!("[Info] Removing {}...", path.display());
                cmd_remove(path)?;
            }
        }
        Commands::Managed => cmd_managed()?,
        Commands::Query(args) => cmd_query(args).await?,
        Commands::Clean(args) => cmd_clean(args).await?,
        Commands::Generate { force } => cmd_generate(force)?,
        Commands::ListManagedPaths => cmd_list_managed_paths()?,
    }

    Ok(())
}

async fn cmd_daemon() -> Result<()> {
    // Acquire singleton lock first — only one daemon instance allowed
    let (uid, _gid) = fsmon::config::resolve_uid_gid();
    let _lock = fsmon::DaemonLock::acquire(uid)?;

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
    eprintln!("  Managed path database:  {}", cfg.managed.file.display());
    eprintln!("  Event logs:     {}", cfg.logging.dir.display());
    eprintln!("  Command socket: {}", cfg.socket.path.display());

    let store = Managed::load(&cfg.managed.file)?;

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

    // Chown config file + store parent dir to the original user (daemon runs as root)
    let (uid, gid) = fsmon::config::resolve_uid_gid();
    chown_path(&config_path, uid, gid);
    if let Some(parent) = config_path.parent() {
        chown_path(parent, uid, gid);
    }
    if let Some(parent) = cfg.managed.file.parent() {
        chown_path(parent, uid, gid);
    }

    let paths_and_options = parse_path_entries(&store.entries)?;

    let store_path = cfg.managed.file.clone();
    let mut monitor = Monitor::new(
        paths_and_options,
        Some(cfg.logging.dir.clone()),
        Some(store_path),
        None,
        Some(socket_listener),
    )?;

    if !store.entries.is_empty() {
        eprintln!("Managed paths ({}):", store.entries.len());
        for entry in &store.entries {
            eprintln!("  {}", entry.path.display());
        }
    }

    monitor.run().await?;
    Ok(())
}

/// Chown a path to the given uid:gid (daemon runs as root, needs to give files back to the user).
fn chown_path(path: &Path, uid: u32, gid: u32) {
    if let Ok(cpath) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        let _ = nix::unistd::chown(
            cpath.as_c_str(),
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        );
    }
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

    // 1. Reject null bytes (would crash C FFI)
    let path_str = args.path.to_string_lossy();
    if path_str.contains('\0') {
        bail!("Invalid path: contains null byte");
    }

    // 2. Expand tilde, then clean/normalize (resolve ., .. without touching fs)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = fsmon::config::expand_tilde(&args.path, &home);
    let cleaned = expanded.clean();

    // 3. Canonicalize for existing paths (resolves symlinks); use cleaned for non-existing
    let path = match cleaned.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            if cleaned.components().count() == 0 {
                bail!("Invalid path (empty after normalization): {}", args.path.display());
            }
            eprintln!("[Note] path does not exist yet — will start monitoring when created.");
            cleaned
        }
    };
    let log_dir_canon = cfg.logging.dir.canonicalize().unwrap_or_else(|_| cfg.logging.dir.clone());
    if log_dir_canon.starts_with(&path) {
        bail!(
            "Cannot monitor '{}': log directory '{}' is inside this path — \
             would cause infinite recursion on every log write.\n\
             Tip: use a different logging.dir or add a more specific path",
            args.path.display(),
            cfg.logging.dir.display()
        );
    }

    let mut store = Managed::load(&cfg.managed.file)?;

    // 4. Check if already monitored
    if let Some(_existing) = store.get(&path) {
        eprintln!(
            "[Note] '{}' is already monitored — new parameters will replace the existing configuration.",
            path.display()
        );
    }

    // 5. Check for monitoring overlap with existing entries
    let new_recursive = args.recursive;
    for entry in &store.entries {
        let ep = &entry.path;
        let e_recursive = entry.recursive.unwrap_or(false);
        // New path is a subdirectory of an existing recursive path
        if e_recursive && path.starts_with(ep) && path != *ep {
            eprintln!(
                "[Note] '{}' is under recursively monitored path '{}' — events already covered.",
                path.display(),
                ep.display()
            );
        }
        // New path is recursive and covers an existing monitored path
        if new_recursive && ep.starts_with(&path) && *ep != path {
            eprintln!(
                "[Note] already monitored path '{}' is under new recursive path '{}' — events already covered.",
                ep.display(),
                path.display()
            );
        }
    }

    let types: Option<Vec<String>> = if args.types.is_empty() {
        None
    } else if args.types.iter().any(|s| s.eq_ignore_ascii_case("all")) {
        Some(vec![
            "ACCESS".into(),
            "MODIFY".into(),
            "CLOSE_WRITE".into(),
            "CLOSE_NOWRITE".into(),
            "OPEN".into(),
            "OPEN_EXEC".into(),
            "ATTRIB".into(),
            "CREATE".into(),
            "DELETE".into(),
            "DELETE_SELF".into(),
            "MOVED_FROM".into(),
            "MOVED_TO".into(),
            "MOVE_SELF".into(),
            "FS_ERROR".into(),
        ])
    } else {
        Some(args.types.clone())
    };
    let min_size = args.min_size.clone();
    let exclude = if args.exclude.is_empty() { None } else { Some(args.exclude.clone()) };
    let exclude_cmd = if args.exclude_cmd.is_empty() { None } else { Some(args.exclude_cmd.clone()) };
    let recursive = if args.recursive { Some(true) } else { None };

    store.add_entry(PathEntry {
        path: path.clone(),
        recursive,
        types: types.clone(),
        min_size: min_size.clone(),
        exclude: exclude.clone(),
        exclude_cmd: exclude_cmd.clone(),
    });

    store.save(&cfg.managed.file)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    let result = socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "add".to_string(),
            path: Some(path.clone()),
            recursive,
            types,
            min_size,
            exclude,
            exclude_cmd,
        },
    );

    match result {
        Ok(resp) if resp.ok => {
            println!("Path added: {}", path.display());
            println!("Daemon updated live");
        }
        Ok(resp) => {
            let is_permanent = resp.error_kind == Some(fsmon::socket::ErrorKind::Permanent);
            if is_permanent {
                // Revert store save — the error will persist after restart
                let mut store = Managed::load(&cfg.managed.file)?;
                store.remove_entry(&path);
                store.save(&cfg.managed.file)?;
                eprintln!("Error: {}", resp.error.unwrap_or_default());
            } else {
                println!("Path added: {}", path.display());
                eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
                eprintln!("Path will be monitored after daemon restart");
            }
        }
        Err(_) => {
            println!("Path added: {}", path.display());
            eprintln!("Daemon not running — path will be monitored after daemon restart.");
        }
    }
    Ok(())
}

fn cmd_remove(raw: PathBuf) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // Normalize path: expand tilde, clean (., ..), resolve symlinks.
    // Must match the normalization done by cmd_add, so store.remove_entry finds the entry.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = fsmon::config::expand_tilde(&raw, &home);
    let cleaned = expanded.clean();
    let path = cleaned.canonicalize().unwrap_or(cleaned);

    let mut store = Managed::load(&cfg.managed.file)?;

    if !store.remove_entry(&path) {
        eprintln!("No monitored path: {}", path.display());
        std::process::exit(1);
    }

    store.save(&cfg.managed.file)?;
    println!("Path removed: {}", path.display());

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "remove".to_string(),
            path: Some(path),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            exclude_cmd: None,
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
    let entries = Managed::load(&cfg.managed.file)
        .map(|s| s.entries)
        .unwrap_or_default();

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
        let exclude_str = entry.exclude.as_ref().map(|v| v.join(",")).as_deref().unwrap_or("-").to_string();
        let exclude_cmd_str = entry.exclude_cmd.as_ref().map(|v| v.join(",")).as_deref().unwrap_or("-").to_string();
        println!(
            "{} | types={} | {} | min_size={} | exclude-path={} | exclude-cmd={}",
            entry.path.display(),
            types_str,
            recursive_str,
            min_size_str,
            exclude_str,
            exclude_cmd_str,
        );
    }

    Ok(())
}

async fn cmd_query(args: QueryArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let paths = if args.path.is_empty() {
        None
    } else {
        Some(args.path.clone())
    };

    let query = Query::new(
        cfg.logging.dir,
        paths,
        args.since,
        args.until,
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

    let paths = if args.path.is_empty() {
        None
    } else {
        Some(args.path.clone())
    };
    let keep_days = args
        .keep_days
        .or(cfg.logging.keep_days)
        .unwrap_or(DEFAULT_KEEP_DAYS);
    let max_size_bytes = args
        .max_size
        .clone()
        .or(cfg.logging.max_size.clone())
        .map(|s| parse_size(&s))
        .transpose()?;
    clean_logs(
        &cfg.logging.dir,
        paths.as_deref(),
        keep_days,
        max_size_bytes,
        args.dry_run,
    )
    .await?;
    Ok(())
}

/// Build a combined regex from a list of patterns.
/// Multiple patterns are OR'd together. If the first pattern starts with `!`,
/// the entire match is inverted (exclude everything except matching).
fn build_exclude_regex(patterns: Option<&[String]>, label: &str) -> Result<(Option<regex::Regex>, bool)> {
    let Some(patterns) = patterns else { return Ok((None, false)); };
    if patterns.is_empty() {
        return Ok((None, false));
    }
    let invert = patterns[0].starts_with('!');
    let parts: Vec<String> = patterns.iter().map(|p| {
        let raw = p.strip_prefix('!').unwrap_or(p);
        if label == "--exclude-cmd" {
            raw.replace("*", ".*")
        } else {
            regex::escape(raw).replace("\\*", ".*")
        }
    }).collect();
    let pattern = parts.join("|");
    let regex = regex::Regex::new(&pattern)
        .with_context(|| format!("invalid {} pattern", label))?;
    Ok((Some(regex), invert))
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
    let (exclude_regex, exclude_invert) = build_exclude_regex(entry.exclude.as_deref(), "exclude")?;
    let (exclude_cmd_regex, exclude_cmd_invert) = build_exclude_regex(entry.exclude_cmd.as_deref(), "--exclude-cmd")?;
    Ok(PathOptions {
        min_size,
        event_types,
        exclude_regex,
        exclude_invert,
        exclude_cmd_regex,
        exclude_cmd_invert,
        recursive: entry.recursive.unwrap_or(false),
    })
}

/// Output all managed paths (one per line) — used by shell completion scripts.
fn cmd_list_managed_paths() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let entries = Managed::load(&cfg.managed.file)
        .map(|s| s.entries)
        .unwrap_or_default();
    for entry in &entries {
        println!("{}", entry.path.display());
    }
    Ok(())
}
