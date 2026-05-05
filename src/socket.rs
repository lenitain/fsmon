//! Unix socket protocol for daemon-CLI communication.
//! Uses TOML over unix stream sockets, with each message separated by a blank line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::UnixListener;

use crate::store::PathEntry;

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
    pub min_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_events: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>, // For "remove" commands
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
             Start it with: sudo systemctl start fsmon",
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
