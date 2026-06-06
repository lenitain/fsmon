use std::os::fd::AsRawFd;
use std::path::Path;

use fanotify_fid::prelude::*;

use crate::fid_parser::{FsGroup, mark_directory, path_mask_from_options};
use crate::filters::PathOptions;

use super::Monitor;

impl Monitor {
    /// Add a temporary fanotify inode mark on the nearest existing ancestor
    /// of `target_path`, so that events during the recreate window are captured.
    pub(crate) fn add_temp_parent_mark(&mut self, target_path: &Path) -> bool {
        let parent = match Self::nearest_existing_ancestor(target_path) {
            Some(p) => p,
            None => return false,
        };
        if parent == *target_path {
            return false;
        }

        let canonical = parent.canonicalize().unwrap_or_else(|_| parent.clone());

        let saved_entries: Vec<_> = self
            .monitored_entries
            .iter()
            .filter(|(p, _)| p == target_path)
            .cloned()
            .collect();
        let pending_opts: Vec<PathOptions> = self
            .pending_paths
            .iter()
            .filter(|(p, _)| p == target_path)
            .filter_map(|(_, entry)| PathOptions::try_from(entry).ok())
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

        let dev_id = std::fs::metadata(&canonical)
            .ok()
            .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
            .unwrap_or(0);

        let group_key =
            if let Some((key, _)) = self.fs_groups.iter().find(|(_, g)| g.dev_id == dev_id) {
                let fan_fd = &self.fs_groups[key].fan_fd;
                if mark_directory(fan_fd, path_mask, &canonical).is_err() {
                    return false;
                }
                self.fs_groups[key].ref_count += 1;
                key
            } else {
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
                let key = self.fs_groups.insert(FsGroup {
                    dev_id,
                    fan_fd: new_fd,
                    mount_fd,
                    ref_count: 1,
                });
                self.spawn_fd_reader(key);
                key
            };

        debug_log!(
            self.debug,
            "temp parent mark: {} ← watching for {}",
            canonical.display(),
            target_path.display()
        );
        self.temp_parent_marks
            .insert(target_path.to_path_buf(), (parent, group_key));
        true
    }

    /// Remove all temporary parent marks whose target path is now actively monitored.
    pub(crate) fn cleanup_temp_parent_marks(&mut self) {
        let to_remove: Vec<_> = self
            .temp_parent_marks
            .keys()
            .filter(|target| self.paths.contains(target))
            .cloned()
            .collect();
        for target in to_remove {
            self.remove_temp_parent_mark(&target);
        }
    }

    /// Remove a single temporary parent mark and tear down its fanotify resources.
    fn remove_temp_parent_mark(&mut self, target_path: &Path) {
        let Some((parent, key)) = self.temp_parent_marks.remove(target_path) else {
            return;
        };

        let canonical = parent.canonicalize().unwrap_or_else(|_| parent.clone());

        if let Some(group) = self.fs_groups.get(key) {
            let fan_fd_raw = group.fan_fd.as_raw_fd();
            let _ = fanotify_mark(
                &group.fan_fd,
                fanotify_fid::consts::FAN_MARK_REMOVE | fanotify_fid::consts::FAN_MARK_FILESYSTEM,
                0,
                fanotify_fid::consts::AT_FDCWD,
                &canonical,
            );
            let _ = fanotify_mark(
                &group.fan_fd,
                fanotify_fid::consts::FAN_MARK_REMOVE,
                0,
                fanotify_fid::consts::AT_FDCWD,
                &canonical,
            );

            self.fs_groups[key].ref_count = self.fs_groups[key].ref_count.saturating_sub(1);
            if self.fs_groups[key].ref_count == 0 {
                debug_log!(
                    self.debug,
                    "temp parent mark removed, freeing FsGroup (fd {})",
                    fan_fd_raw
                );
                self.fs_groups.remove(key);
            }
        }
    }
}
