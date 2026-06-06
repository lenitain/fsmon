//! CLI argument parsing tests.
//!
//! These tests verify clap argument parsing for all CLI subcommands.
//! Moved from src/bin/fsmon.rs to reduce binary file size.

use clap::Parser;
use fsmon::common::{AddArgs, ChangesArgs, CleanArgs, QueryArgs};
use std::path::PathBuf;

// ---- AddArgs CLI parsing ----

#[test]
fn test_add_positional_cmd() {
    let args = AddArgs::try_parse_from(["add", "openclaw", "--path", "/home"]).unwrap();
    assert_eq!(args.cmd, Some("openclaw".to_string()));
    assert_eq!(args.path, Some(PathBuf::from("/home")));
}

#[test]
fn test_add_positional_cmd_only() {
    let args = AddArgs::try_parse_from(["add", "nginx"]).unwrap();
    assert_eq!(args.cmd, Some("nginx".to_string()));
    assert!(args.path.is_none());
}

#[test]
fn test_add_path_only() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp"]).unwrap();
    assert_eq!(args.path, Some(PathBuf::from("/tmp")));
    assert!(args.cmd.is_none());
    assert!(args.types.is_empty());
    assert!(!args.recursive);
    assert!(args.size.is_none());
}

#[test]
fn test_add_types_long() {
    let args = AddArgs::try_parse_from([
        "add", "--path", "/tmp", "--types", "MODIFY", "--types", "CREATE",
    ])
    .unwrap();
    assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
}

#[test]
fn test_add_types_short() {
    let args =
        AddArgs::try_parse_from(["add", "--path", "/tmp", "-t", "MODIFY", "-t", "CREATE"]).unwrap();
    assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
}

#[test]
fn test_add_types_all_long() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "--types", "all"]).unwrap();
    assert_eq!(args.types, vec!["all"]);
}

#[test]
fn test_add_types_mixed() {
    let args =
        AddArgs::try_parse_from(["add", "--path", "/tmp", "-t", "MODIFY", "--types", "CREATE"])
            .unwrap();
    assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
}

#[test]
fn test_add_recursive_short() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-r"]).unwrap();
    assert!(args.recursive);
    assert!(args.cmd.is_none());
}

#[test]
fn test_add_size_short() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-s", "1GB"]).unwrap();
    assert_eq!(args.size, Some("1GB".into()));
}

#[test]
fn test_add_size_long() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "--size", "100MB"]).unwrap();
    assert_eq!(args.size, Some("100MB".into()));
}

#[test]
fn test_add_size_with_operator() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-s", ">=1MB"]).unwrap();
    assert_eq!(args.size, Some(">=1MB".into()));

    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "--size", "<500KB"]).unwrap();
    assert_eq!(args.size, Some("<500KB".into()));

    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-s", "=0"]).unwrap();
    assert_eq!(args.size, Some("=0".into()));
}

#[test]
fn test_add_size_decimal_and_negative() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-s", "1.5KB"]).unwrap();
    assert_eq!(args.size, Some("1.5KB".into()));

    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "--size", ">-1KB"]).unwrap();
    assert_eq!(args.size, Some(">-1KB".into()));
}

#[test]
fn test_add_size_case_insensitive_unit() {
    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "-s", "1mb"]).unwrap();
    assert_eq!(args.size, Some("1mb".into()));

    let args = AddArgs::try_parse_from(["add", "--path", "/tmp", "--size", "100Kb"]).unwrap();
    assert_eq!(args.size, Some("100Kb".into()));
}

#[test]
fn test_add_all_flags() {
    let args = AddArgs::try_parse_from([
        "add", "nginx", "--path", "/tmp", "-r", "-t", "MODIFY", "--types", "CREATE", "-s", "1KB",
    ])
    .unwrap();
    assert_eq!(args.cmd, Some("nginx".to_string()));
    assert_eq!(args.path, Some(PathBuf::from("/tmp")));
    assert!(args.recursive);
    assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    assert_eq!(args.size, Some("1KB".into()));
}

#[test]
fn test_add_positional_cmd_with_recursive() {
    let args = AddArgs::try_parse_from(["add", "openclaw", "--path", "/home", "-r"]).unwrap();
    assert_eq!(args.cmd, Some("openclaw".to_string()));
    assert_eq!(args.path, Some(PathBuf::from("/home")));
    assert!(args.recursive);
}

// ---- QueryArgs CLI parsing ----

#[test]
fn test_query_no_flags() {
    let args = QueryArgs::try_parse_from(["query"]).unwrap();
    assert!(args.path.is_empty());
    assert!(args.time.is_empty());
}

#[test]
fn test_query_path_long() {
    let args = QueryArgs::try_parse_from(["query", "--path", "/tmp", "--path", "/home"]).unwrap();
    assert_eq!(
        args.path,
        vec![PathBuf::from("/tmp"), PathBuf::from("/home")]
    );
}

#[test]
fn test_query_path_short() {
    let args = QueryArgs::try_parse_from(["query", "-p", "/tmp", "-p", "/home"]).unwrap();
    assert_eq!(
        args.path,
        vec![PathBuf::from("/tmp"), PathBuf::from("/home")]
    );
}

#[test]
fn test_query_time_since() {
    let args = QueryArgs::try_parse_from(["query", "-t", ">1h"]).unwrap();
    assert_eq!(args.time, vec![">1h".to_string()]);
}

#[test]
fn test_query_time_until() {
    let args = QueryArgs::try_parse_from(["query", "--time", "<2026-05-01"]).unwrap();
    assert_eq!(args.time, vec!["<2026-05-01".to_string()]);
}

#[test]
fn test_query_time_repeatable() {
    let args = QueryArgs::try_parse_from(["query", "--time", ">1h", "--time", "<now"]).unwrap();
    assert_eq!(args.time, vec![">1h".to_string(), "<now".to_string()]);
}

#[test]
fn test_query_time_with_path() {
    let args = QueryArgs::try_parse_from(["query", "-p", "/tmp", "-t", ">1h"]).unwrap();
    assert_eq!(args.path, vec![PathBuf::from("/tmp")]);
    assert_eq!(args.time, vec![">1h".to_string()]);
}

// ---- DaemonArgs CLI parsing (uses Cli + Commands from binary) ----
// NOTE: These tests require Cli/Commands which stay in the binary crate.
// They are kept in src/bin/fsmon_tests.rs via #[path] include.

// ---- ChangesArgs CLI parsing ----

#[test]
fn test_changes_default() {
    let args = ChangesArgs::try_parse_from(["changes", "_global"]).unwrap();
    assert_eq!(args.cmd, Some("_global".to_string()));
    assert!(args.path.is_empty());
    assert!(args.time.is_empty());
}

#[test]
fn test_changes_with_time_and_path() {
    let args =
        ChangesArgs::try_parse_from(["changes", "nginx", "-p", "/var/www", "-t", ">1h"]).unwrap();
    assert_eq!(args.cmd, Some("nginx".to_string()));
    assert_eq!(args.path, vec![PathBuf::from("/var/www")]);
    assert_eq!(args.time, vec![">1h".to_string()]);
}

#[test]
fn test_changes_path_repeatable() {
    let args =
        ChangesArgs::try_parse_from(["changes", "_global", "-p", "/etc", "-p", "/home"]).unwrap();
    assert_eq!(
        args.path,
        vec![PathBuf::from("/etc"), PathBuf::from("/home")]
    );
}

#[test]
fn test_changes_time_repeatable() {
    let args =
        ChangesArgs::try_parse_from(["changes", "_global", "-t", ">1h", "-t", "<now"]).unwrap();
    assert_eq!(args.time, vec![">1h".to_string(), "<now".to_string()]);
}

#[test]
fn test_changes_no_cmd_no_args() {
    let args = ChangesArgs::try_parse_from(["changes"]).unwrap();
    assert!(args.cmd.is_none());
    assert!(args.path.is_empty());
    assert!(args.time.is_empty());
}

// ---- CleanArgs CLI parsing ----

#[test]
fn test_clean_basic_cmd() {
    let args = CleanArgs::try_parse_from(["clean", "_global"]).unwrap();
    assert_eq!(args.cmd, Some("_global".into()));
    assert!(args.time.is_none());
    assert!(args.size.is_none());
    assert!(!args.dry_run);
}

#[test]
fn test_clean_cmd_with_time() {
    let args = CleanArgs::try_parse_from(["clean", "openclaw", "--time", ">30d"]).unwrap();
    assert_eq!(args.cmd, Some("openclaw".into()));
    assert_eq!(args.time, Some(">30d".into()));
}

#[test]
fn test_clean_cmd_with_size() {
    let args = CleanArgs::try_parse_from(["clean", "nginx", "-s", "500MB"]).unwrap();
    assert_eq!(args.cmd, Some("nginx".into()));
    assert_eq!(args.size, Some("500MB".into()));
}

#[test]
fn test_clean_cmd_with_dry_run() {
    let args = CleanArgs::try_parse_from(["clean", "_global", "--dry-run"]).unwrap();
    assert_eq!(args.cmd, Some("_global".into()));
    assert!(args.dry_run);
}

#[test]
fn test_clean_all_flags() {
    let args = CleanArgs::try_parse_from([
        "clean",
        "openclaw",
        "--time",
        ">30d",
        "-s",
        ">=100MB",
        "--dry-run",
    ])
    .unwrap();
    assert_eq!(args.cmd, Some("openclaw".into()));
    assert_eq!(args.time, Some(">30d".into()));
    assert_eq!(args.size, Some(">=100MB".into()));
    assert!(args.dry_run);
}
