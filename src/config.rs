use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub log_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
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

impl Config {
    pub fn default_config_path() -> PathBuf {
        PathBuf::from("/etc/fsmon/fsmon.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::default_config_path();
        if path.exists() {
            Self::load_from_path(&path)
        } else {
            Ok(Config {
                log_file: None,
                socket_path: None,
                paths: vec![],
            })
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in {}", path.display()))?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::default_config_path();
        let parent = path.parent().context("Config path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&path, content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }

    pub fn generate_default() -> Result<()> {
        let path = Self::default_config_path();
        if path.exists() {
            return Ok(());
        }
        let config = Config {
            log_file: Some(PathBuf::from("/var/log/fsmon/history.log")),
            socket_path: Some(PathBuf::from("/var/run/fsmon/fsmon.sock")),
            paths: vec![],
        };
        config.save()
    }

    pub fn add_path(entry: PathEntry) -> Result<()> {
        let mut config = Self::load()?;
        config.paths.push(entry);
        config.save()
    }

    pub fn remove_path(path: &Path) -> Result<()> {
        let mut config = Self::load()?;
        config.paths.retain(|p| p.path != path);
        config.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_path() {
        assert_eq!(
            Config::default_config_path(),
            PathBuf::from("/etc/fsmon/fsmon.toml")
        );
    }

    #[test]
    fn test_load_from_path_valid() {
        let dir = std::env::temp_dir().join("fsmon_test_config_valid");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("fsmon.toml");

        let toml_content = r#"
log_file = "/var/log/fsmon/history.log"
socket_path = "/var/run/fsmon/fsmon.sock"

[[paths]]
path = "/var/www"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "100MB"
exclude = "*.tmp"
all_events = false
"#;

        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_path(&config_path).unwrap();
        assert_eq!(
            config.log_file,
            Some(PathBuf::from("/var/log/fsmon/history.log"))
        );
        assert_eq!(
            config.socket_path,
            Some(PathBuf::from("/var/run/fsmon/fsmon.sock"))
        );
        assert_eq!(config.paths.len(), 1);
        assert_eq!(config.paths[0].path, PathBuf::from("/var/www"));
        assert!(config.paths[0].recursive.unwrap());
        assert_eq!(
            config.paths[0].types.as_ref().unwrap(),
            &["MODIFY".to_string(), "CREATE".to_string()]
        );
        assert_eq!(config.paths[0].min_size.as_ref().unwrap(), "100MB");
        assert_eq!(config.paths[0].exclude.as_ref().unwrap(), "*.tmp");
        assert!(!config.paths[0].all_events.unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_from_path_invalid_toml() {
        let dir = std::env::temp_dir().join("fsmon_test_config_invalid");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("fsmon.toml");

        fs::write(&config_path, "invalid toml [[[").unwrap();

        let result = Config::load_from_path(&config_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid TOML"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        let dir = std::env::temp_dir().join("fsmon_test_load_default");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let config = Config {
            log_file: None,
            socket_path: None,
            paths: vec![],
        };
        assert!(config.log_file.is_none());
        assert!(config.socket_path.is_none());
        assert!(config.paths.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_toml_round_trip() {
        let config = Config {
            log_file: Some(PathBuf::from("/var/log/fsmon/history.log")),
            socket_path: Some(PathBuf::from("/var/run/fsmon/fsmon.sock")),
            paths: vec![PathEntry {
                path: PathBuf::from("/srv"),
                recursive: Some(true),
                types: Some(vec!["MODIFY".to_string()]),
                min_size: None,
                exclude: Some("*.log".to_string()),
                all_events: Some(false),
            }],
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.log_file, parsed.log_file);
        assert_eq!(config.socket_path, parsed.socket_path);
        assert_eq!(config.paths.len(), parsed.paths.len());
        assert_eq!(config.paths[0].path, parsed.paths[0].path);
        assert_eq!(config.paths[0].recursive, parsed.paths[0].recursive);
        assert_eq!(config.paths[0].types, parsed.paths[0].types);
    }

    #[test]
    fn test_empty_paths() {
        let config = Config {
            log_file: None,
            socket_path: None,
            paths: vec![],
        };
        assert!(config.paths.is_empty());
    }

    #[test]
    fn test_multiple_paths() {
        let config = Config {
            log_file: None,
            socket_path: None,
            paths: vec![
                PathEntry {
                    path: PathBuf::from("/a"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/b"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
            ],
        };
        assert_eq!(config.paths.len(), 2);
    }

    #[test]
    fn test_path_entry_all_fields_none() {
        let entry = PathEntry {
            path: PathBuf::from("/test"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        };
        assert_eq!(entry.path, PathBuf::from("/test"));
        assert!(entry.recursive.is_none());
        assert!(entry.types.is_none());
        assert!(entry.min_size.is_none());
        assert!(entry.exclude.is_none());
        assert!(entry.all_events.is_none());
    }
}
