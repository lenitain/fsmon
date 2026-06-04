use anyhow::Context;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use crate::metrics::MetricsRegistry;
use crate::monitored::{Monitored, PathEntry};
use crate::socket::{SocketCmd, SocketError, SocketResponse};
use crate::utils::format_size;
use crate::{EventType, FileEvent};
use serde_json;

use super::Monitor;

impl Monitor {
    /// Build a health snapshot for the `health` socket command.
    pub(crate) fn health(&self) -> SocketResponse {
        use crate::socket::{HealthInfo, ReaderHealth};

        let readers: Vec<ReaderHealth> = self
            .fs_groups
            .iter()
            .map(|(key, g)| {
                let state = self.reader_states.get(&key);
                let alive = state.is_some_and(|s| {
                    // Only dead when restart_reader explicitly gave up.
                    // gave_up is reset when spawn_fd_reader attempts recovery.
                    !s.gave_up
                });
                let restarts = state.map(|s| s.restart_count).unwrap_or(0);
                ReaderHealth {
                    alive,
                    restarts,
                    fd: g.fan_fd.as_raw_fd(),
                }
            })
            .collect();

        let channel_type = match self.cache_config.channel_capacity {
            Some(cap) => format!("bounded({})", cap),
            None => "unbounded".to_string(),
        };

        SocketResponse::Health(HealthInfo {
            uptime_secs: self.started_at.elapsed().as_secs(),
            channel_type,
            monitored_paths: self.monitored_entries.len(),
            reader_groups: self.fs_groups.len(),
            readers,
        })
    }

    /// Handle a subscribe command: spawn a task that streams events to the socket.
    pub(crate) fn handle_subscribe(
        &self,
        writer: tokio::net::unix::OwnedWriteHalf,
        cmd: &SocketCmd,
    ) {
        let (track_cmd, types, local_time) = match cmd {
            SocketCmd::Subscribe {
                types,
                track_cmd,
                local_time,
            } => (track_cmd.clone(), types.clone(), *local_time),
            _ => {
                // This should never happen, but handle gracefully
                tokio::spawn(write_resp_and_close(
                    writer,
                    Err(SocketError::Permanent(
                        "Expected Subscribe command".to_string(),
                    )),
                ));
                return;
            }
        };

        let tx = match self.event_stream_tx.as_ref() {
            Some(tx) => tx,
            None => {
                tokio::spawn(write_resp_and_close(
                    writer,
                    Err(SocketError::Permanent("subscriptions disabled".to_string())),
                ));
                return;
            }
        };

        let rx = tx.subscribe();
        let types: Option<Vec<EventType>> = types.as_ref().map(|v| {
            v.iter()
                .filter_map(|t| t.parse::<EventType>().ok())
                .collect()
        });

        // Subscriber can override local_time per-connection.
        // If not specified, use daemon default.
        let sub_local = local_time.unwrap_or(self.local_time);
        let sub_metrics = self.metrics.clone();
        self.metrics.inc_subscribers();
        tokio::spawn(subscriber_task(
            writer,
            rx,
            track_cmd,
            types,
            sub_local,
            sub_metrics,
        ));
    }

    pub(crate) fn handle_socket_cmd(
        &mut self,
        cmd: SocketCmd,
    ) -> Result<SocketResponse, SocketError> {
        debug_log!(self.debug, "socket command: {:?}", cmd);
        match cmd {
            SocketCmd::Add {
                path,
                recursive,
                types,
                size,
                track_cmd,
            } => {
                let track_cmd = track_cmd.as_deref().and_then(|c| {
                    if c == crate::monitored::CMD_GLOBAL {
                        None
                    } else {
                        Some(c.to_string())
                    }
                });
                // Remove only this (path, cmd) pair, not other cmd groups for same path
                self.monitored_entries
                    .retain(|(p, o)| !(p == &path && o.cmd == track_cmd));
                let has_other_cmds = self.monitored_entries.iter().any(|(p, _)| p == &path);
                if !has_other_cmds {
                    // No other cmd groups for this path — full teardown + setup
                    let _ = self.remove_path(&path, None);
                }
                // Rebuild fanotify mask: last seen mask stays via path_options
                let entry = PathEntry {
                    path,
                    recursive,
                    types: types.clone(),
                    size: size.clone(),
                    cmd: track_cmd.clone(),
                };
                match self.add_path(&entry) {
                    Ok(()) => Ok(SocketResponse::Ok),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            Err(SocketError::Permanent(msg))
                        } else {
                            Err(SocketError::Transient(msg))
                        }
                    }
                }
            }
            SocketCmd::Remove { path, track_cmd } => {
                match self.remove_path(&path, track_cmd.as_deref()) {
                    Ok(()) => Ok(SocketResponse::Ok),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            Err(SocketError::Permanent(msg))
                        } else {
                            Err(SocketError::Transient(msg))
                        }
                    }
                }
            }
            SocketCmd::List => {
                let paths: Vec<PathEntry> = self
                    .monitored_entries
                    .iter()
                    .map(|(p, opts)| {
                        let cmd = opts
                            .cmd
                            .clone()
                            .or(Some(crate::monitored::CMD_GLOBAL.to_string()));
                        PathEntry {
                            path: p.clone(),
                            recursive: Some(opts.recursive),
                            types: opts
                                .event_types
                                .as_ref()
                                .map(|v| v.iter().map(|t| t.to_string()).collect()),
                            size: opts
                                .size_filter
                                .map(|f| format!("{}{}", f.op, format_size(f.bytes))),
                            cmd,
                        }
                    })
                    .collect();
                Ok(SocketResponse::Paths(paths))
            }
            SocketCmd::Health => Ok(self.health()),
            _ => Err(SocketError::Transient(format!(
                "Unknown command: {:?}",
                cmd
            ))),
        }
    }

    pub(crate) fn reload_config(&mut self) -> anyhow::Result<()> {
        debug_log!(self.debug, "reload_config");
        let monitored_path = self
            .monitored_path
            .as_ref()
            .context("No store path configured")?;
        let store = Monitored::load(monitored_path)?;
        // Add new paths that appear in store
        let flat_entries = store.flatten();
        for entry in &flat_entries {
            if !self.paths.contains(&entry.path)
                && let Err(e) = self.add_path(entry)
            {
                eprintln!("Failed to add path {} on reload: {e}", entry.path.display());
            }
        }
        // Remove paths no longer in store
        let current_paths: Vec<PathBuf> = self.paths.clone();
        for path in &current_paths {
            if !flat_entries.iter().any(|p| p.path == *path)
                && let Err(e) = self.remove_path(path, None)
            {
                eprintln!("Failed to remove path {} on reload: {e}", path.display());
            }
        }

        Ok(())
    }
}

/// Write a JSON response and close the socket (one-shot command helper).
pub(crate) async fn write_resp_and_close(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    result: Result<SocketResponse, SocketError>,
) {
    use tokio::io::AsyncWriteExt;
    let json_str = match result {
        Ok(resp) => serde_json::to_string(&resp).unwrap_or_default(),
        Err(e) => serde_json::to_string(&e).unwrap_or_default(),
    };
    let _ = writer.write_all(format!("{json_str}\n").as_bytes()).await;
}

/// Write bytes and close (for non-subscribe socket commands).
pub(crate) async fn tokio_io_oneshot(mut writer: tokio::net::unix::OwnedWriteHalf, data: &str) {
    use tokio::io::AsyncWriteExt;
    let _ = writer.write_all(data.as_bytes()).await;
}

/// Check if a cmd group name appears in a chain string.
pub(crate) fn chains_contain(chain: &str, cmd_name: &str) -> bool {
    chain.split(" → ").any(|s| s.trim() == cmd_name)
}

/// Stream events from a broadcast receiver to a subscriber socket.
pub(crate) async fn subscriber_task(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut rx: tokio::sync::broadcast::Receiver<(FileEvent, String)>,
    track_cmd: Option<String>,
    type_filter: Option<Vec<EventType>>,
    local_time: bool,
    metrics: MetricsRegistry,
) {
    use tokio::io::AsyncWriteExt;

    // 1. Send initial ok response (JSON)
    let resp = SocketResponse::Ok;
    let resp_str = serde_json::to_string(&resp).unwrap_or_default();
    if writer
        .write_all(format!("{resp_str}\n").as_bytes())
        .await
        .is_err()
    {
        return;
    }

    // 2. Stream events
    loop {
        match rx.recv().await {
            Ok((event, _cmd_name)) => {
                // Optional filter by cmd group.
                // Global events have empty chains (no process tracking).
                if let Some(ref wanted) = track_cmd {
                    let keep = if wanted == crate::monitored::CMD_GLOBAL {
                        event.chain.is_empty()
                    } else {
                        !event.chain.is_empty() && chains_contain(&event.chain, wanted)
                    };
                    if !keep {
                        continue;
                    }
                }
                // Optional filter by event type
                if let Some(ref allowed) = type_filter
                    && !allowed.contains(&event.event_type)
                {
                    continue;
                }

                let line = if local_time {
                    event.to_jsonl_string_local() + "\n"
                } else {
                    event.to_jsonl_string() + "\n"
                };
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break; // subscriber disconnected
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // Subscriber too slow, dropped n events — send a warning JSON line
                let warn = format!(
                    r#"{{"warning":"subscriber too slow, dropped {} events","path":""}}"#,
                    n
                );
                let _ = writer.write_all(format!("{warn}\n").as_bytes()).await;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break; // daemon shutting down
            }
        }
    }
    metrics.dec_subscribers();
    // writer drops → connection closes
}
