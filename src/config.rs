use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// User-managed config storing monitored paths.
///
/// The config file lives at `~/.config/fsmon/config.toml`.
/// All path resolution is based on the **original user** (not root's HOME).
/// Daemon (running as root via sudo) uses SUDO_UID to find the right home.
/// CLI (running as user) uses the user's own HOME directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub paths: Vec<PathEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    pub path: PathBuf,
    pub recursive: Option<bool>,
    pub types: Option<Vec<String>>,
    pub min_size: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
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
    unsafe { libc::geteuid() }
}

/// Resolve the original user's home directory using platform password database.
/// Used by the daemon (running as root) to find the user's config/log paths.
pub fn resolve_home(uid: u32) -> Result<PathBuf> {
    // SAFETY: getpwuid_r is reentrant and thread-safe
    let bufsize = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let bufsize = if bufsize > 0 { bufsize as usize } else { 4096 };
    let mut buf = vec![0u8; bufsize];
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut result: *mut libc::passwd = std::ptr::null_mut();

    let ret = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            bufsize,
            &mut result,
        )
    };

    if ret != 0 || result.is_null() {
        anyhow::bail!(
            "Failed to look up home directory for UID {} (errno: {})",
            uid,
            ret
        );
    }

    // SAFETY: result is non-null and points to initialized passwd struct
    let home_ptr = unsafe { (*result).pw_dir };
    if home_ptr.is_null() {
        anyhow::bail!("Home directory not set for UID {}", uid);
    }
    // SAFETY: pw_dir is a valid C string
    let home = unsafe { std::ffi::CStr::from_ptr(home_ptr) }
        .to_string_lossy()
        .into_owned();
    Ok(PathBuf::from(home))
}

impl UserConfig {
    /// Return the user config path: `$XDG_CONFIG_HOME/fsmon/config.toml`
    /// Falls back to `~/.config/fsmon/config.toml`.
    pub fn path() -> PathBuf {
        let home = Self::guess_home();
        let xdg_config =
            std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
        PathBuf::from(xdg_config).join("fsmon").join("config.toml")
    }

    /// Best-effort guess of user's home directory.
    /// Used by CLI commands (running as user, HOME is correct).
    /// For daemon (root via sudo), use SUDO_UID + getpwuid.
    /// For tests, use HOME env.
    fn guess_home() -> String {
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
        if unsafe { libc::geteuid() } != 0 {
            return std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        }
        match resolve_home(uid) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
        }
    }

    /// Load user paths config. Returns empty config if file doesn't exist.
    pub fn load() -> Result<Self> {
        let p = Self::path();
        if p.exists() {
            let content = fs::read_to_string(&p)
                .with_context(|| format!("Failed to read user config {}", p.display()))?;
            let cfg: UserConfig = toml::from_str(&content)
                .with_context(|| format!("Invalid TOML in {}", p.display()))?;
            Ok(cfg)
        } else {
            Ok(UserConfig { paths: vec![] })
        }
    }

    fn save_to(path: &Path, cfg: &UserConfig) -> Result<()> {
        let parent = path.parent().context("Config path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let content = toml::to_string_pretty(cfg).context("Failed to serialize user config")?;
        fs::write(path, content)
            .with_context(|| format!("Failed to write user config to {}", path.display()))?;
        Ok(())
    }

    /// Save config to the user's config path.
    pub fn save(cfg: &UserConfig) -> Result<()> {
        Self::save_to(&Self::path(), cfg)
    }

    /// Add (or replace) a path entry in the user config.
    pub fn add_path(entry: PathEntry) -> Result<()> {
        let mut cfg = Self::load()?;
        cfg.paths.retain(|p| p.path != entry.path);
        cfg.paths.push(entry);
        cfg.paths.sort_by(|a, b| a.path.cmp(&b.path));
        Self::save(&cfg)
    }

    /// Remove a path entry from the user config.
    pub fn remove_path(path: &Path) -> Result<()> {
        let mut cfg = Self::load()?;
        cfg.paths.retain(|p| p.path != path);
        Self::save(&cfg)
    }

    /// Default log file path: `~/.local/state/fsmon/history.log`
    pub fn default_log_file() -> PathBuf {
        let home = Self::guess_home();
        let xdg_state =
            std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{}/.local/state", home));
        PathBuf::from(xdg_state).join("fsmon").join("history.log")
    }

    /// Default socket path: `/tmp/fsmon-<UID>.sock` with 0666 permissions
    pub fn default_socket_path() -> PathBuf {
        let uid = resolve_uid();
        PathBuf::from("/tmp").join(format!("fsmon-{}.sock", uid))
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

    /// Override HOME + SUDO_UID for test isolation.
    /// Sets SUDO_UID to simulate daemon running under sudo,
    /// but since the process is not root, guess_home() falls back to HOME.
    fn with_isolated_env(f: impl FnOnce()) {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = unique_home_dir();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".config/fsmon")).unwrap();
        fs::create_dir_all(dir.join(".local/state")).unwrap();

        let old_home = std::env::var("HOME").ok();
        let old_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
        let old_xdg_state = std::env::var("XDG_STATE_HOME").ok();
        let old_sudo_uid = std::env::var("SUDO_UID").ok();

        unsafe {
            std::env::set_var("HOME", dir.to_str().unwrap());
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_STATE_HOME");
            std::env::remove_var("SUDO_UID");
        }
        f();
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
            if let Some(v) = old_xdg_state {
                std::env::set_var("XDG_STATE_HOME", v);
            } else {
                std::env::remove_var("XDG_STATE_HOME");
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
    fn test_load_empty_when_no_file() {
        with_isolated_env(|| {
            let cfg = UserConfig::load().unwrap();
            assert!(cfg.paths.is_empty());
        });
    }

    #[test]
    fn test_add_and_remove_path() {
        with_isolated_env(|| {
            let initial = UserConfig::load().unwrap();
            assert!(
                initial.paths.is_empty(),
                "expected empty, got {}",
                initial.paths.len()
            );

            let e1 = PathEntry {
                path: PathBuf::from("/tmp"),
                recursive: Some(true),
                types: None,
                min_size: None,
                exclude: None,
                all_events: None,
            };
            UserConfig::add_path(e1.clone()).unwrap();
            let cfg = UserConfig::load().unwrap();
            assert_eq!(cfg.paths.len(), 1);
            assert_eq!(cfg.paths[0].path, PathBuf::from("/tmp"));

            // Add same path again (should replace)
            let e2 = PathEntry {
                path: PathBuf::from("/tmp"),
                recursive: Some(false),
                types: Some(vec!["CREATE".into()]),
                min_size: None,
                exclude: None,
                all_events: None,
            };
            UserConfig::add_path(e2).unwrap();
            let cfg = UserConfig::load().unwrap();
            assert_eq!(cfg.paths.len(), 1);
            assert_eq!(cfg.paths[0].recursive, Some(false));

            UserConfig::remove_path(Path::new("/tmp")).unwrap();
            let cfg = UserConfig::load().unwrap();
            assert!(cfg.paths.is_empty());
        });
    }

    #[test]
    fn test_default_paths() {
        with_isolated_env(|| {
            let log = UserConfig::default_log_file();
            assert!(
                log.to_string_lossy()
                    .contains(".local/state/fsmon/history.log"),
                "log path: {}",
                log.display()
            );

            let sock = UserConfig::default_socket_path();
            assert!(
                sock.to_string_lossy().contains("/tmp/fsmon-"),
                "socket path: {}",
                sock.display()
            );
            // resolve_uid() returns our UID (not root)
            assert!(
                sock.to_string_lossy()
                    .contains(&format!("fsmon-{}", unsafe { libc::geteuid() })),
                "socket path should contain UID: {}",
                sock.display()
            );
        });
    }

    #[test]
    fn test_toml_round_trip() {
        with_isolated_env(|| {
            let cfg = UserConfig {
                paths: vec![PathEntry {
                    path: PathBuf::from("/srv"),
                    recursive: Some(true),
                    types: Some(vec!["MODIFY".to_string()]),
                    min_size: None,
                    exclude: Some("*.log".to_string()),
                    all_events: Some(false),
                }],
            };
            UserConfig::save(&cfg).unwrap();
            let loaded = UserConfig::load().unwrap();
            assert_eq!(loaded.paths.len(), 1);
            assert_eq!(loaded.paths[0].path, PathBuf::from("/srv"));
            assert_eq!(loaded.paths[0].recursive, Some(true));
            assert_eq!(loaded.paths[0].types.as_ref().unwrap(), &["MODIFY"]);
            assert_eq!(loaded.paths[0].exclude.as_ref().unwrap(), "*.log");
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
        assert_eq!(uid, unsafe { libc::geteuid() });
        // Restore
        if let Some(v) = old {
            unsafe {
                std::env::set_var("SUDO_UID", v);
            }
        }
    }
}
