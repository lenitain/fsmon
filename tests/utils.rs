//! P1 — Utility function tests (parse_size, parse_time, cmd_to_log_name).

use fsmon::common::{
    SizeOp, TimeOp, format_datetime, parse_size, parse_size_filter, parse_time_filter,
};

// ---- parse_size ----

#[test]
fn parse_size_units() {
    assert_eq!(parse_size("1KB").unwrap(), 1024);
    assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
    assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_size("1TB").unwrap(), 1024 * 1024 * 1024 * 1024);

    // Bytes (no suffix)
    assert_eq!(parse_size("0").unwrap(), 0);
    assert_eq!(parse_size("1024").unwrap(), 1024);
    assert_eq!(parse_size("1048576").unwrap(), 1048576);
}

#[test]
fn parse_size_case_insensitive() {
    assert_eq!(parse_size("1kb").unwrap(), 1024);
    assert_eq!(parse_size("1Kb").unwrap(), 1024);
    assert_eq!(parse_size("1Mb").unwrap(), 1024 * 1024);
    assert_eq!(parse_size("1gb").unwrap(), 1024 * 1024 * 1024);
}

#[test]
fn parse_size_fractional() {
    assert_eq!(parse_size("1.5KB").unwrap(), 1536);
    assert_eq!(parse_size("0.5MB").unwrap(), 524288);
}

#[test]
fn parse_size_invalid_returns_err() {
    assert!(parse_size("abc").is_err());
    assert!(parse_size("").is_err());
}

// ---- parse_size_filter ----

#[test]
fn size_filter_gt() {
    let f = parse_size_filter(">1MB").unwrap();
    assert_eq!(f.op(), SizeOp::Gt);
    assert_eq!(f.bytes(), 1024 * 1024);
}

#[test]
fn size_filter_ge() {
    let f = parse_size_filter(">=1GB").unwrap();
    assert_eq!(f.op(), SizeOp::Ge);
    assert_eq!(f.bytes(), 1024 * 1024 * 1024);
}

#[test]
fn size_filter_lt() {
    let f = parse_size_filter("<500KB").unwrap();
    assert_eq!(f.op(), SizeOp::Lt);
    assert_eq!(f.bytes(), 500 * 1024);
}

#[test]
fn size_filter_le() {
    let f = parse_size_filter("<=100MB").unwrap();
    assert_eq!(f.op(), SizeOp::Le);
    assert_eq!(f.bytes(), 100 * 1024 * 1024);
}

#[test]
fn size_filter_eq() {
    let f = parse_size_filter("=0").unwrap();
    assert_eq!(f.op(), SizeOp::Eq);
    assert_eq!(f.bytes(), 0);
}

#[test]
fn size_filter_case_insensitive_units() {
    let f = parse_size_filter(">1mb").unwrap();
    assert_eq!(f.bytes(), 1024 * 1024);
}

#[test]
fn size_filter_invalid_returns_err() {
    assert!(parse_size_filter("garbage").is_err());
    assert!(parse_size_filter("1MB").is_err()); // no operator
}

// ---- parse_time_filter ----

#[test]
fn time_filter_relative_hours() {
    let f = parse_time_filter(">1h").unwrap();
    assert!(matches!(f.op(), TimeOp::Gt));
    // Relative filter should compute a cutoff before now
}

#[test]
fn time_filter_relative_minutes() {
    assert!(parse_time_filter(">30m").is_ok());
    assert!(parse_time_filter("<5m").is_ok());
}

#[test]
fn time_filter_absolute_date_only() {
    let f = parse_time_filter("<2026-05-01").unwrap();
    assert!(matches!(f.op(), TimeOp::Lt));
}

#[test]
fn time_filter_invalid_returns_err() {
    assert!(parse_time_filter("garbage").is_err());
    assert!(parse_time_filter("").is_err());
}

// ---- format_datetime ----

#[test]
fn format_datetime_basic() {
    let dt = chrono::Utc::now();
    let formatted = format_datetime(&dt);
    // Should contain date and time separators
    assert!(formatted.contains('-'));
    assert!(formatted.contains(':'));
}
