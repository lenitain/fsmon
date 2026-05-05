use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const INSTANCE_CONFIG_DIR: &str = "/etc/fsmon";

pub const INSTANCE_CONFIG_TEMPLATE: &str = r#"# fsmon instance configuration
# Place this file at /etc/fsmon/fsmon-{name}.toml

# Directories/files to monitor (required)
paths = []

# Event log file path (omit to skip file logging — events go to journald only)
# output = "/var/log/fsmon/{name}.log"

# Minimum file size change to report (supports KB, MB, GB suffixes)
# min_size = "100MB"

# Comma-separated event types to filter:
# ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC,
# ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF
# types = "MODIFY,CREATE"

# Glob patterns to exclude from monitoring
# exclude = "*.tmp"

# Capture all 14 fanotify events
all_events = false

# Watch subdirectories recursively
recursive = false
"#;

pub const DEFAULT_CONFIG_TEMPLATE: &str = "# fsmon configuration file\n\
# See https://github.com/lenitain/fsmon for full documentation\n\
\n\
[monitor]\n\
# Directories to watch for filesystem events\n\
paths = []\n\
\n\
# Minimum file size change to report (supports KB, MB, GB suffixes, e.g. \"100MB\", \"1GB\")\n\
# min_size = \"100MB\"\n\
\n\
# Comma-separated event types to filter: ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE,\n\
# OPEN, OPEN_EXEC, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF\n\
# types = \"MODIFY,CREATE\"\n\
\n\
# Glob patterns to exclude from monitoring\n\
# exclude = \"*.tmp\"\n\
\n\
# Report all 14 event types regardless of the 'types' filter\n\
all_events = false\n\
\n\
# Path to the event log file\n\
# output = \"/var/log/fsmon.log\"\n\
\n\
# Terminal output format: \"human\", \"json\", or \"csv\" (affects stdout only; log file is always JSON)\n\
format = \"human\"\n\
\n\
# Watch subdirectories recursively\n\
recursive = false\n\
\n\
# Fanotify read buffer size in bytes\n\
buffer_size = 32768\n\
\n\
[query]\n\
# Event log file to query\n\
# log_file = \"/var/log/fsmon.log\"\n\
\n\
# Start time: relative (\"1h\", \"30m\", \"7d\") or absolute (\"2024-05-01 10:00\")\n\
# since = \"1h\"\n\
\n\
# End time: same format as since\n\
# until = \"2h\"\n\
\n\
# Filter by process IDs (comma-separated)\n\
# pid = \"1234,5678\"\n\
\n\
# Filter by process name (wildcard support: nginx*, python)\n\
# cmd = \"nginx\"\n\
\n\
# Filter by usernames (comma-separated)\n\
# user = \"root,admin\"\n\
\n\
# Filter by event types (comma-separated)\n\
# types = \"MODIFY,CREATE\"\n\
\n\
# Minimum size change to include\n\
# min_size = \"100MB\"\n\
\n\
# Terminal output format: \"human\", \"json\", or \"csv\" (affects stdout only; log file is always JSON)\n\
format = \"human\"\n\
\n\
# Sort results: \"time\", \"size\", or \"pid\"\n\
sort = \"time\"\n\
\n\
[clean]\n\
# Event log file to clean\n\
# log_file = \"/var/log/fsmon.log\"\n\
\n\
# Number of days to retain log entries\n\
keep_days = 30\n\
\n\
# Maximum log file size before tail truncation (e.g. \"100MB\", \"1GB\")\n\
# max_size = \"500MB\"\n\
\n\
[install]\n\
# systemd ProtectSystem value (\"yes\", \"no\", \"strict\", \"full\")\n\
protect_system = \"strict\"\n\
\n\
# systemd ProtectHome value (\"yes\", \"no\", \"read-only\")\n\
protect_home = \"read-only\"\n\
\n\
# Additional read-write paths for systemd service (used when ProtectSystem is strict)\n\
read_write_paths = [\"/var/log\"]\n\
\n\
# systemd PrivateTmp value (\"yes\" or \"no\")\n\
private_tmp = \"yes\"\n";
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub monitor: Option<MonitorConfig>,
    pub query: Option<QueryConfig>,
    pub clean: Option<CleanConfig>,
    pub install: Option<InstallConfig>,
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
    pub buffer_size: Option<usize>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallConfig {
    pub protect_system: Option<String>,
    pub protect_home: Option<String>,
    pub read_write_paths: Option<Vec<String>>,
    pub private_tmp: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        match Self::find_config_file() {
            Some(path) => Self::load_from_path(&path),
            None => Ok(Config::default()),
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid TOML in {}: {}", path.display(), e))?;
        Ok(config)
    }

    /// Search config files in priority order:
    ///   1. ~/.fsmon/fsmon.toml
    ///   2. ~/.config/fsmon/fsmon.toml (XDG)
    ///   3. /etc/fsmon/fsmon.toml (system-wide)
    fn find_config_file() -> Option<PathBuf> {
        let candidates = [
            dirs::home_dir().map(|h| h.join(".fsmon").join("fsmon.toml")),
            dirs::config_dir().map(|h| h.join("fsmon").join("fsmon.toml")),
            Some(PathBuf::from("/etc/fsmon/fsmon.toml")),
        ];

        candidates.into_iter().flatten().find(|path| path.exists())
    }

    /// Generate a commented default config file at the XDG path (~/.config/fsmon/fsmon.toml).
    pub fn generate(force: bool) -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("Cannot determine XDG config directory")?
            .join("fsmon");
        let config_path = config_dir.join("fsmon.toml");

        if config_path.exists() && !force {
            anyhow::bail!(
                "Config already exists at {}. Use --force to overwrite.",
                config_path.display()
            );
        }

        fs::create_dir_all(&config_dir)
            .with_context(|| format!("Failed to create {}", config_dir.display()))?;

        let content = DEFAULT_CONFIG_TEMPLATE;
        fs::write(&config_path, content)
            .with_context(|| format!("Failed to write {}", config_path.display()))?;

        println!("Generated config: {}", config_path.display());
        Ok(config_path)
    }

    /// Load instance config by name.
    /// Search order: /etc/fsmon/fsmon-{name}.toml, ~/.config/fsmon/fsmon-{name}.toml
    pub fn load_instance(name: &str) -> Result<Option<InstanceConfig>> {
        let mut candidates: Vec<PathBuf> =
            vec![PathBuf::from(INSTANCE_CONFIG_DIR).join(format!("fsmon-{}.toml", name))];
        if let Some(config_dir) = dirs::config_dir() {
            candidates.push(
                config_dir
                    .join("fsmon")
                    .join(format!("fsmon-{}.toml", name)),
            );
        }

        for path in &candidates {
            if path.exists() {
                let content = fs::read_to_string(path).with_context(|| {
                    format!("Failed to read instance config {}", path.display())
                })?;
                let config: InstanceConfig = toml::from_str(&content)
                    .with_context(|| format!("Invalid TOML in {}: {}", path.display(), content))?;
                if config.paths.is_empty() {
                    anyhow::bail!(
                        "Instance '{}' config {} has no paths configured",
                        name,
                        path.display()
                    );
                }
                return Ok(Some(config));
            }
        }
        Ok(None)
    }
}

/// Per-instance configuration for systemd template instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub paths: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub min_size: Option<String>,
    pub types: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
    pub recursive: Option<bool>,
}

/// Generate an instance config template file at /etc/fsmon/fsmon-{name}.toml.
pub fn generate_instance_config(name: &str, force: bool) -> Result<PathBuf> {
    let config_dir = PathBuf::from(INSTANCE_CONFIG_DIR);
    let config_path = config_dir.join(format!("fsmon-{}.toml", name));

    if config_path.exists() && !force {
        anyhow::bail!(
            "Instance config already exists at {}. Use --force to overwrite.",
            config_path.display()
        );
    }

    fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create {}", config_dir.display()))?;

    let content = INSTANCE_CONFIG_TEMPLATE.replace("{name}", name);
    fs::write(&config_path, &content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    println!("Generated instance config: {}", config_path.display());
    println!(
        "Edit it to set paths and options, then: systemctl enable fsmon@{} --now",
        name
    );
    Ok(config_path)
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
        assert!(config.install.is_none());
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
        assert!(
            err_msg.contains("Invalid TOML"),
            "error should mention invalid TOML: {err_msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_load_buffer_size() {
        let dir = std::env::temp_dir().join("fsmon_test_config_buffer_size");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");

        let toml_content = r#"
[monitor]
buffer_size = 65536
"#;

        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_path(&config_path).unwrap();
        let monitor = config.monitor.unwrap();
        assert_eq!(monitor.buffer_size.unwrap(), 65536);

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

        assert_eq!(
            monitor.paths.as_ref().unwrap(),
            &vec![PathBuf::from("/var/log")]
        );
        assert_eq!(monitor.min_size.as_deref(), Some("100MB"));
        assert_eq!(monitor.types.as_deref(), Some("MODIFY"));

        let cli_min_size: Option<String> = Some("50MB".to_string());
        let merged_min_size = cli_min_size.or(monitor.min_size.clone());
        assert_eq!(merged_min_size.as_deref(), Some("50MB"));

        let cli_types: Option<String> = None;
        let merged_types = cli_types.or(monitor.types.clone());
        assert_eq!(merged_types.as_deref(), Some("MODIFY"));
    }

    #[test]
    fn test_install_config() {
        let toml_content = r#"
[install]
protect_system = "false"
protect_home = "false"
read_write_paths = ["/var/log", "/tmp"]
private_tmp = "no"
"#;

        let config: Config = toml::from_str(toml_content).unwrap();
        let install = config.install.unwrap();
        assert_eq!(install.protect_system.as_deref(), Some("false"));
        assert_eq!(install.protect_home.as_deref(), Some("false"));
        assert_eq!(
            install.read_write_paths.unwrap(),
            vec!["/var/log".to_string(), "/tmp".to_string()]
        );
        assert_eq!(install.private_tmp.as_deref(), Some("no"));
    }

    #[test]
    fn test_install_config_partial() {
        let toml_content = r#"
[install]
protect_system = "false"
"#;

        let config: Config = toml::from_str(toml_content).unwrap();
        let install = config.install.unwrap();
        assert_eq!(install.protect_system.as_deref(), Some("false"));
        assert!(install.protect_home.is_none());
        assert!(install.read_write_paths.is_none());
        assert!(install.private_tmp.is_none());
    }
}
