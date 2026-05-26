//! Unix socket protocol for daemon-CLI communication.
//! Uses TOML over unix stream sockets, with each message separated by a blank line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::UnixListener;

use crate::monitored::PathEntry;

#[derive(Debug, Serialize, Deserialize)]
pub struct SocketCmd {
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_cmd: Option<String>,
    /// Use local time instead of UTC in event timestamps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_time: Option<bool>,
}

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

#[derive(Debug, Serialize, Deserialize)]
pub struct SocketResp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ErrorKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<PathEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthInfo>,
}

impl SocketResp {
    pub fn ok() -> Self {
        SocketResp {
            ok: true,
            error: None,
            error_kind: None,
            paths: None,
            health: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        SocketResp {
            ok: false,
            error: Some(msg.into()),
            error_kind: None,
            paths: None,
            health: None,
        }
    }

    pub fn permanent_err(msg: impl Into<String>) -> Self {
        SocketResp {
            ok: false,
            error: Some(msg.into()),
            error_kind: Some(ErrorKind::Permanent),
            paths: None,
            health: None,
        }
    }

    pub fn health(info: HealthInfo) -> Self {
        SocketResp {
            ok: true,
            error: None,
            error_kind: None,
            paths: None,
            health: Some(info),
        }
    }
}

/// Serialize a value to a single TOML document string.
fn to_toml_string<T: Serialize>(value: &T) -> Result<String> {
    Ok(toml::to_string(value)?)
}

/// Deserialize a value from a TOML document string.
fn from_toml_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    Ok(toml::from_str(s)?)
}

/// Send a command to the running daemon and get the response.
pub fn send_cmd(socket_path: &Path, cmd: &SocketCmd) -> Result<SocketResp> {
    let stream = UnixStream::connect(socket_path).with_context(|| {
        format!(
            "Failed to connect to fsmon daemon at {}. Is the daemon running? \
             Start it with: sudo fsmon daemon",
            socket_path.display()
        )
    })?;

    let toml = to_toml_string(cmd)?;

    {
        let mut writer = stream.try_clone()?;
        // Write TOML document followed by blank line as delimiter
        write!(writer, "{toml}\n\n")?;
        writer.flush()?;
    }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    // Read until EOF — the server closes the connection after sending the response.
    // Don't break on blank lines because TOML serialization of Vec fields contains
    // embedded blank lines between array-of-tables entries (e.g., [[paths]]).
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break; // EOF
        }
        response.push_str(&line);
    }

    if response.trim().is_empty() {
        bail!("Empty response from daemon");
    }

    let resp: SocketResp =
        from_toml_str(response.trim()).with_context(|| "Failed to parse daemon response")?;
    Ok(resp)
}

/// Read a complete TOML message (separated by blank line) from an async buffered reader.
async fn read_toml_message(
    reader: &mut AsyncBufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<String> {
    let mut message = String::new();
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break; // EOF
        }
        if line.trim().is_empty() && !message.is_empty() {
            break; // blank line ends the message
        }
        message.push_str(&line);
    }
    Ok(message)
}

/// Listen for client connections on a unix socket, parse commands,
/// call handler for each, and send back responses.
pub async fn listen(
    socket_path: &Path,
    handler: impl Fn(SocketCmd) -> Result<SocketResp>,
) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path).with_context(|| {
            format!("Failed to remove existing socket {}", socket_path.display())
        })?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("Failed to bind socket at {}", socket_path.display()))?;

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let (reader, mut writer) = stream.into_split();
                let mut buf_reader = AsyncBufReader::new(reader);

                match read_toml_message(&mut buf_reader).await {
                    Ok(message) if message.trim().is_empty() => continue,
                    Ok(message) => {
                        let resp = match from_toml_str::<SocketCmd>(message.trim()) {
                            Ok(cmd) => match handler(cmd) {
                                Ok(resp) => resp,
                                Err(e) => SocketResp::err(e.to_string()),
                            },
                            Err(e) => SocketResp::err(format!("Invalid command: {e}")),
                        };

                        let resp_toml = match to_toml_string(&resp) {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("Failed to serialize response: {e}");
                                continue;
                            }
                        };

                        // Write TOML response followed by blank line delimiter
                        let resp_bytes = format!("{resp_toml}\n");
                        if let Err(e) = writer.write_all(resp_bytes.as_bytes()).await {
                            eprintln!("Failed to write response: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from socket: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to accept connection: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener as TokioUnixListener, UnixStream as TokioUnixStream};
    use tempfile::TempDir;

    // ── SocketCmd TOML serialization round-trip ──

    #[test]
    fn test_socket_cmd_subscribe_roundtrip() {
        let cmd = SocketCmd {
            cmd: "subscribe".into(),
            path: None,
            recursive: None,
            types: None,
            size: None,
            track_cmd: None,
            local_time: None,
        };
        let toml_str = toml::to_string(&cmd).unwrap();
        let parsed: SocketCmd = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.cmd, "subscribe");
        assert!(parsed.path.is_none());
    }

    #[test]
    fn test_socket_cmd_subscribe_with_filters_roundtrip() {
        let cmd = SocketCmd {
            cmd: "subscribe".into(),
            path: None,
            recursive: None,
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            size: None,
            track_cmd: Some("nginx".into()),
            local_time: Some(true),
        };
        let toml_str = toml::to_string(&cmd).unwrap();
        let parsed: SocketCmd = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.cmd, "subscribe");
        assert_eq!(parsed.track_cmd, Some("nginx".into()));
        assert_eq!(parsed.types, Some(vec!["CREATE".into(), "DELETE".into()]));
        assert_eq!(parsed.local_time, Some(true));
    }

    #[test]
    fn test_socket_cmd_add_roundtrip() {
        let cmd = SocketCmd {
            cmd: "add".into(),
            path: Some(PathBuf::from("/tmp/test")),
            recursive: Some(true),
            types: Some(vec!["MODIFY".into()]),
            size: Some(">=1MB".into()),
            track_cmd: Some("openclaw".into()),
            local_time: None,
        };
        let toml_str = toml::to_string(&cmd).unwrap();
        let parsed: SocketCmd = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.cmd, "add");
        assert_eq!(parsed.path, Some(PathBuf::from("/tmp/test")));
        assert_eq!(parsed.track_cmd, Some("openclaw".into()));
    }

    #[test]
    fn test_socket_cmd_metrics_roundtrip() {
        let cmd = SocketCmd {
            cmd: "metrics".into(),
            path: None,
            recursive: None,
            types: None,
            size: None,
            track_cmd: None,
            local_time: None,
        };
        let toml_str = toml::to_string(&cmd).unwrap();
        assert!(toml_str.contains("cmd = \"metrics\""));
        let parsed: SocketCmd = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.cmd, "metrics");
    }

    // ── SocketResp format ──

    #[test]
    fn test_socket_resp_ok_subscribe_format() {
        let resp = SocketResp::ok();
        let toml_str = toml::to_string(&resp).unwrap();
        assert!(toml_str.contains("ok = true"));
        assert!(!toml_str.contains("error"));
    }

    #[test]
    fn test_socket_resp_error_format() {
        let resp = SocketResp::err("daemon not running");
        let toml_str = toml::to_string(&resp).unwrap();
        assert!(toml_str.contains("ok = false"));
        assert!(toml_str.contains("daemon not running"));
        // Permanent error has error_kind
        let resp = SocketResp::permanent_err("log directory conflict");
        let toml_str = toml::to_string(&resp).unwrap();
        assert!(toml_str.contains("error_kind = \"Permanent\""));
    }

    #[test]
    fn test_socket_resp_health_format() {
        let health = HealthInfo {
            uptime_secs: 3600,
            channel_type: "bounded(1024)".into(),
            monitored_paths: 5,
            reader_groups: 2,
            readers: vec![
                ReaderHealth { alive: true, restarts: 0, fd: 4 },
                ReaderHealth { alive: true, restarts: 3, fd: 5 },
            ],
        };
        let resp = SocketResp::health(health);
        let toml_str = toml::to_string(&resp).unwrap();
        assert!(toml_str.contains("ok = true"));
        assert!(toml_str.contains("uptime_secs = 3600"));
        assert!(toml_str.contains("monitored_paths = 5"));
        assert!(toml_str.contains("reader_groups = 2"));
        assert!(toml_str.contains("fd = 4"));
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
            eprintln!("[server] read {} bytes: {:?}", n, String::from_utf8_lossy(&buf[..n]));

            // Send TOML ok response then JSONL event
            let resp = SocketResp::ok();
            let resp_toml = toml::to_string(&resp).unwrap().trim().to_string();
            let event_json = r#"{"time":"2026-05-28T10:00:00Z","event_type":"CREATE","path":"/tmp/test.txt","pid":1234,"cmd":"touch","user":"root","file_size":0,"ppid":1,"tgid":1234,"chain":""}"#;
            let payload = format!("{}\n{}\n", resp_toml, event_json);
            eprintln!("[server] sending {} bytes: {:?}", payload.len(), payload);
            stream.write_all(payload.as_bytes()).await.unwrap();
            eprintln!("[server] sent, shutting down");
        });

        let mut stream = TokioUnixStream::connect(&socket_path).await.unwrap();

        stream.write_all(b"cmd = \"subscribe\"\n\n").await.unwrap();

        // Read ALL data with timeout
        let mut all_data = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut all_data),
        ).await.unwrap().unwrap();
        let response = String::from_utf8_lossy(&all_data[..n]).to_string();
        eprintln!("[client] received {} bytes: {:?}", n, response);

        // Parse response: skip empty lines, first non-empty = TOML, second = JSONL
        let non_empty: Vec<&str> = response.lines().filter(|l| !l.trim().is_empty()).collect();
        let resp_line = non_empty.first().expect("no TOML response");
        let resp: SocketResp = toml::from_str(resp_line).unwrap();
        assert!(resp.ok, "subscribe should return ok = true");

        let event_line = non_empty.get(1).expect("no event line");
        assert!(event_line.contains("\"event_type\":\"CREATE\""), "got: {}", event_line);
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
            let cmd: SocketCmd = toml::from_str(cmd_str.trim()).unwrap();
            assert_eq!(cmd.cmd, "subscribe");
            assert_eq!(cmd.track_cmd, Some("nginx".into()));
            assert_eq!(cmd.types, Some(vec!["CREATE".into(), "DELETE".into()]));

            let resp = SocketResp::ok();
            let resp_toml = toml::to_string(&resp).unwrap();
            stream.write_all(format!("{}\n", resp_toml).as_bytes()).await.unwrap();
        });

        let mut stream = TokioUnixStream::connect(&socket_path).await.unwrap();
        let subscribe_cmd = SocketCmd {
            cmd: "subscribe".into(),
            path: None,
            recursive: None,
            types: Some(vec!["CREATE".into(), "DELETE".into()]),
            size: None,
            track_cmd: Some("nginx".into()),
            local_time: Some(false),
        };
        let toml_payload = toml::to_string(&subscribe_cmd).unwrap();
        stream.write_all(format!("{}\n\n", toml_payload).as_bytes()).await.unwrap();

        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: SocketResp = toml::from_str(resp_line.trim()).unwrap();
        assert!(resp.ok, "subscribe with filters should succeed, got: {}", resp_line);

        server.await.unwrap();
    }
}
