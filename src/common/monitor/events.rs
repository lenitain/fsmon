use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fanotify_fid::consts::FAN_Q_OVERFLOW;
use fanotify_fid::types::FidEvent;

use crate::common::fid_parser::mask_to_event_types;
#[cfg(test)]
use crate::common::filters;
use crate::common::filters::PathOptions;
use crate::common::monitored::PathEntry;
use crate::common::utils::{format_size, get_process_info_by_pid};
use crate::common::{EventType, FileEvent};
use proc_tree::ProcessStore;

use super::Monitor;

/// Pending event ready for broadcast, held back to allow late proc event drain.
pub(crate) struct PendingEvent {
    pub event: FileEvent,
    pub cmd_name: String,
    pub pid: u32,
}

impl Monitor {
    /// Process a batch of fanotify events: match paths, filter, build FileEvents.
    /// Events are NOT sent to broadcast here — they are returned as PendingEvents
    /// so the caller can drain proc events and resolve "unknown" fields before
    /// publishing. Metrics are still incremented immediately.
    pub(crate) fn process_event_batch(&mut self, events: &[FidEvent]) -> Vec<PendingEvent> {
        let mut pending: Vec<PendingEvent> = Vec::new();

        for raw in events {
            if raw.mask() & FAN_Q_OVERFLOW != 0 {
                eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                continue;
            }

            let event_types = mask_to_event_types(raw.mask());
            let matched_path = self.matching_path(raw.path()).cloned();

            // Detect canonical root DELETE_SELF — needs cleanup after recording.
            let is_delete_self = event_types.contains(&EventType::DeleteSelf)
                || event_types.contains(&EventType::MovedFrom)
                || event_types.contains(&EventType::Delete);
            let is_canonical_root =
                is_delete_self && self.canonical_paths.iter().any(|cp| cp == raw.path());

            let event_pid = raw.pid().unsigned_abs();

            // Exclude fsmon daemon's own events to prevent self-triggering.
            // This is the safety net that also covers socket files (lock.sock,
            // daemon.sock): their bind/unlink produce FAN_CREATE/FAN_DELETE,
            // but those events carry daemon_pid and are skipped here.  See
            // also the comment in add.rs explaining why sockets don't need
            // a dedicated path guard.
            if event_pid == self.daemon_pid {
                debug_log!(self.debug, "skip daemon self-event (pid={})", event_pid);
                continue;
            }

            // Also filter events from fsmon's log directory to prevent
            // feedback loops when cmd=global is used.
            if raw.path().starts_with("/var/log/fsmon") {
                debug_log!(self.debug, "skip fsmon log event: {}", raw.path().display());
                continue;
            }

            // Match event against ALL cmd groups for this path.
            // Computed BEFORE canonical-root cleanup — DELETE_SELF must be
            // recorded before the path is removed from monitored_entries.
            let matching_entries = self.matching_opts_for_event(raw.path());

            // Immediately add fanotify marks for newly created subdirectories
            // under recursively-monitored paths.  Waiting for inotify would
            // create a race window where events inside the new subdirectory
            // arrive before the mark is placed.
            let is_new_dir = event_types.contains(&EventType::Create)
                || event_types.contains(&EventType::MovedTo);
            if is_new_dir && raw.path().is_dir() {
                for (monitored, opts) in &matching_entries {
                    if opts.recursive && raw.path() != *monitored {
                        self.on_new_subdirectory(raw.path());
                        break;
                    }
                }
            }
            if matching_entries.is_empty() {
                debug_log!(
                    self.debug,
                    "event on {} (pid={}): no matching entries",
                    raw.path().display(),
                    event_pid
                );
            }
            for (_monitored_path, opts) in &matching_entries {
                // Check process tree filter
                if !self.matches_process_tree(opts.cmd.as_deref(), event_pid) {
                    continue;
                }

                for event_type in &event_types {
                    let event = self.build_file_event_for_opts(raw, *event_type, opts);

                    if !self.is_path_in_scope_for_opts(&event.path, opts) {
                        debug_log!(self.debug, "  -> out of scope for this opts");
                        continue;
                    }

                    if self.should_output_for_opts(&event, opts) {
                        debug_log!(
                            self.debug,
                            "  -> {}_log.jsonl",
                            opts.cmd.as_deref().unwrap_or("global")
                        );
                        let cmd_name = opts
                            .cmd
                            .as_deref()
                            .unwrap_or(crate::common::monitored::CMD_GLOBAL)
                            .to_string();

                        pending.push(PendingEvent {
                            event,
                            cmd_name,
                            pid: event_pid,
                        });
                    }
                }
            }

            // After recording DELETE_SELF events: remove the deleted
            // monitored directory from active monitoring and move to
            // pending_paths so it can be re-monitored if recreated.
            if is_canonical_root && let Some(ref path) = matched_path {
                self.handle_canonical_root_deleted(path);
            }
        }

        pending
    }

    /// Check if an event's PID matches the process tree filter for a cmd group.
    /// Returns true if no filter is set or if the PID is a descendant of the target cmd.
    fn matches_process_tree(&self, cmd: Option<&str>, event_pid: u32) -> bool {
        match cmd {
            Some(cmd_name) => {
                let matched = self
                    .proc
                    .store
                    .as_ref()
                    .map(|store| proc_tree::is_descendant(store, event_pid, cmd_name))
                    .unwrap_or(false);
                debug_log!(
                    self.debug,
                    "  check cmd=\"{}\" pid={}: {}",
                    cmd_name,
                    event_pid,
                    if matched { "MATCH" } else { "SKIP" }
                );
                matched
            }
            None => {
                debug_log!(self.debug, "  check cmd=global pid={}: MATCH", event_pid);
                true
            }
        }
    }

    /// Handle deletion of a monitored canonical root directory.
    /// Moves the path to pending_paths for re-monitoring on recreation,
    /// sets up inotify watches and temporary parent marks.
    fn handle_canonical_root_deleted(&mut self, path: &Path) {
        debug_log!(
            self.debug,
            "monitored directory deleted: {}",
            path.display()
        );
        // Preserve ALL cmd groups before removing
        let all_opts: Vec<PathOptions> = self.opts_for_path(path).into_iter().cloned().collect();
        if let Err(e) = self.remove_path(path, None) {
            eprintln!(
                "[WARNING] Failed to remove deleted path '{}': {e}",
                path.display()
            );
        }
        let path_buf = path.to_path_buf();
        for opts in all_opts {
            self.inotify_state.pending_paths.push((
                path_buf.clone(),
                PathEntry {
                    path: path_buf.clone(),
                    recursive: Some(opts.recursive),
                    types: opts
                        .event_types
                        .as_ref()
                        .map(|v| v.iter().map(|t| t.to_string()).collect()),
                    size: opts
                        .size_filter
                        .map(|f| format!("{}{}", f.op(), format_size(f.bytes()))),
                    cmd: opts.cmd,
                    max_depth: opts.max_depth,
                    symlink_target: None,
                },
            ));
        }
        self.setup_inotify_watches();
        if self.add_temp_parent_mark(path) {
            debug_log!(self.debug, "temp parent mark active for {}", path.display());
        }
        // Path may have been recreated before the inotify watch was established.
        self.check_pending();
    }

    /// Resolve "unknown" fields in pending events after proc events have been drained.
    /// Called by the event loop after the second drain.
    pub(crate) fn patch_pending_events(&self, pending: &mut [PendingEvent]) {
        for pe in pending {
            let ev = &mut pe.event;
            if ev.cmd == "unknown" || ev.user == "unknown" || ev.ppid == 0 || ev.tgid == 0 {
                // Try proc store (now populated by the second drain)
                if let Some(ref store) = self.proc.store
                    && let Some(info) = store.get_process(pe.pid)
                {
                    if ev.cmd == "unknown" {
                        ev.cmd = info.cmd().to_string();
                    }
                    if ev.user == "unknown" {
                        ev.user = info.user().to_string();
                    }
                    if ev.ppid == 0 {
                        ev.ppid = info.ppid();
                    }
                    if ev.tgid == 0 {
                        ev.tgid = info.tgid();
                    }
                }
            }
        }
    }

    /// Like `build_file_event` but uses a specific PathOptions for chain building.
    pub(crate) fn build_file_event_for_opts(
        &mut self,
        raw: &FidEvent,
        event_type: EventType,
        opts: &PathOptions,
    ) -> FileEvent {
        let pid = raw.pid().unsigned_abs();
        let info = get_process_info_by_pid(pid, raw.path(), self.proc.store.as_ref());

        let file_size = match event_type {
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(raw.path()).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.put(raw.path().to_path_buf(), size);
                size
            }
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.pop(raw.path()).unwrap_or(0)
            }
            _ => self.file_size_cache.get(raw.path()).map_or(0, |&s| s),
        };

        // Chain building based on the specific opts' cmd
        let chain = opts
            .cmd
            .as_ref()
            .and_then(|_| {
                self.proc
                    .store
                    .as_ref()
                    .map(|store| proc_tree::build_chain_string(store, pid))
            })
            .unwrap_or_default();

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path().to_path_buf(),
            pid,
            cmd: info.cmd().to_string(),
            user: info.user().to_string(),
            file_size,
            ppid: info.ppid(),
            tgid: info.tgid(),
            chain,
        }
    }

    /// Find the PathOptions matching a given event path.
    #[cfg(test)]
    pub(crate) fn get_matching_path_options(&self, path: &Path) -> Option<&PathOptions> {
        filters::get_matching_path_options(
            &self.paths,
            &self.monitored_entries,
            &self.canonical_paths,
            path,
        )
    }

    /// Return all PathOptions matching an event path (owned, no borrow conflict).
    /// Uses `monitored_entries` directly (not `path_options`), so (path, cmd) pairs
    /// are preserved even when the same path exists under multiple cmd groups.
    ///
    /// Also checks `pending_paths` so that events captured by temporary parent
    /// marks during the delete-recreate window are matched.
    pub(crate) fn matching_opts_for_event(&self, event_path: &Path) -> Vec<(PathBuf, PathOptions)> {
        let mut result = Vec::new();
        debug_log!(self.debug, "matching path={}", event_path.display());

        // Match monitored_entries
        Self::collect_matching_entries(
            event_path,
            &self.monitored_entries,
            &mut result,
            self.debug,
        );

        // Match pending_paths (convert PathEntry → PathOptions)
        for (pending_path, entry) in &self.inotify_state.pending_paths {
            if !Self::path_matches(event_path, pending_path, entry.recursive.unwrap_or(false)) {
                continue;
            }
            let opts = match PathOptions::try_from(entry) {
                Ok(o) => o,
                Err(_) => continue,
            };
            debug_log!(
                self.debug,
                "  check {}/pending (cmd={}, recursive={}): MATCH",
                pending_path.display(),
                opts.cmd.as_deref().unwrap_or("global"),
                opts.recursive
            );
            result.push((pending_path.clone(), opts));
        }
        if result.is_empty() {
            debug_log!(self.debug, "  -> no matching entries");
        }
        result
    }

    /// Check if an event path matches a monitored path (recursive or direct).
    fn path_matches(event_path: &Path, entry_path: &Path, recursive: bool) -> bool {
        if recursive {
            event_path.starts_with(entry_path)
        } else {
            event_path == entry_path || event_path.parent() == Some(entry_path)
        }
    }

    /// Collect matching (path, opts) from a slice into result, with debug logging.
    fn collect_matching_entries(
        event_path: &Path,
        entries: &[(PathBuf, PathOptions)],
        result: &mut Vec<(PathBuf, PathOptions)>,
        debug: bool,
    ) {
        for (monitored_path, opts) in entries {
            let matches = Self::path_matches(event_path, monitored_path, opts.recursive);
            debug_log!(
                debug,
                "  check {} (cmd={}, recursive={}): {}",
                monitored_path.display(),
                opts.cmd.as_deref().unwrap_or("global"),
                opts.recursive,
                if matches { "MATCH" } else { "no" }
            );
            if matches {
                result.push((monitored_path.clone(), opts.clone()));
            }
        }
    }
}
