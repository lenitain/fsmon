use std::path::Path;

pub use sizefilter::{SizeFilter, SizeOp, format_size, parse_size, parse_size_filter};
pub use timefilter::{TimeFilter, TimeOp, format_datetime, parse_time, parse_time_filter};

use chrono::{DateTime, Utc};
use proc_tree::{ProcessTracker, Tgid};

/// Extension trait for TimeFilter to provide matching and classification methods.
pub trait TimeFilterExt {
    /// Check if a timestamp matches this filter.
    fn matches(&self, time: DateTime<Utc>) -> bool;

    /// Check if this filter is a lower bound (Gt or Ge).
    fn is_lower_bound(&self) -> bool;

    /// Check if this filter is an upper bound (Lt or Le).
    fn is_upper_bound(&self) -> bool;
}

impl TimeFilterExt for TimeFilter {
    fn matches(&self, time: DateTime<Utc>) -> bool {
        match self.op() {
            TimeOp::Gt => time > self.time(),
            TimeOp::Ge => time >= self.time(),
            TimeOp::Lt => time < self.time(),
            TimeOp::Le => time <= self.time(),
            TimeOp::Eq => time == self.time(),
        }
    }

    fn is_lower_bound(&self) -> bool {
        matches!(self.op(), TimeOp::Gt | TimeOp::Ge)
    }

    fn is_upper_bound(&self) -> bool {
        matches!(self.op(), TimeOp::Lt | TimeOp::Le)
    }
}

/// Threshold for disk space pre-check.
/// `Percent(pct)` — warn when free space drops below `pct`% of total.
/// `Bytes(n)`    — warn when free space drops below `n` bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskFreeThreshold {
    Percent(f64),
    Bytes(u64),
}

/// Parse a `--disk-min-free` style value.
/// "10%" → DiskFreeThreshold::Percent(10.0)
/// "5GB" → DiskFreeThreshold::Bytes(5_368_709_120)
/// "500MB" → DiskFreeThreshold::Bytes(524_288_000)
pub fn parse_disk_min_free(s: &str) -> Result<DiskFreeThreshold, String> {
    let s = s.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let val: f64 = pct
            .trim()
            .parse()
            .map_err(|e| format!("invalid percentage '{}': {}", pct, e))?;
        if val <= 0.0 || val > 100.0 {
            return Err(format!("percentage must be between 0 and 100, got {}", val));
        }
        Ok(DiskFreeThreshold::Percent(val))
    } else {
        let bytes = parse_size(s).map_err(|e| format!("invalid size '{}': {}", s, e))?;
        if bytes <= 0 {
            return Err("disk-min-free must be positive".to_string());
        }
        Ok(DiskFreeThreshold::Bytes(bytes as u64))
    }
}

/// Resolved process info for a file event (event-driven, RUN-23).
///
/// The tracker contributes generation-safe topology (tgid/ppid, comm from
/// the cn_proc `Comm` events); `/proc` is read on demand for the fields the
/// event stream cannot carry (cmdline, uid). PID reuse needs no manual
/// verification — `ProcessKey` generations guarantee the current lookup.
#[derive(Debug, Clone, Default)]
pub struct ProcInfo {
    pub comm: String,
    pub cmd: String,
    pub user: String,
    pub ppid: u32,
    pub tgid: u32,
}

pub fn get_proc_info(
    tracker: Option<&ProcessTracker>,
    pid: u32,
    file_path: &Path,
) -> ProcInfo {
    // 1. Generation-safe topology from the event-driven tracker.
    let mut info = ProcInfo::default();
    if let Some(t) = tracker {
        let view = t.view();
        if let Some(key) = view.current(Tgid(pid)) {
            info.tgid = key.tgid.0;
            if let Some(parent) = view.get(key).and_then(|n| n.current_parent()) {
                info.ppid = parent.tgid.0;
            }
            if let Some(meta) = view.metadata(key) {
                info.comm = meta.comm.clone().unwrap_or_default();
            }
        }
    }
    // 2. On-demand /proc for comm/cmd/uid (short-lived processes may be
    // gone; fall back to file owner for user).
    let proc_root = std::path::Path::new("/proc");
    if let Some(meta) = proc_tree::read_metadata(proc_root, pid) {
        if info.comm.is_empty() {
            info.comm = meta.comm.clone().unwrap_or_default();
        }
        if let Some(uid) = meta.uid {
            info.user = uid_to_username(uid);
        }
    }
    if info.cmd.is_empty() {
        info.cmd = proc_tree::read_cmdline(proc_root, pid).unwrap_or_default();
    }
    if info.user.is_empty() {
        info.user = read_file_owner(file_path).unwrap_or_default();
    }
    info
}

/// UID → username via the `users` crate (0.5's `uid_to_username` was
/// removed with the compat layer).
pub fn uid_to_username(uid: u32) -> String {
    users::get_user_by_uid(uid)
        .map(|u| u.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| uid.to_string())
}

/// Build the current-parent ancestry chain for a PID (RUN-23): walks the
/// tracker's generation-safe ancestry, resolving comm from metadata and
/// cmdline on demand.
pub fn build_chain(tracker: &ProcessTracker, pid: u32) -> Vec<crate::common::ChainLink> {
    let view = tracker.view();
    let Some(key) = view.current(Tgid(pid)) else {
        return Vec::new();
    };
    let (keys, _end) = view.ancestors(key, proc_tree::AncestorMode::CurrentParent);
    let proc_root = std::path::Path::new("/proc");
    keys.into_iter()
        .filter_map(|k| {
            let node = view.get(k)?;
            let comm = node
                .metadata()
                .and_then(|m| m.comm.clone())
                .unwrap_or_default();
            let user = node
                .metadata()
                .and_then(|m| m.uid)
                .map(uid_to_username)
                .unwrap_or_default();
            let cmd = proc_tree::read_cmdline(proc_root, k.tgid.0).unwrap_or_default();
            Some(crate::common::ChainLink {
                pid: k.tgid.0,
                comm,
                cmd,
                user,
            })
        })
        .collect()
}

/// Fallback: read file owner UID from filesystem metadata
fn read_file_owner(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some(uid_to_username(metadata.uid()))
}

/// Convert a monitored path to a deterministic, fixed-length log filename.
/// Resolve log filename from cmd name.
/// `"_global"` → `"_global_log.jsonl"`, `"openclaw"` → `"openclaw_log.jsonl"`.
pub fn cmd_to_log_name(cmd: &str) -> String {
    // Replace path separators and other filesystem-unsafe characters with underscores.
    // This is necessary because cmd may be a full cmdline like
    // "bun /home/user/.bun/bin/pi" which contains '/'.
    let safe = cmd.replace('/', "_");
    format!("{}_log.jsonl", safe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike};
    use chrono::{Duration, Utc};

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
        assert_eq!(f.op(), TimeOp::Gt);
        let diff = Utc::now() - f.time();
        assert!(diff >= chrono::Duration::minutes(59) && diff <= chrono::Duration::minutes(61));
    }

    #[test]
    fn test_parse_time_filter_ge() {
        let f = parse_time_filter(">=7d").unwrap();
        assert_eq!(f.op(), TimeOp::Ge);
        let diff = Utc::now() - f.time();
        assert!(diff >= chrono::Duration::days(6) && diff <= chrono::Duration::days(8));
    }

    #[test]
    fn test_parse_time_filter_lt() {
        let f = parse_time_filter("<2026-05-01").unwrap();
        assert_eq!(f.op(), TimeOp::Lt);
        assert_eq!(f.time().year(), 2026);
        assert_eq!(f.time().month(), 5);
        assert_eq!(f.time().day(), 1);
    }

    #[test]
    fn test_parse_time_filter_le() {
        let f = parse_time_filter("<=30m").unwrap();
        assert_eq!(f.op(), TimeOp::Le);
        let diff = Utc::now() - f.time();
        assert!(diff >= chrono::Duration::minutes(29) && diff <= chrono::Duration::minutes(31));
    }

    #[test]
    fn test_parse_time_filter_eq() {
        let f = parse_time_filter("=2026-05-01 10:00").unwrap();
        assert_eq!(f.op(), TimeOp::Eq);
        assert_eq!(f.time().year(), 2026);
        assert_eq!(f.time().month(), 5);
        assert_eq!(f.time().day(), 1);
        assert_eq!(f.time().hour(), 10);
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
        assert_eq!(f.op(), SizeOp::Ge);
        assert_eq!(f.bytes(), 524288000);
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
            matches!(
                op,
                TimeOp::Gt | TimeOp::Ge | TimeOp::Lt | TimeOp::Le | TimeOp::Eq
            )
        }
        assert!(check_op(TimeOp::Gt));
        assert!(check_op(TimeOp::Eq));

        let tf = parse_time_filter(">=1h").unwrap();
        assert!(matches!(tf.op(), TimeOp::Ge));
    }

    #[test]
    fn test_parse_disk_min_free_percent() {
        let t = parse_disk_min_free("10%").unwrap();
        assert_eq!(t, DiskFreeThreshold::Percent(10.0));

        let t = parse_disk_min_free("0.5%").unwrap();
        assert_eq!(t, DiskFreeThreshold::Percent(0.5));
    }

    #[test]
    fn test_parse_disk_min_free_bytes() {
        let t = parse_disk_min_free("1GB").unwrap();
        assert_eq!(t, DiskFreeThreshold::Bytes(1_073_741_824));

        let t = parse_disk_min_free("500MB").unwrap();
        assert_eq!(t, DiskFreeThreshold::Bytes(524_288_000));
    }

    #[test]
    fn test_parse_disk_min_free_errors() {
        assert!(parse_disk_min_free("101%").is_err());
        assert!(parse_disk_min_free("0%").is_err());
        assert!(parse_disk_min_free("-1GB").is_err());
        assert!(parse_disk_min_free("invalid").is_err());
    }

    #[test]
    fn test_parse_disk_min_free_invalid_percent_range() {
        assert!(parse_disk_min_free("150%").is_err(), ">100 should fail");
        assert!(parse_disk_min_free("-5%").is_err(), "negative should fail");
        assert!(parse_disk_min_free("0%").is_err(), "0 should fail");
    }
}
