use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fanotify_fid::consts::FAN_Q_OVERFLOW;
use fanotify_fid::types::FidEvent;

use crate::fid_parser::mask_to_event_types;
#[cfg(test)]
use crate::filters;
use crate::filters::PathOptions;
use crate::monitored::PathEntry;
use crate::proc_cache::build_chain;
use crate::utils::{format_size, get_process_info_by_pid};
use crate::{EventType, FileEvent};

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
    pub(crate) fn process_event_batch(
        &mut self,
        events: &[FidEvent],
    ) -> Vec<PendingEvent> {
        let mut pending: Vec<PendingEvent> = Vec::new();

        for raw in events {
            if raw.mask & FAN_Q_OVERFLOW != 0 {
                eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                continue;
            }

            let event_types = mask_to_event_types(raw.mask);
            let matched_path = self.matching_path(&raw.path).cloned();

            // Detect canonical root DELETE_SELF — needs cleanup after recording.
            let is_delete_self = event_types.contains(&EventType::DeleteSelf)
                || event_types.contains(&EventType::MovedFrom);
            let is_canonical_root = is_delete_self
                && self.canonical_paths.iter().any(|cp| cp == &raw.path);

            let event_pid = raw.pid.unsigned_abs();

            // Exclude fsmon daemon's own events to prevent self-triggering.
            if event_pid == self.daemon_pid {
                if self.debug {
                    eprintln!("[DEBUG] skip daemon self-event (pid={})", event_pid);
                }
                continue;
            }

            // Match event against ALL cmd groups for this path.
            // Computed BEFORE canonical-root cleanup — DELETE_SELF must be
            // recorded before the path is removed from monitored_entries.
            let matching_entries = self.matching_opts_for_event(&raw.path);
            if self.debug && matching_entries.is_empty() {
                eprintln!("[DEBUG] event on {} (pid={}): no matching entries",
                    raw.path.display(), event_pid);
            }
            for (_monitored_path, opts) in &matching_entries {
                // Check process tree filter
                let cmd_match = if let Some(ref cmd_name) = opts.cmd {
                    let matched = self.pid_tree.as_ref()
                        .map(|tree| crate::proc_cache::is_descendant(tree, event_pid, cmd_name))
                        .unwrap_or(false);
                    if self.debug {
                        eprintln!("[DEBUG]   check cmd=\"{}\" pid={}: {}",
                            cmd_name, event_pid, if matched { "MATCH" } else { "SKIP" });
                    }
                    matched
                } else {
                    if self.debug {
                        eprintln!("[DEBUG]   check cmd=global pid={}: MATCH", event_pid);
                    }
                    true
                };
                if !cmd_match {
                    continue;
                }

                for event_type in &event_types {
                    let event = self.build_file_event_for_opts(raw, *event_type, opts);

                    if !self.is_path_in_scope_for_opts(&event.path, opts) {
                        if self.debug {
                            eprintln!("[DEBUG]   -> out of scope for this opts");
                        }
                        continue;
                    }

                    if self.should_output_for_opts(&event, opts) {
                        if self.debug {
                            let cmd = opts.cmd.as_deref().unwrap_or("global");
                            eprintln!("[DEBUG]   -> {}_log.jsonl", cmd);
                        }
                        let cmd_name = opts.cmd.as_deref()
                            .unwrap_or(crate::monitored::CMD_GLOBAL)
                            .to_string();
                        self.metrics.inc_event(&event_type.to_string(), &cmd_name);
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
            if is_canonical_root {
                if self.debug {
                    eprintln!("[DEBUG] monitored directory deleted: {}", raw.path.display());
                }
                if let Some(ref path) = matched_path {
                    // Preserve ALL cmd groups before removing
                    let all_opts: Vec<PathOptions> = self.opts_for_path(path).into_iter().cloned().collect();
                    if let Err(e) = self.remove_path(path, None) {
                        eprintln!("[WARNING] Failed to remove deleted path '{}': {e}", path.display());
                    }
                    for opts in all_opts {
                        self.pending_paths.push((
                            path.clone(),
                            PathEntry {
                                path: path.clone(),
                                recursive: Some(opts.recursive),
                                types: opts.event_types.as_ref().map(
                                    |v| v.iter().map(|t| t.to_string()).collect()
                                ),
                                size: opts.size_filter.map(|f| format!("{}{}", f.op, format_size(f.bytes))),
                                cmd: opts.cmd,
                            },
                        ));
                    }
                    self.setup_inotify_watches();
                    // Path may have been recreated before the inotify watch was
                    // established. Check immediately to avoid missing the window.
                    self.check_pending();
                }
            }
        }

        pending
    }

    /// Resolve "unknown" fields in pending events after proc events have been drained.
    /// Called by the event loop after the second drain.
    pub(crate) fn patch_pending_events(&self, pending: &mut [PendingEvent]) {
        for pe in pending {
            let ev = &mut pe.event;
            if ev.cmd == "unknown" || ev.user == "unknown" || ev.ppid == 0 || ev.tgid == 0 {
                // Try proc_cache (now populated by the second drain)
                if let Some(ref cache) = self.proc_cache {
                    if let Some(info) = cache.get(&pe.pid) {
                        if ev.cmd == "unknown" {
                            ev.cmd = info.cmd.clone();
                        }
                        if ev.user == "unknown" {
                            ev.user = info.user.clone();
                        }
                        if ev.ppid == 0 {
                            ev.ppid = info.ppid;
                        }
                        if ev.tgid == 0 {
                            ev.tgid = info.tgid;
                        }
                    }
                }
                // Also try PidTree for cmd/ppid
                if let Some(ref tree) = self.pid_tree {
                    if let Some(node) = tree.get(&pe.pid) {
                        if ev.cmd == "unknown" && !node.cmd.is_empty() {
                            ev.cmd = node.cmd.clone();
                        }
                        if ev.ppid == 0 {
                            ev.ppid = node.ppid;
                        }
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
        let pid = raw.pid.unsigned_abs();
        let info = get_process_info_by_pid(pid, &raw.path, self.proc_cache.as_ref());

        let file_size = match event_type {
            EventType::Create | EventType::Modify | EventType::CloseWrite => {
                let size = fs::metadata(&raw.path).map(|m| m.len()).unwrap_or(0);
                self.file_size_cache.put(raw.path.clone(), size);
                size
            }
            EventType::Delete | EventType::DeleteSelf | EventType::MovedFrom => {
                self.file_size_cache.pop(&raw.path).unwrap_or(0)
            }
            _ => self.file_size_cache.get(&raw.path).map_or(0, |&s| s),
        };

        // Chain building based on the specific opts' cmd
        let chain = opts
            .cmd
            .as_ref()
            .and_then(|_| {
                self.pid_tree.as_ref().and_then(|tree| {
                    self.proc_cache
                        .as_ref()
                        .map(|cache| build_chain(tree, cache, pid))
                })
            })
            .unwrap_or_default();

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path.clone(),
            pid,
            cmd: info.cmd,
            user: info.user,
            file_size,
            ppid: info.ppid,
            tgid: info.tgid,
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
    pub(crate) fn matching_opts_for_event(&self, event_path: &Path) -> Vec<(PathBuf, PathOptions)> {
        let mut result = Vec::new();
        if self.debug {
            eprintln!("[DEBUG] matching path={}", event_path.display());
        }
        for (monitored_path, opts) in &self.monitored_entries {
            let matches = if opts.recursive {
                event_path.starts_with(monitored_path)
            } else {
                event_path == monitored_path.as_path()
                    || event_path.parent() == Some(monitored_path.as_path())
            };
            if self.debug {
                let label = opts.cmd.as_deref().unwrap_or("global");
                eprintln!(
                    "[DEBUG]   check {} (cmd={}, recursive={}): {}",
                    monitored_path.display(),
                    label,
                    opts.recursive,
                    if matches { "MATCH" } else { "no" }
                );
            }
            if matches {
                result.push((monitored_path.clone(), opts.clone()));
            }
        }
        if self.debug && result.is_empty() {
            eprintln!("[DEBUG]   -> no matching entries");
        }
        result
    }
}
