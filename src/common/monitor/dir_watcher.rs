use std::path::{Path, PathBuf};

use crate::common::dir_cache;
use crate::common::fid_parser::{
    mark_directory_at, mark_recursive_with_depth, open_dir_safe, path_mask_from_options,
};
use crate::common::filters::PathOptions;
use crate::common::monitored::PathEntry;

use super::Monitor;

impl Monitor {
    /// Set up inotify watches on:
    /// 1. Parent directories of pending paths (retry when created)
    /// 2. Recursively-monitored directory roots for new subdir creation and self-deletion.
    /// 3. Non-recursive monitored directories for self-deletion.
    ///
    /// This function is **incremental**: it only adds watches that don't already
    /// exist in `self.inotify_state.watches`.  Previously it cleared the vector
    /// on every call, which dropped watches added by `on_new_subdirectory` during
    /// inotify event processing — causing events from those watches to be silently
    /// ignored (the WatchDescriptor lookup in `handle_inotify_events` would fail).
    pub(crate) fn setup_inotify_watches(&mut self) {
        use inotify::WatchMask;

        let ino = match self.inotify_state.inotify.as_ref() {
            Some(ino) => ino,
            None => return,
        };

        let mask = WatchMask::CREATE | WatchMask::MOVED_TO;
        let dir_self_mask = WatchMask::DELETE_SELF | WatchMask::MOVE_SELF;
        let dir_root_mask = mask | dir_self_mask;

        // 1. Watch parent dirs of pending paths (skip if already tracked)
        for (path, _) in &self.inotify_state.pending_paths {
            if let Some(parent) = Self::nearest_existing_ancestor(path)
                && !self.inotify_state.watches.iter().any(|(p, _)| *p == parent)
                && let Ok(wd) = ino.watches().add(&parent, mask)
            {
                self.inotify_state.watches.push((parent, wd));
            }
        }

        // 2. Watch recursively-monitored directory roots
        for (path, opts) in &self.monitored_entries {
            if !opts.recursive || !path.is_dir() {
                continue;
            }
            if !self.inotify_state.watches.iter().any(|(p, _)| p == path)
                && let Ok(wd) = ino.watches().add(path, dir_root_mask)
            {
                debug_log!(
                    self.debug,
                    "inotify watch added on {} (mask: CREATE|MOVED_TO|DELETE_SELF|MOVE_SELF)",
                    path.display()
                );
                self.inotify_state.watches.push((path.clone(), wd));
            }
        }

        // 3. Watch non-recursive monitored directories for self-deletion
        for (path, opts) in &self.monitored_entries {
            if opts.recursive || !path.is_dir() {
                continue;
            }
            if !self.inotify_state.watches.iter().any(|(p, _)| p == path)
                && let Ok(wd) = ino.watches().add(path, dir_self_mask)
            {
                self.inotify_state.watches.push((path.clone(), wd));
            }
        }
    }

    /// Recursively add inotify watches for `dir` and all existing subdirectories.
    pub(crate) fn watch_recursive(
        inotify: &inotify::Inotify,
        mask: inotify::WatchMask,
        dir: &Path,
        watches: &mut Vec<(PathBuf, inotify::WatchDescriptor)>,
    ) {
        if watches.iter().any(|(p, _)| p == dir) {
            return;
        }
        if let Ok(wd) = inotify.watches().add(dir, mask) {
            watches.push((dir.to_path_buf(), wd));
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::watch_recursive(inotify, mask, &path, watches);
                }
            }
        }
    }

    /// Parse inotify events: handle directory deletion and new subdirectory creation.
    pub(crate) fn handle_inotify_events(&mut self) {
        let inotify = match self.inotify_state.inotify.as_mut() {
            Some(ino) => ino,
            None => return,
        };
        debug_log!(self.debug, "handle_inotify_events: called");
        let mut buf = [0u8; 4096];
        let events = match inotify.read_events(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                debug_log!(self.debug, "handle_inotify_events: read_events error: {e}");
                self.check_pending();
                return;
            }
        };

        let events: Vec<_> = events.collect();

        let dir_mask = inotify::EventMask::CREATE | inotify::EventMask::ISDIR;
        let dir_moved = inotify::EventMask::MOVED_TO | inotify::EventMask::ISDIR;

        // First pass: handle DELETE_SELF / MOVE_SELF on monitored directories.
        let mut deleted_paths: Vec<PathBuf> = Vec::new();
        for event in &events {
            if !event.mask.intersects(inotify::EventMask::DELETE_SELF)
                && !event.mask.intersects(inotify::EventMask::MOVE_SELF)
            {
                continue;
            }
            let Some(watched) = self
                .inotify_state
                .watches
                .iter()
                .find(|(_, wd)| *wd == event.wd)
                .map(|(p, _)| p.clone())
            else {
                continue;
            };
            if !self.paths.contains(&watched) {
                continue;
            }
            deleted_paths.push(watched);
        }
        for path in &deleted_paths {
            debug_log!(
                self.debug,
                "inotify: monitored directory deleted (self): {}",
                path.display()
            );
            let all_opts: Vec<PathOptions> =
                self.opts_for_path(path).into_iter().cloned().collect();
            if let Err(e) = self.remove_path(path, None) {
                eprintln!(
                    "[WARNING] inotify delete-self: failed to remove path '{}': {e}",
                    path.display()
                );
            }
            for opts in all_opts {
                // Dedup check: skip if already pending (F-030)
                let already_pending = self
                    .inotify_state
                    .pending_paths
                    .iter()
                    .any(|(p, e)| p == path && e.cmd == opts.cmd);
                if !already_pending {
                    self.inotify_state.pending_paths.push((
                        path.clone(),
                        PathEntry {
                            path: path.clone(),
                            recursive: Some(opts.recursive),
                            types: opts
                                .event_types
                                .as_ref()
                                .map(|v| v.iter().map(|t| t.to_string()).collect()),
                            size: opts.size_filter.map(|f| {
                                format!(
                                    "{}{}",
                                    f.op(),
                                    crate::common::utils::format_size(f.bytes())
                                )
                            }),
                            cmd: opts.cmd,
                            max_depth: opts.max_depth,
                            symlink_target: None,
                        },
                    ));
                }
            }
            self.add_temp_parent_mark(path);
        }
        if !deleted_paths.is_empty() {
            self.setup_inotify_watches();
            self.check_pending();
        }

        // Second pass: handle new subdirectory creation.
        for event in events {
            let is_new_dir = event.mask.intersects(dir_mask) || event.mask.intersects(dir_moved);
            if !is_new_dir {
                continue;
            }
            let Some(name) = event.name else { continue };
            let Some(parent) = self
                .inotify_state
                .watches
                .iter()
                .find(|(_, wd)| *wd == event.wd)
                .map(|(p, _)| p.clone())
            else {
                continue;
            };
            let new_dir = parent.join(name);
            self.on_new_subdirectory(&new_dir);
        }

        self.check_pending();
    }

    /// Add fanotify mark + cache + inotify watch for a newly detected subdirectory.
    /// Add fanotify mark + cache + inotify watch for a newly detected subdirectory.
    /// Returns a list of subdirectories that were discovered during recursive marking
    /// (i.e., directories that already existed but were not yet monitored).
    pub(crate) fn on_new_subdirectory(&mut self, path: &Path) -> Vec<PathBuf> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !canonical.is_dir() {
            return Vec::new();
        }
        let dev_id = match std::fs::metadata(&canonical)
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
        {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let Some((gi, _)) = self
            .fanotify
            .groups
            .iter()
            .find(|(_, g)| g.dev_id == dev_id)
        else {
            return Vec::new();
        };
        let path_mask = self
            .monitored_entries
            .iter()
            .map(|(_, o)| path_mask_from_options(o))
            .fold(0, |a, b| a | b);
        // Get the minimum max_depth from all monitored entries (most restrictive).
        // None means unlimited, so we only set a limit if ALL entries have a limit.
        let max_depth = self
            .monitored_entries
            .iter()
            .filter_map(|(_, o)| o.max_depth)
            .min();

        debug_log!(
            self.debug,
            "new subdirectory under recursive watch: {} (dev={})",
            canonical.display(),
            dev_id
        );

        let fan_fd = &self.fanotify.groups[gi].fan_fd;
        let dir_fd = match open_dir_safe(&canonical) {
            Ok(fd) => fd,
            Err(_) => return Vec::new(),
        };
        if mark_directory_at(fan_fd, &dir_fd, path_mask).is_err() {
            return Vec::new();
        }

        if let Some(ref cache) = self.fanotify.shared_dir_cache {
            dir_cache::cache_dir_handle(cache, &canonical);
        }
        let discovered = mark_recursive_with_depth(fan_fd, path_mask, &canonical, max_depth);
        if let Some(ref cache) = self.fanotify.shared_dir_cache {
            dir_cache::cache_recursive(cache, &canonical);
        }

        let ino = self.inotify_state.inotify.as_ref();
        let watches = &mut self.inotify_state.watches;
        if let Some(inotify) = ino {
            Self::watch_recursive(
                inotify,
                inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                &canonical,
                watches,
            );
        }

        discovered
    }

    /// Retry monitoring for paths that didn't exist at add time.
    pub(crate) fn check_pending(&mut self) {
        if self.inotify_state.pending_paths.is_empty()
            && self.inotify_state.temp_parent_marks.is_empty()
        {
            return;
        }

        if !self.inotify_state.pending_paths.is_empty() {
            debug_log!(
                self.debug,
                "check_pending: {} pending path(s)",
                self.inotify_state.pending_paths.len()
            );
        }
        let mut i = 0;
        while i < self.inotify_state.pending_paths.len() {
            if self.inotify_state.pending_paths[i].0.exists() {
                let entry = self.inotify_state.pending_paths.remove(i);
                match self.add_path(&entry.1) {
                    Ok(()) => {
                        info_log!("Path '{}' now exists — monitoring started.", entry.0.display());
                    }
                    Err(e) => {
                        eprintln!(
                            "[WARNING] Path '{}' exists but monitoring setup failed: {e}",
                            entry.0.display()
                        );
                        self.inotify_state.pending_paths.push(entry);
                    }
                }
            } else {
                i += 1;
            }
        }

        self.cleanup_temp_parent_marks();
        self.setup_inotify_watches();
        self.metrics
            .set_pending_paths(self.inotify_state.pending_paths.len() as i64);
    }
}
