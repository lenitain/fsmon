use anyhow::{Context, Result, bail};
use fsmon::DaemonLock;
use fsmon::config::{CacheConfig, CliCacheOverride, Config};
use fsmon::monitor::Monitor;
use fsmon::monitored::Monitored;
use std::fs;
use std::path::Path;

use super::parse_path_entries;

pub async fn cmd_daemon(
    debug: bool,
    cli_cache: CliCacheOverride,
    disk_min_free: Option<String>,
    sync_interval: Option<u64>,
    local_time: bool,
    metrics_interval: Option<u64>,
    watchdog_interval: Option<u64>,
    watchdog_multiplier: Option<u64>,
) -> Result<()> {
    // Acquire singleton lock first — only one daemon instance allowed
    let (uid, _gid) = fsmon::config::resolve_uid_gid();
    let _lock = DaemonLock::acquire(uid)?;

    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    eprintln!("Config loaded:");
    eprintln!(
        "  Monitored path database:  {}",
        cfg.monitored.path.display()
    );
    // Log path comes purely from config
    let log_path = cfg.logging.path.clone();
    if let Some(ref p) = log_path {
        eprintln!("  Event logs:     {}", p.display());
    } else {
        eprintln!("  Event logs:     disabled (path not configured)");
    }
    eprintln!("  Command socket: {}", cfg.socket.path.display());

    let store = Monitored::load(&cfg.monitored.path)?;

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

    // Chown store parent dir to the original user (daemon runs as root)
    if let Some(parent) = cfg.monitored.path.parent() {
        fsmon::config::chown_to_original_user(parent);
    }

    // Merge cache config: CLI > fsmon.toml > code defaults
    let cache_cfg = cfg
        .cache
        .as_ref()
        .map(|c| c.resolve_with_cli(&cli_cache))
        .unwrap_or_else(|| {
            let empty = CacheConfig {
                dir_capacity: None,
                dir_ttl_secs: None,
                file_size_capacity: None,
                proc_ttl_secs: None,
                stats_interval_secs: None,
                channel_capacity: None,
                subscribe_buf: None,
            };
            empty.resolve_with_cli(&cli_cache)
        });

    if debug {
        eprintln!("[DEBUG] --- cache configuration ---");
        eprintln!("[DEBUG]   dir_capacity:       {}", cache_cfg.dir_capacity);
        eprintln!("[DEBUG]   dir_ttl_secs:       {}", cache_cfg.dir_ttl_secs);
        eprintln!(
            "[DEBUG]   file_size_capacity: {}",
            cache_cfg.file_size_capacity
        );
        eprintln!("[DEBUG]   proc_ttl_secs:      {}", cache_cfg.proc_ttl_secs);
        eprintln!(
            "[DEBUG]   stats_interval_secs: {}",
            cache_cfg.stats_interval_secs
        );
        eprintln!("[DEBUG]   buffer_size:        {}", cache_cfg.buffer_size);
        match cache_cfg.channel_capacity {
            Some(cap) => eprintln!("[DEBUG]   channel_capacity:   {} (bounded)", cap),
            None => eprintln!("[DEBUG]   channel_capacity:   unbounded"),
        }
    }

    let paths_and_options = parse_path_entries(&store.flatten())?;

    // Merge disk_min_free: CLI > config > None
    let disk_min_free = disk_min_free.or_else(|| cfg.logging.disk_min_free.clone());

    // Merge sync_interval: CLI > config > None (disabled)
    let sync_interval = sync_interval
        .or(cfg.logging.sync_interval_secs)
        .filter(|&n| n > 0)
        .map(std::time::Duration::from_secs);

    // Merge watchdog_interval: CLI > config > None (disabled)
    let watchdog_interval = watchdog_interval
        .or(cfg.watchdog.as_ref().and_then(|w| w.interval_secs))
        .filter(|&n| n > 0);

    // Merge watchdog_multiplier: CLI > config > None (default: 2)
    let watchdog_multiplier = watchdog_multiplier
        .or(cfg.watchdog.as_ref().and_then(|w| w.multiplier))
        .unwrap_or(2);

    // Validate watchdog_multiplier
    if watchdog_interval.is_some() && watchdog_multiplier <= 1 {
        bail!(
            "watchdog multiplier must be > 1, got {}. \
             WatchdogSec = interval × multiplier, must be > interval \
             to allow heartbeat tolerance.",
            watchdog_multiplier
        );
    }

    // Compute WatchdogSec = interval × multiplier
    let watchdog_sec = watchdog_interval.map(|i| i * watchdog_multiplier);

    if debug {
        if let Some(d) = sync_interval {
            eprintln!("[DEBUG]   sync_interval:      {}s", d.as_secs());
        } else {
            eprintln!("[DEBUG]   sync_interval:      disabled");
        }
        if let Some(i) = watchdog_interval {
            eprintln!("[DEBUG]   watchdog_interval:  {}s", i);
            eprintln!("[DEBUG]   watchdog_multiplier: {}x", watchdog_multiplier);
            if let Some(s) = watchdog_sec {
                eprintln!("[DEBUG]   watchdog_sec:       {}s", s);
            }
        } else {
            eprintln!("[DEBUG]   watchdog:           disabled");
        }
    }

    let store_path = cfg.monitored.path.clone();
    let subscribe_buf = cache_cfg.subscribe_buf;
    let log_dir = log_path;
    if debug {
        eprintln!(
            "[DEBUG]   local logging:      {}",
            if log_dir.is_some() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }
    let mut monitor = Monitor::new(
        paths_and_options,
        log_dir,
        Some(store_path),
        Some(cache_cfg.buffer_size),
        Some(socket_listener),
        debug,
        Some(cache_cfg),
        disk_min_free,
        sync_interval,
        Some(subscribe_buf),
        local_time || cfg.logging.local_time.unwrap_or(false),
        metrics_interval,
        watchdog_interval,
    )?;

    if !store.is_empty() {
        for group in &store.groups {
            let cmd_label = if group.cmd == fsmon::monitored::CMD_GLOBAL {
                "[global]".to_string()
            } else {
                format!("[{}]", group.cmd)
            };
            eprintln!("  {} ({} path(s)):", cmd_label, group.paths.len());
            for path in group.paths.keys() {
                eprintln!("    {}", path.display());
            }
        }
    }

    monitor.run().await?;
    Ok(())
}

/// Set socket permissions to 0666 so non-root users can communicate with the daemon.
pub fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = fs::Permissions::from_mode(0o666);
    fs::set_permissions(path, perm)
        .with_context(|| format!("Failed to set socket permissions on {}", path.display()))?;
    Ok(())
}
