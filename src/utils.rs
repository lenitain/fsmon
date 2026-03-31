use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Local, NaiveDateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use crate::proc_cache::ProcCache;

/// Parse human-readable size (e.g., "1GB", "100MB", "1024")
pub fn parse_size(size_str: &str) -> Result<i64> {
    let size_str = size_str.trim().to_uppercase();

    let (num_part, unit) = if let Some(pos) = size_str.find(|c: char| c.is_alphabetic()) {
        size_str.split_at(pos)
    } else {
        (size_str.as_str(), "")
    };

    let num: f64 = num_part.trim().parse()?;

    let multiplier = match unit.trim() {
        "" | "B" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024i64,
        "T" | "TB" => 1024 * 1024 * 1024 * 1024i64,
        _ => return Err(anyhow!("Unknown size unit: {}", unit)),
    };

    Ok((num * multiplier as f64) as i64)
}

/// Format size to human-readable string
pub fn format_size(size: i64) -> String {
    let abs_size = size.abs() as f64;
    let prefix = if size < 0 { "-" } else { "" };

    if abs_size >= 1024.0 * 1024.0 * 1024.0 * 1024.0 {
        format!(
            "{}{:.1}TB",
            prefix,
            abs_size / (1024.0 * 1024.0 * 1024.0 * 1024.0)
        )
    } else if abs_size >= 1024.0 * 1024.0 * 1024.0 {
        format!("{}{:.1}GB", prefix, abs_size / (1024.0 * 1024.0 * 1024.0))
    } else if abs_size >= 1024.0 * 1024.0 {
        format!("{}{:.1}MB", prefix, abs_size / (1024.0 * 1024.0))
    } else if abs_size >= 1024.0 {
        format!("{}{:.1}KB", prefix, abs_size / 1024.0)
    } else {
        format!("{}{:.0}B", prefix, abs_size)
    }
}

/// Parse human-readable time (e.g., "1h", "30m", "2024-05-01 10:00")
pub fn parse_time(time_str: &str) -> Result<DateTime<Utc>> {
    let time_str = time_str.trim();

    // Try relative time formats
    if let Some(num) = time_str.strip_suffix('h') {
        let hours: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::hours(hours));
    }

    if let Some(num) = time_str.strip_suffix("hr") {
        let hours: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::hours(hours));
    }

    if let Some(num) = time_str.strip_suffix('m') {
        let minutes: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::minutes(minutes));
    }

    if let Some(num) = time_str.strip_suffix("min") {
        let minutes: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::minutes(minutes));
    }

    if let Some(num) = time_str.strip_suffix('d') {
        let days: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::days(days));
    }

    if let Some(num) = time_str.strip_suffix('s') {
        let seconds: i64 = num.trim().parse()?;
        return Ok(Utc::now() - Duration::seconds(seconds));
    }

    // Try absolute time formats
    // Format: "2024-05-01 10:00"
    if let Ok(naive) = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M") {
        return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
    }

    // Format: "2024-05-01 10:00:00"
    if let Ok(naive) = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S") {
        return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
    }

    // Format: "2024-05-01"
    if let Ok(naive) =
        NaiveDateTime::parse_from_str(&format!("{} 00:00", time_str), "%Y-%m-%d %H:%M")
    {
        return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
    }

    Err(anyhow!("Failed to parse time format: {}", time_str))
}

/// Format datetime for display
pub fn format_datetime(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Get process info by PID (from fanotify event)
/// Checks proc connector cache first (for short-lived processes),
/// then falls back to /proc (for long-lived processes),
/// then falls back to file owner for USER.
pub fn get_process_info_by_pid(
    pid: u32,
    file_path: &Path,
    proc_cache: Option<&ProcCache>,
) -> (String, String) {
    // Check proc connector cache first (only source for short-lived processes)
    if let Some(cache) = proc_cache
        && let Some(info) = cache.get(&pid) {
        return (info.cmd.clone(), info.user.clone());
    }

    // Fallback to reading /proc directly (for long-lived processes)
    let cmd = read_proc_comm(pid).unwrap_or_else(|| "unknown".to_string());
    let user = read_proc_user(pid)
        .or_else(|| read_file_owner(file_path))
        .unwrap_or_else(|| "unknown".to_string());
    (cmd, user)
}

fn read_proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_proc_user(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let uid: u32 = status
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    uid_to_username(uid)
}

/// Fallback: read file owner UID from filesystem metadata
fn read_file_owner(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    uid_to_username(metadata.uid())
}

fn uid_passwd_map() -> &'static HashMap<u32, String> {
    static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for entry in passwd.lines() {
                let mut parts = entry.splitn(4, ':');
                let name = parts.next();
                let _shell = parts.next(); // password field
                let uid_str = parts.next();
                if let (Some(name), Some(uid_str)) = (name, uid_str)
                    && let Ok(uid) = uid_str.parse::<u32>() {
                    map.insert(uid, name.to_string());
                }
            }
        }
        map
    })
}

pub fn uid_to_username(uid: u32) -> Option<String> {
    uid_passwd_map().get(&uid).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("100B").unwrap(), 100);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(100), "100B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
        assert_eq!(format_size(-1024), "-1.0KB");
    }
}
