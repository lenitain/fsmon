use std::path::PathBuf;
use anyhow::Context;
use inotify::{Inotify, WatchDescriptor};

use crate::monitored::PathEntry;

/// Manages inotify watches for pending (non-existent) paths.
pub struct InotifyManager {
    /// inotify instance watching parent dirs of pending paths
    pub inotify: Option<Inotify>,
    /// Watch descriptors kept alive so watches stay active
    /// (watched_path, watch_descriptor) — maps wd back to the directory we're watching.
    pub watches: Vec<(PathBuf, WatchDescriptor)>,
    /// Paths that didn't exist at add/startup time, retried on directory creation
    pub pending_paths: Vec<(PathBuf, PathEntry)>,
}

impl InotifyManager {
    pub fn new() -> Self {
        Self {
            inotify: None,
            watches: Vec::new(),
            pending_paths: Vec::new(),
        }
    }

    /// Initialize the inotify instance.
    pub fn init(&mut self) -> anyhow::Result<()> {
        self.inotify = Some(Inotify::init().context("inotify_init")?);
        Ok(())
    }

    /// Add a pending path to be monitored when it's created.
    pub fn add_pending_path(&mut self, path: PathBuf, entry: PathEntry) {
        self.pending_paths.push((path, entry));
    }

    /// Remove a pending path (e.g., when it's created).
    pub fn remove_pending_path(&mut self, path: &PathBuf) -> Option<(PathBuf, PathEntry)> {
        if let Some(pos) = self.pending_paths.iter().position(|(p, _)| p == path) {
            Some(self.pending_paths.remove(pos))
        } else {
            None
        }
    }

    /// Check if there are any pending paths.
    pub fn has_pending_paths(&self) -> bool {
        !self.pending_paths.is_empty()
    }

    /// Get the number of pending paths.
    pub fn pending_paths_count(&self) -> usize {
        self.pending_paths.len()
    }
}
