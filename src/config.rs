use anyhow::{Context, Result};
use users::os::unix::UserExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Infrastructure configuration for fsmon.
///
/// The config file lives at `~/.config/fsmon/config.toml`.
/// All path resolution is based on the **original user** (not root's HOME).
/// Daemon (running as root via sudo) uses SUDO_UID to find the right home.
/// CLI (running as user) uses the user's own HOME directly.
///
/// This file is manually edited. Only infrastructure paths go here.
/// Monitored path entries are stored in the separate store file (see `[managed].file`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub managed: ManagedConfig,
    pub logging: LoggingConfig,
    pub socket: SocketConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedConfig {
    pub file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub dir: PathBuf,
    /// Keep log entries for at most this many days (default: 30).
    pub keep_days: Option<u32>,
    /// Maximum size per log file before truncation.
    pub max_size: Option<String>,
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
                file: PathBuf::from("~/.local/share/fsmon/managed.jsonl"),
            },
            logging: LoggingConfig {
                dir: PathBuf::from("~/.local/state/fsmon"),
                keep_days: None,
                max_size: None,
            },
            socket: SocketConfig {
                path: PathBuf::from("/tmp/fsmon-<UID>.sock"),
            },
        }
    }
}

impl Config {
    /// Return the config file path: `$XDG_CONFIG_HOME/fsmon/config.toml`
    /// Falls back to `~/.config/fsmon/config.toml`.
    pub fn path() -> PathBuf {
        let home = guess_home();
        let xdg_config =
            std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
        PathBuf::from(xdg_config).join("fsmon").join("config.toml")
    }

    /// Load config from file. Returns default Config if file doesn't exist.
    /// If the file exists but is invalid, overwrites with fresh defaults.
    pub fn load() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&p)
            .with_context(|| format!("Failed to read config {}", p.display()))?;
        match toml::from_str::<Config>(&content) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                eprintln!(
                    "[WARNING] Invalid config file at {}, overwriting with defaults.\n  Reason: {}",
                    p.display(),
                    e
                );
                Self::generate_default()?;
                Ok(Config::default())
            }
        }
    }

    /// Expand `~` in all paths using the original user's home directory.
    /// Replace `<UID>` in socket path with the actual numeric UID.
    pub fn resolve_paths(&mut self) -> Result<()> {
        let home = guess_home();
        let uid = resolve_uid();

        self.managed.file = expand_tilde(&self.managed.file, &home);
        self.logging.dir = expand_tilde(&self.logging.dir, &home);

        let socket_str = self.socket.path.to_string_lossy().to_string();
        self.socket.path = PathBuf::from(socket_str.replace("<UID>", &uid.to_string()));
        // Also expand tilde in socket path if present
        self.socket.path = expand_tilde(&self.socket.path, &home);

        Ok(())
    }

    /// Generate a default configuration file at Config::path().
    /// Creates parent directories if needed.
    pub fn generate_default() -> Result<()> {
        let path = Self::path();
        let parent = path.parent().context("Config path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let content = r#"# fsmon configuration file
#
# Infrastructure paths for fsmon. Monitored paths are managed separately
# via 'fsmon add' / 'fsmon remove' and persisted in [managed].file.
# All paths support ~ expansion. <UID> is replaced with the numeric UID at runtime.
#
# The defaults work out of the box. Change only if you need custom locations.

[managed]
# Path to the auto-managed monitored paths database.
file = "~/.local/share/fsmon/managed.jsonl"

[logging]
# Directory containing per-path log files (named by path hash).
dir = "~/.local/state/fsmon"
# Defaults for 'fsmon clean' (not auto-cleaned by daemon; use cron/timer).
#   keep_days: delete entries older than N days
#   max_size:  truncate log file when exceeding this size
# Both can be overridden at runtime:
#   fsmon clean --keep-days 7 --max-size 500MB
keep_days = 30
max_size = "1GB"

[socket]
# Unix socket path for daemon-CLI live communication.
path = "/tmp/fsmon-<UID>.sock"
"#;
        fs::write(&path, content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;

        // Chown to original user if running as root (daemon via sudo)
        chown_to_original_user(&path);
        if let Some(parent) = path.parent() {
            chown_to_original_user(parent);
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
    fn with_isolated_home(f: impl FnOnce(&Path)) {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = unique_home_dir();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".config/fsmon")).unwrap();

        let old_home = std::env::var("HOME").ok();
        let old_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
        let old_sudo_uid = std::env::var("SUDO_UID").ok();

        unsafe {
            std::env::set_var("HOME", dir.to_str().unwrap());
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("SUDO_UID");
        }

        f(&dir);

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
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        with_isolated_home(|_| {
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.managed.file.to_string_lossy(),
                "~/.local/share/fsmon/managed.jsonl"
            );
            assert_eq!(cfg.logging.dir.to_string_lossy(), "~/.local/state/fsmon");
            assert_eq!(cfg.socket.path.to_string_lossy(), "/tmp/fsmon-<UID>.sock");
        });
    }

    #[test]
    fn test_load_reads_existing_file() {
        with_isolated_home(|_| {
            // Write a config file
            let content = r#"[managed]
file = "/custom/managed.jsonl"

[logging]
dir = "/custom/logs"

[socket]
path = "/tmp/custom.sock"
"#;
            fs::write(Config::path(), content).unwrap();

            let cfg = Config::load().unwrap();
            assert_eq!(cfg.managed.file, PathBuf::from("/custom/managed.jsonl"));
            assert_eq!(cfg.logging.dir, PathBuf::from("/custom/logs"));
            assert_eq!(cfg.socket.path, PathBuf::from("/tmp/custom.sock"));
        });
    }

    #[test]
    fn test_resolve_paths_expands_tilde_and_uid() {
        with_isolated_home(|home| {
            let mut cfg = Config::default();
            cfg.resolve_paths().unwrap();

            let home_str = home.to_string_lossy();
            assert!(
                cfg.managed.file.to_string_lossy().starts_with(&*home_str),
                "managed.file should start with home dir: {} vs {}",
                cfg.managed.file.display(),
                home_str
            );
            assert!(
                cfg.logging.dir.to_string_lossy().starts_with(&*home_str),
                "logging.dir should start with home dir"
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
    fn test_generate_default_creates_valid_config() {
        with_isolated_home(|_| {
            let path = Config::path();
            assert!(!path.exists(), "config should not exist before generate");

            Config::generate_default().unwrap();
            assert!(path.exists(), "config should exist after generate");

            // Must be parseable
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.managed.file.to_string_lossy(),
                "~/.local/share/fsmon/managed.jsonl"
            );
            assert_eq!(cfg.logging.dir.to_string_lossy(), "~/.local/state/fsmon");
            assert_eq!(cfg.socket.path.to_string_lossy(), "/tmp/fsmon-<UID>.sock");
        });
    }

    #[test]
    fn test_generate_default_overwrites_without_error() {
        with_isolated_home(|_| {
            Config::generate_default().unwrap();
            // Generate again — should overwrite without error
            Config::generate_default().unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.managed.file.to_string_lossy(),
                "~/.local/share/fsmon/managed.jsonl"
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
                .contains("/custom/xdg/config/fsmon/config.toml")
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let path = Config::path();
        assert!(
            path.to_string_lossy()
                .contains("/home/test/.config/fsmon/config.toml")
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
