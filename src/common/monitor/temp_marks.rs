use std::os::fd::AsRawFd;
use std::path::Path;

use crate::debug_log;

use crate::common::fid_parser::{
    FsGroup, mark_directory_at, open_dir_safe, path_mask_from_options, unmark_directory_at,
};
use crate::common::filters::PathOptions;

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
            .inotify_state
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

        let Some(factory) = self.fanotify.factory.clone() else {
            return false;
        };

        let group_key = if let Some((key, _)) = self
            .fanotify
            .groups
            .iter()
            .find(|(_, g)| g.dev_id == dev_id)
        {
            let fan_fd = &self.fanotify.groups[key].fan_fd;
            let dir_fd = match open_dir_safe(&canonical) {
                Ok(fd) => fd,
                Err(_) => return false,
            };
            if mark_directory_at(&factory, fan_fd, &dir_fd, path_mask).is_err() {
                return false;
            }
            self.fanotify.groups[key].ref_count += 1;
            key
        } else {
            let dir_fd = match open_dir_safe(&canonical) {
                Ok(fd) => fd,
                Err(_) => return false,
            };
            let new_fd =
                match factory.create_group_and_mark(&dir_fd, self.group_init_flags(), path_mask) {
                    Ok(fd) => fd,
                    Err(_) => return false,
                };
            let key = self.fanotify.groups.insert(FsGroup {
                dev_id,
                fan_fd: new_fd,
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
        self.inotify_state
            .temp_parent_marks
            .insert(target_path.to_path_buf(), (parent, group_key));
        true
    }

    /// Remove all temporary parent marks whose target path is now actively monitored.
    pub(crate) fn cleanup_temp_parent_marks(&mut self) {
        let to_remove: Vec<_> = self
            .inotify_state
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
        let Some((parent, key)) = self.inotify_state.temp_parent_marks.remove(target_path) else {
            return;
        };

        let canonical = parent.canonicalize().unwrap_or_else(|_| parent.clone());
        let factory = self.fanotify.factory.clone();

        let fan_fd_raw = self.fanotify.groups.get(key).map(|g| g.fan_fd.as_raw_fd());
        if let (Some(factory), Some(raw), Some(group)) =
            (factory.as_ref(), fan_fd_raw, self.fanotify.groups.get(key))
        {
            if let Ok(dir_fd) = open_dir_safe(&canonical) {
                let _ = unmark_directory_at(factory, &group.fan_fd, &dir_fd);
            }
            debug_log!(self.debug, "temp parent mark removed (fd {})", raw);
        }

        if let Some(group) = self.fanotify.groups.get_mut(key) {
            group.ref_count = group.ref_count.saturating_sub(1);
        }
        if self
            .fanotify
            .groups
            .get(key)
            .is_some_and(|g| g.ref_count == 0)
        {
            self.fanotify.groups.remove(key);
        }
    }
}
