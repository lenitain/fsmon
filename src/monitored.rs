use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::chown_to_original_user;

/// Sentinel value for the global cmd group (no specific process).
pub const CMD_GLOBAL: &str = "_global";

fn default_cmd() -> String {
    CMD_GLOBAL.to_string()
}

/// The monitored paths database, stored in the file configured by `[monitored].path`.
///
/// Monitored automatically by `fsmon add` and `fsmon remove`.
///
/// # JSONL Format (grouped by cmd)
/// Each line groups paths under a common `cmd`:
/// ```json
/// {"cmd":"bash","paths":{"/a":{"recursive":true},"/b":{"recursive":false,"types":["MODIFY"]}}}
/// {"cmd":"_global","paths":{"/c":{"recursive":true}}}
/// ```
/// `cmd` is always present. Use `"_global"` for the global group (no specific process).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Monitored {
    /// Monitored path groups, each keyed by cmd.
    pub groups: Vec<CmdGroup>,
}

/// Per-process-name group of monitored paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdGroup {
    /// Process name for process-tree tracking. `"_global"` = match all processes.
    #[serde(default = "default_cmd")]
    pub cmd: String,
    /// Map of path → per-path parameters.
    pub paths: BTreeMap<PathBuf, PathParams>,
}

/// Per-path filtering parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathParams {
    /// Watch subdirectories recursively.
    pub recursive: Option<bool>,
    /// Only monitor specified event types (e.g. `["MODIFY", "CREATE"]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    /// Size filter with comparison operator (e.g. >1MB, >=500KB, <100MB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

/// A single monitored path entry (flat form) — used for internal transport
/// between Monitored store, Monitor, socket, and CLI commands.
/// Not serialized to monitored.jsonl anymore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    /// Process name for process-tree tracking.
    /// `None` means `"_global"` (no specific process).
    pub cmd: Option<String>,
    /// Filesystem path to monitor.
    pub path: PathBuf,
    /// Watch subdirectories recursively.
    pub recursive: Option<bool>,
    /// Only monitor specified event types (e.g. `["MODIFY", "CREATE"]`).
    pub types: Option<Vec<String>>,
    /// Size filter with comparison operator (e.g. >1MB, >=500KB, <100MB).
    pub size: Option<String>,
}

impl PathParams {
    pub fn new(recursive: Option<bool>, types: Option<Vec<String>>, size: Option<String>) -> Self {
        PathParams { recursive, types, size }
    }
}

impl From<&PathEntry> for PathParams {
    fn from(e: &PathEntry) -> Self {
        PathParams {
            recursive: e.recursive,
            types: e.types.clone(),
            size: e.size.clone(),
        }
    }
}

impl Monitored {
    /// Load Monitored from file (JSONL format). Returns empty Monitored if file doesn't exist.
    /// Automatically validates and repairs common consistency issues.
    ///
    /// Handles migration from old flat-format and from old null-cmd format.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Monitored::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read store {}", path.display()))?;

        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut groups = Vec::new();
        let mut needs_migration = false;

        for trimmed in &lines {
            match serde_json::from_str::<CmdGroup>(trimmed) {
                Ok(group) => groups.push(group),
                Err(_) => {
                    // Not a CmdGroup — try old flat PathEntry format
                    needs_migration = true;
                    break;
                }
            }
        }

        if needs_migration {
            return Self::migrate_from_flat(&lines, path);
        }

        let mut store = Monitored { groups };
        store.validate();
        Ok(store)
    }

    /// Migrate old flat-format PathEntry lines to new grouped CmdGroup format.
    fn migrate_from_flat(lines: &[&str], path: &Path) -> Result<Self> {
        eprintln!("[migration] Converting monitored.jsonl to new grouped format...");
        let mut old_entries = Vec::new();
        for trimmed in lines {
            match serde_json::from_str::<PathEntry>(trimmed) {
                Ok(entry) => old_entries.push(entry),
                Err(e) => {
                    anyhow::bail!(
                        "Failed to parse old-format entry in store {}: {} — {}.",
                        path.display(), trimmed, e,
                    );
                }
            }
        }
        let mut store = Monitored::default();
        for entry in old_entries {
            store.add_entry(entry);
        }
        store.validate();
        if let Err(e) = store.save(path) {
            eprintln!("[warning] Could not re-save migrated store: {e}");
        }
        Ok(store)
    }

    /// Validate and repair consistency issues in-place.
    /// Deduplicate paths within each group. Remove empty groups.
    /// Returns `true` if any repairs were made.
    pub fn validate(&mut self) -> bool {
        let mut repaired = false;
        let mut deduped: Vec<CmdGroup> = Vec::with_capacity(self.groups.len());
        for group in self.groups.drain(..) {
            if group.paths.is_empty() {
                repaired = true;
                continue;
            }
            deduped.push(group);
        }
        self.groups = deduped;
        repaired
    }

    /// Flatten all groups into a Vec<PathEntry> (compatibility with legacy code).
    pub fn flatten(&self) -> Vec<PathEntry> {
        let mut entries = Vec::new();
        for group in &self.groups {
            for (path, params) in &group.paths {
                entries.push(PathEntry {
                    cmd: Some(group.cmd.clone()),
                    path: path.clone(),
                    recursive: params.recursive,
                    types: params.types.clone(),
                    size: params.size.clone(),
                });
            }
        }
        entries
    }

    /// Save Monitored to file (JSONL format). Creates parent directories if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Monitored path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let mut file = fs::File::create(path)
            .with_context(|| format!("Failed to create store {}", path.display()))?;
        chown_to_original_user(path);
        chown_to_original_user(parent);
        for group in &self.groups {
            let line = serde_json::to_string(group)
                .context("Failed to serialize store group")?;
            writeln!(file, "{}", line)
                .context("Failed to write store group")?;
        }
        Ok(())
    }

    /// Add an entry. If entry.cmd is None, it's treated as `"_global"`.
    pub fn add_entry(&mut self, entry: PathEntry) {
        let cmd = entry.cmd.clone().unwrap_or_else(|| CMD_GLOBAL.to_string());
        let params = PathParams::from(&entry);
        if let Some(group) = self.groups.iter_mut().find(|g| g.cmd == cmd) {
            group.paths.insert(entry.path.clone(), params);
        } else {
            let mut paths = BTreeMap::new();
            paths.insert(entry.path.clone(), params);
            self.groups.push(CmdGroup { cmd, paths });
        }
    }

    /// Remove entries matching path and optionally cmd.
    /// If cmd is Some, only removes from that cmd group.
    /// If cmd is None, removes from `"_global"` group.
    /// Returns `true` if any entry was removed.
    pub fn remove_entry(&mut self, path: &Path, cmd: Option<&str>) -> bool {
        let target = cmd.unwrap_or(CMD_GLOBAL);
        let mut removed = false;
        for group in self.groups.iter_mut() {
            if group.cmd != target {
                continue;
            }
            removed |= group.paths.remove(path).is_some();
        }
        self.groups.retain(|g| !g.paths.is_empty());
        removed
    }

    /// Get an entry by (path, cmd) pair. cmd=None → `"_global"` group.
    pub fn get(&self, path: &Path, cmd: Option<&str>) -> Option<PathEntry> {
        let target = cmd.unwrap_or(CMD_GLOBAL);
        for group in &self.groups {
            if group.cmd != target {
                continue;
            }
            if let Some(params) = group.paths.get(path) {
                return Some(PathEntry {
                    cmd: Some(group.cmd.clone()),
                    path: path.to_path_buf(),
                    recursive: params.recursive,
                    types: params.types.clone(),
                    size: params.size.clone(),
                });
            }
        }
        None
    }

    /// Check whether there are any entries.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() || self.groups.iter().all(|g| g.paths.is_empty())
    }

    /// Total number of path entries across all groups.
    pub fn entry_count(&self) -> usize {
        self.groups.iter().map(|g| g.paths.len()).sum()
    }

    /// Remove an entire cmd group by cmd name (None = `"_global"`).
    pub fn remove_cmd_group(&mut self, cmd: Option<&str>) -> bool {
        let target = cmd.unwrap_or(CMD_GLOBAL);
        let len_before = self.groups.len();
        self.groups.retain(|g| g.cmd != target);
        self.groups.len() < len_before
    }

    /// Check if a specific (path, cmd) entry exists.
    /// cmd=None → `"_global"` group.
    pub fn has_entry(&self, path: &Path, cmd: Option<&str>) -> bool {
        let target = cmd.unwrap_or(CMD_GLOBAL);
        self.groups.iter().any(|g| {
            g.cmd == target && g.paths.contains_key(path)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path() -> (PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "fsmon_monitored_test_{}_{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let monitored_path = dir.join("monitored.jsonl");
        (dir, monitored_path)
    }

    fn make_entry(path: &str, cmd: Option<&str>, recursive: Option<bool>) -> PathEntry {
        PathEntry {
            path: PathBuf::from(path),
            recursive,
            types: None,
            size: None,
            cmd: cmd.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        let (_dir, path) = temp_path();
        assert!(!path.exists());
        let store = Monitored::load(&path).unwrap();
        assert!(store.groups.is_empty());
    }

    #[test]
    fn test_add_entry_uses_cmd_as_key() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/tmp", None, Some(true)));
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(Path::new("/tmp"), None).is_some());
        assert!(store.get(Path::new("/tmp"), Some("_global")).is_some());

        store.add_entry(make_entry("/var/log", Some("bash"), Some(false)));
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_add_entry_replaces_same_path_and_cmd() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/home", None, Some(true)));
        assert_eq!(store.entry_count(), 1);

        store.add_entry(make_entry("/home", None, Some(false)));
        assert_eq!(store.entry_count(), 1);
        let entry = store.get(Path::new("/home"), None).unwrap();
        assert_eq!(entry.recursive, Some(false));
    }

    #[test]
    fn test_add_entry_different_cmd_same_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/home", Some("bash"), Some(true)));
        store.add_entry(make_entry("/home", None, Some(false)));
        assert_eq!(store.entry_count(), 2);
        assert_eq!(store.groups.len(), 2);
    }

    #[test]
    fn test_remove_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/tmp", None, None));
        store.add_entry(make_entry("/var", None, None));

        assert!(store.remove_entry(Path::new("/tmp"), None));
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(Path::new("/var"), None).is_some());

        assert!(!store.remove_entry(Path::new("/nonexistent"), None));
        assert_eq!(store.entry_count(), 1);
    }

    #[test]
    fn test_remove_entry_by_path_and_cmd() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/tmp", Some("bash"), None));
        store.add_entry(make_entry("/tmp", None, Some(true)));

        assert_eq!(store.entry_count(), 2);

        assert!(store.remove_entry(Path::new("/tmp"), Some("bash")));
        assert_eq!(store.entry_count(), 1);

        // /tmp in _global should still exist
        assert!(store.get(Path::new("/tmp"), None).is_some());
        assert!(store.get(Path::new("/tmp"), Some("bash")).is_none());
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/srv"),
            recursive: Some(true),
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            size: Some("1KB".into()),
            cmd: None,
        });

        store.save(&path).unwrap();

        let loaded = Monitored::load(&path).unwrap();
        assert_eq!(loaded.entry_count(), 1);
        let entry = loaded.get(Path::new("/srv"), None).unwrap();
        assert_eq!(entry.recursive, Some(true));
        assert_eq!(entry.types.as_ref().unwrap(), &["CREATE", "DELETE"]);
        assert_eq!(entry.size.as_ref().unwrap(), "1KB");
    }

    #[test]
    fn test_get_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/data", None, None));
        assert!(store.get(Path::new("/data"), None).is_some());
        assert!(store.get(Path::new("/nonexistent"), None).is_none());
    }

    #[test]
    fn test_empty_monitored_defaults() {
        let store = Monitored::default();
        assert!(store.groups.is_empty());
        assert!(store.is_empty());
    }

    #[test]
    fn test_flatten_groups() {
        let mut store = Monitored::default();
        store.add_entry(make_entry("/a", Some("bash"), Some(true)));
        store.add_entry(make_entry("/b", None, Some(false)));

        let flat = store.flatten();
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().any(|e| e.path == PathBuf::from("/a")
            && e.cmd.as_deref() == Some("bash")));
        assert!(flat.iter().any(|e| e.path == PathBuf::from("/b")
            && e.cmd.as_deref() == Some("_global")));
    }

    #[test]
    fn test_save_load_grouped_format() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::default();

        store.add_entry(make_entry("/a", Some("bash"), Some(true)));
        store.add_entry(make_entry("/b", Some("bash"), Some(false)));
        store.add_entry(make_entry("/c", None, Some(true)));

        store.save(&path).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let line0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(line0["cmd"], "bash");
        assert!(line0["paths"].is_object());

        let line1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(line1["cmd"], "_global");
        assert!(line1["paths"].is_object());

        let loaded = Monitored::load(&path).unwrap();
        assert_eq!(loaded.entry_count(), 3);
        assert_eq!(loaded.groups.len(), 2);
    }

    #[test]
    fn test_validate_removes_empty_groups() {
        let mut store = Monitored {
            groups: vec![
                CmdGroup {
                    cmd: "bash".into(),
                    paths: BTreeMap::new(),
                },
                CmdGroup {
                    cmd: CMD_GLOBAL.into(),
                    paths: {
                        let mut m = BTreeMap::new();
                        m.insert(PathBuf::from("/tmp"), PathParams::new(Some(true), None, None));
                        m
                    },
                },
            ],
        };
        assert!(store.validate());
        assert_eq!(store.groups.len(), 1);
    }

    #[test]
    fn test_validate_no_repair_on_unique_paths() {
        let mut store = Monitored {
            groups: vec![CmdGroup {
                cmd: CMD_GLOBAL.into(),
                paths: {
                    let mut m = BTreeMap::new();
                    m.insert(PathBuf::from("/a"), PathParams::new(None, None, None));
                    m
                },
            }],
        };
        assert!(!store.validate());
        assert_eq!(store.groups.len(), 1);
    }

    #[test]
    fn test_validate_empty_noop() {
        let mut store = Monitored::default();
        assert!(!store.validate());
    }

    #[test]
    fn test_jsonl_grouped_format_with_cmd() {
        let jsonl = concat!(
            r#"{"cmd":"bash","paths":{"/tmp":{"recursive":true},"/home":{"recursive":false,"types":["MODIFY"]}}}"#,
            "\n",
            r#"{"cmd":"_global","paths":{"/var":{"recursive":true,"size":">1MB"}}}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Monitored::load(&path).unwrap();
        assert_eq!(store.groups.len(), 2);
        assert_eq!(store.entry_count(), 3);
    }

    /// Old format without cmd field defaults to `"_global"`.
    #[test]
    fn test_jsonl_old_no_cmd_defaults_to_global() {
        let jsonl = concat!(
            r#"{"paths":{"/tmp":{"recursive":true}}}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Monitored::load(&path).unwrap();
        assert_eq!(store.groups.len(), 1);
        assert_eq!(store.groups[0].cmd, "_global");
    }

    /// Old flat PathEntry format auto-migrates.
    #[test]
    fn test_jsonl_old_flat_format_auto_migrates() {
        let jsonl = concat!(
            r#"{"path":"/tmp","recursive":true,"extra_field":99}"#,
            "\n",
            r#"{"path":"/home","recursive":false}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Monitored::load(&path).unwrap();
        assert_eq!(store.entry_count(), 2);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"cmd\":\"_global\""),
            "migrated file should contain cmd:_global");
    }

    #[test]
    fn test_entry_count() {
        let mut store = Monitored::default();
        assert_eq!(store.entry_count(), 0);
        store.add_entry(make_entry("/a", None, None));
        assert_eq!(store.entry_count(), 1);
        store.add_entry(make_entry("/b", Some("x"), None));
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_is_empty() {
        assert!(Monitored::default().is_empty());
    }

    #[test]
    fn test_flatten_no_groups() {
        assert!(Monitored::default().flatten().is_empty());
    }

    #[test]
    fn test_add_with_path_and_cmd_key() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(make_entry("/tmp", Some("bash"), Some(true)));
        store.add_entry(make_entry("/tmp", Some("nginx"), Some(false)));
        assert_eq!(store.entry_count(), 2);
        assert_eq!(store.groups.len(), 2);

        let bash_entry = store.get(Path::new("/tmp"), Some("bash")).unwrap();
        assert_eq!(bash_entry.recursive, Some(true));
        let nginx_entry = store.get(Path::new("/tmp"), Some("nginx")).unwrap();
        assert_eq!(nginx_entry.recursive, Some(false));
    }

    #[test]
    fn test_global_group_explicit() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        // Adding with explicit _global cmd
        store.add_entry(make_entry("/x", Some("_global"), Some(true)));
        // Adding with None cmd — should merge into same group
        store.add_entry(make_entry("/y", None, Some(false)));

        assert_eq!(store.groups.len(), 1);
        assert_eq!(store.groups[0].cmd, "_global");
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_remove_cmd_group_global() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::default();
        store.add_entry(make_entry("/a", None, None));
        store.add_entry(make_entry("/b", Some("x"), None));

        assert!(store.remove_cmd_group(None)); // removes _global
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(Path::new("/b"), Some("x")).is_some());
    }

    #[test]
    fn test_has_entry() {
        let mut store = Monitored::default();
        store.add_entry(make_entry("/a", None, None));
        store.add_entry(make_entry("/b", Some("x"), None));

        assert!(store.has_entry(Path::new("/a"), None));
        assert!(store.has_entry(Path::new("/a"), Some("_global")));
        assert!(store.has_entry(Path::new("/b"), Some("x")));
        assert!(!store.has_entry(Path::new("/b"), None));
        assert!(!store.has_entry(Path::new("/x"), None));
    }
}
