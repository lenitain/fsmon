use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    /// Unique numeric identifier (auto-assigned).
    pub id: u64,
    /// Filesystem path to monitor.
    pub path: PathBuf,
    /// Watch subdirectories recursively.
    pub recursive: Option<bool>,
    /// Only monitor specified event types (e.g. `["MODIFY", "CREATE"]`).
    pub types: Option<Vec<String>>,
    /// Only record events with size change >= this value.
    pub min_size: Option<String>,
    /// Paths to exclude from monitoring (wildcard patterns).
    pub exclude: Option<String>,
    /// Capture all 14 fanotify event types.
    pub all_events: Option<bool>,
}

impl Store {
    /// Load Store from file. Returns empty Store if file doesn't exist.
    /// Automatically validates and repairs common consistency issues:
    ///   - Duplicate paths: keeps the last entry per unique path
    ///   - Duplicate IDs: reassigns new IDs to duplicates
    ///
    /// If repairs were made, callers should re-save the store.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Store::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read store {}", path.display()))?;
        let mut store: Store = toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in {}", path.display()))?;
        store.validate();
        Ok(store)
    }

    /// Compute the next available unique ID: `max(existing ids) + 1`.
    /// Returns 1 when there are no entries.
    pub fn next_id(&self) -> u64 {
        self.entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
    }

    /// Validate and repair consistency issues in-place.
    ///
    /// 1. Deduplicate paths — if multiple entries share the same path,
    ///    only the last one survives (later add = newer config).
    /// 2. Deduplicate IDs — if multiple entries share the same ID,
    ///    the first occurrence keeps the ID, later ones get new IDs
    ///    (max existing + 1, +2, ...).
    ///
    /// Returns `true` if any repairs were made.
    pub fn validate(&mut self) -> bool {
        let mut repaired = false;

        // 1. Dedup by path: keep the last entry per path (reverse scan)
        if self.entries.len() > 1 {
            let mut seen = std::collections::HashSet::new();
            let mut deduped: Vec<PathEntry> = Vec::with_capacity(self.entries.len());
            for entry in self.entries.drain(..).rev() {
                if seen.insert(entry.path.clone()) {
                    deduped.push(entry);
                } else {
                    repaired = true;
                }
            }
            deduped.reverse();
            self.entries = deduped;
        }

        // 2. Dedup by ID: first occurrence keeps the ID
        let mut next_id = self.next_id();
        if self.entries.len() > 1 {
            let mut seen = std::collections::HashSet::new();
            for entry in &mut self.entries {
                if !seen.insert(entry.id) {
                    // Duplicate ID — assign a fresh one
                    entry.id = next_id;
                    next_id += 1;
                    repaired = true;
                }
            }
        }

        repaired
    }

    /// Save Store to file. Creates parent directories if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Store path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        let content = toml::to_string_pretty(self).context("Failed to serialize store")?;
        fs::write(path, content)
            .with_context(|| format!("Failed to write store to {}", path.display()))?;
        Ok(())
    }

    /// Add an entry, auto-assigning a unique numeric ID.
    /// If an entry with the same path already exists, it is replaced
    /// (all old entries with that path are removed first).
    /// Returns the assigned ID.
    pub fn add_entry(&mut self, mut entry: PathEntry) -> u64 {
        let id = self.next_id(); // capture before removal so removal doesn't drop max
        // Remove all existing entries with the same path so the new one replaces them
        self.entries.retain(|e| e.path != entry.path);
        entry.id = id;
        self.entries.push(entry);
        id
    }

    /// Remove an entry by its numeric ID.
    /// Returns `true` if an entry was found and removed.
    pub fn remove_entry(&mut self, id: u64) -> bool {
        let len_before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() < len_before
    }

    /// Get an entry by its numeric ID.
    pub fn get(&self, id: u64) -> Option<&PathEntry> {
        self.entries.iter().find(|e| e.id == id)
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
        let store_path = dir.join("store.toml");
        (dir, store_path)
    }

    #[test]
    fn test_load_returns_default_when_no_file() {
        let (_dir, path) = temp_path();
        assert!(!path.exists());
        let store = Store::load(&path).unwrap();
        assert_eq!(store.next_id(), 1);
        assert!(store.entries.is_empty());
    }

    #[test]
    fn test_add_entry_auto_assigns_id() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        let id1 = store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/tmp"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        });
        assert_eq!(id1, 1);
        assert_eq!(store.next_id(), 2);
        assert_eq!(store.entries.len(), 1);

        let id2 = store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/var/log"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            min_size: None,
            exclude: None,
            all_events: None,
        });
        assert_eq!(id2, 2);
        assert_eq!(store.next_id(), 3);
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn test_add_entry_replaces_same_path() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        let id1 = store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/home"),
            recursive: Some(true),
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        });
        assert_eq!(id1, 1);
        assert_eq!(store.entries.len(), 1);

        // Adding same path again replaces old entry, gets fresh ID
        let id2 = store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/home"),
            recursive: Some(false),
            types: Some(vec!["MODIFY".into()]),
            min_size: None,
            exclude: None,
            all_events: None,
        });
        assert_eq!(id2, 2);
        assert_eq!(store.entries.len(), 1); // replaced, not duplicated
        assert_eq!(store.entries[0].id, 2);
        assert_eq!(store.entries[0].path, PathBuf::from("/home"));
        assert_eq!(store.entries[0].recursive, Some(false)); // new params
    }

    #[test]
    fn test_remove_entry_by_id() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/tmp"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        });
        store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/var"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        });

        assert!(store.remove_entry(1));
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].id, 2);

        assert!(!store.remove_entry(99)); // non-existent
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/srv"),
            recursive: Some(true),
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            min_size: Some("1KB".into()),
            exclude: Some("*.tmp".into()),
            all_events: Some(false),
        });

        store.save(&path).unwrap();

        let loaded = Store::load(&path).unwrap();
        assert_eq!(loaded.next_id(), 2);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].id, 1);
        assert_eq!(loaded.entries[0].path, PathBuf::from("/srv"));
        assert_eq!(
            loaded.entries[0].types.as_ref().unwrap(),
            &["CREATE", "DELETE"]
        );
        assert_eq!(loaded.entries[0].min_size.as_ref().unwrap(), "1KB");
        assert_eq!(loaded.entries[0].exclude.as_ref().unwrap(), "*.tmp");
    }

    #[test]
    fn test_get_entry() {
        let (_dir, path) = temp_path();
        let mut store = Store::load(&path).unwrap();

        let id = store.add_entry(PathEntry {
            id: 0,
            path: PathBuf::from("/data"),
            recursive: None,
            types: None,
            min_size: None,
            exclude: None,
            all_events: None,
        });

        let entry = store.get(id);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().path, PathBuf::from("/data"));

        assert!(store.get(999).is_none());
    }

    #[test]
    fn test_empty_store_defaults() {
        let store = Store::default();
        assert_eq!(store.next_id(), 1);
        assert!(store.entries.is_empty());
    }

    #[test]
    fn test_validate_dedup_path_keeps_last() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    id: 1,
                    path: PathBuf::from("/home"),
                    recursive: Some(true),
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    id: 2,
                    path: PathBuf::from("/tmp"),
                    recursive: Some(false),
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    id: 3,
                    path: PathBuf::from("/home"), // dup path, should replace id=1
                    recursive: Some(false),
                    types: Some(vec!["MODIFY".into()]),
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
            ],
        };
        assert!(store.validate());
        assert_eq!(store.entries.len(), 2);
        // /home entry should be the LAST one (newer wins)
        let target: &Path = "/home".as_ref();
        let home = store.entries.iter().find(|e| e.path == target).unwrap();
        assert_eq!(home.id, 3);
        assert_eq!(home.recursive, Some(false));
    }

    #[test]
    fn test_validate_dedup_id_reassigns_duplicates() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    id: 1,
                    path: PathBuf::from("/a"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    id: 1, // dup
                    path: PathBuf::from("/b"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    id: 1, // dup
                    path: PathBuf::from("/c"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
            ],
        };
        assert!(store.validate());
        assert_eq!(store.entries.len(), 3);
        let ids: Vec<u64> = store.entries.iter().map(|e| e.id).collect();
        // first dup keeps id=1, rest get max(1)+1=2, +1=3
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(store.next_id(), 4);
    }

    #[test]
    fn test_validate_clean_store_unchanged() {
        let mut store = Store {
            entries: vec![
                PathEntry {
                    id: 1,
                    path: PathBuf::from("/a"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
                PathEntry {
                    id: 2,
                    path: PathBuf::from("/b"),
                    recursive: None,
                    types: None,
                    min_size: None,
                    exclude: None,
                    all_events: None,
                },
            ],
        };
        assert!(!store.validate()); // no repairs needed
        assert_eq!(store.next_id(), 3);
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn test_validate_empty_noop() {
        let mut store = Store::default();
        assert!(!store.validate());
    }

    /// Old-format store.toml had a `next_id` field — serde ignores it on load.
    #[test]
    fn test_old_format_with_next_id_field() {
        let toml_str = concat!(
            "next_id = 10\n",
            "\n",
            "[[entries]]\n",
            "id = 3\n",
            "path = \"/tmp\"\n",
            "recursive = true\n",
            "\n",
            "[[entries]]\n",
            "id = 3\n",
            "path = \"/home\"\n",
            "recursive = false\n",
        );
        let mut store: Store = toml::from_str(toml_str).unwrap();
        assert_eq!(store.entries.len(), 2);
        let r = store.validate();
        assert!(r);
        // ID dedup: first keeps 3, second gets next_id = max(3)+1 = 4
        let ids: Vec<u64> = store.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 4]);
        assert_eq!(store.next_id(), 5);
    }
}
