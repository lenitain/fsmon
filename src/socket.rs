//! Unix socket protocol for daemon-CLI communication.
//! Uses JSON over unix stream sockets, with each message as a single line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json;

use crate::monitored::PathEntry;

/// Type-safe socket commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SocketCmd {
    /// Add a path to monitoring.
    Add {
        path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        recursive: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        types: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        track_cmd: Option<String>,
    },
    /// Remove a path from monitoring.
    Remove {
        path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        track_cmd: Option<String>,
    },
    /// List all monitored paths.
    List,
    /// Get daemon health information.
    Health,
    /// Subscribe to file system events.
    Subscribe {
        #[serde(skip_serializing_if = "Option::is_none")]
        types: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        track_cmd: Option<String>,
        /// Use local time instead of UTC in event timestamps.
        #[serde(skip_serializing_if = "Option::is_none")]
        local_time: Option<bool>,
    },
}

/// Type-safe socket responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SocketResponse {
    /// Command succeeded.
    Ok,
    /// List of monitored paths.
    Paths(Vec<PathEntry>),
    /// Health information.
    Health(HealthInfo),
}

/// Type-safe socket errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SocketError {
    /// Permanent error (will persist after daemon restart).
    Permanent(String),
    /// Transient error (runtime issue, will work after restart).
    Transient(String),
}

impl std::fmt::Display for SocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocketError::Permanent(msg) => write!(f, "Permanent error: {}", msg),
            SocketError::Transient(msg) => write!(f, "Transient error: {}", msg),
        }
    }
}

impl std::error::Error for SocketError {}

/// Health info for a single reader task (index-aligned with FsGroup).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderHealth {
    pub alive: bool,
    pub restarts: u32,
    pub fd: i32,
}

/// Snapshot of daemon health, returned by the `health` socket command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthInfo {
    pub uptime_secs: u64,
    pub channel_type: String,
    pub monitored_paths: usize,
    pub reader_groups: usize,
    /// Index-aligned with fs_groups, one entry per reader task.
    pub readers: Vec<ReaderHealth>,
}

/// Classifies whether an error is permanent (will persist after daemon restart)
/// or transient (runtime issue, will work after restart).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ErrorKind {
    /// The error will persist across daemon restarts (e.g., path conflicts, invalid config).
    Permanent,
    /// The error is a runtime issue (e.g., daemon not running, fanotify failure).
    /// This is the default when `error_kind` is absent.
    Transient,
}

/// Send a command to the running daemon and get the response.
///
/// # Protocol semantics
///
/// - Each command is processed independently (no atomicity between commands).
/// - Non-subscribe commands: send JSON → receive JSON response → connection closes.
/// - Subscribe command: send JSON → receive JSON OK → stream JSONL events → connection stays open.
/// - Each connection handles exactly one command.
pub fn send_cmd(socket_path: &Path, cmd: &SocketCmd) -> Result<SocketResponse, SocketError> {
    let stream = UnixStream::connect(socket_path).map_err(|e| {
        SocketError::Transient(format!(
            "Failed to connect to fsmon daemon at {}. Is the daemon running? \
             Start it with: sudo fsmon daemon: {}",
            socket_path.display(),
            e
        ))
    })?;

    let json = serde_json::to_string(cmd).map_err(|e| SocketError::Transient(e.to_string()))?;

    {
        let mut writer = stream
            .try_clone()
            .map_err(|e| SocketError::Transient(e.to_string()))?;
        // Write JSON command followed by newline as delimiter
        writeln!(writer, "{json}").map_err(|e| SocketError::Transient(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| SocketError::Transient(e.to_string()))?;
    }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    // Read until EOF — the server closes the connection after sending the response.
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|e| SocketError::Transient(e.to_string()))?;
        if bytes == 0 {
            break; // EOF
        }
        response.push_str(&line);
    }

    if response.trim().is_empty() {
        return Err(SocketError::Transient(
            "Empty response from daemon".to_string(),
        ));
    }

    let resp: SocketResponse = serde_json::from_str(response.trim())
        .map_err(|e| SocketError::Transient(format!("Failed to parse daemon response: {}", e)))?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener as TokioUnixListener, UnixStream as TokioUnixStream};

    // ── SocketCmd JSON serialization round-trip ──

    #[test]
    fn test_socket_cmd_subscribe_roundtrip() {
        let cmd = SocketCmd::Subscribe {
            types: None,
            track_cmd: None,
            local_time: None,
        };
        let json_str = serde_json::to_string(&cmd).unwrap();
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::Subscribe {
                types,
                track_cmd,
                local_time,
            } => {
                assert!(types.is_none());
                assert!(track_cmd.is_none());
                assert!(local_time.is_none());
            }
            _ => panic!("Expected Subscribe variant"),
        }
    }

    #[test]
    fn test_socket_cmd_subscribe_with_filters_roundtrip() {
        let cmd = SocketCmd::Subscribe {
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            track_cmd: Some("nginx".into()),
            local_time: Some(true),
        };
        let json_str = serde_json::to_string(&cmd).unwrap();
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::Subscribe {
                types,
                track_cmd,
                local_time,
            } => {
                assert_eq!(track_cmd, Some("nginx".into()));
                assert_eq!(types, Some(vec!["CREATE".into(), "DELETE".into()]));
                assert_eq!(local_time, Some(true));
            }
            _ => panic!("Expected Subscribe variant"),
        }
    }

    #[test]
    fn test_socket_cmd_add_roundtrip() {
        let cmd = SocketCmd::Add {
            path: PathBuf::from("/tmp/test"),
            recursive: Some(true),
            types: Some(vec!["MODIFY".into()]),
            size: Some(">=1MB".into()),
            track_cmd: Some("openclaw".into()),
        };
        let json_str = serde_json::to_string(&cmd).unwrap();
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::Add {
                path,
                recursive,
                types,
                size,
                track_cmd,
            } => {
                assert_eq!(path, PathBuf::from("/tmp/test"));
                assert_eq!(recursive, Some(true));
                assert_eq!(types, Some(vec!["MODIFY".into()]));
                assert_eq!(size, Some(">=1MB".into()));
                assert_eq!(track_cmd, Some("openclaw".into()));
            }
            _ => panic!("Expected Add variant"),
        }
    }

    #[test]
    fn test_socket_cmd_list_roundtrip() {
        let cmd = SocketCmd::List;
        let json_str = serde_json::to_string(&cmd).unwrap();
        assert!(json_str.contains("List"));
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::List => {}
            _ => panic!("Expected List variant"),
        }
    }

    #[test]
    fn test_socket_cmd_health_roundtrip() {
        let cmd = SocketCmd::Health;
        let json_str = serde_json::to_string(&cmd).unwrap();
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::Health => {}
            _ => panic!("Expected Health variant"),
        }
    }

    #[test]
    fn test_socket_cmd_remove_roundtrip() {
        let cmd = SocketCmd::Remove {
            path: PathBuf::from("/tmp/test"),
            track_cmd: Some("nginx".into()),
        };
        let json_str = serde_json::to_string(&cmd).unwrap();
        let parsed: SocketCmd = serde_json::from_str(&json_str).unwrap();
        match parsed {
            SocketCmd::Remove { path, track_cmd } => {
                assert_eq!(path, PathBuf::from("/tmp/test"));
                assert_eq!(track_cmd, Some("nginx".into()));
            }
            _ => panic!("Expected Remove variant"),
        }
    }

    // ── SocketResponse / SocketError JSON format ──

    #[test]
    fn test_socket_response_ok_format() {
        let resp = SocketResponse::Ok;
        let json_str = serde_json::to_string(&resp).unwrap();
        assert_eq!(json_str, "\"Ok\"");
        let parsed: SocketResponse = serde_json::from_str(&json_str).unwrap();
        assert!(matches!(parsed, SocketResponse::Ok));
    }

    #[test]
    fn test_socket_error_format() {
        let err = SocketError::Transient("daemon not running".to_string());
        let json_str = serde_json::to_string(&err).unwrap();
        assert!(json_str.contains("Transient"));
        assert!(json_str.contains("daemon not running"));
        let parsed: SocketError = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.to_string(), "Transient error: daemon not running");

        let err = SocketError::Permanent("log directory conflict".to_string());
        let json_str = serde_json::to_string(&err).unwrap();
        assert!(json_str.contains("Permanent"));
        assert!(json_str.contains("log directory conflict"));
        let parsed: SocketError = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            parsed.to_string(),
            "Permanent error: log directory conflict"
        );
    }

    #[test]
    fn test_socket_response_health_format() {
        let health = HealthInfo {
            uptime_secs: 3600,
            channel_type: "bounded(1024)".into(),
            monitored_paths: 5,
            reader_groups: 2,
            readers: vec![
                ReaderHealth {
                    alive: true,
                    restarts: 0,
                    fd: 4,
                },
                ReaderHealth {
                    alive: true,
                    restarts: 3,
                    fd: 5,
                },
            ],
        };
        let resp = SocketResponse::Health(health);
        let json_str = serde_json::to_string(&resp).unwrap();
        assert!(json_str.contains("Health"));
        assert!(json_str.contains("uptime_secs"));
        assert!(json_str.contains("3600"));
        assert!(json_str.contains("monitored_paths"));
        assert!(json_str.contains("5"));
        assert!(json_str.contains("reader_groups"));
        assert!(json_str.contains("2"));
        assert!(json_str.contains("fd"));
        assert!(json_str.contains("4"));
    }

    // ── Subscribe protocol end-to-end via Unix socket ──

    #[tokio::test]
    async fn test_subscribe_protocol_wire_format() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let listener = TokioUnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Consume client's command
            let mut buf = vec![0u8; 256];
            let n = stream.read(&mut buf).await.unwrap();
            eprintln!(
                "[server] read {} bytes: {:?}",
                n,
                String::from_utf8_lossy(&buf[..n])
            );

            // Send JSON ok response then JSONL event
            let resp = SocketResponse::Ok;
            let resp_json = serde_json::to_string(&resp).unwrap();
            let event_json = r#"{"time":"2026-05-28T10:00:00Z","event_type":"CREATE","path":"/tmp/test.txt","pid":1234,"cmd":"touch","user":"root","file_size":0,"ppid":1,"tgid":1234,"chain":""}"#;
            let payload = format!("{}\n{}\n", resp_json, event_json);
            eprintln!("[server] sending {} bytes: {:?}", payload.len(), payload);
            stream.write_all(payload.as_bytes()).await.unwrap();
            eprintln!("[server] sent, shutting down");
        });

        let mut stream = TokioUnixStream::connect(&socket_path).await.unwrap();

        stream.write_all(b"{\"Subscribe\":{}}\n").await.unwrap();

        // Read ALL data with timeout
        let mut all_data = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut all_data),
        )
        .await
        .unwrap()
        .unwrap();
        let response = String::from_utf8_lossy(&all_data[..n]).to_string();
        eprintln!("[client] received {} bytes: {:?}", n, response);

        // Parse response: lines, first = JSON ok, second = JSONL event
        let non_empty: Vec<&str> = response.lines().filter(|l| !l.trim().is_empty()).collect();
        let resp_line = non_empty.first().expect("no JSON response");
        let resp: SocketResponse = serde_json::from_str(resp_line).unwrap();
        assert!(
            matches!(resp, SocketResponse::Ok),
            "subscribe should return Ok"
        );

        let event_line = non_empty.get(1).expect("no event line");
        assert!(
            event_line.contains("\"event_type\":\"CREATE\""),
            "got: {}",
            event_line
        );
        assert!(event_line.contains("/tmp/test.txt"));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_subscribe_protocol_with_track_cmd_filter() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test_filter.sock");

        let listener = TokioUnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read the command
            let mut buf = vec![0u8; 512];
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0);

            // Parse and verify the command
            let cmd_str = String::from_utf8_lossy(&buf[..n]);
            let cmd: SocketCmd = serde_json::from_str(cmd_str.trim()).unwrap();
            match cmd {
                SocketCmd::Subscribe {
                    types, track_cmd, ..
                } => {
                    assert_eq!(track_cmd, Some("nginx".into()));
                    assert_eq!(types, Some(vec!["CREATE".into(), "DELETE".into()]));
                }
                _ => panic!("Expected Subscribe variant"),
            }

            let resp = SocketResponse::Ok;
            let resp_json = serde_json::to_string(&resp).unwrap();
            stream
                .write_all(format!("{}\n", resp_json).as_bytes())
                .await
                .unwrap();
        });

        let mut stream = TokioUnixStream::connect(&socket_path).await.unwrap();
        let subscribe_cmd = SocketCmd::Subscribe {
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            track_cmd: Some("nginx".into()),
            local_time: Some(false),
        };
        let json_payload = serde_json::to_string(&subscribe_cmd).unwrap();
        stream
            .write_all(format!("{}\n", json_payload).as_bytes())
            .await
            .unwrap();

        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: SocketResponse = serde_json::from_str(resp_line.trim()).unwrap();
        assert!(
            matches!(resp, SocketResponse::Ok),
            "subscribe with filters should succeed, got: {}",
            resp_line
        );

        server.await.unwrap();
    }
}
