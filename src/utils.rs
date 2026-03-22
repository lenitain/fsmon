use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Local, NaiveDateTime, Utc};
use std::path::Path;

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
        "G" | "GB" => 1024 * 1024 * 1024,
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
        format!("{}{:.1}TB", prefix, abs_size / (1024.0 * 1024.0 * 1024.0 * 1024.0))
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
    if let Ok(naive) = NaiveDateTime::parse_from_str(&format!("{} 00:00", time_str), "%Y-%m-%d %H:%M") {
        return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
    }

    Err(anyhow!("无法解析时间格式: {}", time_str))
}

/// Format datetime for display
pub fn format_datetime(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Get process info for a given file path
pub fn get_process_info(_path: &Path) -> (u32, String, String) {
    // Try to get current process info as fallback
    let pid = std::process::id();

    // Try to read process info from /proc
    if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
        let cmd = cmdline
            .split('\0')
            .next()
            .unwrap_or("unknown")
            .split('/')
            .last()
            .unwrap_or("unknown")
            .to_string();

        // Try to get user info
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", pid)) {
            for line in status.lines() {
                if line.starts_with("Uid:") {
                    let uid_str: String = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("0")
                        .to_string();

                    // Try to get username
                    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
                        for entry in passwd.lines() {
                            let parts: Vec<&str> = entry.split(':').collect();
                            if parts.len() >= 3 && parts[2] == uid_str {
                                return (pid, cmd, parts[0].to_string());
                            }
                        }
                    }

                    return (pid, cmd, format!("uid:{}", uid_str));
                }
            }
        }

        return (pid, cmd, "unknown".to_string());
    }

    (pid, "unknown".to_string(), "unknown".to_string())
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
