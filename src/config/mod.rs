use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use users::os::unix::UserExt;

/// Infrastructure configuration for fsmon.
///
/// The config file lives at `~/.config/fsmon/fsmon.toml`.
/// All path resolution is based on the **original user** (not root's HOME).
/// Daemon (running as root via sudo) uses SUDO_UID to find the right home.
/// CLI (running as user) uses the user's own HOME directly.
///
/// This file is manually edited. Only infrastructure paths go here.
/// Monitored path entries are stored in the separate store file (see `[monitored].path`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub monitored: MonitoredConfig,
    pub logging: LoggingConfig,
    pub socket: SocketConfig,
    pub cache: Option<CacheConfig>,
    pub watchdog: Option<WatchdogConfig>,
}

/// Configuration for the monitored paths store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoredConfig {
    pub path: PathBuf,
}

/// Configuration for log file output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log file output directory. None or absent = file logging disabled.
    /// Set a path to enable persistent JSONL log file writing.
    /// Same pattern as metrics.listen: absent = off, present = on.
    pub path: Option<PathBuf>,
    /// Keep log entries for at most this many days (default: 30).
    pub keep_days: Option<u32>,
    /// Maximum size per log file before truncation.
    pub size: Option<String>,
    /// Minimum free disk space before warning (e.g. "10%", "5GB").
    /// None = no check. Only applies to the log directory filesystem.
    pub disk_min_free: Option<String>,
    /// Log file sync interval in seconds. 0 or None = disabled.
    /// When set, fdatasync is called on all dirty log files every N seconds.
    pub sync_interval_secs: Option<u64>,
    /// Use local time instead of UTC in event timestamps. Default: false.
    /// When true, timestamps in JSONL output are converted to local timezone
    /// (e.g. "2026-05-27T07:12:50+08:00" instead of "2026-05-26T23:12:50Z").
    pub local_time: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketConfig {
    pub path: PathBuf,
}

/// Cache configuration (optional — missing fields use code defaults).
///
/// Priority: CLI args > fsmon.toml > code defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Directory handle cache capacity (default: 100,000).
    /// Each entry ≈ 150-200 bytes. Lower this on memory-constrained systems,
    /// raise it when monitoring large directory trees (>100k dirs).
    pub dir_capacity: Option<u64>,
    /// Directory handle cache TTL in seconds (default: 3600).
    /// Shorter TTL frees memory faster for volatile directory structures,
    /// longer TTL reduces handle re-resolution for stable directories.
    pub dir_ttl_secs: Option<u64>,
    /// File size cache capacity (default: 10,000).
    /// Each entry ≈ 80-120 bytes. Raise for high-file-volume workloads.
    pub file_size_capacity: Option<usize>,
    /// Process cache TTL in seconds (default: 600).
    /// Applies to both proc_cache and pid_tree. Shorter TTL cleans up
    /// zombie process entries faster; longer TTL reduces /proc reads.
    pub proc_ttl_secs: Option<u64>,
    /// Interval in seconds between periodic cache stats log output in debug
    /// mode (default: 60). Set to 0 to disable periodic cache stats.
    pub stats_interval_secs: Option<u64>,
    /// Event channel capacity between reader tasks and the main loop.
    /// Default: unbounded. Set to a finite number (e.g. 1024) to cap
    /// memory under extreme event storms — reader tasks block when
    /// the buffer is full, with fanotify overflow as final backstop.
    pub channel_capacity: Option<usize>,
    /// Subscribe event stream buffer capacity.
    /// Number of events the broadcast channel can buffer before
    /// dropping oldest for slow subscribers. Default: 4096.
    /// Raise for high-throughput workloads with many subscribers.
    pub subscribe_buf: Option<usize>,
}

/// Watchdog configuration for systemd integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogConfig {
    /// Watchdog interval in seconds. None or 0 = disabled.
    /// When enabled, sends periodic WATCHDOG=1 notifications to systemd.
    /// systemd will restart the service if no notification is received
    /// within WatchdogSec (configured in the service unit).
    pub interval_secs: Option<u64>,
    /// Watchdog timeout multiplier for WatchdogSec. Default: 2.
    /// WatchdogSec = interval_secs × multiplier.
    /// Recommended: 2-4. Higher = more tolerant of transient stalls.
    pub multiplier: Option<u64>,
}

/// Resolved cache configuration with all defaults filled in.
#[derive(Debug, Clone)]
pub struct ResolvedCacheConfig {
    pub dir_capacity: u64,
    pub dir_ttl_secs: u64,
    pub file_size_capacity: usize,
    pub proc_ttl_secs: u64,
    pub buffer_size: usize,
    pub stats_interval_secs: u64,
    /// None = unbounded, Some(N) = bounded(N).
    pub channel_capacity: Option<usize>,
    /// Subscribe event stream buffer capacity.
    pub subscribe_buf: usize,
}

impl Default for ResolvedCacheConfig {
    fn default() -> Self {
        Self {
            dir_capacity: crate::fid_parser::DIR_CACHE_CAP,
            dir_ttl_secs: crate::fid_parser::DIR_CACHE_TTL_SECS,
            file_size_capacity: crate::fid_parser::FILE_SIZE_CACHE_CAP,
            proc_ttl_secs: crate::proc_cache::PROC_CACHE_TTL_SECS,
            buffer_size: 4096 * 8, // 32KB — default from Monitor::new()
            stats_interval_secs: 60,
            channel_capacity: None, // unbounded by default
            subscribe_buf: 4096,
        }
    }
}

impl CacheConfig {
    /// Merge: explicit values from this config override defaults,
    /// then CLI overrides override config values.
    pub fn resolve_with_cli(&self, cli: &CliCacheOverride) -> ResolvedCacheConfig {
        let mut r = ResolvedCacheConfig::default();
        if let Some(v) = self.dir_capacity {
            r.dir_capacity = v;
        }
        if let Some(v) = self.dir_ttl_secs {
            r.dir_ttl_secs = v;
        }
        if let Some(v) = self.file_size_capacity {
            r.file_size_capacity = v;
        }
        if let Some(v) = self.proc_ttl_secs {
            r.proc_ttl_secs = v;
        }
        if let Some(v) = self.stats_interval_secs {
            r.stats_interval_secs = v;
        }
        if let Some(v) = self.channel_capacity {
            r.channel_capacity = Some(v);
        }
        // Apply CLI overrides (highest priority)
        if let Some(v) = cli.dir_capacity {
            r.dir_capacity = v;
        }
        if let Some(v) = cli.dir_ttl_secs {
            r.dir_ttl_secs = v;
        }
        if let Some(v) = cli.file_size_capacity {
            r.file_size_capacity = v;
        }
        if let Some(v) = cli.proc_ttl_secs {
            r.proc_ttl_secs = v;
        }
        if let Some(v) = cli.stats_interval_secs {
            r.stats_interval_secs = v;
        }
        if let Some(v) = cli.buffer_size {
            r.buffer_size = v;
        }
        if let Some(v) = cli.channel_capacity {
            r.channel_capacity = Some(v);
        }
        if let Some(v) = self.subscribe_buf {
            r.subscribe_buf = v;
        }
        if let Some(v) = cli.subscribe_buf {
            r.subscribe_buf = v;
        }
        r
    }
}

/// CLI-level cache overrides (highest priority in the merge chain).
#[derive(Debug, Clone, Default)]
pub struct CliCacheOverride {
    pub dir_capacity: Option<u64>,
    pub dir_ttl_secs: Option<u64>,
    pub file_size_capacity: Option<usize>,
    pub proc_ttl_secs: Option<u64>,
    pub stats_interval_secs: Option<u64>,
    pub buffer_size: Option<usize>,
    pub channel_capacity: Option<usize>,
    pub subscribe_buf: Option<usize>,
}

// ---- Helpers ----

/// Resolve the original user's UID and GID, regardless of how fsmon was started:
/// - `SUDO_UID`/`SUDO_GID` (sudo) — fast path, no syscall
/// - `$HOME` owner (systemd / any root context with HOME set) — one `stat` call
/// - Current process UID/GID (normal user, no root) — no syscall
pub fn resolve_uid_gid() -> (u32, u32) {
    // 1. SUDO_UID — sudo
    if let Ok(uid_str) = std::env::var("SUDO_UID")
        && let Ok(uid) = uid_str.parse::<u32>()
    {
        let gid = std::env::var("SUDO_GID")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        return (uid, gid);
    }

    // 2. Running as root → use $HOME directory owner
    //    (works for systemd, sudo without SUDO_UID, or any root-launched context
    //     where HOME is set to the target user's directory)
    if nix::unistd::geteuid().is_root()
        && let Ok(home) = std::env::var("HOME")
        && let Ok(meta) = std::fs::metadata(&home)
    {
        use std::os::linux::fs::MetadataExt;
        return (meta.st_uid(), meta.st_gid());
    }

    // 3. Running as normal user
    (
        nix::unistd::geteuid().as_raw(),
        nix::unistd::getegid().as_raw(),
    )
}

/// Chown a path to the original user (daemon runs as root, files should go to the user).
/// Silently no-ops if already running as the target user (no sudo).
pub fn chown_to_original_user(path: &Path) {
    let _ = crate::fid_parser::chown_to_user(path);
}

/// Resolve the original user's UID:
/// - `SUDO_UID` (sudo)
/// - `$HOME` owner (systemd / root)
/// - Current process UID (normal user)
pub fn resolve_uid() -> u32 {
    resolve_uid_gid().0
}

/// Resolve the original user's home directory using platform password database.
/// Used by the daemon (running as root) to find the user's config/log paths.
pub fn resolve_home(uid: u32) -> Result<PathBuf> {
    let user = users::get_user_by_uid(uid)
        .ok_or_else(|| anyhow::anyhow!("User not found for UID {}", uid))?;

    let home = user.home_dir().to_path_buf();
    if home.as_os_str().is_empty() {
        anyhow::bail!("Home directory not set for UID {}", uid);
    }
    Ok(home)
}

/// Best-effort guess of user's home directory.
/// Used by CLI commands (running as user, HOME is correct).
/// For daemon (root via sudo), use SUDO_UID + getpwuid.
/// For tests, use HOME env.
pub fn guess_home() -> String {
    // 1. SUDO_UID — daemon running via sudo
    let uid_str = match std::env::var("SUDO_UID") {
        Ok(s) => s,
        Err(_) => return std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
    };
    let uid = match uid_str.parse::<u32>() {
        Ok(u) => u,
        Err(_) => return std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
    };
    // If we're not actually root (e.g. in tests where SUDO_UID is unset),
    // just use HOME. If we are root, try getpwuid.
    if nix::unistd::geteuid().as_raw() != 0 {
        return std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    }
    match resolve_home(uid) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
    }
}

/// Expand a leading `~` in a path to the given home directory.
pub fn expand_tilde(path: &Path, home: &str) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return PathBuf::from(format!("{}{}", home, rest));
    }
    path.to_path_buf()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            monitored: MonitoredConfig {
                path: PathBuf::from("~/.local/share/fsmon/monitored.jsonl"),
            },
            logging: LoggingConfig {
                path: Some(PathBuf::from("~/.local/state/fsmon")),
                keep_days: None,
                size: None,
                disk_min_free: None,
                sync_interval_secs: None,
                local_time: None,
            },
            socket: SocketConfig {
                path: PathBuf::from("/tmp/fsmon-<UID>.sock"),
            },
            cache: None,
            watchdog: None,
        }
    }
}

impl Config {
    /// Return the config file path: `$XDG_CONFIG_HOME/fsmon/fsmon.toml`
    /// Falls back to `~/.config/fsmon/fsmon.toml`.
    pub fn path() -> PathBuf {
        let home = guess_home();
        let xdg_config =
            std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
        PathBuf::from(xdg_config).join("fsmon").join("fsmon.toml")
    }

    /// Load config from file. Returns default Config if file doesn't exist.
    /// Also returns default if file exists but contains only comments (e.g.
    /// a reference file created by `fsmon init`).
    pub fn load() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&p)
            .with_context(|| format!("Failed to read config {}", p.display()))?;
        if is_comment_only(&content) {
            return Ok(Config::default());
        }
        match toml::from_str::<Config>(&content) {
            Ok(cfg) => Ok(cfg),
            Err(e) => bail!("Invalid config file at {}: {}", p.display(), e),
        }
    }

    /// Expand `~` in all paths using the original user's home directory.
    /// Replace `<UID>` in socket path with the actual numeric UID.
    pub fn resolve_paths(&mut self) -> Result<()> {
        let home = guess_home();
        let uid = resolve_uid();

        self.monitored.path = expand_tilde(&self.monitored.path, &home);
        if let Some(ref mut p) = self.logging.path {
            *p = expand_tilde(p, &home);
        }

        let socket_str = self.socket.path.to_string_lossy().to_string();
        self.socket.path = PathBuf::from(socket_str.replace("<UID>", &uid.to_string()));
        // Also expand tilde in socket path if present
        self.socket.path = expand_tilde(&self.socket.path, &home);

        Ok(())
    }

    /// Ensure the monitored store's parent directory exists.
    /// Called by `fsmon add` / `fsmon monitored` on first use.
    pub fn ensure_monitored_dir() -> Result<()> {
        let mut cfg = Config::load()?;
        cfg.resolve_paths()?;
        let parent = cfg
            .monitored
            .path
            .parent()
            .context("Monitored file path has no parent")?
            .to_path_buf();
        if !parent.exists() {
            fs::create_dir_all(&parent).with_context(|| {
                format!("Failed to create monitored directory: {}", parent.display())
            })?;
            chown_to_original_user(&parent);
        }
        Ok(())
    }

    /// Create the config file only. Directories are created on first use:
    /// - Monitored dir: created by `fsmon add` / `fsmon monitored`
    /// - Log dir: created by `fsmon cd` or `fsmon daemon`
    pub fn init_dirs() -> Result<()> {
        let config_path = Self::path();

        if !config_path.exists() {
            Self::create_default_config(&config_path)?;
        } else {
            eprintln!("Exists config:        {}", config_path.display());
        }
        Ok(())
    }

    /// Return the default config as a TOML string with all values commented out.
    fn default_commented_toml() -> String {
        r#"# ================================================================
# fsmon configuration file
# ================================================================
#
# All settings are optional. Commented values show defaults.
# Uncomment to override. Changes take effect on next daemon start.
# CLI flags override config file values.

[monitored]
# Monitored paths database file path.
# Config-only (no CLI flag).
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
# Log file output directory. Remove this section to disable file logging.
# Config-only (no CLI flag).
path = "~/.local/state/fsmon"
#
# Auto-clean: keep entries for at most N days.
# Config-only (clean command accepts -t/--time per invocation).
# keep_days = 30
#
# Auto-clean: truncate log file when it exceeds this size.
# Config-only (clean command accepts -s/--size per invocation).
# size = ">=1GB"
#
# Warn when free disk space drops below this threshold.
# Format: percentage ("10%") or absolute ("5GB").
# Default: no check. CLI: --disk-min-free 10%
# disk_min_free = "10%"
#
# Periodic fdatasync interval in seconds.
# Prevents event loss on crash (kill -9, power loss).
# Recommended: 5. Default: disabled. CLI: --sync-interval 5
# sync_interval_secs = 5
#
# Use local time instead of UTC in event timestamps.
# Default: false (UTC). CLI: --local-time
# local_time = false

[socket]
# Unix socket for CLI-to-daemon communication.
# <UID> is replaced with the actual user ID at runtime.
# Config-only (no CLI flag).
path = "/tmp/fsmon-<UID>.sock"

# ----------------------------------------------------------------
# Cache settings. Uncomment to override defaults.
# ----------------------------------------------------------------
# [cache]
#
# Directory handle cache capacity.
# Each entry is ~150-200 bytes. Lower on memory-constrained systems.
# Default: 100000. CLI: --cache-dir-cap N
# dir_capacity = 100000
#
# Directory handle cache TTL in seconds.
# Shorter = faster memory reclaim for volatile directories.
# Default: 3600. CLI: --cache-dir-ttl SECS
# dir_ttl_secs = 3600
#
# File size cache capacity.
# Raise for high-file-volume workloads.
# Default: 10000. CLI: --cache-file-size N
# file_size_capacity = 10000
#
# Process cache TTL in seconds.
# Applies to proc_cache and pid_tree.
# Default: 600. CLI: --cache-proc-ttl SECS
# proc_ttl_secs = 600
#
# Cache stats output interval in debug mode (seconds).
# Set to 0 to disable. Default: 60. CLI: --cache-stats-interval SECS
# stats_interval_secs = 60
#
# Event channel capacity between reader tasks and main loop.
# Default: unbounded. CLI: --channel-capacity N
# channel_capacity = 1024
#
# Subscribe event stream buffer capacity.
# Events buffered for slow subscribers before dropping oldest.
# Default: 4096. CLI: --subscribe-buf N
# subscribe_buf = 4096

# ----------------------------------------------------------------
# systemd watchdog integration.
# Sends periodic WATCHDOG=1 to prevent systemd from restarting.
# ----------------------------------------------------------------
# [watchdog]
#
# Heartbeat interval in seconds.
# Must be > 0 to enable watchdog. Default: disabled.
# CLI: --watchdog-interval SECS
# interval_secs = 15
#
# Timeout multiplier. WatchdogSec = interval_secs × multiplier.
# MUST be > 1 (daemon refuses to start otherwise).
# Recommended: 2-4. Higher = more tolerant of transient stalls.
# Default: 2. CLI: --watchdog-multiplier N
# multiplier = 2
"#
        .to_string()
    }

    /// Write a commented reference config file to the canonical path.
    /// Creates parent directories if needed.
    fn create_default_config(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            chown_to_original_user(parent);
        }
        fs::write(path, Self::default_commented_toml())?;
        chown_to_original_user(path);
        eprintln!("Created config: {}", path.display());
        Ok(())
    }
}

/// Check if a config file contains only comments and whitespace.
fn is_comment_only(s: &str) -> bool {
    s.lines().all(|l| {
        let t = l.trim();
        t.is_empty() || t.starts_with('#')
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
