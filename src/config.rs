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
    pub path: PathBuf,
    /// Keep log entries for at most this many days (default: 30).
    pub keep_days: Option<u32>,
    /// Maximum size per log file before truncation.
    /// Size limit per log file before truncation.
    pub size: Option<String>,
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
}

/// Resolved cache configuration with all defaults filled in.
#[derive(Debug, Clone)]
pub struct ResolvedCacheConfig {
    pub dir_capacity: u64,
    pub dir_ttl_secs: u64,
    pub file_size_capacity: usize,
    pub proc_ttl_secs: u64,
    pub buffer_size: usize,
}

impl Default for ResolvedCacheConfig {
    fn default() -> Self {
        Self {
            dir_capacity: crate::fid_parser::DIR_CACHE_CAP,
            dir_ttl_secs: crate::fid_parser::DIR_CACHE_TTL_SECS,
            file_size_capacity: crate::fid_parser::FILE_SIZE_CACHE_CAP,
            proc_ttl_secs: crate::proc_cache::PROC_CACHE_TTL_SECS,
            buffer_size: 4096 * 8, // 32KB — default from Monitor::new()
        }
    }
}

impl CacheConfig {
    /// Merge: explicit values from this config override defaults,
    /// then CLI overrides override config values.
    pub fn resolve_with_cli(
        &self,
        cli: &CliCacheOverride,
    ) -> ResolvedCacheConfig {
        let mut r = ResolvedCacheConfig::default();
        if let Some(v) = self.dir_capacity { r.dir_capacity = v; }
        if let Some(v) = self.dir_ttl_secs { r.dir_ttl_secs = v; }
        if let Some(v) = self.file_size_capacity { r.file_size_capacity = v; }
        if let Some(v) = self.proc_ttl_secs { r.proc_ttl_secs = v; }
        // Apply CLI overrides (highest priority)
        if let Some(v) = cli.dir_capacity { r.dir_capacity = v; }
        if let Some(v) = cli.dir_ttl_secs { r.dir_ttl_secs = v; }
        if let Some(v) = cli.file_size_capacity { r.file_size_capacity = v; }
        if let Some(v) = cli.proc_ttl_secs { r.proc_ttl_secs = v; }
        if let Some(v) = cli.buffer_size { r.buffer_size = v; }
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
    pub buffer_size: Option<usize>,
}

// ---- Helpers ----

/// Resolve the original user's UID and GID:
/// - If SUDO_UID is set (sudo), look up the passwd entry for that UID
/// - Otherwise use the current process UID/GID
pub fn resolve_uid_gid() -> (u32, u32) {
    let uid = if let Ok(uid_str) = std::env::var("SUDO_UID")
        && let Ok(uid) = uid_str.parse::<u32>()
    {
        uid
    } else {
        nix::unistd::geteuid().as_raw()
    };
    let gid = if let Ok(gid_str) = std::env::var("SUDO_GID")
        && let Ok(gid) = gid_str.parse::<u32>()
    {
        gid
    } else {
        nix::unistd::getegid().as_raw()
    };
    (uid, gid)
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
/// - If SUDO_UID is set (sudo), use that
/// - Otherwise use the current process UID
pub fn resolve_uid() -> u32 {
    if let Ok(uid_str) = std::env::var("SUDO_UID")
        && let Ok(uid) = uid_str.parse::<u32>()
    {
        return uid;
    }
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
                path: PathBuf::from("~/.local/state/fsmon"),
                keep_days: None,
                size: None,
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
    /// Errors if the file exists but is invalid — file is never modified.
    pub fn load() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&p)
            .with_context(|| format!("Failed to read config {}", p.display()))?;
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
        self.logging.path = expand_tilde(&self.logging.path, &home);

        let socket_str = self.socket.path.to_string_lossy().to_string();
        self.socket.path = PathBuf::from(socket_str.replace("<UID>", &uid.to_string()));
        // Also expand tilde in socket path if present
        self.socket.path = expand_tilde(&self.socket.path, &home);

        Ok(())
    }

    /// Create the default data directories (chezmoi-style init).
    /// Creates log dir and monitored data dir. Config file is optional.
    pub fn init_dirs() -> Result<()> {
        let config_path = Self::path();
        let using_defaults = !config_path.exists();

        let mut cfg = if config_path.exists() {
            Config::load()?
        } else {
            Config::default()
        };
        cfg.resolve_paths()?;

        let monitored_dir = cfg
            .monitored
            .path
            .parent()
            .context("Monitored file path has no parent")?
            .to_path_buf();

        fs::create_dir_all(&cfg.logging.path).with_context(|| {
            format!(
                "Failed to create log directory: {}",
                cfg.logging.path.display()
            )
        })?;
        fs::create_dir_all(&monitored_dir).with_context(|| {
            format!(
                "Failed to create monitored directory: {}",
                monitored_dir.display()
            )
        })?;

        // Chown to original user
        chown_to_original_user(&cfg.logging.path);
        chown_to_original_user(&monitored_dir);

        eprintln!("Created log directory:  {}", cfg.logging.path.display());
        eprintln!("Created monitored directory: {}", monitored_dir.display());
        if using_defaults {
            eprintln!(
                "(config file is optional \u{2014} defaults apply without {})",
                config_path.display()
            );
        }
        Ok(())
    }
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
            &[
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
            assert_eq!(cfg.logging.path.to_string_lossy(), "~/.local/state/fsmon");
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
            assert_eq!(cfg.logging.path, PathBuf::from("/custom/logs"));
            assert_eq!(cfg.socket.path, PathBuf::from("/tmp/custom.sock"));
        });
    }

    #[test]
    fn test_load_invalid_config_returns_error() {
        with_isolated_home(|_| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            fs::write(&config_path, "").unwrap();

            // Should error, not silently use defaults
            assert!(Config::load().is_err());

            // File should be untouched
            let content = fs::read_to_string(&config_path).unwrap();
            assert!(
                content.trim().is_empty(),
                "file content should be untouched"
            );
        });
    }

    #[test]
    fn test_resolve_paths_expands_tilde_and_uid() {
        with_isolated_home(|home| {
            let mut cfg = Config::default();
            cfg.resolve_paths().unwrap();

            let home_str = home.to_string_lossy();
            assert!(
                cfg.monitored.path.to_string_lossy().starts_with(&*home_str),
                "monitored.path should start with home dir: {} vs {}",
                cfg.monitored.path.display(),
                home_str
            );
            assert!(
                cfg.logging.path.to_string_lossy().starts_with(&*home_str),
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
            &[
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
            let config_dir = home.join(".config/fsmon");

            assert!(log_dir.exists(), "log dir should exist");
            assert!(monitored_dir.exists(), "monitored dir should exist");
            assert!(
                !config_dir.exists(),
                "config dir should NOT be created by init"
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
            assert!(log_dir.exists(), "log dir should exist");
            assert!(monitored_dir.exists(), "monitored dir should exist");
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

            assert!(custom_log.exists(), "custom log dir should exist");
            assert!(
                custom_monitored_dir.exists(),
                "custom monitored dir should exist"
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
    }

    #[test]
    fn test_cache_config_resolve_with_cli_override() {
        // Config empty, CLI overrides → CLI values win
        let cfg = CacheConfig {
            dir_capacity: None,
            dir_ttl_secs: None,
            file_size_capacity: None,
            proc_ttl_secs: None,
        };
        let cli = CliCacheOverride {
            dir_capacity: Some(50000),
            dir_ttl_secs: Some(7200),
            file_size_capacity: Some(5000),
            proc_ttl_secs: Some(300),
            buffer_size: Some(65536),
        };
        let r = cfg.resolve_with_cli(&cli);
        assert_eq!(r.dir_capacity, 50000);
        assert_eq!(r.dir_ttl_secs, 7200);
        assert_eq!(r.file_size_capacity, 5000);
        assert_eq!(r.proc_ttl_secs, 300);
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
        };
        let cli = CliCacheOverride {
            dir_capacity: Some(99999),
            dir_ttl_secs: None,
            file_size_capacity: Some(999),
            proc_ttl_secs: None,
            buffer_size: None,
        };
        let r = cfg.resolve_with_cli(&cli);
        assert_eq!(r.dir_capacity, 99999);    // CLI wins
        assert_eq!(r.dir_ttl_secs, 100);       // Config (CLI didn't set)
        assert_eq!(r.file_size_capacity, 999); // CLI wins
        assert_eq!(r.proc_ttl_secs, 50);       // Config (CLI didn't set)
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
