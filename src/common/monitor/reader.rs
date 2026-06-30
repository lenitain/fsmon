use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;

use crate::{debug_log, info_log, warning_log, error_log};
use tokio::io::unix::AsyncFd;

use crate::common::fid_parser::read_fid_events_cached;

use super::Monitor;
use super::channel::EventSender;

// ---- Reader supervision ----

/// Per-reader-task restart tracking for exponential backoff.
/// Restarts are capped at MAX_RESTARTS within BACKOFF_WINDOW.
pub(crate) struct ReaderState {
    pub(crate) restart_count: u32,
    pub(crate) last_restart: std::time::Instant,
    /// Set when restart_reader gives up (backoff exhausted within window).
    /// Reset when spawn_fd_reader attempts a new spawn (even if it later fails).
    /// Used by health() for reliable alive/dead reporting.
    pub(crate) gave_up: bool,
}

impl std::fmt::Debug for ReaderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderState")
            .field("restart_count", &self.restart_count)
            .field("gave_up", &self.gave_up)
            .finish()
    }
}

pub(crate) const MAX_RESTARTS: u32 = 3;
pub(crate) const BACKOFF_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

impl Monitor {
    /// Duplicate a file descriptor, returning an owned fd.
    /// The returned `OwnedFd` has independent lifetime from the source
    /// and will be closed on drop.
    pub(crate) fn dup_fd(fd: &impl AsFd) -> std::io::Result<OwnedFd> {
        nix::unistd::dup(fd).map_err(std::io::Error::other)
    }

    /// Open a directory and return an owned fd.
    /// The returned `OwnedFd` has the directory open and will be
    /// closed on drop.
    pub(crate) fn open_dir(path: &Path) -> std::io::Result<OwnedFd> {
        // Use O_PATH for minimal permissions (F-016)
        // O_DIRECTORY ensures we open a directory
        // O_CLOEXEC prevents fd leaks to child processes
        nix::fcntl::open(
            path,
            nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_PATH
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(std::io::Error::other)
    }

    /// Spawn a tokio reader task for `group_key` in `fs_groups`.
    /// Both the fanotify fd and mount fd are duplicated so the reader task
    /// owns independent copies, avoiding double-close with Monitor's OwnedFd.
    pub(crate) fn spawn_fd_reader(&mut self, group_key: super::FsGroupKey) {
        let tx = match self.event_tx.as_ref() {
            Some(t) => t.clone(),
            None => {
                error_log!("Cannot spawn reader: event_tx not initialized");
                return;
            }
        };
        let dc = match &self.fanotify.shared_dir_cache {
            Some(d) => d.clone(),
            None => {
                error_log!("Cannot spawn reader: shared_dir_cache not initialized");
                return;
            }
        };
        let death_tx = self.reader_death_tx.clone();
        let buf_size = self.buffer_size;
        let debug = self.debug;
        let group = &self.fanotify.groups[group_key];

        // Duplicate fds so the reader task owns independent copies
        let owned_fan_fd = match Self::dup_fd(&group.fan_fd) {
            Ok(fd) => fd,
            Err(e) => {
                error_log!("Failed to dup fanotify fd {}: {}", group.fan_fd.as_raw_fd(), e);
                return;
            }
        };
        let owned_mount_fd = match Self::dup_fd(&group.mount_fd) {
            Ok(fd) => fd,
            Err(e) => {
                error_log!("Failed to dup mount fd {}: {}", group.mount_fd.as_raw_fd(), e);
                // owned_fan_fd drops here, closing the dup'd fan fd
                return;
            }
        };
        let raw_fd = owned_fan_fd.as_raw_fd();
        let mfds = Arc::new(vec![owned_mount_fd]);

        if debug {
            debug_log!(debug, "spawning reader for group {:?} (fd {})", group_key, raw_fd);
        }

        tokio::spawn(async move {
            if debug {
                debug_log!(debug, "reader task spawned for group {:?} (fd {})", group_key, raw_fd);
            }
            let afd = match AsyncFd::new(owned_fan_fd) {
                Ok(a) => {
                    if debug {
                        debug_log!(debug, "reader {} AsyncFd created, entering loop", raw_fd);
                    }
                    a
                }
                Err(e) => {
                    error_log!("AsyncFd for fd {}: {}", raw_fd, e);
                    let _ = death_tx.send(group_key);
                    return;
                }
            };
            let mut buf = vec![0u8; buf_size];
            loop {
                let result = afd.readable().await;
                let mut guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        error_log!("fd {} readable: {}", raw_fd, e);
                        break;
                    }
                };
                let events = read_fid_events_cached(afd.get_ref(), &mfds, &dc, &mut buf);
                if debug {
                    debug_log!(debug, "fd {} reader: got {} event(s)", raw_fd, events.len());
                }
                if !events.is_empty() {
                    let send_err = match &tx {
                        EventSender::Unbounded(tx) => tx.send(events).is_err(),
                        EventSender::Bounded(tx) => tx.send(events).await.is_err(),
                    };
                    if send_err {
                        break;
                    }
                    // Edge-triggered epoll: retain readiness so the next
                    // readable().await resolves immediately if more events
                    // are still queued (e.g. DELETE → DELETE_SELF batch).
                    guard.retain_ready();
                    if debug {
                        debug_log!(debug, "fd {} reader: retain_ready, looping", raw_fd);
                    }
                } else {
                    if debug {
                        debug_log!(debug, "fd {} reader: empty read, clear_ready", raw_fd);
                    }
                    guard.clear_ready();
                }
            }
            if debug {
                debug_log!(debug, "Reader task for group {:?} (fd {}) exited", group_key, raw_fd);
            }
            let _ = death_tx.send(group_key);
        });

        // Track reader state for restart backoff
        if let Some(state) = self.reader_states.get_mut(&group_key) {
            state.restart_count += 1;
            state.last_restart = std::time::Instant::now();
            state.gave_up = false;
        } else {
            self.reader_states.insert(
                group_key,
                ReaderState {
                    restart_count: 1,
                    last_restart: std::time::Instant::now(),
                    gave_up: false,
                },
            );
        }
        self.metrics
            .set_reader_groups(self.fanotify.groups.len() as i64);
    }

    /// Restart a reader task that has died.
    ///
    /// Applies exponential backoff: up to MAX_RESTARTS within BACKOFF_WINDOW.
    /// If the backoff limit is exceeded, logs a warning and gives up.
    /// On success, the dead task's fds are re-duplicated from FsGroup and
    /// a new reader is spawned.
    pub(crate) fn restart_reader(&mut self, group_key: super::FsGroupKey) {
        // Check backoff limits
        let now = std::time::Instant::now();
        let state = self.reader_states.get(&group_key);
        if let Some(s) = state {
            let in_window = now.duration_since(s.last_restart) < BACKOFF_WINDOW;
            if in_window && s.restart_count >= MAX_RESTARTS {
                error_log!(
                    "Reader task for group {:?} has crashed {} times in the last {}s — giving up. fsmon daemon restart required.",
                    group_key, MAX_RESTARTS, BACKOFF_WINDOW.as_secs(),
                );
                // Mark gave_up so health() reports accurate alive/dead status.
                // This will be reset when spawn_fd_reader is called again.
                if let Some(s) = self.reader_states.get_mut(&group_key) {
                    s.gave_up = true;
                }
                return;
            }
        }

        // Verify the FsGroup still exists (may have been removed during shutdown)
        if !self.fanotify.groups.contains_key(group_key) {
            warning_log!("Cannot restart reader for group {:?}: group no longer exists", group_key);
            return;
        }

        let dev_id = self.fanotify.groups[group_key].dev_id;
        info_log!("Restarting reader task for group {:?} (dev_id={})...", group_key, dev_id);
        self.spawn_fd_reader(group_key);
    }
}
