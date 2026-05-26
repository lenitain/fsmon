use std::path::{Path, PathBuf};

use crate::filters::{self, PathOptions};
use crate::FileEvent;

use super::Monitor;

impl Monitor {
    /// Get all PathOptions for a path from monitored_entries (single source of truth).
    pub(crate) fn opts_for_path(&self, path: &Path) -> Vec<&PathOptions> {
        self.monitored_entries
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, o)| o)
            .collect()
    }

    /// Get the first PathOptions entry for a path (for mask calculation, recursive flag, etc.).
    pub(crate) fn first_opt_for_path(&self, path: &Path) -> Option<&PathOptions> {
        self.monitored_entries
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, o)| o)
    }

    #[cfg(test)]
    pub(crate) fn should_output(&self, event: &FileEvent) -> bool {
        let opts = self.get_matching_path_options(&event.path);
        filters::should_output(opts, event)
    }

    /// Check output filters using a specific PathOptions instead of auto-detecting.
    pub(crate) fn should_output_for_opts(&self, event: &FileEvent, opts: &PathOptions) -> bool {
        filters::should_output(Some(opts), event)
    }

    /// Find the configured path that matches a given event path.
    /// Checks configured paths (direct or recursive prefix), then canonical paths.
    pub(crate) fn matching_path(&self, path: &Path) -> Option<&PathBuf> {
        filters::matching_path(&self.paths, &self.canonical_paths, path)
    }

    #[cfg(test)]
    pub(crate) fn is_path_in_scope(&self, path: &Path) -> bool {
        filters::is_path_in_scope(
            &self.paths,
            &self.monitored_entries,
            &self.canonical_paths,
            path,
        )
    }

    /// Check if event path is within scope of a specific PathOptions.
    /// Uses `monitored_entries` directly (not `path_options`).
    pub(crate) fn is_path_in_scope_for_opts(&self, event_path: &Path, opts: &PathOptions) -> bool {
        self.monitored_entries.iter().any(|(mp, stored_opts)| {
            if stored_opts.cmd != opts.cmd || stored_opts.recursive != opts.recursive {
                return false;
            }
            if opts.recursive {
                event_path.starts_with(mp)
            } else {
                event_path == mp.as_path() || event_path.parent() == Some(mp.as_path())
            }
        })
    }
}
