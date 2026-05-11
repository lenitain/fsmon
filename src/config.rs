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
/// Monitored path entries are stored in the separate store file (see `[managed].path`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub managed: ManagedConfig,
    pub logging: LoggingConfig,
    pub socket: SocketConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedConfig {
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
            managed: ManagedConfig {
                path: PathBuf::from("~/.local/share/fsmon/managed.jsonl"),
            },
            logging: LoggingConfig {
                path: PathBuf::from("~/.local/state/fsmon"),
                keep_days: None,
                size: None,
            },
            socket: SocketConfig {
                path: PathBuf::from("/tmp/fsmon-<UID>.sock"),
            },
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
            Err(e) => bail!(
                "Invalid config file at {}: {}",
                p.display(),
                e
            ),
        }
    }

    /// Expand `~` in all paths using the original user's home directory.
    /// Replace `<UID>` in socket path with the actual numeric UID.
    pub fn resolve_paths(&mut self) -> Result<()> {
        let home = guess_home();
        let uid = resolve_uid();

        self.managed.path = expand_tilde(&self.managed.path, &home);
        self.logging.path = expand_tilde(&self.logging.path, &home);

        let socket_str = self.socket.path.to_string_lossy().to_string();
        self.socket.path = PathBuf::from(socket_str.replace("<UID>", &uid.to_string()));
        // Also expand tilde in socket path if present
        self.socket.path = expand_tilde(&self.socket.path, &home);

        Ok(())
    }

    /// Create the default data directories (chezmoi-style init).
    /// Creates log dir and managed data dir. Config file is optional.
    pub fn init_dirs() -> Result<()> {
        let config_path = Self::path();
        let using_defaults = !config_path.exists();

        let mut cfg = if config_path.exists() {
            Config::load()?
        } else {
            Config::default()
        };
        cfg.resolve_paths()?;

        let managed_dir = cfg
            .managed
            .path
            .parent()
            .context("Managed file path has no parent")?
            .to_path_buf();

        fs::create_dir_all(&cfg.logging.path).with_context(|| {
            format!(
                "Failed to create log directory: {}",
                cfg.logging.path.display()
            )
        })?;
        fs::create_dir_all(&managed_dir).with_context(|| {
            format!(
                "Failed to create managed directory: {}",
                managed_dir.display()
            )
        })?;

        // Chown to original user
        chown_to_original_user(&cfg.logging.path);
        chown_to_original_user(&managed_dir);

        eprintln!("Created log directory:  {}", cfg.logging.path.display());
        eprintln!("Created managed directory: {}", managed_dir.display());
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

        let old_home = std::env::var("HOME").ok();
        let old_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
        let old_sudo_uid = std::env::var("SUDO_UID").ok();

        unsafe {
            std::env::set_var("HOME", dir.to_str().unwrap());
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("SUDO_UID");
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));

        unsafe {
            if let Some(v) = old_home {
                std::env::set_var("HOME", v);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(v) = old_xdg_config {
                std::env::set_var("XDG_CONFIG_HOME", v);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            if let Some(v) = old_sudo_uid {
                std::env::set_var("SUDO_UID", v);
            } else {
                std::env::remove_var("SUDO_UID");
            }
        }
        let _ = fs::remove_dir_all(dir);
        drop(lock);

        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        with_isolated_home(|_| {
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.managed.path.to_string_lossy(),
                "~/.local/share/fsmon/managed.jsonl"
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
            let content = r#"[managed]
path = "/custom/managed.jsonl"

[logging]
path = "/custom/logs"

[socket]
path = "/tmp/custom.sock"
"#;
            fs::write(&config_path, content).unwrap();

            let cfg = Config::load().unwrap();
            assert_eq!(cfg.managed.path, PathBuf::from("/custom/managed.jsonl"));
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
            assert!(content.trim().is_empty(), "file content should be untouched");
        });
    }

    #[test]
    fn test_resolve_paths_expands_tilde_and_uid() {
        with_isolated_home(|home| {
            let mut cfg = Config::default();
            cfg.resolve_paths().unwrap();

            let home_str = home.to_string_lossy();
            assert!(
                cfg.managed.path.to_string_lossy().starts_with(&*home_str),
                "managed.path should start with home dir: {} vs {}",
                cfg.managed.path.display(),
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
        let old = std::env::var("XDG_CONFIG_HOME").ok();
        let old_home = std::env::var("HOME").ok();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "/custom/xdg/config");
            std::env::set_var("HOME", "/home/test");
        }

        let path = Config::path();
        assert!(
            path.to_string_lossy()
                .contains("/custom/xdg/config/fsmon/fsmon.toml")
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let path = Config::path();
        assert!(
            path.to_string_lossy()
                .contains("/home/test/.config/fsmon/fsmon.toml")
        );

        // Restore
        if let Some(v) = old {
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", v);
            }
        }
        if let Some(v) = old_home {
            unsafe {
                std::env::set_var("HOME", v);
            }
        }
    }

    #[test]
    fn test_init_dirs_creates_directories() {
        with_isolated_home(|home| {
            Config::init_dirs().unwrap();

            let log_dir = home.join(".local/state/fsmon");
            let managed_dir = home.join(".local/share/fsmon");
            let config_dir = home.join(".config/fsmon");

            assert!(log_dir.exists(), "log dir should exist");
            assert!(managed_dir.exists(), "managed dir should exist");
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
            fs::write(&config_path, r#"[managed]
path = "~/.local/share/fsmon/managed.jsonl"

[logging]
path = "~/.local/state/fsmon"

[socket]
path = "/tmp/fsmon-<UID>.sock"
"#).unwrap();

            Config::init_dirs().unwrap();

            let log_dir = home.join(".local/state/fsmon");
            let managed_dir = home.join(".local/share/fsmon");
            assert!(log_dir.exists(), "log dir should exist");
            assert!(managed_dir.exists(), "managed dir should exist");
        });
    }

    #[test]
    fn test_init_dirs_uses_custom_config_paths() {
        with_isolated_home(|home| {
            let config_path = Config::path();
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            let custom_log = home.join("my_logs");
            let custom_managed_dir = home.join("my_data");
            let _custom_managed_file = custom_managed_dir.join("paths.jsonl");
            let content = format!(
                r#"[managed]
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
            assert!(custom_managed_dir.exists(), "custom managed dir should exist");
        });
    }

    #[test]
    fn test_resolve_uid_no_sudo() {
        // Without SUDO_UID, resolve_uid returns our own UID
        let _lock = ENV_LOCK.lock().unwrap();
        let old = std::env::var("SUDO_UID").ok();
        unsafe {
            std::env::remove_var("SUDO_UID");
        }
        let uid = resolve_uid();
        assert_eq!(uid, nix::unistd::geteuid().as_raw());
        // Restore
        if let Some(v) = old {
            unsafe {
                std::env::set_var("SUDO_UID", v);
            }
        }
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
}
