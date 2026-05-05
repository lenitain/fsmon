use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The monitored paths database, stored in the file configured by `[store].file`.
///
/// Managed automatically by `fsmon add` and `fsmon remove`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    /// Auto-incrementing ID counter. Monotonically increasing, never reused.
    pub next_id: u64,
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

impl Default for Store {
    fn default() -> Self {
        Store {
            next_id: 1,
            entries: Vec::new(),
        }
    }
}

impl Store {
    /// Load Store from file. Returns empty Store if file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Store::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read store {}", path.display()))?;
        let store: Store = toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in {}", path.display()))?;
        Ok(store)
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
    /// Returns the assigned ID.
    pub fn add_entry(&mut self, mut entry: PathEntry) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
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
        assert_eq!(store.next_id, 1);
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
        assert_eq!(store.next_id, 2);
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
        assert_eq!(store.next_id, 3);
        assert_eq!(store.entries.len(), 2);
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
        assert_eq!(loaded.next_id, 2);
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
        assert_eq!(store.next_id, 1);
        assert!(store.entries.is_empty());
    }
}
