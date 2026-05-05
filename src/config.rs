use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// User-managed config storing monitored paths.
///
/// The config file lives at `~/.config/fsmon/config.toml` and is managed entirely
/// by the user — no root needed for add/remove/query/clean/managed.
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

impl UserConfig {
    /// Return the user config path: `$XDG_CONFIG_HOME/fsmon/config.toml`
    /// Falls back to `~/.config/fsmon/config.toml`.
    fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let xdg_config =
            std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
        PathBuf::from(xdg_config).join("fsmon").join("config.toml")
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

    pub fn save(cfg: &UserConfig) -> Result<()> {
        Self::save_to(&Self::path(), cfg)
    }

    pub fn add_path(entry: PathEntry) -> Result<()> {
        let mut cfg = Self::load()?;
        cfg.paths.retain(|p| p.path != entry.path);
        cfg.paths.push(entry);
        cfg.paths.sort_by(|a, b| a.path.cmp(&b.path));
        Self::save(&cfg)
    }

    pub fn remove_path(path: &Path) -> Result<()> {
        let mut cfg = Self::load()?;
        cfg.paths.retain(|p| p.path != path);
        Self::save(&cfg)
    }

    /// Return the daemon data directory: `$XDG_DATA_HOME/fsmon`
    /// Falls back to `~/.local/share/fsmon`.
    fn data_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let xdg_data =
            std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));
        PathBuf::from(xdg_data).join("fsmon")
    }

    /// Default log file path: `$XDG_DATA_HOME/fsmon/history.log`
    pub fn default_log_file() -> PathBuf {
        Self::data_dir().join("history.log")
    }

    /// Default socket path: `$XDG_RUNTIME_DIR/fsmon.sock` or `$XDG_DATA_HOME/fsmon/fsmon.sock`
    pub fn default_socket_path() -> PathBuf {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
        if let Some(dir) = runtime_dir {
            dir.join("fsmon.sock")
        } else {
            Self::data_dir().join("fsmon.sock")
        }
    }

    /// Migration: copy `[[paths]]` from old `/etc/fsmon/fsmon.toml` to new user config.
    /// Called once on first `fsmon daemon` run or `fsmon add` after upgrade.
    pub fn migrate_from_etc() -> Result<()> {
        let old_path = PathBuf::from("/etc/fsmon/fsmon.toml");
        if !old_path.exists() {
            return Ok(());
        }
        let new_path = Self::path();
        if new_path.exists() {
            return Ok(());
        }
        // Read old format — it may contain log_file/socket_path/paths
        #[derive(Deserialize)]
        struct OldConfig {
            paths: Option<Vec<PathEntry>>,
            #[allow(dead_code)]
            log_file: Option<PathBuf>,
            #[allow(dead_code)]
            socket_path: Option<PathBuf>,
        }
        let content = match fs::read_to_string(&old_path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let old: OldConfig = match toml::from_str(&content) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let paths = old.paths.unwrap_or_default();
        if paths.is_empty() {
            return Ok(());
        }
        let cfg = UserConfig { paths };
        Self::save_to(&new_path, &cfg)?;
        println!(
            "Migrated {} path(s) from {} to {}",
            cfg.paths.len(),
            old_path.display(),
            new_path.display()
        );
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

    /// Temporarily override HOME so UserConfig paths point into temp dir
    fn with_isolated_home(f: impl FnOnce()) {
        // SAFETY: test-only single-threaded env var manipulation
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = unique_home_dir();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".config/fsmon")).unwrap();
        fs::create_dir_all(dir.join(".local/share")).unwrap();
        let old_home = std::env::var("HOME").ok();
        let old_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
        let old_xdg_data = std::env::var("XDG_DATA_HOME").ok();
        unsafe {
            std::env::set_var("HOME", dir.to_str().unwrap());
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_DATA_HOME");
        }
        f();
        // Restore
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
            if let Some(v) = old_xdg_data {
                std::env::set_var("XDG_DATA_HOME", v);
            } else {
                std::env::remove_var("XDG_DATA_HOME");
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_load_empty_when_no_file() {
        with_isolated_home(|| {
            let cfg = UserConfig::load().unwrap();
            assert!(cfg.paths.is_empty());
        });
    }

    #[test]
    fn test_add_and_remove_path() {
        with_isolated_home(|| {
            // Load fresh — should be empty
            let initial = UserConfig::load().unwrap();
            assert!(
                initial.paths.is_empty(),
                "expected empty config, got {} paths",
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
            assert_eq!(cfg.paths.len(), 1, "expected 1 path after add");
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
    fn test_migrate_from_etc_skips_when_no_old_config() {
        // This test only works if /etc/fsmon/fsmon.toml doesn't exist
        // or if it exists but has no paths. In CI it's likely missing.
        let old_path = PathBuf::from("/etc/fsmon/fsmon.toml");
        let has_old = old_path.exists();
        if has_old {
            // Still run — migration may succeed and write paths,
            // which is fine. Just don't assert empty.
            let _ = UserConfig::migrate_from_etc();
        } else {
            UserConfig::migrate_from_etc().unwrap();
            let cfg = UserConfig::load().unwrap();
            assert!(cfg.paths.is_empty());
        }
    }

    #[test]
    fn test_default_paths() {
        with_isolated_home(|| {
            let log = UserConfig::default_log_file();
            assert!(
                log.to_string_lossy()
                    .contains(".local/share/fsmon/history.log")
            );

            // Socket path: if XDG_RUNTIME_DIR is set, uses that;
            // otherwise falls back to data dir.
            let sock = UserConfig::default_socket_path();
            let has_runtime_dir = std::env::var("XDG_RUNTIME_DIR").is_ok();
            if has_runtime_dir {
                assert!(sock.to_string_lossy().contains("fsmon.sock"));
            } else {
                assert!(
                    sock.to_string_lossy()
                        .contains(".local/share/fsmon/fsmon.sock")
                );
            }
        });
    }

    #[test]
    fn test_toml_round_trip() {
        with_isolated_home(|| {
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
}
