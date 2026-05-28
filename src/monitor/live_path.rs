use anyhow::{Context, bail};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use fanotify_fid::consts::{
    AT_FDCWD, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_MARK_ADD, FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE,
    FAN_NONBLOCK, FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;

use crate::dir_cache;
use crate::fid_parser::{
    FsGroup, mark_directory, mark_recursive, path_mask_from_options,
};
use crate::filters::{self, PathOptions};
use crate::monitored::PathEntry;
use crate::utils::parse_size_filter;
use crate::EventType;

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
                if self.fs_groups[gi].is_fs_mark {
                    let _ = fanotify_mark(
                        fan_fd,
                        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                        new_mask,
                        AT_FDCWD,
                        &canonical,
                    );
                } else {
                    let _ = mark_directory(fan_fd, new_mask, &canonical);
                }
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
            self.metrics.set_monitored_paths(self.monitored_entries.len() as i64);
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
            // Reuse existing group — just add inode mark if needed
            if !self.fs_groups[idx].is_fs_mark {
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

            let is_fs_mark = match fanotify_mark(
                &new_fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                path_mask,
                AT_FDCWD,
                &canonical,
            ) {
                Ok(()) => {
                    eprintln!(
                        "[INFO] Monitoring {} (fs mark) on new fd {}",
                        canonical.display(),
                        new_fd.as_raw_fd()
                    );
                    true
                }
                Err(FanotifyError::Mark(code)) if code == libc::EXDEV => {
                    // Fall back to inode mark
                    match mark_directory(&new_fd, path_mask, &canonical) {
                        Ok(()) => {
                            eprintln!(
                                "[INFO] Monitoring {} (inode mark) on new fd {}",
                                canonical.display(),
                                new_fd.as_raw_fd()
                            );
                            if recursive && canonical.is_dir() {
                                mark_recursive(&new_fd, path_mask, &canonical);
                            }
                            false
                        }
                        Err(e) => {
                            eprintln!(
                                "[WARNING] Cannot monitor {} (inode mark): {:#}",
                                canonical.display(),
                                e
                            );
                            drop(new_fd);
                            bail!("Failed to mark {}: {:#}", canonical.display(), e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARNING] Cannot monitor {}: {:#}", canonical.display(), e);
                    drop(new_fd);
                    bail!("Failed to mark {}: {:#}", canonical.display(), e);
                }
            };

            // Open directory fd for handle resolution
            let mount_fd = Self::open_dir(&canonical)?;

            let idx = self.fs_groups.len();
            self.fs_groups.push(FsGroup {
                dev_id,
                is_fs_mark,
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

        self.metrics.set_monitored_paths(self.monitored_entries.len() as i64);
        Ok(())
    }

    pub fn remove_path(&mut self, path: &Path, cmd: Option<&str>) -> anyhow::Result<()> {
        if self.debug {
            let label = cmd.unwrap_or("*");
            eprintln!("[DEBUG] remove_path: path={} cmd={}", path.display(), label);
        }

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
                let canonical = &self.canonical_paths[pos];
                if let Some(opts) = self.first_opt_for_path(path) {
                    let path_mask = path_mask_from_options(opts);
                    if let Some(&gi) = self.path_to_group.get(path) {
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
                if self.fs_groups[gi].is_fs_mark {
                    let _ = fanotify_mark(
                        fan_fd,
                        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                        new_mask,
                        AT_FDCWD,
                        &canonical,
                    );
                } else {
                    let _ = mark_directory(fan_fd, new_mask, &canonical);
                }
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
        self.metrics.set_monitored_paths(self.monitored_entries.len() as i64);
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
                eprintln!("[WARNING] Cannot stat filesystem for '{}': {}", log_dir.display(), e);
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

    /// Set up inotify watches on parent directories of all pending paths.
    /// Removes stale watches first.
    pub(crate) fn setup_inotify_watches(&mut self) {
        use inotify::WatchMask;

        // Drop old watches
        self._inotify_watches.clear();

        let inotify = match self.inotify.as_ref() {
            Some(ino) => ino,
            None => return,
        };

        for (path, _) in &self.pending_paths {
            if let Some(parent) = Self::nearest_existing_ancestor(path)
                && let Ok(wd) = inotify
                    .watches()
                    .add(&parent, WatchMask::CREATE | WatchMask::MOVED_TO)
            {
                self._inotify_watches.push(wd);
            }
        }
    }

    /// Retry setting up fanotify monitoring for paths that didn't exist before.
    /// Called when inotify detects directory creation under a watched parent.
    pub(crate) fn check_pending(&mut self) {
        if self.debug && !self.pending_paths.is_empty() {
            eprintln!(
                "[DEBUG] check_pending: {} pending path(s)",
                self.pending_paths.len()
            );
        }
        let mut i = 0;
        while i < self.pending_paths.len() {
            let (path, _) = &self.pending_paths[i];
            if path.exists() {
                let entry = self.pending_paths.swap_remove(i);
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
                        i += 1;
                    }
                }
            } else {
                i += 1;
            }
        }

        // Refresh inotify watches for remaining pending paths
        self.setup_inotify_watches();
        self.metrics.set_pending_paths(self.pending_paths.len() as i64);
    }
}
