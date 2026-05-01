use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub monitor: Option<MonitorConfig>,
    pub query: Option<QueryConfig>,
    pub clean: Option<CleanConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MonitorConfig {
    pub paths: Option<Vec<PathBuf>>,
    pub min_size: Option<String>,
    pub types: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
    pub output: Option<PathBuf>,
    pub format: Option<String>,
    pub recursive: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryConfig {
    pub log_file: Option<PathBuf>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub pid: Option<String>,
    pub cmd: Option<String>,
    pub user: Option<String>,
    pub types: Option<String>,
    pub min_size: Option<String>,
    pub format: Option<String>,
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CleanConfig {
    pub log_file: Option<PathBuf>,
    pub keep_days: Option<u32>,
    pub max_size: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        match Self::find_config_file() {
            Some(path) => Self::load_from_path(&path),
            None => Ok(Config::default()),
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Failed to read config {}: {}", path.display(), e)
        })?;
        let config: Config = toml::from_str(&content).map_err(|e| {
            anyhow::anyhow!("Invalid TOML in {}: {}", path.display(), e)
        })?;
        Ok(config)
    }

    fn find_config_file() -> Option<PathBuf> {
        if let Some(home) = dirs::home_dir() {
            let home_config = home.join(".fsmon").join("config.toml");
            if home_config.exists() {
                return Some(home_config);
            }
        }

        let etc_config = PathBuf::from("/etc/fsmon/config.toml");
        if etc_config.exists() {
            return Some(etc_config);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_config_load_nonexistent() {
        let config = Config::load().unwrap();
        assert!(config.monitor.is_none());
        assert!(config.query.is_none());
        assert!(config.clean.is_none());
    }

    #[test]
    fn test_config_load_valid() {
        let dir = std::env::temp_dir().join("fsmon_test_config_valid");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");

        let toml_content = r#"
[monitor]
paths = ["/var/log", "/tmp"]
min_size = "100MB"
types = "MODIFY,CREATE"
exclude = "*.tmp"
all_events = true
output = "/var/log/fsmon.log"
format = "json"
recursive = true

[query]
log_file = "/var/log/fsmon.log"
since = "1h"
format = "json"
sort = "size"

[clean]
keep_days = 7
max_size = "500MB"
"#;

        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_path(&config_path).unwrap();

        let monitor = config.monitor.unwrap();
        assert_eq!(monitor.paths.unwrap().len(), 2);
        assert_eq!(monitor.min_size.unwrap(), "100MB");
        assert_eq!(monitor.types.unwrap(), "MODIFY,CREATE");
        assert_eq!(monitor.exclude.unwrap(), "*.tmp");
        assert!(monitor.all_events.unwrap());
        assert_eq!(monitor.output.unwrap(), PathBuf::from("/var/log/fsmon.log"));
        assert_eq!(monitor.format.unwrap(), "json");
        assert!(monitor.recursive.unwrap());

        let query = config.query.unwrap();
        assert_eq!(query.log_file.unwrap(), PathBuf::from("/var/log/fsmon.log"));
        assert_eq!(query.since.unwrap(), "1h");
        assert_eq!(query.format.unwrap(), "json");
        assert_eq!(query.sort.unwrap(), "size");

        let clean = config.clean.unwrap();
        assert_eq!(clean.keep_days.unwrap(), 7);
        assert_eq!(clean.max_size.unwrap(), "500MB");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_load_invalid() {
        let dir = std::env::temp_dir().join("fsmon_test_config_invalid");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");

        let invalid_toml = "this is not valid toml [[[[";

        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(invalid_toml.as_bytes()).unwrap();

        let result = Config::load_from_path(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid TOML"), "error should mention invalid TOML: {err_msg}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_merge_cli_overrides() {
        let toml_content = r#"
[monitor]
paths = ["/var/log"]
min_size = "100MB"
types = "MODIFY"
"#;

        let config: Config = toml::from_str(toml_content).unwrap();
        let monitor = config.monitor.as_ref().unwrap();

        assert_eq!(monitor.paths.as_ref().unwrap(), &vec![PathBuf::from("/var/log")]);
        assert_eq!(monitor.min_size.as_deref(), Some("100MB"));
        assert_eq!(monitor.types.as_deref(), Some("MODIFY"));

        let cli_min_size: Option<String> = Some("50MB".to_string());
        let merged_min_size = cli_min_size.or(monitor.min_size.clone());
        assert_eq!(merged_min_size.as_deref(), Some("50MB"));

        let cli_types: Option<String> = None;
        let merged_types = cli_types.or(monitor.types.clone());
        assert_eq!(merged_types.as_deref(), Some("MODIFY"));
    }
}
