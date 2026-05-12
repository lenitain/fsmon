use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

pub use sizefilter::{SizeFilter, SizeOp, format_size, parse_size, parse_size_filter};
pub use timefilter::{TimeFilter, TimeOp, format_datetime, parse_time, parse_time_filter};

use crate::proc_cache::{ProcCache, ProcInfo};

/// Get process info by PID (from fanotify event)
/// Checks proc connector cache first (for short-lived processes),
/// then falls back to /proc (for long-lived processes),
/// then falls back to file owner for USER.
pub fn get_process_info_by_pid(
    pid: u32,
    file_path: &Path,
    proc_cache: Option<&ProcCache>,
) -> ProcInfo {
    // Check proc connector cache first (only source for short-lived processes)
    if let Some(cache) = proc_cache
        && let Some(info) = cache.get(&pid)
    {
        return info.clone();
    }

    // Fallback to reading /proc directly (for long-lived processes)
    // If the process just exited, /proc/{pid} might still exist briefly
    // as a zombie before the parent reaps it. Retry with short sleep.
    let cmd = retry(|| read_proc_comm(pid)).unwrap_or_else(|| "unknown".to_string());
    let (user, ppid, tgid) = retry(|| read_proc_status_fields(pid)).unwrap_or_else(|| {
        let fallback_user = read_file_owner(file_path).unwrap_or_else(|| "unknown".to_string());
        (fallback_user, 0u32, 0u32)
    });
    ProcInfo {
        cmd,
        user,
        ppid,
        tgid,
    }
}

/// Retry a fallible operation up to 3 times with 500µs sleep between attempts.
fn retry<T, F>(mut f: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    if let Some(val) = f() {
        return Some(val);
    }
    for _ in 0..2 {
        std::thread::sleep(std::time::Duration::from_micros(500));
        if let Some(val) = f() {
            return Some(val);
        }
    }
    None
}

fn read_proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read user, ppid, tgid from /proc/{pid}/status in one pass.
fn read_proc_status_fields(pid: u32) -> Option<(String, u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let mut user = String::new();
    let mut ppid = 0u32;
    let mut tgid = 0u32;
    for line in status.lines() {
        if let Some(val) = line.strip_prefix("Uid:") {
            let uid: u32 = val.split_whitespace().next()?.parse().ok()?;
            user = uid_to_username(uid).unwrap_or_else(|| "unknown".to_string());
        } else if let Some(val) = line.strip_prefix("PPid:") {
            ppid = val.trim().parse().ok()?;
        } else if let Some(val) = line.strip_prefix("Tgid:") {
            tgid = val.trim().parse().ok()?;
        }
    }
    Some((user, ppid, tgid))
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

/// Convert a monitored path to a deterministic, fixed-length log filename.
/// Resolve log filename from cmd name.
/// `"_global"` → `"_global_log.jsonl"`, `"openclaw"` → `"openclaw_log.jsonl"`.
pub fn cmd_to_log_name(cmd: &str) -> String {
    format!("{}_log.jsonl", cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use chrono::{Datelike, TimeZone, Timelike};

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

    // ---- parse_time_filter tests ----

    #[test]
    fn test_parse_time_filter_gt() {
        let f = parse_time_filter(">1h").unwrap();
        assert_eq!(f.op, TimeOp::Gt);
        let diff = Utc::now() - f.time;
        assert!(diff >= chrono::Duration::minutes(59) && diff <= chrono::Duration::minutes(61));
    }

    #[test]
    fn test_parse_time_filter_ge() {
        let f = parse_time_filter(">=7d").unwrap();
        assert_eq!(f.op, TimeOp::Ge);
        let diff = Utc::now() - f.time;
        assert!(diff >= chrono::Duration::days(6) && diff <= chrono::Duration::days(8));
    }

    #[test]
    fn test_parse_time_filter_lt() {
        let f = parse_time_filter("<2026-05-01").unwrap();
        assert_eq!(f.op, TimeOp::Lt);
        assert_eq!(f.time.year(), 2026);
        assert_eq!(f.time.month(), 5);
        assert_eq!(f.time.day(), 1);
    }

    #[test]
    fn test_parse_time_filter_le() {
        let f = parse_time_filter("<=30m").unwrap();
        assert_eq!(f.op, TimeOp::Le);
        let diff = Utc::now() - f.time;
        assert!(diff >= chrono::Duration::minutes(29) && diff <= chrono::Duration::minutes(31));
    }

    #[test]
    fn test_parse_time_filter_eq() {
        let f = parse_time_filter("=2026-05-01 10:00").unwrap();
        assert_eq!(f.op, TimeOp::Eq);
        assert_eq!(f.time.year(), 2026);
        assert_eq!(f.time.month(), 5);
        assert_eq!(f.time.day(), 1);
        assert_eq!(f.time.hour(), 10);
    }

    #[test]
    fn test_parse_time_filter_no_operator_errors() {
        assert!(parse_time_filter("1h").is_err());
        assert!(parse_time_filter("30d").is_err());
        assert!(parse_time_filter("2026-05-01").is_err());
    }

    #[test]
    fn test_parse_time_filter_invalid() {
        assert!(parse_time_filter(">abc").is_err());
        assert!(parse_time_filter(">=").is_err());
    }

    #[test]
    fn test_format_datetime() {
        let dt = Utc.with_ymd_and_hms(2024, 5, 1, 10, 30, 45).unwrap();
        let formatted = format_datetime(&dt);
        // Output depends on local timezone, just check it's non-empty and contains date parts
        assert!(!formatted.is_empty());
        assert!(formatted.contains("2024"));
    }

    // -- cross-crate integration tests --

    #[test]
    fn reexported_parse_size_still_works() {
        // These functions now come from sizefilter crate via re-export
        assert_eq!(parse_size("1GB").unwrap(), 1073741824);
        assert_eq!(format_size(1024), "1.0KB");
        let f = parse_size_filter(">=500MB").unwrap();
        assert_eq!(f.op, SizeOp::Ge);
        assert_eq!(f.bytes, 524288000);
    }

    #[test]
    fn size_error_converts_to_anyhow() {
        // SizeError implements std::error::Error → auto-converts to anyhow::Error via ?
        fn returns_anyhow() -> anyhow::Result<i64> {
            Ok(parse_size("invalid")?)
        }
        let err = returns_anyhow().unwrap_err();
        assert!(err.to_string().contains("failed to parse number"));
    }

    #[test]
    fn time_filter_now_uses_timeop() {
        // After timefilter integration, TimeFilter uses TimeOp not SizeOp
        fn check_op(op: TimeOp) -> bool {
            matches!(op, TimeOp::Gt | TimeOp::Ge | TimeOp::Lt | TimeOp::Le | TimeOp::Eq)
        }
        assert!(check_op(TimeOp::Gt));
        assert!(check_op(TimeOp::Eq));

        let tf = parse_time_filter(">=1h").unwrap();
        assert!(matches!(tf.op, TimeOp::Ge));
    }
}
