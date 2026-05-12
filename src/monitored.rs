use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::chown_to_original_user;

/// The monitored paths database, stored in the file configured by `[monitored].path`.
///
/// Monitored automatically by `fsmon add` and `fsmon remove`.
///
/// # JSONL Format (grouped by cmd)
/// Each line groups paths under a common `cmd`:
/// ```json
/// {"cmd":"bash","paths":{"/a":{"recursive":true},"/b":{"recursive":false,"types":["MODIFY"]}}}
/// {"paths":{"/c":{"recursive":true}}}
/// ```
/// When `cmd` is null, it is omitted from JSON for brevity.
///
/// # Migration from old flat format
/// If the file contains old-style flat `PathEntry` lines, `load()` automatically
/// converts them to the new grouped format and re-saves.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Monitored {
    /// Monitored path groups, each keyed by cmd.
    pub groups: Vec<CmdGroup>,
}

/// Per-process-name group of monitored paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdGroup {
    /// Process name for process-tree tracking. None = match all processes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cmd: Option<String>,
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
    /* Path glob patterns to exclude.
    pub exclude_path: Option<Vec<String>>,
    /// Process names to exclude (glob, repeatable).
    pub exclude_cmd: Option<Vec<String>>,*/
}

/// A single monitored path entry (flat form) — used for internal transport
/// between Monitored store, Monitor, socket, and CLI commands.
/// Not serialized to monitored.jsonl anymore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    /// Process name for process-tree tracking.
    pub cmd: Option<String>,
    /// Filesystem path to monitor.
    pub path: PathBuf,
    /// Watch subdirectories recursively.
    pub recursive: Option<bool>,
    /// Only monitor specified event types (e.g. `["MODIFY", "CREATE"]`).
    pub types: Option<Vec<String>>,
    /// Size filter with comparison operator (e.g. >1MB, >=500KB, <100MB).
    pub size: Option<String>,
    /* Path glob patterns to exclude.
    pub exclude_path: Option<Vec<String>>,
    /// Process names to exclude (glob, repeatable).
    pub exclude_cmd: Option<Vec<String>>,*/
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

impl From<&PathParams> for PathEntry {
    fn from(p: &PathParams) -> Self {
        PathEntry {
            cmd: None,
            path: PathBuf::new(),
            recursive: p.recursive,
            types: p.types.clone(),
            size: p.size.clone(),
        }
    }
}

impl Monitored {
    /// Load Monitored from file (JSONL format). Returns empty Monitored if file doesn't exist.
    /// Automatically validates and repairs common consistency issues:
    ///   - Duplicate paths: keeps the last entry per unique path within each group
    ///
    /// If the file contains old flat-format PathEntry lines, they are auto-migrated
    /// to the new grouped format and the file is rewritten.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Monitored::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read store {}", path.display()))?;

        // Try parsing each line as the new grouped format first
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut groups = Vec::new();
        let mut needs_migration = false;

        for trimmed in &lines {
            match serde_json::from_str::<CmdGroup>(trimmed) {
                Ok(group) => groups.push(group),
                Err(_) => {
                    // Not a CmdGroup — might be old flat PathEntry format
                    needs_migration = true;
                    break;
                }
            }
        }

        if needs_migration {
            // Migrate old flat format to new grouped format
            eprintln!("[migration] Converting monitored.jsonl to new grouped format...");
            let mut old_entries = Vec::new();
            for trimmed in &lines {
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
            // Re-save in new format
            if let Err(e) = store.save(path) {
                eprintln!("[warning] Could not re-save migrated store: {e}");
            }
            return Ok(store);
        }

        let mut store = Monitored { groups };
        store.validate();
        Ok(store)
    }

    /// Validate and repair consistency issues in-place.
    /// Deduplicate paths within each group — if multiple entries share the same path,
    /// only the last one survives (later add = newer config).
    /// Remove empty groups.
    /// Returns `true` if any repairs were made.
    pub fn validate(&mut self) -> bool {
        let mut repaired = false;
        let mut deduped: Vec<CmdGroup> = Vec::with_capacity(self.groups.len());
        for group in self.groups.drain(..) {
            // No dedup needed since BTreeMap already has unique keys.
            // But ensure we remove empty groups.
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
                    cmd: group.cmd.clone(),
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
        // Chown to original user if running as root
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

    /// Add an entry. If a group with matching cmd exists, the path is inserted/updated
    /// in that group's paths map. Otherwise a new group is created.
    pub fn add_entry(&mut self, entry: PathEntry) {
        let params = PathParams::from(&entry);
        // Find existing group with matching cmd
        if let Some(group) = self.groups.iter_mut().find(|g| g.cmd == entry.cmd) {
            group.paths.insert(entry.path.clone(), params);
        } else {
            let mut paths = BTreeMap::new();
            paths.insert(entry.path.clone(), params);
            self.groups.push(CmdGroup {
                cmd: entry.cmd,
                paths,
            });
        }
    }

    /// Remove entries matching path and optionally cmd.
    /// If cmd is Some, only removes the entry from group with matching cmd.
    /// If cmd is None, removes from all groups.
    /// Returns `true` if any entry was removed.
    pub fn remove_entry(&mut self, path: &Path, cmd: Option<&str>) -> bool {
        let mut removed = false;
        self.groups.iter_mut().for_each(|group| {
            if let Some(cmd_str) = cmd {
                // Only remove from this specific cmd group
                if group.cmd.as_deref() == Some(cmd_str) {
                    removed |= group.paths.remove(path).is_some();
                }
            } else {
                // cmd is None: remove path from all groups that have it
                removed |= group.paths.remove(path).is_some();
            }
        });
        // Remove empty groups
        self.groups.retain(|g| !g.paths.is_empty());
        removed
    }

    /// Get an entry by (path, cmd) pair.
    pub fn get(&self, path: &Path, cmd: Option<&str>) -> Option<PathEntry> {
        for group in &self.groups {
            if group.cmd.as_deref() != cmd {
                continue;
            }
            if let Some(params) = group.paths.get(path) {
                return Some(PathEntry {
                    cmd: group.cmd.clone(),
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

    /// Remove an entire cmd group by cmd name.
    /// Returns `true` if the group was found and removed.
    pub fn remove_cmd_group(&mut self, cmd: &str) -> bool {
        let len_before = self.groups.len();
        self.groups.retain(|g| g.cmd.as_deref() != Some(cmd));
        self.groups.len() < len_before
    }

    /// Check if a specific (path, cmd) entry exists.
    /// If cmd is None, checks across ALL groups.
    /// If cmd is Some, checks only the matching cmd group.
    pub fn has_entry(&self, path: &Path, cmd: Option<&str>) -> bool {
        self.groups.iter().any(|g| {
            if let Some(cmd_str) = cmd {
                if g.cmd.as_deref() != Some(cmd_str) {
                    return false;
                }
            }
            g.paths.contains_key(path)
        })
    }

    /// Total number of path entries across all groups.
    pub fn entry_count(&self) -> usize {
        self.groups.iter().map(|g| g.paths.len()).sum()
    }
}

// ---- Backward-compatible helpers ----

/// Given a `PathEntry`, find the matching `PathParam` key (the path itself) 
/// for use in grouped lookup. Kept for API compatibility.
pub fn entry_cmd_key(entry: &PathEntry) -> Option<String> {
    entry.cmd.clone()
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

    #[test]
    fn test_load_returns_default_when_no_file() {
        let (_dir, path) = temp_path();
        assert!(!path.exists());
        let store = Monitored::load(&path).unwrap();
        assert!(store.groups.is_empty());
    }

    #[test]
    fn test_add_entry_uses_path_as_key() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: None,
        });
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(Path::new("/tmp"), None).is_some());

        store.add_entry(PathEntry {
            path: PathBuf::from("/var/log"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            size: None,
            cmd: None,
        });
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_add_entry_replaces_same_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: None,
        });
        assert_eq!(store.entry_count(), 1);

        // Adding same path again replaces old entry
        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            size: None,
            cmd: None,
        });
        assert_eq!(store.entry_count(), 1); // replaced, not duplicated
        let entry = store.get(Path::new("/home"), None).unwrap();
        assert_eq!(entry.path, PathBuf::from("/home"));
        assert_eq!(entry.recursive, Some(false)); // new params
    }

    #[test]
    fn test_add_entry_different_cmd_same_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: Some("bash".into()),
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(false),
            types: None,
            size: None,
            cmd: None,
        });
        // Two different cmd groups, both can have /home
        assert_eq!(store.entry_count(), 2);
        assert_eq!(store.groups.len(), 2);
    }

    #[test]
    fn test_remove_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: None,
            types: None,
            size: None,
            cmd: None,
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/var"),
            recursive: None,
            types: None,
            size: None,
            cmd: None,
        });

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

        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: None,
            types: None,
            size: None,
            cmd: Some("bash".into()),
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: None,
        });

        assert_eq!(store.entry_count(), 2);

        // Remove only bash's /tmp
        assert!(store.remove_entry(Path::new("/tmp"), Some("bash")));
        assert_eq!(store.entry_count(), 1);

        // /tmp without cmd should still exist
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
        assert_eq!(entry.path, PathBuf::from("/srv"));
        assert_eq!(entry.types.as_ref().unwrap(), &["CREATE", "DELETE"]);
        assert_eq!(entry.size.as_ref().unwrap(), "1KB");
    }

    #[test]
    fn test_get_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/data"),
            recursive: None,
            types: None,
            size: None,
            cmd: None,
        });

        let entry = store.get(Path::new("/data"), None);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().path, PathBuf::from("/data"));

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
        store.add_entry(PathEntry {
            path: PathBuf::from("/a"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: Some("bash".into()),
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/b"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            size: None,
            cmd: None,
        });

        let flat = store.flatten();
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().any(|e| e.path == PathBuf::from("/a") && e.cmd.as_deref() == Some("bash")));
        assert!(flat.iter().any(|e| e.path == PathBuf::from("/b") && e.cmd.is_none()));
    }

    #[test]
    fn test_save_load_grouped_format() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::default();

        // Two paths under "bash" group
        store.add_entry(PathEntry {
            path: PathBuf::from("/a"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: Some("bash".into()),
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/b"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            size: None,
            cmd: Some("bash".into()),
        });
        // One path under None group
        store.add_entry(PathEntry {
            path: PathBuf::from("/c"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: None,
        });

        store.save(&path).unwrap();

        // Verify file content is in new grouped format
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 JSONL lines (2 cmd groups)");

        // First line should have cmd="bash" and 2 paths
        let line0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(line0["cmd"], serde_json::json!("bash"));
        assert!(line0["paths"].is_object());

        // Second line should have cmd omitted (None) and 1 path
        let line1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert!(line1.get("cmd").is_none() || line1["cmd"].is_null());
        assert!(line1["paths"].is_object());

        // Reload and verify
        let loaded = Monitored::load(&path).unwrap();
        assert_eq!(loaded.entry_count(), 3);
        assert_eq!(loaded.groups.len(), 2);
    }

    #[test]
    fn test_validate_removes_empty_groups() {
        let mut store = Monitored {
            groups: vec![
                CmdGroup {
                    cmd: Some("bash".into()),
                    paths: BTreeMap::new(), // empty!
                },
                CmdGroup {
                    cmd: None,
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
                cmd: None,
                paths: {
                    let mut m = BTreeMap::new();
                    m.insert(PathBuf::from("/a"), PathParams::new(None, None, None));
                    m.insert(PathBuf::from("/b"), PathParams::new(None, None, None));
                    m
                },
            }],
        };
        assert!(!store.validate());
        assert_eq!(store.groups.len(), 1);
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_validate_empty_noop() {
        let mut store = Monitored::default();
        assert!(!store.validate());
    }

    /// Test JSONL format with new grouped structure.
    #[test]
    fn test_jsonl_grouped_format() {
        let jsonl = concat!(
            r#"{"cmd":"bash","paths":{"/tmp":{"recursive":true},"/home":{"recursive":false,"types":["MODIFY"]}}}"#,
            "\n",
            r#"{"paths":{"/var":{"recursive":true,"size":">1MB"}}}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Monitored::load(&path).unwrap();
        assert_eq!(store.groups.len(), 2);
        assert_eq!(store.entry_count(), 3);

        // Verify bash group
        let bash_group = store.groups.iter().find(|g| g.cmd.as_deref() == Some("bash")).unwrap();
        assert_eq!(bash_group.paths.len(), 2);
        assert!(bash_group.paths.contains_key(Path::new("/tmp")));
        assert!(bash_group.paths.contains_key(Path::new("/home")));

        // Verify null cmd group
        let null_group = store.groups.iter().find(|g| g.cmd.is_none()).unwrap();
        assert_eq!(null_group.paths.len(), 1);
        assert!(null_group.paths.contains_key(Path::new("/var")));
        assert_eq!(null_group.paths[Path::new("/var")].size.as_ref().unwrap(), ">1MB");
    }

    /// Test backward compat: old flat format auto-migrates to new grouped format.
    #[test]
    fn test_jsonl_old_flat_format_auto_migrates() {
        let jsonl = concat!(
            r#"{"path":"/tmp","recursive":true,"extra_field":99}"#,
            "\n",
            r#"{"path":"/home","id":"old","recursive":false}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Monitored::load(&path).unwrap();
        assert_eq!(store.entry_count(), 2);
        // After migration, file should be rewritten in new format
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.iter().all(|l| l.contains("paths")), "all lines should have 'paths' field");
    }

    #[test]
    fn test_entry_count() {
        let mut store = Monitored::default();
        assert_eq!(store.entry_count(), 0);
        store.add_entry(PathEntry {
            path: PathBuf::from("/a"),
            recursive: None,
            types: None,
            size: None,
            cmd: None,
        });
        assert_eq!(store.entry_count(), 1);
        store.add_entry(PathEntry {
            path: PathBuf::from("/b"),
            recursive: None,
            types: None,
            size: None,
            cmd: Some("x".into()),
        });
        assert_eq!(store.entry_count(), 2);
    }

    #[test]
    fn test_is_empty() {
        let store = Monitored::default();
        assert!(store.is_empty());
    }

    #[test]
    fn test_flatten_no_groups() {
        let store = Monitored::default();
        assert!(store.flatten().is_empty());
    }

    #[test]
    fn test_add_with_path_and_cmd_key() {
        let (_dir, path) = temp_path();
        let mut store = Monitored::load(&path).unwrap();

        // Same path, different cmds → two groups
        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: Some(true),
            types: None,
            size: None,
            cmd: Some("bash".into()),
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: Some(false),
            types: None,
            size: None,
            cmd: Some("nginx".into()),
        });
        assert_eq!(store.entry_count(), 2);
        assert_eq!(store.groups.len(), 2);

        // Verify each cmd has its own parameters
        let bash_entry = store.get(Path::new("/tmp"), Some("bash")).unwrap();
        assert_eq!(bash_entry.recursive, Some(true));
        let nginx_entry = store.get(Path::new("/tmp"), Some("nginx")).unwrap();
        assert_eq!(nginx_entry.recursive, Some(false));
    }
}
