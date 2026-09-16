use std::fs;
use std::path::{Path, PathBuf};

use crate::debug_log;
use chrono::Utc;
use fanotify_fid::consts::{FAN_MOVED_FROM, FAN_MOVED_TO, FAN_Q_OVERFLOW, FAN_RENAME};
use fanotify_fid::types::FidEvent;

use crate::common::fid_parser::mask_to_event_types;
#[cfg(test)]
use crate::common::filters;
use crate::common::filters::PathOptions;
use crate::common::monitored::PathEntry;
use crate::common::utils::{format_size, get_proc_info};
use crate::common::{EventType, FileEvent};

use super::Monitor;

/// Pending event ready for broadcast, held back to allow late proc event drain.
pub(crate) struct PendingEvent {
    pub event: FileEvent,
    pub cmd_name: String,
    pub pid: u32,
}

impl Monitor {
    /// Return `events` with each `FAN_RENAME` split into its two sides.
    ///
    /// The kernel reports a rename as one event whose payload lives in two info
    /// records: the old parent directory handle + old name, and the new parent
    /// handle + new name.  Without this step the event has no path at all, so
    /// path matching finds nothing and the rename is dropped.
    ///
    /// Each side is resolved through the directory cache; a side whose parent
    /// handle is unknown is skipped, and the event is reported as unresolved so
    /// the caller can tell "no rename" apart from "rename we could not place".
    /// The cache is normally complete because the marking walk seeds a handle
    /// for every directory it descends into.
    ///
    /// Events that are not renames are moved across unchanged.
    fn expand_renames(&self, events: &[FidEvent]) -> Vec<FidEvent> {
        let mut out: Vec<FidEvent> = Vec::with_capacity(events.len());
        let mut unresolved = 0usize;

        for ev in events {
            if ev.mask() & FAN_RENAME == 0 {
                out.push(ev.clone());
                continue;
            }

            let mut sides = 0usize;
            for (side, bit) in [
                (ev.rename_source(), FAN_MOVED_FROM),
                (ev.rename_target(), FAN_MOVED_TO),
            ] {
                let Some(side) = side else { continue };
                let Some(dir) = self.fanotify.dir_cache.get(&side.handle) else {
                    continue;
                };
                let path = if side.name.is_empty() {
                    dir
                } else {
                    dir.join(&side.name)
                };

                let mut split = FidEvent::new(bit, ev.pid(), path, None, None, None);
                split.set_dfid_name(side.handle.clone(), side.name.clone());
                out.push(split);
                sides += 1;
            }

            if sides == 0 {
                unresolved += 1;
            }
        }

        if unresolved > 0 {
            debug_log!(
                self.debug,
                "{} rename(s) dropped: neither parent directory handle was in the cache",
                unresolved
            );
            self.metrics.inc_events_unresolved_rename(unresolved as u64);
        }

        out
    }

    /// Process a batch of fanotify events: match paths, filter, build FileEvents.
    /// Events are NOT sent to broadcast here — they are returned as PendingEvents
    /// so the caller can drain proc events and resolve "unknown" fields before
    /// publishing. Metrics are still incremented immediately.
    pub(crate) fn process_event_batch(&mut self, events: &[FidEvent]) -> Vec<PendingEvent> {
        let mut pending: Vec<PendingEvent> = Vec::new();

        // A `FAN_RENAME` carries both locations in one event.  Split it before
        // the per-event logic below, which is single-path by construction: the
        // old side becomes MOVED_FROM and the new side MOVED_TO, exactly as if
        // the kernel had sent the two halves.  That keeps every downstream rule
        // — new-subdirectory marking, canonical-root cleanup, path matching —
        // working unchanged.
        let expanded = self.expand_renames(events);

        for raw in &expanded {
            if raw.mask() & FAN_Q_OVERFLOW != 0 {
                eprintln!("[WARNING] fanotify queue overflow - some events may have been lost");
                continue;
            }

            // The parser preserves info records it has no typed field for
            // (RANGE, MNT, future kernel additions) instead of dropping them.
            // fsmon reads none of those, so count them — otherwise the
            // preservation stays invisible from the outside.
            let unparsed = raw.unknown_info_records();
            if !unparsed.is_empty() {
                let types: Vec<String> = unparsed.iter().map(|(t, _)| t.to_string()).collect();
                debug_log!(
                    self.debug,
                    "event on {} carries {} unparsed info record(s): types [{}]",
                    raw.path().display(),
                    unparsed.len(),
                    types.join(", ")
                );
                self.metrics
                    .inc_unparsed_info_records(unparsed.len() as u64);
            }

            let event_types = mask_to_event_types(raw.mask());

            // Detect a canonical root that is gone (deleted, or renamed away) —
            // it needs cleanup after recording.
            //
            // This cannot go through `matching_path`: that maps an event path to
            // the *watched* path enclosing it, and a root renamed out of the
            // watched tree has no watched path that is its prefix.  So it must
            // be decided from the event's own path, which is exactly the path
            // the cleanup removes.
            let is_delete_self = event_types.contains(&EventType::DeleteSelf)
                || event_types.contains(&EventType::MovedFrom)
                || event_types.contains(&EventType::Delete);
            // The *canonical* path, not the event's path: the entry to remove
            // from `monitored_entries` is the root as configured, while the
            // event may name the root at a new location.
            let gone_root: Option<PathBuf> = if is_delete_self {
                self.canonical_paths
                    .iter()
                    .find(|cp| Self::is_canonical_root_path(cp, raw.path()))
                    .cloned()
            } else {
                None
            };

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

            // After recording: remove the gone canonical root from active
            // monitoring and move it to pending_paths so it can be re-monitored
            // if it is recreated.
            //
            // The event's own path is the one to remove — for `DELETE_SELF` it is
            // the root itself, and for a root renamed away it is the root's name
            // at its new location.  `matched_path` cannot be used here: it is the
            // *watched* path enclosing the event, which for a root moved out of
            // the tree does not exist at all, and removing it would delete the
            // wrong entry.
            if let Some(ref root) = gone_root {
                self.handle_canonical_root_deleted(root);
            }
        }

        pending
    }

    /// Does `event_path` denote the canonical root `canonical`?
    ///
    /// Two shapes have to be recognised, and they are the only two that mean
    /// "the root object itself is gone":
    ///
    /// * the event path **is** the root — `DELETE_SELF` on the watched directory;
    /// * the event path is the root's name reached from **elsewhere** — a rename
    ///   whose source is the root but whose parent is some other directory.  That
    ///   parent need not be watched at all, which is why this cannot be answered
    ///   from the watched paths.
    ///
    /// Matching is on the final component, because that is what survives a move.
    /// A same-named path elsewhere (e.g. `/other/watched` for a root `/watched`)
    /// therefore counts as a match; for a destructive event whose target is
    /// already gone that is a safe over-approximation, and the alternative —
    /// never cleaning up a moved-away root — is strictly worse.
    pub(crate) fn is_canonical_root_path(canonical: &Path, event_path: &Path) -> bool {
        canonical == event_path
            || (canonical.file_name().is_some() && canonical.file_name() == event_path.file_name())
    }

    /// Check if an event's PID matches the process tree filter for a cmd group.
    /// Returns true if no filter is set or if the PID is a descendant of the target cmd.
    fn matches_process_tree(&self, cmd: Option<&str>, event_pid: u32) -> bool {
        match cmd {
            Some(cmd_name) => {
                let matched = self.proc.tracker.as_ref().is_some_and(|t| {
                    let view = t.view();
                    let Some(child) = view.current(proc_tree::Tgid(event_pid)) else {
                        return false;
                    };
                    // comm match + descendant check (RUN-23): comm comes from
                    // cn_proc Comm events / bootstrap enrichment; processes
                    // without comm data simply never match.
                    view.live_keys().any(|k| {
                        view.metadata(k).and_then(|m| m.comm.as_deref()) == Some(cmd_name)
                            && view.descendant_of(child, k)
                    })
                });
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
            if ev.comm.is_empty()
                || ev.cmd.is_empty()
                || ev.user.is_empty()
                || ev.ppid == 0
                || ev.tgid == 0
            {
                // Generation-safe topology from the event-driven tracker
                // (after the second drain). comm comes from cn_proc Comm
                // events; cmd needs /proc (may be gone for short-lived
                // processes); ppid/tgid are topology.
                if let Some(t) = self.proc.tracker.as_ref() {
                    let view = t.view();
                    if let Some(key) = view.current(proc_tree::Tgid(pe.pid)) {
                        let node = view.get(key);
                        if ev.comm.is_empty()
                            && let Some(comm) = view.metadata(key).and_then(|m| m.comm.clone())
                        {
                            ev.comm = comm;
                        }
                        if ev.cmd.is_empty() {
                            ev.cmd = proc_tree::read_cmdline(std::path::Path::new("/proc"), pe.pid)
                                .unwrap_or_default();
                        }
                        if ev.ppid == 0 {
                            ev.ppid = node
                                .and_then(|n| n.current_parent())
                                .map(|p| p.tgid.0)
                                .unwrap_or(0);
                        }
                        if ev.tgid == 0 {
                            ev.tgid = key.tgid.0;
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
        let pid = raw.pid().unsigned_abs();
        let info = get_proc_info(self.proc.tracker.as_ref(), pid, raw.path());

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

        // Chain building based on the specific opts' cmd (RUN-23): walk the
        // current-parent ancestry from the generation-safe tracker; cmdline
        // is read on demand.
        let chain = match (opts.cmd.as_ref(), self.proc.tracker.as_ref()) {
            (Some(_), Some(t)) => crate::common::utils::build_chain(t, pid),
            _ => Vec::new(),
        };

        // FS_ERROR carries the filesystem's own error code; without it the
        // record only says "some error happened".  Absent for other types.
        let error = if event_type == EventType::FsError {
            raw.fs_error()
        } else {
            None
        };

        FileEvent {
            time: Utc::now(),
            event_type,
            path: raw.path().to_path_buf(),
            pid,
            comm: info.comm,
            cmd: info.cmd,
            user: info.user,
            file_size,
            ppid: info.ppid,
            tgid: info.tgid,
            chain,
            fs_error: error,
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
