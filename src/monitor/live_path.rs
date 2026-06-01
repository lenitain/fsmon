use anyhow::{Context, bail};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use fanotify_fid::consts::{
    AT_FDCWD, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE, FAN_NONBLOCK,
    FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;

use crate::EventType;
use crate::dir_cache;
use crate::fid_parser::{FsGroup, mark_directory, mark_recursive, path_mask_from_options};
use crate::filters::{self, PathOptions};
use crate::monitored::PathEntry;
use crate::utils::parse_size_filter;

use super::Monitor;

impl Monitor {
    pub fn add_path(&mut self, entry: &PathEntry) -> anyhow::Result<()> {
        if self.debug {
            let cmd = entry.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
            eprintln!(
                "[DEBUG] add_path: path={} cmd={}",
                entry.path.display(),
                cmd
            );
        }
        let path = filters::resolve_recursion_check(&entry.path);

        let is_new_path = !self.paths.contains(&path);
        if !is_new_path {
            if self.debug {
                eprintln!(
                    "[DEBUG]   path already monitored — adding cmd and updating fanotify mask"
                );
            }
            let cmd = entry.cmd.as_deref().and_then(|c| {
                if c == crate::monitored::CMD_GLOBAL {
                    None
                } else {
                    Some(c.to_string())
                }
            });
            let event_types = entry.types.as_ref().map(|types| {
                types
                    .iter()
                    .filter_map(|s| s.parse::<EventType>().ok())
                    .collect()
            });
            let size_filter = entry
                .size
                .as_ref()
                .map(|s| parse_size_filter(s))
                .transpose()?;
            let recursive = entry.recursive.unwrap_or(false);
            let opts = PathOptions {
                size_filter,
                event_types,
                recursive,
                cmd,
            };
            self.monitored_entries.push((path.clone(), opts.clone()));

            // Update fanotify mask: OR all entries for this path
            let new_mask = self
                .monitored_entries
                .iter()
                .filter(|(p, _)| p == &path)
                .map(|(_, o)| path_mask_from_options(o))
                .fold(0, |a, b| a | b);
            if let Some(&gi) = self.path_to_group.get(&path) {
                let fan_fd = &self.fs_groups[gi].fan_fd;
                let canonical = self
                    .paths
                    .iter()
                    .position(|p| p == &path)
                    .and_then(|i| self.canonical_paths.get(i).cloned())
                    .unwrap_or_else(|| path.clone());
                let _ = mark_directory(fan_fd, new_mask, &canonical);
                if self.debug {
                    eprintln!("[DEBUG]   updated fanotify mask to {:#x}", new_mask);
                }
            }
            let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
            println!(
                "Monitoring entry: [{}] {} (recursive={})",
                cmd_label,
                path.display(),
                recursive
            );
            self.metrics
                .set_monitored_paths(self.monitored_entries.len() as i64);
            return Ok(());
        }

        // Reject paths that overlap with the log directory.
        // - Exact match (path == log dir) → always reject (it IS the log dir)
        // - Parent + recursive → reject (would capture log file writes)
        // - Parent + non-recursive → allow (only direct children, log files deeper)
        if let Some(ref log_dir) = self.log_dir {
            let log_canonical = log_dir.canonicalize().unwrap_or_else(|_| log_dir.clone());
            let is_exact = log_canonical == path;
            let is_parent_recursive =
                entry.recursive.unwrap_or(false) && log_canonical.starts_with(&path);
            if is_exact || is_parent_recursive {
                bail!(
                    "Cannot monitor '{}': {} — \
                     Tip: use a path outside the log directory, or use a different logging.path",
                    path.display(),
                    if is_exact {
                        "this path is the log directory itself".to_string()
                    } else {
                        format!("log directory '{}' is inside this path", log_dir.display())
                    },
                );
            }
        }

        if !path.exists() {
            // Avoid duplicate pending entries for the same (path, cmd)
            let already_pending = self
                .pending_paths
                .iter()
                .any(|(p, e)| p == &path && e.cmd == entry.cmd);
            if !already_pending {
                eprintln!(
                    "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                    path.display()
                );
                self.pending_paths.push((path.clone(), entry.clone()));
                self.setup_inotify_watches();
            }
            return Ok(());
        }

        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

        let event_types = entry.types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|s| s.parse::<EventType>().ok())
                .collect()
        });
        let size_filter = entry
            .size
            .as_ref()
            .map(|s| parse_size_filter(s))
            .transpose()?;
        let recursive = entry.recursive.unwrap_or(false);
        // `_global` in PathEntry means no process tracking → convert to None
        let cmd = entry.cmd.as_deref().and_then(|c| {
            if c == crate::monitored::CMD_GLOBAL {
                None
            } else {
                Some(c.to_string())
            }
        });
        // Reject cmd=fsmon: daemon's own events are excluded by PID filter.
        // This mirrors the validation in Monitor::new() for runtime socket adds.
        if cmd.as_deref() == Some("fsmon") {
            bail!(
                "Cannot monitor 'fsmon' process: fsmon daemon's own events \
                 are excluded from monitoring."
            );
        }

        let opts = PathOptions {
            size_filter,
            event_types,
            recursive,
            cmd,
        };

        let path_mask = path_mask_from_options(&opts);

        let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
        println!(
            "Monitoring entry: [{}] {} (recursive={})",
            cmd_label,
            path.display(),
            recursive,
        );

        // Determine filesystem device ID for dedup lookup
        let dev_id = std::fs::metadata(&canonical)
            .ok()
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
            .unwrap_or(0);

        // Find existing FsGroup for this filesystem
        let existing_idx = self.fs_groups.iter().position(|g| g.dev_id == dev_id);

        let group_idx = if let Some(idx) = existing_idx {
            // Reuse existing group — add inode mark
            let fan_fd = &self.fs_groups[idx].fan_fd;
            if let Err(e) = mark_directory(fan_fd, path_mask, &canonical) {
                eprintln!(
                    "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                    canonical.display(),
                    fan_fd.as_raw_fd(),
                    e
                );
            } else {
                if recursive && canonical.is_dir() {
                    mark_recursive(fan_fd, path_mask, &canonical);
                }
            }
            self.fs_groups[idx].ref_count += 1;
            eprintln!(
                "[INFO] Monitoring {} on existing fd {}",
                canonical.display(),
                self.fs_groups[idx].fan_fd.as_raw_fd()
            );
            idx
        } else {
            // New filesystem — create fanotify fd + mount fd
            let new_fd = fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            )
            .with_context(|| {
                format!(
                    "fanotify_init failed for {} (requires Linux 5.9+ kernel)",
                    canonical.display()
                )
            })?;

            if self
                .add_mark_upward(&new_fd, path_mask, &canonical, recursive)
                .is_none()
            {
                bail!("Failed to mark {}: inode mark failed", canonical.display());
            }

            // Open directory fd for handle resolution
            let mount_fd = Self::open_dir(&canonical)?;

            let idx = self.fs_groups.len();
            self.fs_groups.push(FsGroup {
                dev_id,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });

            // Spawn reader for this new group
            self.spawn_fd_reader(idx);
            idx
        };

        // Update path tracking
        self.path_to_group.insert(path.clone(), group_idx);
        self.paths.push(path.clone());
        self.canonical_paths.push(canonical.clone());
        self.monitored_entries.push((path.clone(), opts.clone()));

        // Pre-cache directory handles in the shared cache
        if canonical.is_dir()
            && let Some(ref cache) = self.shared_dir_cache
        {
            if recursive {
                dir_cache::cache_recursive(cache, &canonical);
            } else {
                dir_cache::cache_dir_handle(cache, &canonical);
            }
        }

        self.metrics
            .set_monitored_paths(self.monitored_entries.len() as i64);
        Ok(())
    }

    /// Set up inode-based fanotify monitoring for a directory.
    /// Returns `Some(())` on success, `None` if the inode mark failed.
    pub(crate) fn add_mark_upward(
        &self,
        new_fd: &OwnedFd,
        path_mask: u64,
        canonical: &std::path::Path,
        recursive: bool,
    ) -> Option<()> {
        match mark_directory(new_fd, path_mask, canonical) {
            Ok(()) => {
                eprintln!(
                    "[INFO] Monitoring {} (inode mark) on fd {}",
                    canonical.display(),
                    new_fd.as_raw_fd()
                );
                if recursive && canonical.is_dir() {
                    mark_recursive(new_fd, path_mask, canonical);
                }
                Some(())
            }
            Err(e) => {
                eprintln!(
                    "[WARNING] Cannot monitor {} (inode mark): {:#}",
                    canonical.display(),
                    e
                );
                None
            }
        }
    }

    pub fn remove_path(&mut self, path: &Path, cmd: Option<&str>) -> anyhow::Result<()> {
        if self.debug {
            let label = cmd.unwrap_or("*");
            eprintln!("[DEBUG] remove_path: path={} cmd={}", path.display(), label);
        }

        // Save path options BEFORE removing entries from monitored_entries.
        // first_opt_for_path() queries monitored_entries, so it must be called
        // before the retain below.
        let saved_opts = self.first_opt_for_path(path).cloned();

        // Remove matching entries from monitored_entries
        let before = self.monitored_entries.len();
        self.monitored_entries.retain(|(p, o)| {
            if p != path {
                return true;
            }
            if let Some(c) = cmd {
                o.cmd.as_deref() != Some(c) // keep if cmd doesn't match
            } else {
                false // remove all entries for this path
            }
        });
        let removed = before - self.monitored_entries.len();
        if removed == 0 {
            return Err(anyhow::anyhow!("Path not found: {}", path.display()));
        }

        // Check if other cmd groups still monitor this path
        let has_other = self.monitored_entries.iter().any(|(p, _)| p == path);

        if !has_other {
            // No more entries for this path — tear down fanotify
            if let Some(pos) = self.paths.iter().position(|p| p == path) {
                if let Some(ref opts) = saved_opts {
                    let path_mask = path_mask_from_options(opts);
                    if let Some(&gi) = self.path_to_group.get(path) {
                        let canonical = &self.canonical_paths[pos];
                        let fan_fd = &self.fs_groups[gi].fan_fd;
                        let _ = fanotify_mark(
                            fan_fd,
                            FAN_MARK_REMOVE | FAN_MARK_FILESYSTEM,
                            path_mask,
                            AT_FDCWD,
                            canonical,
                        );
                        let _ =
                            fanotify_mark(fan_fd, FAN_MARK_REMOVE, path_mask, AT_FDCWD, canonical);
                        self.fs_groups[gi].ref_count =
                            self.fs_groups[gi].ref_count.saturating_sub(1);
                        if self.fs_groups[gi].ref_count == 0 {
                            self.fs_groups.remove(gi);
                            self.path_to_group.iter_mut().for_each(|(_, idx)| {
                                if *idx > gi {
                                    *idx -= 1;
                                }
                            });
                        }
                    }
                }
                self.paths.remove(pos);
                self.canonical_paths.remove(pos);
                self.path_to_group.remove(path);
            }
            println!("Removed entry: {}", path.display());
        } else {
            // Other cmd groups still exist — update fanotify mask
            let new_mask = self
                .monitored_entries
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, o)| path_mask_from_options(o))
                .fold(0, |a, b| a | b);
            if let Some(&gi) = self.path_to_group.get(path) {
                let fan_fd = &self.fs_groups[gi].fan_fd;
                let canonical = self
                    .paths
                    .iter()
                    .position(|p| p == path)
                    .and_then(|i| self.canonical_paths.get(i).cloned())
                    .unwrap_or_else(|| path.to_path_buf());
                let _ = mark_directory(fan_fd, new_mask, &canonical);
            }
            if self.debug {
                eprintln!(
                    "[DEBUG]   updated fanotify mask to {:#x} (other cmd groups remain)",
                    new_mask
                );
            }
            let label = cmd.unwrap_or("?");
            println!("Removed entry: [{}] {}", label, path.display());
        }
        self.metrics
            .set_monitored_paths(self.monitored_entries.len() as i64);
        self.metrics.set_reader_groups(self.fs_groups.len() as i64);
        Ok(())
    }

    /// Check disk space for the log directory against the configured threshold.
    /// Prints a warning if free space is below the threshold.
    pub(crate) fn check_disk_space(log_dir: &std::path::Path, threshold_str: &str) {
        let threshold = match crate::utils::parse_disk_min_free(threshold_str) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[WARNING] Invalid disk-min-free '{}': {}", threshold_str, e);
                return;
            }
        };

        // Get filesystem stats
        let stat = match nix::sys::statvfs::statvfs(log_dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[WARNING] Cannot stat filesystem for '{}': {}",
                    log_dir.display(),
                    e
                );
                return;
            }
        };

        let block_size = stat.block_size() as u64;
        let total = stat.blocks() as u64 * block_size;
        let free = stat.blocks_available() as u64 * block_size;

        if total == 0 {
            return;
        }

        let below = match threshold {
            crate::utils::DiskFreeThreshold::Percent(min_pct) => {
                let free_pct = (free as f64 / total as f64) * 100.0;
                if free_pct < min_pct {
                    eprintln!(
                        "[WARNING] Low disk space on '{}': {:.1}% free ({}/{}), \
                         threshold is {}%",
                        log_dir.display(),
                        free_pct,
                        crate::utils::format_size(free as i64),
                        crate::utils::format_size(total as i64),
                        min_pct,
                    );
                    true
                } else {
                    false
                }
            }
            crate::utils::DiskFreeThreshold::Bytes(min_bytes) => {
                if free < min_bytes {
                    eprintln!(
                        "[WARNING] Low disk space on '{}': {} free, threshold is {}",
                        log_dir.display(),
                        crate::utils::format_size(free as i64),
                        crate::utils::format_size(min_bytes as i64),
                    );
                    true
                } else {
                    false
                }
            }
        };

        if !below {
            eprintln!(
                "[INFO] Disk space OK on '{}': {} free",
                log_dir.display(),
                crate::utils::format_size(free as i64),
            );
        }
    }

    /// Find the deepest existing ancestor directory of a path.
    /// Walks up until it finds a directory that exists, or returns None.
    pub(crate) fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
        let mut p = path.to_path_buf();
        loop {
            if p.is_dir() {
                return Some(p);
            }
            if !p.pop() {
                return None;
            }
        }
    }

    /// Set up inotify watches on:
    /// 1. Parent directories of pending paths (retry when created)
    /// 2. Recursively-monitored directories with inode marks and all their
    ///    existing subdirectories (detect new subdirs created after startup).
    ///    Full recursive walk — re-discovers subdirs on each call so watches
    ///    survive `setup_inotify_watches` → `clear()` cycles triggered by
    ///    `check_pending` / canonical-root cleanup.
    pub(crate) fn setup_inotify_watches(&mut self) {
        use inotify::WatchMask;

        // Drop old watches
        self._inotify_watches.clear();

        let inotify = self.inotify.as_ref();
        let watches = &mut self._inotify_watches;

        let ino = match inotify {
            Some(ino) => ino,
            None => return,
        };

        let mask = WatchMask::CREATE | WatchMask::MOVED_TO;
        // Mask for monitored directory roots themselves: detect when the
        // directory is deleted (DELETE_SELF) so we can move it to pending.
        // fanotify FAN_DELETE_SELF is unreliable with FID mode, so inotify
        // acts as the primary trigger for the delete→pending transition.
        let dir_self_mask = WatchMask::DELETE_SELF | WatchMask::MOVE_SELF;
        let dir_root_mask = mask | dir_self_mask;

        // 1. Watch parent dirs of pending paths
        for (path, _) in &self.pending_paths {
            if let Some(parent) = Self::nearest_existing_ancestor(path)
                && let Ok(wd) = ino.watches().add(&parent, mask)
            {
                watches.push((parent, wd));
            }
        }

        // 2. Watch recursively-monitored directory roots (inode marks)
        //    for new subdirectory creation AND self-deletion detection.
        //    Watching just the roots is sufficient — on_new_subdirectory adds
        //    watches for newly created subdirs as they appear.  Recursively
        //    walking ~25k subdirs at startup would add excessive latency.
        for (path, opts) in &self.monitored_entries {
            if !opts.recursive || !path.is_dir() {
                continue;
            }
            if !watches.iter().any(|(p, _)| p == path)
                && let Ok(wd) = ino.watches().add(path, dir_root_mask)
            {
                if self.debug {
                    eprintln!(
                        "[DEBUG] inotify watch added on {} (mask: CREATE|MOVED_TO|DELETE_SELF|MOVE_SELF)",
                        path.display()
                    );
                }
                watches.push((path.clone(), wd));
            }
        }

        // 3. Watch non-recursive monitored directories for self-deletion.
        //    Recursive dirs are covered by section 2 above.
        for (path, opts) in &self.monitored_entries {
            if opts.recursive || !path.is_dir() {
                continue;
            }
            if !watches.iter().any(|(p, _)| p == path)
                && let Ok(wd) = ino.watches().add(path, dir_self_mask)
            {
                watches.push((path.clone(), wd));
            }
        }
    }

    /// Add inotify watches for `dir` and all existing subdirectories.
    /// Used only for newly detected subdirectories (not at startup).
    fn watch_recursive(
        inotify: &inotify::Inotify,
        mask: inotify::WatchMask,
        dir: &std::path::Path,
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

    /// Parse inotify events and handle new subdirectory creation under
    /// recursively-monitored directories.  Also retries pending paths.
    pub(crate) fn handle_inotify_events(&mut self) {
        let inotify = match self.inotify.as_mut() {
            Some(ino) => ino,
            None => return,
        };
        if self.debug {
            eprintln!("[DEBUG] handle_inotify_events: called");
        }
        let mut buf = [0u8; 4096];
        let events = match inotify.read_events(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                if self.debug {
                    eprintln!("[DEBUG] handle_inotify_events: read_events error: {e}");
                }
                self.check_pending();
                return;
            }
        };

        let events: Vec<_> = events.collect();

        let dir_mask = inotify::EventMask::CREATE | inotify::EventMask::ISDIR;
        let dir_moved = inotify::EventMask::MOVED_TO | inotify::EventMask::ISDIR;

        // First pass: handle DELETE_SELF / MOVE_SELF on monitored directories.
        // fanotify FAN_DELETE_SELF is unreliable with FID mode, so inotify is
        // the primary trigger for the delete→pending transition.
        let mut deleted_paths: Vec<PathBuf> = Vec::new();
        for event in &events {
            if !event.mask.intersects(inotify::EventMask::DELETE_SELF)
                && !event.mask.intersects(inotify::EventMask::MOVE_SELF)
            {
                continue;
            }
            // Map wd → watched directory path
            let Some(watched) = self
                ._inotify_watches
                .iter()
                .find(|(_, wd)| *wd == event.wd)
                .map(|(p, _)| p.clone())
            else {
                continue;
            };

            // Only handle directories that are actively monitored
            if !self.paths.contains(&watched) {
                continue;
            }
            deleted_paths.push(watched);
        }
        for path in &deleted_paths {
            if self.debug {
                eprintln!(
                    "[DEBUG] inotify: monitored directory deleted (self): {}",
                    path.display()
                );
            }
            let all_opts: Vec<PathOptions> =
                self.opts_for_path(path).into_iter().cloned().collect();
            if let Err(e) = self.remove_path(path, None) {
                eprintln!(
                    "[WARNING] inotify delete-self: failed to remove path '{}': {e}",
                    path.display()
                );
            }
            for opts in all_opts {
                self.pending_paths.push((
                    path.clone(),
                    PathEntry {
                        path: path.clone(),
                        recursive: Some(opts.recursive),
                        types: opts
                            .event_types
                            .as_ref()
                            .map(|v| v.iter().map(|t| t.to_string()).collect()),
                        size: opts
                            .size_filter
                            .map(|f| format!("{}{}", f.op, crate::utils::format_size(f.bytes))),
                        cmd: opts.cmd,
                    },
                ));
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

            // Map wd → watched directory path
            let Some(parent) = self
                ._inotify_watches
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

    /// Add fanotify inode mark + cache + inotify watch for a newly detected
    /// subdirectory under a recursively-monitored path.
    pub(crate) fn on_new_subdirectory(&mut self, path: &std::path::Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !canonical.is_dir() {
            return;
        }
        let dev_id = match std::fs::metadata(&canonical)
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
        {
            Ok(d) => d,
            Err(_) => return,
        };
        let Some(gi) = self.fs_groups.iter().position(|g| g.dev_id == dev_id) else {
            return;
        };
        // Compute combined mask from all monitored entries
        let path_mask = self
            .monitored_entries
            .iter()
            .map(|(_, o)| path_mask_from_options(o))
            .fold(0, |a, b| a | b);

        if self.debug {
            eprintln!(
                "[DEBUG] new subdirectory under recursive watch: {} (dev={})",
                canonical.display(),
                dev_id
            );
        }

        let fan_fd = &self.fs_groups[gi].fan_fd;
        if mark_directory(fan_fd, path_mask, &canonical).is_err() {
            return;
        }

        // Cache handle and mark/cache subdirectories recursively
        if let Some(ref cache) = self.shared_dir_cache {
            dir_cache::cache_dir_handle(cache, &canonical);
        }
        mark_recursive(fan_fd, path_mask, &canonical);
        if let Some(ref cache) = self.shared_dir_cache {
            dir_cache::cache_recursive(cache, &canonical);
        }

        // Watch the new directory and all its existing children so
        // future grandchildren are detected without waiting for the next
        // setup_inotify_watches()→clear() cycle.
        let ino = self.inotify.as_ref();
        let watches = &mut self._inotify_watches;
        if let Some(inotify) = ino {
            Self::watch_recursive(
                inotify,
                inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                &canonical,
                watches,
            );
        }
    }

    /// Retry setting up fanotify monitoring for paths that didn't exist before.
    /// Called when inotify detects directory creation under a watched parent.
    pub(crate) fn check_pending(&mut self) {
        // Fast path: nothing pending and no temp marks to clean up.
        if self.pending_paths.is_empty() && self.temp_parent_marks.is_empty() {
            return;
        }

        if self.debug && !self.pending_paths.is_empty() {
            eprintln!(
                "[DEBUG] check_pending: {} pending path(s)",
                self.pending_paths.len()
            );
        }
        let mut i = 0;
        while i < self.pending_paths.len() {
            let path_exists = self.pending_paths[i].0.exists();
            if path_exists {
                // Use remove (not swap_remove) so that failed entries can be
                // re-inserted without corrupting iteration order.
                let entry = self.pending_paths.remove(i);
                match self.add_path(&entry.1) {
                    Ok(()) => {
                        eprintln!(
                            "[INFO] Path '{}' now exists — monitoring started.",
                            entry.0.display()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[WARNING] Path '{}' exists but monitoring setup failed: {e}",
                            entry.0.display()
                        );
                        // Re-insert so the entry will be retried on the next
                        // check_pending invocation.
                        self.pending_paths.push(entry);
                        // Don't advance i: remove() already shifted elements left.
                    }
                }
            } else {
                i += 1;
            }
        }

        // Clean up any temp parent marks whose target path is now being
        // actively monitored (added by add_path above).
        self.cleanup_temp_parent_marks();

        // Refresh inotify watches for remaining pending paths
        self.setup_inotify_watches();
        self.metrics
            .set_pending_paths(self.pending_paths.len() as i64);
    }

    // ---- Temporary parent marks ----

    /// Add a temporary fanotify inode mark on the nearest existing ancestor
    /// of `target_path`, so that events during the recreate window are
    /// captured.  Returns `true` if a mark was added.
    pub(crate) fn add_temp_parent_mark(&mut self, target_path: &std::path::Path) -> bool {
        let parent = match Self::nearest_existing_ancestor(target_path) {
            Some(p) => p,
            None => return false,
        };
        if parent == *target_path {
            // target_path itself exists — no need for a temp mark
            return false;
        }

        let canonical = parent.canonicalize().unwrap_or_else(|_| parent.clone());

        // Compute the combined mask (same as the original monitored entry)
        let saved_entries: Vec<_> = self
            .monitored_entries
            .iter()
            .filter(|(p, _)| p == target_path)
            .cloned()
            .collect();
        // Also check pending_paths for the target (entries were moved there)
        let pending_opts: Vec<PathOptions> = self
            .pending_paths
            .iter()
            .filter(|(p, _)| p == target_path)
            .map(|(_, entry)| {
                let types = entry.types.as_ref().map(|t| {
                    t.iter()
                        .filter_map(|s| s.parse::<crate::EventType>().ok())
                        .collect()
                });
                PathOptions {
                    size_filter: None,
                    event_types: types,
                    recursive: entry.recursive.unwrap_or(false),
                    cmd: entry.cmd.clone().and_then(|c| {
                        if c == crate::monitored::CMD_GLOBAL {
                            None
                        } else {
                            Some(c)
                        }
                    }),
                }
            })
            .collect();

        if saved_entries.is_empty() && pending_opts.is_empty() {
            return false;
        }

        let path_mask: u64 = saved_entries
            .iter()
            .map(|(_, o)| path_mask_from_options(o))
            .chain(pending_opts.iter().map(path_mask_from_options))
            .fold(0, |a, b| a | b);

        if path_mask == 0 {
            return false;
        }

        // Determine filesystem device ID for dedup
        let dev_id = std::fs::metadata(&canonical)
            .ok()
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
            .unwrap_or(0);

        // Try to reuse an existing FsGroup on the same filesystem
        let group_idx = if let Some(idx) = self.fs_groups.iter().position(|g| g.dev_id == dev_id) {
            // Reuse — add inode mark on parent
            let fan_fd = &self.fs_groups[idx].fan_fd;
            if mark_directory(fan_fd, path_mask, &canonical).is_err() {
                return false;
            }
            self.fs_groups[idx].ref_count += 1;
            idx
        } else {
            // Create a new fanotify fd for the parent
            use fanotify_fid::consts::{
                FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_NONBLOCK, FAN_REPORT_DIR_FID, FAN_REPORT_FID,
                FAN_REPORT_NAME,
            };
            let new_fd = match fanotify_fid::prelude::fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            ) {
                Ok(fd) => fd,
                Err(_) => return false,
            };
            if mark_directory(&new_fd, path_mask, &canonical).is_err() {
                drop(new_fd);
                return false;
            }
            let mount_fd = match Self::open_dir(&canonical) {
                Ok(fd) => fd,
                Err(_) => {
                    drop(new_fd);
                    return false;
                }
            };
            let idx = self.fs_groups.len();
            self.fs_groups.push(FsGroup {
                dev_id,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });
            self.spawn_fd_reader(idx);
            idx
        };

        if self.debug {
            eprintln!(
                "[DEBUG] temp parent mark: {} ← watching for {}",
                canonical.display(),
                target_path.display()
            );
        }
        self.temp_parent_marks
            .insert(target_path.to_path_buf(), (parent, group_idx));
        true
    }

    /// Remove all temporary parent marks whose target path is now being
    /// actively monitored (i.e. is in `self.paths`).
    fn cleanup_temp_parent_marks(&mut self) {
        let mut to_remove: Vec<PathBuf> = Vec::new();
        for target in self.temp_parent_marks.keys() {
            if self.paths.contains(target) {
                to_remove.push(target.clone());
            }
        }
        for target in &to_remove {
            self.remove_temp_parent_mark(target);
        }
    }

    /// Remove a single temporary parent mark and tear down its fanotify
    /// resources.  The caller must ensure `target_path` is in
    /// `temp_parent_marks`.
    fn remove_temp_parent_mark(&mut self, target_path: &std::path::Path) {
        let Some((parent, gi)) = self.temp_parent_marks.remove(target_path) else {
            return;
        };

        let canonical = parent.canonicalize().unwrap_or_else(|_| parent.clone());

        // Remove the fanotify mark(s) on the parent directory
        if gi < self.fs_groups.len() {
            let fan_fd_raw = self.fs_groups[gi].fan_fd.as_raw_fd();
            let _ = fanotify_fid::prelude::fanotify_mark(
                &self.fs_groups[gi].fan_fd,
                fanotify_fid::consts::FAN_MARK_REMOVE | fanotify_fid::consts::FAN_MARK_FILESYSTEM,
                0,
                fanotify_fid::consts::AT_FDCWD,
                &canonical,
            );
            let _ = fanotify_fid::prelude::fanotify_mark(
                &self.fs_groups[gi].fan_fd,
                fanotify_fid::consts::FAN_MARK_REMOVE,
                0,
                fanotify_fid::consts::AT_FDCWD,
                &canonical,
            );

            self.fs_groups[gi].ref_count = self.fs_groups[gi].ref_count.saturating_sub(1);
            if self.fs_groups[gi].ref_count == 0 {
                if self.debug {
                    eprintln!(
                        "[DEBUG] temp parent mark removed, freeing FsGroup {} (fd {})",
                        gi, fan_fd_raw
                    );
                }
                self.fs_groups.remove(gi);
                self.path_to_group.iter_mut().for_each(|(_, idx)| {
                    if *idx > gi {
                        *idx -= 1;
                    }
                });
                // Also fix up temp_parent_marks indices
                let mut updates: Vec<(PathBuf, (PathBuf, usize))> = Vec::new();
                for (tgt, (p, idx)) in self.temp_parent_marks.iter() {
                    if *idx > gi {
                        updates.push((tgt.clone(), (p.clone(), *idx - 1)));
                    }
                }
                for (tgt, val) in updates {
                    self.temp_parent_marks.insert(tgt, val);
                }
            }
        }
    }
}
