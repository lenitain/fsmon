use anyhow::{Context, bail};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use fanotify_fid::consts::{
    AT_FDCWD, FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_MARK_FILESYSTEM, FAN_MARK_REMOVE, FAN_NONBLOCK,
    FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;

use crate::dir_cache;
use crate::fid_parser::{FsGroup, mark_directory, mark_recursive, path_mask_from_options};
use crate::filters::{self, PathOptions};
use crate::monitored::PathEntry;

use super::Monitor;

impl Monitor {
    pub fn add_path(&mut self, entry: &PathEntry) -> anyhow::Result<()> {
        debug_log!(
            self.debug,
            "add_path: path={} cmd={}",
            entry.path.display(),
            entry.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL)
        );
        let path = filters::resolve_recursion_check(&entry.path);

        let is_new_path = !self.paths.contains(&path);
        if !is_new_path {
            debug_log!(
                self.debug,
                "  path already monitored — adding cmd and updating fanotify mask"
            );
            let opts = PathOptions::try_from(entry)?;
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
                debug_log!(self.debug, "  updated fanotify mask to {:#x}", new_mask);
            }
            let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
            println!(
                "Monitoring entry: [{}] {} (recursive={})",
                cmd_label,
                path.display(),
                opts.recursive
            );
            self.metrics
                .set_monitored_paths(self.monitored_entries.len() as i64);
            return Ok(());
        }

        // Reject paths that overlap with the log directory.
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

        let opts = PathOptions::try_from(entry)?;
        if opts.cmd.as_deref() == Some("fsmon") {
            bail!(
                "Cannot monitor 'fsmon' process: fsmon daemon's own events \
                 are excluded from monitoring."
            );
        }

        let path_mask = path_mask_from_options(&opts);

        let cmd_label = opts.cmd.as_deref().unwrap_or(crate::monitored::CMD_GLOBAL);
        println!(
            "Monitoring entry: [{}] {} (recursive={})",
            cmd_label,
            path.display(),
            opts.recursive,
        );

        let dev_id = std::fs::metadata(&canonical)
            .ok()
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
            .unwrap_or(0);

        let existing_key = self
            .fs_groups
            .iter()
            .find_map(|(key, g)| if g.dev_id == dev_id { Some(key) } else { None });

        let group_key = if let Some(key) = existing_key {
            let fan_fd = &self.fs_groups[key].fan_fd;
            if let Err(e) = mark_directory(fan_fd, path_mask, &canonical) {
                eprintln!(
                    "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                    canonical.display(),
                    fan_fd.as_raw_fd(),
                    e
                );
            } else if opts.recursive && canonical.is_dir() {
                mark_recursive(fan_fd, path_mask, &canonical);
            }
            self.fs_groups[key].ref_count += 1;
            eprintln!(
                "[INFO] Monitoring {} on existing fd {}",
                canonical.display(),
                self.fs_groups[key].fan_fd.as_raw_fd()
            );
            key
        } else {
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
                .add_mark_upward(&new_fd, path_mask, &canonical, opts.recursive)
                .is_none()
            {
                bail!("Failed to mark {}: inode mark failed", canonical.display());
            }

            let mount_fd = Self::open_dir(&canonical)?;

            let key = self.fs_groups.insert(FsGroup {
                dev_id,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });

            self.spawn_fd_reader(key);
            key
        };

        self.path_to_group.insert(path.clone(), group_key);
        self.paths.push(path.clone());
        self.canonical_paths.push(canonical.clone());
        self.monitored_entries.push((path.clone(), opts.clone()));

        if canonical.is_dir()
            && let Some(ref cache) = self.shared_dir_cache
        {
            if opts.recursive {
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
        debug_log!(
            self.debug,
            "remove_path: path={} cmd={}",
            path.display(),
            cmd.unwrap_or("*")
        );

        let saved_opts = self.first_opt_for_path(path).cloned();

        let before = self.monitored_entries.len();
        self.monitored_entries.retain(|(p, o)| {
            if p != path {
                return true;
            }
            if let Some(c) = cmd {
                o.cmd.as_deref() != Some(c)
            } else {
                false
            }
        });
        let removed = before - self.monitored_entries.len();
        if removed == 0 {
            return Err(anyhow::anyhow!("Path not found: {}", path.display()));
        }

        let has_other = self.monitored_entries.iter().any(|(p, _)| p == path);

        if !has_other {
            if let Some(pos) = self.paths.iter().position(|p| p == path) {
                if let Some(ref opts) = saved_opts {
                    let path_mask = path_mask_from_options(opts);
                    if let Some(&key) = self.path_to_group.get(path) {
                        let canonical = &self.canonical_paths[pos];
                        let fan_fd = &self.fs_groups[key].fan_fd;
                        let _ = fanotify_mark(
                            fan_fd,
                            FAN_MARK_REMOVE | FAN_MARK_FILESYSTEM,
                            path_mask,
                            AT_FDCWD,
                            canonical,
                        );
                        let _ =
                            fanotify_mark(fan_fd, FAN_MARK_REMOVE, path_mask, AT_FDCWD, canonical);
                        self.fs_groups[key].ref_count =
                            self.fs_groups[key].ref_count.saturating_sub(1);
                        if self.fs_groups[key].ref_count == 0 {
                            self.fs_groups.remove(key);
                        }
                    }
                }
                self.paths.remove(pos);
                self.canonical_paths.remove(pos);
                self.path_to_group.remove(path);
            }
            println!("Removed entry: {}", path.display());
        } else {
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
            debug_log!(
                self.debug,
                "  updated fanotify mask to {:#x} (other cmd groups remain)",
                new_mask
            );
            let label = cmd.unwrap_or("?");
            println!("Removed entry: [{}] {}", label, path.display());
        }
        self.metrics
            .set_monitored_paths(self.monitored_entries.len() as i64);
        self.metrics.set_reader_groups(self.fs_groups.len() as i64);
        Ok(())
    }

    /// Check disk space for the log directory against the configured threshold.
    pub(crate) fn check_disk_space(log_dir: &std::path::Path, threshold_str: &str) {
        let threshold = match crate::utils::parse_disk_min_free(threshold_str) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[WARNING] Invalid disk-min-free '{}': {}", threshold_str, e);
                return;
            }
        };

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
}
