use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// The monitored paths database, stored in the file configured by `[store].file`.
///
/// Managed automatically by `fsmon add` and `fsmon remove`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Store {
    /// Monitored path entries.
    pub entries: Vec<PathEntry>,
}

/// A single monitored path with its filtering options.
/// The path itself serves as the unique identifier (like chezmoi).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    /// Filesystem path to monitor (unique identifier).
    pub path: PathBuf,
    /// Watch subdirectories recursively.
    pub recursive: Option<bool>,
    /// Only monitor specified event types (e.g. `["MODIFY", "CREATE"]`).
    pub types: Option<Vec<String>>,
    /// Only record events with size change >= this value.
    pub min_size: Option<String>,
    /// Paths to exclude from monitoring (wildcard patterns).
    pub exclude: Option<String>,
    /// Process names to exclude (glob, e.g. "rsync|apt").
    pub exclude_cmd: Option<String>,
    /// Only capture events from these process names (glob).
    pub only_cmd: Option<String>,
    /// Capture all 14 fanotify event types.
    pub all_events: Option<bool>,
}

impl Store {
    /// Load Store from file (JSONL format). Returns empty Store if file doesn't exist.
    /// Automatically validates and repairs common consistency issues:
    ///   - Duplicate paths: keeps the last entry per unique path
    ///
    /// If repairs were made, callers should re-save the store.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Store::default());
        }
        let file = fs::File::open(path)
            .with_context(|| format!("Failed to open store {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: PathEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("Invalid JSON in store {}: {}", path.display(), trimmed))?;
            entries.push(entry);
        }
        let mut store = Store { entries };
        store.validate();
        Ok(store)
    }

    /// Validate and repair consistency issues in-place.
    /// Deduplicate paths — if multiple entries share the same path,
    /// only the last one survives (later add = newer config).
    /// Returns `true` if any repairs were made.
    pub fn validate(&mut self) -> bool {
        if self.entries.len() <= 1 {
            return false;
        }
        let mut seen = std::collections::HashSet::new();
        let mut deduped: Vec<PathEntry> = Vec::with_capacity(self.entries.len());
        let mut repaired = false;
        for entry in self.entries.drain(..).rev() {
            if seen.insert(entry.path.clone()) {
                deduped.push(entry);
            } else {
                repaired = true;
            }
        }
        deduped.reverse();
        self.entries = deduped;
        repaired
    }

    /// Save Store to file (JSONL format). Creates parent directories if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Store path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let mut file = fs::File::create(path)
            .with_context(|| format!("Failed to create store {}", path.display()))?;
        for entry in &self.entries {
            let line = serde_json::to_string(entry)
                .context("Failed to serialize store entry")?;
            writeln!(file, "{}", line)
                .context("Failed to write store entry")?;
        }
        Ok(())
    }

    /// Add an entry. If an entry with the same path already exists,
    /// it is replaced (all old entries with that path are removed first).
    pub fn add_entry(&mut self, entry: PathEntry) {
        // Remove all existing entries with the same path so the new one replaces them
        self.entries.retain(|e| e.path != entry.path);
        self.entries.push(entry);
    }

    /// Remove an entry by its path.
    /// Returns `true` if an entry was found and removed.
    pub fn remove_entry(&mut self, path: &Path) -> bool {
        let len_before = self.entries.len();
        self.entries.retain(|e| e.path != path);
        self.entries.len() < len_before
    }

    /// Get an entry by its path.
    pub fn get(&self, path: &Path) -> Option<&PathEntry> {
        self.entries.iter().find(|e| e.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fsmon_store_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("store.jsonl");
        (dir, store_path)
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        let (_dir, path) = temp_path();
        assert!(!path.exists());
        let store = Store::load(&path).unwrap();
        assert!(store.entries.is_empty());
    }

    #[test]
    fn test_add_entry_uses_path_as_key() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });
        assert_eq!(store.entries.len(), 1);
        assert!(store.get(Path::new("/tmp")).is_some());

        store.add_entry(PathEntry {
            path: PathBuf::from("/var/log"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn test_add_entry_replaces_same_path() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });
        assert_eq!(store.entries.len(), 1);

        // Adding same path again replaces old entry
        store.add_entry(PathEntry {
            path: PathBuf::from("/home"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });
        assert_eq!(store.entries.len(), 1); // replaced, not duplicated
        assert_eq!(store.entries[0].path, PathBuf::from("/home"));
        assert_eq!(store.entries[0].recursive, Some(false)); // new params
    }

    #[test]
    fn test_remove_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/tmp"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });
        store.add_entry(PathEntry {
            path: PathBuf::from("/var"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });

        assert!(store.remove_entry(Path::new("/tmp")));
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].path, PathBuf::from("/var"));

        assert!(!store.remove_entry(Path::new("/nonexistent")));
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/srv"),
            recursive: Some(true),
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            min_size: Some("1KB".into()),
            exclude: Some("*.tmp".into()),
    exclude_cmd: None,
    only_cmd: None,
            all_events: Some(false),
        });

        store.save(&path).unwrap();

        let loaded = Store::load(&path).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].path, PathBuf::from("/srv"));
        assert_eq!(
            loaded.entries[0].types.as_ref().unwrap(),
            &["CREATE", "DELETE"]
        );
        assert_eq!(loaded.entries[0].min_size.as_ref().unwrap(), "1KB");
        assert_eq!(loaded.entries[0].exclude.as_ref().unwrap(), "*.tmp");
    }

    #[test]
    fn test_get_entry_by_path() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            path: PathBuf::from("/data"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
    exclude_cmd: None,
    only_cmd: None,
            all_events: None,
        });

        let entry = store.get(Path::new("/data"));
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().path, PathBuf::from("/data"));

        assert!(store.get(Path::new("/nonexistent")).is_none());
    }

    #[test]
    fn test_empty_store_defaults() {
        let store = Store::default();
        assert!(store.entries.is_empty());
    }

    #[test]
    fn test_validate_dedup_path_keeps_last() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    path: PathBuf::from("/home"),
                    recursive: Some(true),
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/tmp"),
                    recursive: Some(false),
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/home"), // dup path, should keep last
                    recursive: Some(false),
                    types: Some(vec!["MODIFY".into()]),
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
            ],
        };
        assert!(store.validate());
        assert_eq!(store.entries.len(), 2);
        // /home entry should be the LAST one (newer wins)
        let target: &Path = "/home".as_ref();
        let home = store.entries.iter().find(|e| e.path == target).unwrap();
        assert_eq!(home.recursive, Some(false));
    }

    #[test]
    fn test_validate_no_repair_on_unique_paths() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    path: PathBuf::from("/a"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/b"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/c"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
            ],
        };
        assert!(!store.validate());
        assert_eq!(store.entries.len(), 3);
    }

    #[test]
    fn test_validate_clean_store_unchanged() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    path: PathBuf::from("/a"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
                PathEntry {
                    path: PathBuf::from("/b"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
    exclude_cmd: None,
    only_cmd: None,
                    all_events: None,
                },
            ],
        };
        assert!(!store.validate()); // no repairs needed
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn test_validate_empty_noop() {
        let mut store = Store::default();
        assert!(!store.validate());
    }

    /// Test JSONL format with extra fields (serde ignores unknown fields).
    #[test]
    fn test_jsonl_extra_fields_ignored() {
        let jsonl = concat!(
            r#"{"path":"/tmp","recursive":true,"extra_field":99}"#,
            "\n",
            r#"{"path":"/home","id":"old","recursive":false}"#,
            "\n",
        );
        let (_dir, path) = temp_path();
        fs::write(&path, jsonl).unwrap();
        let store = Store::load(&path).unwrap();
        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].path, PathBuf::from("/tmp"));
        assert_eq!(store.entries[1].path, PathBuf::from("/home"));
    }
}
