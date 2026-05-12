pub mod clean;
pub mod config;
pub mod dir_cache;
pub mod fid_parser;
pub mod filters;
pub mod help;
pub mod monitor;
pub mod monitored;
pub mod proc_cache;
pub mod query;
pub mod socket;
pub mod utils;
pub use utils::{SizeFilter, SizeOp, TimeOp, TimeFilter, parse_size, parse_size_filter, parse_time, parse_time_filter, format_datetime};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;

/// Enforces single daemon instance via `flock`.
/// Lock file at `/tmp/fsmon-<UID>.lock`.
/// Lock released automatically when process exits or crashes.
pub struct DaemonLock {
    #[allow(dead_code)]
    file: fs::File,
    _path: PathBuf,
}

impl DaemonLock {
    /// Acquire exclusive lock. Fails if another daemon is already running.
    pub fn acquire(uid: u32) -> Result<Self> {
        let path = PathBuf::from(format!("/tmp/fsmon-{}.lock", uid));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to open daemon lock file '{}': {}",
                    path.display(),
                    e
                )
            })?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Read existing PID for helpful message
                let pid_str = fs::read_to_string(&path).unwrap_or_default();
                let pid_hint = if pid_str.trim().is_empty() {
                    String::new()
                } else {
                    format!(" (PID {})", pid_str.trim())
                };
                bail!("Another fsmon daemon is already running{}", pid_hint);
            }
            Err(e) => bail!("Failed to acquire daemon lock: {}", e),
        }

        // Write PID for diagnostic purposes (not relied on for correctness)
        let _ = fs::write(&path, format!("{}", std::process::id()));

        Ok(DaemonLock { file, _path: path })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // fd closes → kernel releases flock automatically
    }
}
use std::str::FromStr;

pub const DEFAULT_KEEP_DAYS: u32 = 30;
pub const DEFAULT_MAX_SIZE: &str = ">=1GB";

pub const EXIT_CONFIG: i32 = 78;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    Access,
    Modify,
    CloseWrite,
    CloseNowrite,
    Open,
    OpenExec,
    Attrib,
    Create,
    Delete,
    DeleteSelf,
    MovedFrom,
    MovedTo,
    MoveSelf,
    FsError,
}

impl EventType {
    pub const ALL: &'static [EventType] = &[
        EventType::Access,
        EventType::Modify,
        EventType::CloseWrite,
        EventType::CloseNowrite,
        EventType::Open,
        EventType::OpenExec,
        EventType::Attrib,
        EventType::Create,
        EventType::Delete,
        EventType::DeleteSelf,
        EventType::MovedFrom,
        EventType::MovedTo,
        EventType::MoveSelf,
        EventType::FsError,
    ];
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            EventType::Access => "ACCESS",
            EventType::Modify => "MODIFY",
            EventType::CloseWrite => "CLOSE_WRITE",
            EventType::CloseNowrite => "CLOSE_NOWRITE",
            EventType::Open => "OPEN",
            EventType::OpenExec => "OPEN_EXEC",
            EventType::Attrib => "ATTRIB",
            EventType::Create => "CREATE",
            EventType::Delete => "DELETE",
            EventType::DeleteSelf => "DELETE_SELF",
            EventType::MovedFrom => "MOVED_FROM",
            EventType::MovedTo => "MOVED_TO",
            EventType::MoveSelf => "MOVE_SELF",
            EventType::FsError => "FS_ERROR",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for EventType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "ACCESS" => Ok(EventType::Access),
            "MODIFY" => Ok(EventType::Modify),
            "CLOSE_WRITE" => Ok(EventType::CloseWrite),
            "CLOSE_NOWRITE" => Ok(EventType::CloseNowrite),
            "OPEN" => Ok(EventType::Open),
            "OPEN_EXEC" => Ok(EventType::OpenExec),
            "ATTRIB" => Ok(EventType::Attrib),
            "CREATE" => Ok(EventType::Create),
            "DELETE" => Ok(EventType::Delete),
            "DELETE_SELF" => Ok(EventType::DeleteSelf),
            "MOVED_FROM" => Ok(EventType::MovedFrom),
            "MOVED_TO" => Ok(EventType::MovedTo),
            "MOVE_SELF" => Ok(EventType::MoveSelf),
            "FS_ERROR" => Ok(EventType::FsError),
            _ => Err(format!("Unknown event type: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub time: DateTime<Utc>,
    pub event_type: EventType,
    pub path: PathBuf,
    pub pid: u32,
    pub cmd: String,
    pub user: String,
    pub file_size: u64,
    #[serde(default)]
    pub ppid: u32,
    #[serde(default)]
    pub tgid: u32,
    pub chain: String,
}

impl FileEvent {
    /// Serialize to a single JSON line (for log storage / pipe output)
    pub fn to_jsonl_string(&self) -> String {
        serde_json::to_string(self).expect("FileEvent serialization should not fail")
    }

    /// Deserialize from a single JSON line
    pub fn from_jsonl_str(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// Parse a JSONL line into a FileEvent.
pub fn parse_log_line_jsonl(line: &str) -> Option<FileEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    FileEvent::from_jsonl_str(trimmed)
}
