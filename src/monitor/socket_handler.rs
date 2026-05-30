use anyhow::Context;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use crate::metrics::MetricsRegistry;
use crate::monitored::{Monitored, PathEntry};
use crate::socket::{SocketCmd, SocketResp};
use crate::utils::format_size;
use crate::{EventType, FileEvent};

use super::Monitor;

impl Monitor {
    /// Build a health snapshot for the `health` socket command.
    pub(crate) fn health(&self) -> SocketResp {
        use crate::socket::{HealthInfo, ReaderHealth};

        let readers: Vec<ReaderHealth> = self
            .fs_groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let state = self.reader_states.get(i).and_then(|s| s.as_ref());
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

        SocketResp::health(HealthInfo {
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
        let tx = match self.event_stream_tx.as_ref() {
            Some(tx) => tx,
            None => {
                tokio::spawn(write_resp_and_close(
                    writer,
                    SocketResp::permanent_err("subscriptions disabled"),
                ));
                return;
            }
        };

        let rx = tx.subscribe();
        let track_cmd = cmd.track_cmd.clone();
        let types: Option<Vec<EventType>> = cmd.types.as_ref().map(|v| {
            v.iter()
                .filter_map(|t| t.parse::<EventType>().ok())
                .collect()
        });

        // Subscriber can override local_time per-connection.
        // If not specified, use daemon default.
        let sub_local = cmd.local_time.unwrap_or(self.local_time);
        let sub_metrics = self.metrics.clone();
        self.metrics.inc_subscribers();
        tokio::spawn(subscriber_task(writer, rx, track_cmd, types, sub_local, sub_metrics));
    }

    pub(crate) fn handle_socket_cmd(&mut self, cmd: SocketCmd) -> SocketResp {
        if self.debug {
            eprintln!(
                "[DEBUG] socket command: {} path={:?} track_cmd={:?}",
                cmd.cmd, cmd.path, cmd.track_cmd
            );
        }
        match cmd.cmd.as_str() {
            "add" => {
                let raw = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                let path = raw;
                let track_cmd = cmd.track_cmd.as_deref().and_then(|c| {
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
                    recursive: cmd.recursive,
                    types: cmd.types.clone(),
                    size: cmd.size.clone(),
                    cmd: cmd.track_cmd.clone(),
                };
                match self.add_path(&entry) {
                    Ok(()) => SocketResp::ok(),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    }
                }
            }
            "remove" => {
                let path = match &cmd.path {
                    Some(p) => p.clone(),
                    None => {
                        return SocketResp::err("Missing 'path' field");
                    }
                };
                match self.remove_path(&path, cmd.track_cmd.as_deref()) {
                    Ok(()) => SocketResp::ok(),
                    Err(e) => {
                        // Classify: recursion/conflict errors are permanent (will fail after restart)
                        let msg = e.to_string();
                        if msg.contains("infinite recursion") || msg.contains("log directory") {
                            SocketResp::permanent_err(msg)
                        } else {
                            SocketResp::err(msg)
                        }
                    }
                }
            }
            "list" => {
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
                SocketResp {
                    ok: true,
                    error: None,
                    error_kind: None,
                    paths: Some(paths),
                    health: None,
                }
            }
            "health" => self.health(),
            _ => SocketResp::err(format!("Unknown command: {}", cmd.cmd)),
        }
    }

    pub(crate) fn reload_config(&mut self) -> anyhow::Result<()> {
        if self.debug {
            eprintln!("[DEBUG] reload_config");
        }
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

/// Write a TOML response and close the socket (one-shot command helper).
pub(crate) async fn write_resp_and_close(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    resp: SocketResp,
) {
    use tokio::io::AsyncWriteExt;
    if let Ok(toml_str) = toml::to_string(&resp) {
        let _ = writer.write_all(format!("{toml_str}\n").as_bytes()).await;
    }
}

/// Write bytes and close (for non-subscribe socket commands).
pub(crate) async fn tokio_io_oneshot(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    data: &str,
) {
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

    // 1. Send initial ok response (TOML)
    let resp = SocketResp::ok();
    let resp_str = toml::to_string(&resp).unwrap_or_default();
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
                    && !allowed.contains(&event.event_type) {
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
