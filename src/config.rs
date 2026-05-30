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

}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoredConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log file output directory. None or absent = file logging disabled.
    /// Set a path to enable persistent JSONL log file writing.
    /// Same pattern as [metrics].listen: absent = off, present = on.
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
            && let Ok(meta) = std::fs::metadata(&home) {
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
    let (uid, gid) = resolve_uid_gid();
    if nix::unistd::geteuid().as_raw() == 0
        && let Ok(cpath) = std::ffi::CString::new(path.to_string_lossy().as_ref())
    {
        let _ = nix::unistd::chown(
            cpath.as_c_str(),
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        );
    }
}

/// Resolve the original user's UID:
/// - `SUDO_UID` (sudo)
/// - `$HOME` owner (systemd / root)
/// - Current process UID (normal user)
pub fn resolve_uid() -> u32 {
    // 1. SUDO_UID — sudo
    if let Ok(uid_str) = std::env::var("SUDO_UID")
        && let Ok(uid) = uid_str.parse::<u32>()
    {
        return uid;
    }

    // 2. Running as root → $HOME owner
    if nix::unistd::geteuid().is_root()
        && let Ok(home) = std::env::var("HOME")
            && let Ok(meta) = std::fs::metadata(&home) {
                use std::os::linux::fs::MetadataExt;
                return meta.st_uid();
            }

    // 3. Current process UID
    nix::unistd::geteuid().as_raw()
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
# This file uses defaults where commented. Uncomment keys to override.
#
# Changes take effect on the next daemon start (or SIGHUP reload).

[monitored]
#   Where the monitored paths database is stored.
#   Config-only (no CLI flag).
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
#   Log file output directory. Delete this section to disable file logging.
#   Config-only (no CLI flag).
path = "~/.local/state/fsmon"
#   Auto-clean: keep entries for at most N days.
#   Config-only (clean command accepts -t/--time per invocation).
# keep_days = 30
#   Auto-clean: keep log file under this size.
#   Config-only (clean command accepts -s/--size per invocation).
# size = ">=1GB"
#   Warn when free disk space drops below this threshold.
#   Percentage ("10%") or absolute ("5GB"). Default: no check.
#   CLI: --disk-min-free 10%
# disk_min_free = "10%"
#   Log file sync interval in seconds (fdatasync). Default: disabled.
#   Recommended: 5. Prevents event loss on crash (kill -9, power loss).
#   CLI: --sync-interval 5
# sync_interval_secs = 5
#   Use local time instead of UTC in event timestamps. Default: false.
#   CLI: --local-time
# local_time = false

[socket]
#   Unix socket for CLI-to-daemon communication.
#   Config-only (no CLI flag).
path = "/tmp/fsmon-<UID>.sock"

# [cache]
#   Directory handle cache capacity (default: 100000).
#   CLI: --cache-dir-cap 200000
# dir_capacity = 100000
#   Directory handle cache TTL in seconds (default: 3600).
#   CLI: --cache-dir-ttl 7200
# dir_ttl_secs = 3600
#   File size cache capacity (default: 10000).
#   CLI: --cache-file-size 20000
# file_size_capacity = 10000
#   Process cache TTL in seconds (default: 600).
#   CLI: --cache-proc-ttl 1200
# proc_ttl_secs = 600
#   Cache stats output interval in debug mode (default: 60).
#   CLI: --cache-stats-interval 30
# stats_interval_secs = 60
#   Fanotify read buffer size in bytes (default: 32768).
#   CLI: --buffer-size 65536
# buffer_size = 32768
#   Event channel capacity. Default: unbounded.
#   CLI: --channel-capacity 1024
# channel_capacity = 1024
#   Subscribe event stream buffer capacity. Default: 4096.
#   CLI: --subscribe-buf 8192
# subscribe_buf = 4096

# [metrics]
#   TCP HTTP /metrics endpoint address. Socket "metrics" command is always
#   available; this enables Prometheus direct scrape.
#   CLI: --metrics-listen 127.0.0.1:9845
# listen = "127.0.0.1:9845"
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
mod tests {
    use super::*;
    use std::fs;
    use temp_env;

    fn unique_home_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = std::process::id();
        let thread = std::thread::current().id();
        std::env::temp_dir().join(format!("fsmon_home_test_{}_{:?}_{}", id, thread, n))
    }

    /// Mutex to prevent concurrent env var manipulation across tests
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Override HOME for test isolation.
    /// Uses catch_unwind to prevent mutex poisoning on panic.
    fn with_isolated_home(f: impl FnOnce(&Path)) {
        let lock = ENV_LOCK.lock().unwrap();
        let dir = unique_home_dir();
        let _ = fs::remove_dir_all(&dir);
        let home_val = dir.to_string_lossy().to_string();

        temp_env::with_vars(
            [
                ("HOME", Some(home_val.as_str())),
                ("XDG_CONFIG_HOME", None::<&str>),
                ("SUDO_UID", None::<&str>),
            ],
            || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
                let _ = fs::remove_dir_all(&dir);
                if let Err(e) = result {
                    std::panic::resume_unwind(e);
                }
            },
        );

        drop(lock);
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        with_isolated_home(|_| {
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.monitored.path.to_string_lossy(),
                "~/.local/share/fsmon/monitored.jsonl"
            );
            assert_eq!(
                cfg.logging.path,
                Some(PathBuf::from("~/.local/state/fsmon"))
            );
            assert_eq!(cfg.socket.path.to_string_lossy(), "/tmp/fsmon-<UID>.sock");
        });
    }

    #[test]
    fn test_load_reads_existing_file() {
        with_isolated_home(|_| {
            // Write a config file
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            let content = r#"[monitored]
path = "/custom/monitored.jsonl"

[logging]
path = "/custom/logs"

[socket]
path = "/tmp/custom.sock"
"#;
            fs::write(&config_path, content).unwrap();

            let cfg = Config::load().unwrap();
            assert_eq!(cfg.monitored.path, PathBuf::from("/custom/monitored.jsonl"));
            assert_eq!(cfg.logging.path, Some(PathBuf::from("/custom/logs")));
            assert_eq!(cfg.socket.path, PathBuf::from("/tmp/custom.sock"));
        });
    }

    #[test]
    fn test_load_invalid_config_returns_error_for_bad_toml() {
        with_isolated_home(|_| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            // Write invalid TOML (not comment-only)
            fs::write(&config_path, "garbage [[[").unwrap();

            // Should error
            assert!(Config::load().is_err());

            // File should be untouched
            let content = fs::read_to_string(&config_path).unwrap();
            assert_eq!(content.trim(), "garbage [[[");
        });
    }

    #[test]
    fn test_load_empty_file_returns_defaults() {
        with_isolated_home(|_| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            // Empty file or comment-only → return defaults (same as no file)
            fs::write(&config_path, "").unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.monitored.path.to_string_lossy(),
                "~/.local/share/fsmon/monitored.jsonl"
            );
        });
    }

    #[test]
    fn test_resolve_paths_expands_tilde_and_uid() {
        with_isolated_home(|home| {
            let mut cfg = Config::default();
            // Set log path explicitly for test (default is None now)
            cfg.logging.path = Some(PathBuf::from("~/.local/state/fsmon"));
            cfg.resolve_paths().unwrap();

            let home_str = home.to_string_lossy();
            assert!(
                cfg.monitored.path.to_string_lossy().starts_with(&*home_str),
                "monitored.path should start with home dir: {} vs {}",
                cfg.monitored.path.display(),
                home_str
            );
            assert!(
                cfg.logging
                    .path
                    .as_ref()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&*home_str),
                "logging.path should start with home dir"
            );
            assert!(
                cfg.socket.path.to_string_lossy().contains("/tmp/fsmon-"),
                "socket should contain /tmp/fsmon-"
            );
            assert!(
                !cfg.socket.path.to_string_lossy().contains("<UID>"),
                "socket should not contain <UID> placeholder"
            );
        });
    }

    #[test]
    fn test_config_path_uses_xdg_config_home() {
        let _lock = ENV_LOCK.lock().unwrap();

        temp_env::with_vars(
            [
                ("XDG_CONFIG_HOME", Some("/custom/xdg/config")),
                ("HOME", Some("/home/test")),
            ],
            || {
                let path = Config::path();
                assert!(
                    path.to_string_lossy()
                        .contains("/custom/xdg/config/fsmon/fsmon.toml")
                );

                temp_env::with_var_unset("XDG_CONFIG_HOME", || {
                    let path = Config::path();
                    assert!(
                        path.to_string_lossy()
                            .contains("/home/test/.config/fsmon/fsmon.toml")
                    );
                });
            },
        );
    }

    #[test]
    fn test_init_dirs_creates_directories() {
        with_isolated_home(|home| {
            Config::init_dirs().unwrap();

            let log_dir = home.join(".local/state/fsmon");
            let monitored_dir = home.join(".local/share/fsmon");
            let config_file = home.join(".config/fsmon/fsmon.toml");

            assert!(
                !log_dir.exists(),
                "log dir should not exist (init only creates config)"
            );
            assert!(
                !monitored_dir.exists(),
                "monitored dir should not exist (init only creates config)"
            );
            assert!(
                config_file.exists(),
                "config file should be created by init"
            );
            // Config should load from the new file (comment-only → defaults)
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.monitored.path.to_string_lossy(),
                "~/.local/share/fsmon/monitored.jsonl"
            );
        });
    }

    #[test]
    fn test_init_dirs_uses_config_when_present() {
        with_isolated_home(|home| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            // Write config with default paths
            fs::write(
                &config_path,
                r#"[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
path = "~/.local/state/fsmon"

[socket]
path = "/tmp/fsmon-<UID>.sock"
"#,
            )
            .unwrap();

            Config::init_dirs().unwrap();

            let log_dir = home.join(".local/state/fsmon");
            let monitored_dir = home.join(".local/share/fsmon");
            assert!(
                !log_dir.exists(),
                "log dir should not exist (init only creates config)"
            );
            assert!(
                !monitored_dir.exists(),
                "monitored dir should not exist (init only creates config)"
            );
        });
    }

    #[test]
    fn test_init_dirs_uses_custom_config_paths() {
        with_isolated_home(|home| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            let custom_log = home.join("my_logs");
            let custom_monitored_dir = home.join("my_data");
            let _custom_monitored_file = custom_monitored_dir.join("paths.jsonl");
            let content = format!(
                r#"[monitored]
path = "{}/my_data/paths.jsonl"

[logging]
path = "{}/my_logs"

[socket]
path = "/tmp/test.sock"
"#,
                home.to_string_lossy(),
                home.to_string_lossy(),
            );
            fs::write(&config_path, content).unwrap();

            Config::init_dirs().unwrap();

            assert!(!custom_log.exists(), "init only creates config, not dirs");
            assert!(
                !custom_monitored_dir.exists(),
                "init only creates config, not dirs"
            );
        });
    }

    #[test]
    fn test_resolve_uid_no_sudo() {
        // Without SUDO_UID, resolve_uid returns our own UID
        let _lock = ENV_LOCK.lock().unwrap();
        temp_env::with_var_unset("SUDO_UID", || {
            let uid = resolve_uid();
            assert_eq!(uid, nix::unistd::geteuid().as_raw());
        });
    }

    #[test]
    fn test_expand_tilde_basic() {
        assert_eq!(
            expand_tilde(Path::new("~/foo/bar"), "/home/user"),
            PathBuf::from("/home/user/foo/bar")
        );
        assert_eq!(
            expand_tilde(Path::new("~"), "/home/user"),
            PathBuf::from("/home/user")
        );
        assert_eq!(
            expand_tilde(Path::new("/absolute/path"), "/home/user"),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn test_cache_config_defaults() {
        let r = ResolvedCacheConfig::default();
        assert_eq!(r.dir_capacity, crate::fid_parser::DIR_CACHE_CAP);
        assert_eq!(r.dir_ttl_secs, crate::fid_parser::DIR_CACHE_TTL_SECS);
        assert_eq!(r.file_size_capacity, crate::fid_parser::FILE_SIZE_CACHE_CAP);
        assert_eq!(r.proc_ttl_secs, crate::proc_cache::PROC_CACHE_TTL_SECS);
        assert_eq!(r.buffer_size, 4096 * 8);
        assert_eq!(r.stats_interval_secs, 60);
    }

    #[test]
    fn test_cache_config_resolve_with_cli_override() {
        // Config empty, CLI overrides → CLI values win
        let cfg = CacheConfig {
            dir_capacity: None,
            dir_ttl_secs: None,
            file_size_capacity: None,
            proc_ttl_secs: None,
            stats_interval_secs: None,
            channel_capacity: None,
            subscribe_buf: None,
        };
        let cli = CliCacheOverride {
            dir_capacity: Some(50000),
            dir_ttl_secs: Some(7200),
            file_size_capacity: Some(5000),
            proc_ttl_secs: Some(300),
            stats_interval_secs: Some(30),
            buffer_size: Some(65536),
            channel_capacity: None,
            subscribe_buf: None,
        };
        let r = cfg.resolve_with_cli(&cli);
        assert_eq!(r.dir_capacity, 50000);
        assert_eq!(r.dir_ttl_secs, 7200);
        assert_eq!(r.file_size_capacity, 5000);
        assert_eq!(r.proc_ttl_secs, 300);
        assert_eq!(r.stats_interval_secs, 30);
        assert_eq!(r.buffer_size, 65536);
    }

    #[test]
    fn test_cache_config_resolve_config_over_default() {
        // Config has values, CLI empty → config values win
        let cfg = CacheConfig {
            dir_capacity: Some(200000),
            dir_ttl_secs: None,
            file_size_capacity: Some(20000),
            proc_ttl_secs: None,
            stats_interval_secs: None,
            channel_capacity: None,
            subscribe_buf: None,
        };
        let cli = CliCacheOverride::default();
        let r = cfg.resolve_with_cli(&cli);
        assert_eq!(r.dir_capacity, 200000);
        assert_eq!(r.dir_ttl_secs, crate::fid_parser::DIR_CACHE_TTL_SECS);
        assert_eq!(r.file_size_capacity, 20000);
        assert_eq!(r.proc_ttl_secs, crate::proc_cache::PROC_CACHE_TTL_SECS);
    }

    #[test]
    fn test_cache_config_cli_highest_priority() {
        // Both config and CLI have values → CLI wins
        let cfg = CacheConfig {
            dir_capacity: Some(50000),
            dir_ttl_secs: Some(100),
            file_size_capacity: Some(500),
            proc_ttl_secs: Some(50),
            stats_interval_secs: None,
            channel_capacity: None,
            subscribe_buf: None,
        };
        let cli = CliCacheOverride {
            dir_capacity: Some(99999),
            dir_ttl_secs: None,
            file_size_capacity: Some(999),
            proc_ttl_secs: None,
            stats_interval_secs: Some(120),
            buffer_size: None,
            channel_capacity: None,
            subscribe_buf: None,
        };
        let r = cfg.resolve_with_cli(&cli);
        assert_eq!(r.dir_capacity, 99999); // CLI wins
        assert_eq!(r.dir_ttl_secs, 100); // Config (CLI didn't set)
        assert_eq!(r.file_size_capacity, 999); // CLI wins
        assert_eq!(r.proc_ttl_secs, 50); // Config (CLI didn't set)
    }

    #[test]
    fn test_cache_config_toml_parsing() {
        // Verify that the TOML config can be parsed with [cache] section
        let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[socket]
path = "/tmp/sock"

[cache]
dir_capacity = 123456
dir_ttl_secs = 7200
file_size_capacity = 5000
proc_ttl_secs = 300
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        let cache = cfg.cache.expect("cache section should be parsed");
        assert_eq!(cache.dir_capacity, Some(123456));
        assert_eq!(cache.dir_ttl_secs, Some(7200));
        assert_eq!(cache.file_size_capacity, Some(5000));
        assert_eq!(cache.proc_ttl_secs, Some(300));
    }
}
