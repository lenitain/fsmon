pub mod clean;
pub mod config;
pub mod dir_cache;
pub mod fid_parser;
pub mod filters;
pub mod help;
pub mod metrics;
pub mod monitor;
pub mod monitored;
pub mod proc_cache;
pub mod query;
pub mod socket;
pub mod utils;
pub use utils::{
    SizeFilter, SizeOp, TimeFilter, TimeOp, format_datetime, parse_size, parse_size_filter,
    parse_time, parse_time_filter,
};

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

        // When running as root (daemon), chown lock file to original user
        // so CLI commands (running as user) can read/manage it.
        crate::config::chown_to_original_user(&path);

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

        Ok(DaemonLock { file })
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
    /// Timestamp is in UTC (ISO 8601 with Z suffix).
    pub fn to_jsonl_string(&self) -> String {
        serde_json::to_string(self).expect("FileEvent serialization should not fail")
    }

    /// Serialize to JSONL with timestamp converted to local time.
    /// Preserves the exact field order of the struct (same as to_jsonl_string),
    /// only replaces the time value with local timezone offset (e.g. +08:00).
    pub fn to_jsonl_string_local(&self) -> String {
        use chrono::TimeZone;
        let local_time = chrono::Local
            .from_utc_datetime(&self.time.naive_utc())
            .to_rfc3339();
        // Serialize normally to preserve struct field order,
        // then patch only the time value inline.
        let json = serde_json::to_string(self).expect("FileEvent serialization");
        // Find "time":"..." and replace the value
        if let Some(start) = json.find("\"time\":\"") {
            let val_start = start + 8; // after "time":"
            if let Some(end) = json[val_start..].find('"') {
                let val_end = val_start + end;
                let mut out = String::with_capacity(json.len() + 10);
                out.push_str(&json[..val_start]);
                out.push_str(&local_time);
                out.push_str(&json[val_end..]);
                return out;
            }
        }
        json
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_to_jsonl_string_field_order() {
        let ev = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: std::path::PathBuf::from("/tmp/test.txt"),
            pid: 1234,
            cmd: "touch".into(),
            user: "pilot".into(),
            file_size: 0,
            ppid: 100,
            tgid: 1234,
            chain: "1234|touch|pilot;100|bash|pilot".into(),
        };

        let normal = ev.to_jsonl_string();
        let local = ev.to_jsonl_string_local();

        fn field_names(s: &str) -> Vec<String> {
            s[1..s.len() - 1]
                .split(',')
                .map(|p| p.split(':').next().unwrap().trim_matches('"').to_string())
                .collect()
        }
        assert_eq!(
            field_names(&normal),
            field_names(&local),
            "field order must be identical"
        );

        assert!(
            normal.contains("\"time\":\"") && normal.contains("Z\""),
            "normal uses UTC Z"
        );
        assert!(
            local.contains("+"),
            "local time should have +HH:MM offset, got: {}",
            &local[..local.len().min(120)]
        );
    }
}
