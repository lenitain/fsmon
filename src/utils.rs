use anyhow::{Result, anyhow};
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
        && let Some(info) = cache.get(&pid)
    {
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
                    && let Ok(uid) = uid_str.parse::<u32>()
                {
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
    use chrono::{Datelike, TimeZone, Timelike};

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("100B").unwrap(), 100);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_edge_cases() {
        // Case insensitive
        assert_eq!(parse_size("1kb").unwrap(), 1024);
        assert_eq!(parse_size("1Kb").unwrap(), 1024);
        // With whitespace
        assert_eq!(parse_size("  1KB  ").unwrap(), 1024);
        // Decimal
        assert_eq!(parse_size("1.5KB").unwrap(), 1536);
        // Zero
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("0B").unwrap(), 0);
        // TB
        assert_eq!(parse_size("1TB").unwrap(), 1024i64 * 1024 * 1024 * 1024);
        // Short unit
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_invalid() {
        assert!(parse_size("1XB").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(100), "100B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
        assert_eq!(format_size(-1024), "-1.0KB");
    }

    #[test]
    fn test_format_size_edge_cases() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(-0), "0B");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0GB");
        assert_eq!(format_size(-1024 * 1024), "-1.0MB");
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(-500), "-500B");
    }

    #[test]
    fn test_parse_time_relative_hours() {
        let now = Utc::now();
        let parsed = parse_time("1h").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::minutes(59));
        assert!(diff <= Duration::minutes(61));
    }

    #[test]
    fn test_parse_time_relative_minutes() {
        let now = Utc::now();
        let parsed = parse_time("30m").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::minutes(29));
        assert!(diff <= Duration::minutes(31));
    }

    #[test]
    fn test_parse_time_relative_days() {
        let now = Utc::now();
        let parsed = parse_time("7d").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::hours(167));
        assert!(diff <= Duration::hours(169));
    }

    #[test]
    fn test_parse_time_relative_seconds() {
        let now = Utc::now();
        let parsed = parse_time("30s").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::seconds(29));
        assert!(diff <= Duration::seconds(31));
    }

    #[test]
    fn test_parse_time_relative_hr_min_suffix() {
        let now = Utc::now();
        let parsed = parse_time("2hr").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::minutes(119));
        assert!(diff <= Duration::minutes(121));

        let parsed = parse_time("15min").unwrap();
        let diff = now - parsed;
        assert!(diff >= Duration::minutes(14));
        assert!(diff <= Duration::minutes(16));
    }

    #[test]
    fn test_parse_time_absolute_datetime() {
        let parsed = parse_time("2024-05-01 10:00").unwrap();
        assert_eq!(parsed.year(), 2024);
        assert_eq!(parsed.month(), 5);
        assert_eq!(parsed.day(), 1);
        assert_eq!(parsed.hour(), 10);
        assert_eq!(parsed.minute(), 0);
    }

    #[test]
    fn test_parse_time_absolute_with_seconds() {
        let parsed = parse_time("2024-12-25 15:30:45").unwrap();
        assert_eq!(parsed.year(), 2024);
        assert_eq!(parsed.month(), 12);
        assert_eq!(parsed.day(), 25);
        assert_eq!(parsed.hour(), 15);
        assert_eq!(parsed.minute(), 30);
        assert_eq!(parsed.second(), 45);
    }

    #[test]
    fn test_parse_time_absolute_date_only() {
        let parsed = parse_time("2024-01-15").unwrap();
        assert_eq!(parsed.year(), 2024);
        assert_eq!(parsed.month(), 1);
        assert_eq!(parsed.day(), 15);
        assert_eq!(parsed.hour(), 0);
        assert_eq!(parsed.minute(), 0);
    }

    #[test]
    fn test_parse_time_invalid() {
        assert!(parse_time("invalid").is_err());
        assert!(parse_time("2024-13-01 10:00").is_err());
        assert!(parse_time("abc").is_err());
    }

    #[test]
    fn test_format_datetime() {
        let dt = Utc.with_ymd_and_hms(2024, 5, 1, 10, 30, 45).unwrap();
        let formatted = format_datetime(&dt);
        // Output depends on local timezone, just check it's non-empty and contains date parts
        assert!(!formatted.is_empty());
        assert!(formatted.contains("2024"));
    }

    #[test]
    fn test_parse_size_roundtrip() {
        // parse_size -> format_size should be consistent for round numbers
        let sizes = vec![0, 1024, 1024 * 1024, 1024 * 1024 * 1024];
        for s in sizes {
            let formatted = format_size(s);
            let parsed = parse_size(&formatted).unwrap() as f64;
            // Allow small floating point differences
            assert!(
                (parsed - s as f64).abs() < s as f64 * 0.01 + 1.0,
                "roundtrip failed for {}: format={}, parse={}",
                s,
                formatted,
                parsed
            );
        }
    }
}
