use std::collections::HashMap;
use std::path::PathBuf;

use moka::sync::Cache;

use crate::fid_parser::FsGroup;

/// Manages fanotify filesystem groups, directory cache, and path-to-group mapping.
pub struct FanotifyManager {
    /// One `FsGroup` per unique filesystem (fan_fd + mount_fd dedup'd)
    pub fs_groups: Vec<FsGroup>,
    /// Maps monitored path → index in fs_groups for fast lookup in remove_path
    pub path_to_group: HashMap<PathBuf, usize>,
    /// Directory handle cache for resolving fanotify file handles to paths
    pub dir_cache: Cache<fanotify_fid::types::HandleKey, PathBuf>,
}

impl FanotifyManager {
    pub fn new(
        dir_cache_capacity: u64,
    ) -> Self {
        Self {
            fs_groups: Vec::new(),
            path_to_group: HashMap::new(),
            dir_cache: Cache::builder()
                .max_capacity(dir_cache_capacity)
                .build(),
        }
    }

    /// Get the FsGroup index for a given path, if it exists.
    pub fn group_index_for_path(&self, path: &PathBuf) -> Option<usize> {
        self.path_to_group.get(path).copied()
    }

    /// Add a new FsGroup and return its index.
    pub fn add_group(&mut self, group: FsGroup) -> usize {
        let idx = self.fs_groups.len();
        self.fs_groups.push(group);
        idx
    }

    /// Map a path to an FsGroup index.
    pub fn map_path_to_group(&mut self, path: PathBuf, group_idx: usize) {
        self.path_to_group.insert(path, group_idx);
    }

    /// Remove a path from the path-to-group mapping.
    pub fn unmap_path(&mut self, path: &PathBuf) -> Option<usize> {
        self.path_to_group.remove(path)
    }

    /// Get all paths mapped to a specific group index.
    pub fn paths_for_group(&self, group_idx: usize) -> Vec<&PathBuf> {
        self.path_to_group
            .iter()
            .filter(|(_, idx)| **idx == group_idx)
            .map(|(path, _)| path)
            .collect()
    }
}
