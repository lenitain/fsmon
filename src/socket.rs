//! Unix socket protocol for daemon-CLI communication.
//! Uses JSON lines over unix stream sockets.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::UnixListener;

use crate::config::PathEntry;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum SocketCmd {
    #[serde(rename = "add")]
    Add {
        path: PathBuf,
        recursive: Option<bool>,
        types: Option<Vec<String>>,
        min_size: Option<String>,
        exclude: Option<String>,
        all_events: Option<bool>,
    },
    #[serde(rename = "remove")]
    Remove { id: u64 },
    #[serde(rename = "list")]
    List,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SocketResp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<PathEntry>>,
}

/// Send a command to the running daemon and get the response.
pub fn send_cmd(socket_path: &Path, cmd: &SocketCmd) -> Result<SocketResp> {
    let stream = UnixStream::connect(socket_path).with_context(|| {
        format!(
            "Failed to connect to fsmon daemon at {}. Is the daemon running? \
             Start it with: sudo systemctl start fsmon",
            socket_path.display()
        )
    })?;

    let json = serde_json::to_string(cmd)?;

    {
        let mut writer = stream.try_clone()?;
        writeln!(writer, "{json}")?;
        writer.flush()?;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    if line.trim().is_empty() {
        bail!("Empty response from daemon");
    }

    let resp: SocketResp =
        serde_json::from_str(line.trim()).with_context(|| "Failed to parse daemon response")?;
    Ok(resp)
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
                let mut line = String::new();

                match buf_reader.read_line(&mut line).await {
                    Ok(0) => continue,
                    Ok(_) => {
                        let trimmed = line.trim();
                        let resp = match serde_json::from_str::<SocketCmd>(trimmed) {
                            Ok(cmd) => match handler(cmd) {
                                Ok(resp) => resp,
                                Err(e) => SocketResp {
                                    ok: false,
                                    error: Some(e.to_string()),
                                    id: None,
                                    paths: None,
                                },
                            },
                            Err(e) => SocketResp {
                                ok: false,
                                error: Some(format!("Invalid command: {e}")),
                                id: None,
                                paths: None,
                            },
                        };

                        let resp_json = match serde_json::to_string(&resp) {
                            Ok(j) => j,
                            Err(e) => {
                                eprintln!("Failed to serialize response: {e}");
                                continue;
                            }
                        };

                        let resp_bytes = format!("{resp_json}\n");
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
