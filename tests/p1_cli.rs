//! P1 — CLI command integration tests.
//!
//! These tests invoke the `fsmon` binary directly and verify behavior
//! by reading the monitored store and log files via the library API.
//!
//! Unlike the inline tests in `src/bin/fsmon.rs` (which test internal
//! command handler APIs), these tests exercise the full CLI → config →
//! store pipeline.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use common::fsmon_client::*;
use fsmon::common::config::Config;
use fsmon::common::monitored::Monitored;

/// Global mutex for tests that modify HOME env var.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Generate a unique temp directory path for test isolation.
fn unique_temp_home() -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("fsmon_cli_test_{}_{}", std::process::id(), n))
}

/// Run a test with an isolated HOME directory.
/// Creates `{home}/monitored` as a monitored path.
fn with_isolated_home(f: impl FnOnce(&Path, &Path)) {
    let _lock = match ENV_LOCK.lock() {
        Ok(l) => l,
        Err(e) => e.into_inner(),
    };
    let dir = unique_temp_home();
    let _ = fs::remove_dir_all(&dir);
    let home_str = dir.to_string_lossy().to_string();

    let monitored_path = dir.join("monitored");
    fs::create_dir_all(&monitored_path).unwrap();

    // Create config directory so Config::load works
    let config_dir = dir.join(".config/fsmon");
    fs::create_dir_all(&config_dir).unwrap();

    temp_env::with_vars(
        [
            ("HOME", Some(home_str.as_str())),
            ("XDG_CONFIG_HOME", None::<&str>),
            ("SUDO_UID", None::<&str>),
        ],
        || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir, &monitored_path)));
            let _ = fs::remove_dir_all(&dir);
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        },
    );
}

/// Load the monitored store from the default path under the isolated home.
fn load_store() -> Monitored {
    let mut cfg = Config::load().unwrap();
    cfg.resolve_paths().unwrap();
    Monitored::load(&cfg.monitored.path).unwrap()
}

// ---- add 命令 ----

#[test]
fn add_global_with_path() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);

        let store = load_store();
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(mp, None).is_some());
    });
}

#[test]
fn add_with_cmd_group() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "myapp", "--path", &p]);

        let store = load_store();
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(mp, Some("myapp")).is_some());
    });
}

#[test]
fn add_recursive_flag() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-r"]);

        let store = load_store();
        let entry = store.get(mp, None).unwrap();
        assert_eq!(entry.recursive, Some(true));
    });
}

#[test]
fn add_with_event_types() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&[
            "add", "_global", "--path", &p, "-t", "MODIFY", "-t", "CREATE",
        ]);

        let store = load_store();
        let entry = store.get(mp, None).unwrap();
        let types = entry.types.unwrap();
        assert!(types.contains(&"MODIFY".to_string()));
        assert!(types.contains(&"CREATE".to_string()));
    });
}

#[test]
fn add_with_size_filter() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-s", ">1MB"]);

        let store = load_store();
        let entry = store.get(mp, None).unwrap();
        assert_eq!(entry.size.as_deref(), Some(">1MB"));
    });
}

#[test]
fn add_missing_cmd_fails() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        let (ok, _, _) = run_fsmon_raw(&["add", "--path", &p]);
        assert!(!ok, "add without CMD should fail");
    });
}

#[test]
fn add_fsmon_self_rejected() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        let (ok, _, _) = run_fsmon_raw(&["add", "fsmon", "--path", &p]);
        assert!(!ok, "add fsmon should be rejected");
    });
}

#[test]
fn add_duplicate_replaces() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-r"]);
        run_fsmon_success(&["add", "_global", "--path", &p]);

        let store = load_store();
        assert_eq!(store.entry_count(), 1, "should replace, not duplicate");
        let entry = store.get(mp, None).unwrap();
        assert_eq!(
            entry.recursive,
            Some(false),
            "should be replaced with new flags"
        );
    });
}

#[test]
fn add_to_multiple_cmd_groups() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        run_fsmon_success(&["add", "myapp", "--path", &p]);

        let store = load_store();
        assert_eq!(store.entry_count(), 2);
    });
}

// ---- remove 命令 ----

#[test]
fn remove_single_path() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        run_fsmon_success(&["remove", "_global", "--path", &p]);

        let store = load_store();
        assert_eq!(store.entry_count(), 0);
    });
}

#[test]
fn remove_entire_cmd_group() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "myapp", "--path", &p]);
        assert_eq!(load_store().entry_count(), 1);

        run_fsmon_success(&["remove", "myapp"]);
        assert_eq!(load_store().entry_count(), 0);
    });
}

#[test]
fn remove_path_from_cmd_group_keeps_others() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        run_fsmon_success(&["add", "app_a", "--path", &p]);
        run_fsmon_success(&["add", "app_b", "--path", &p]);
        assert_eq!(load_store().entry_count(), 3);

        run_fsmon_success(&["remove", "app_a"]);

        let store = load_store();
        assert_eq!(store.entry_count(), 2);
        assert!(store.get(mp, None).is_some());
        assert!(store.get(mp, Some("app_b")).is_some());
        assert!(store.get(mp, Some("app_a")).is_none());
    });
}

#[test]
fn remove_nonexistent_cmd_fails() {
    with_isolated_home(|_home, _mp| {
        let (ok, _, _) = run_fsmon_raw(&["remove", "ghost"]);
        assert!(!ok, "remove nonexistent cmd should fail");
    });
}

#[test]
fn remove_missing_cmd_fails() {
    with_isolated_home(|_home, _mp| {
        let (ok, _, _) = run_fsmon_raw(&["remove", "--path", "/tmp"]);
        assert!(!ok, "remove without CMD should fail");
    });
}

// ---- monitored 命令 ----

#[test]
fn monitored_lists_entries() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);

        let stdout = run_fsmon_success(&["monitored"]);
        let entries = parse_monitored_output(&stdout);
        assert_eq!(entries.len(), 1);
    });
}

#[test]
fn monitored_empty_when_no_paths() {
    with_isolated_home(|_home, _mp| {
        let stdout = run_fsmon_success(&["monitored"]);
        // Empty output or just newlines
        assert!(stdout.trim().is_empty() || parse_monitored_output(&stdout).is_empty());
    });
}

// ---- query 命令 ----

#[test]
fn query_nonexistent_cmd_does_not_crash() {
    with_isolated_home(|_home, _mp| {
        let stdout = run_fsmon_success(&["query", "_global"]);
        // No log files → should succeed with a message, not crash
        assert!(!stdout.trim().is_empty());
    });
}

#[test]
fn query_without_cmd_fails() {
    with_isolated_home(|_home, _mp| {
        let (ok, _, _) = run_fsmon_raw(&["query"]);
        assert!(!ok, "query without CMD should fail");
    });
}

// ---- changes 命令 ----

#[test]
fn changes_nonexistent_cmd_does_not_crash() {
    with_isolated_home(|_home, _mp| {
        let stdout = run_fsmon_success(&["changes", "_global"]);
        assert!(!stdout.trim().is_empty());
    });
}

// ---- clean 命令 ----

#[test]
fn clean_missing_cmd_fails() {
    with_isolated_home(|_home, _mp| {
        let (ok, _, _) = run_fsmon_raw(&["clean"]);
        assert!(!ok, "clean without CMD should fail");
    });
}

#[test]
fn clean_nonexistent_log_succeeds() {
    with_isolated_home(|_home, _mp| {
        // Clean ghost cmd with no log — should succeed
        let stdout = run_fsmon_success(&["clean", "ghost"]);
        assert!(!stdout.is_empty(), "should print a message");
    });
}

#[test]
fn clean_dry_run_preserves_data() {
    with_isolated_home(|_home, _mp| {
        // Create a mock log file with 2 events
        let mut cfg = Config::load().unwrap();
        cfg.logging.path = Some(std::path::PathBuf::from("~/.local/state/fsmon"));
        cfg.resolve_paths().unwrap();
        let log_dir = cfg.logging.path.unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        use chrono::Utc;
        use std::io::Write;
        let log_path = log_dir.join("_global_log.jsonl");
        let mut f = fs::File::create(&log_path).unwrap();
        let ts = Utc::now();
        let old = format!(
            r#"{{"time":"{}","event_type":"CREATE","path":"/old","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}}"#,
            (ts - chrono::Duration::days(100)).to_rfc3339()
        );
        let recent = format!(
            r#"{{"time":"{}","event_type":"MODIFY","path":"/recent","pid":2,"cmd":"y","user":"r","file_size":100,"ppid":0,"tgid":0,"chain":""}}"#,
            ts.to_rfc3339()
        );
        writeln!(f, "{}", old).unwrap();
        writeln!(f, "{}", recent).unwrap();

        let stdout = run_fsmon_success(&["clean", "_global", "--dry-run"]);
        assert!(
            stdout.contains("Dry run") || stdout.contains("dry"),
            "should mention dry run"
        );

        // File still has 2 lines (dry run)
        let content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(content.lines().count(), 2, "dry run should not modify");
    });
}
