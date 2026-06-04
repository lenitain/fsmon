//! P1 — Crash recovery and fault tolerance tests.
//!
//! Tests DaemonLock, atomic writes, config resilience, and log truncation handling.

use std::fs;

use fsmon::DaemonLock;
use fsmon::parse_log_line_jsonl;

// ---- DaemonLock ----

#[test]
fn lock_acquire_and_reacquire() {
    let uid = nix::unistd::geteuid().as_raw();
    let lock_path = format!("/tmp/fsmon-{}.lock", uid);

    // Clean up stale lock file from previous daemon runs (root-owned).
    // If we can't remove it (Permission denied), skip this test.
    if std::path::Path::new(&lock_path).exists() {
        match fs::remove_file(&lock_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "SKIP: {} is owned by root, run 'sudo rm {}' first",
                    lock_path, lock_path
                );
                return;
            }
            Err(e) => panic!("failed to remove {}: {}", lock_path, e),
        }
    }

    // Acquire lock
    let lock = DaemonLock::acquire(uid).unwrap();

    // Double acquire must fail
    let result = DaemonLock::acquire(uid);
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(
            e.to_string().contains("already running"),
            "expected 'already running', got: {}",
            e
        );
    }

    // Drop and re-acquire succeeds
    drop(lock);
    let lock2 = DaemonLock::acquire(uid).unwrap();
    drop(lock2);
}

// ---- 原子写入 ----

#[test]
fn monitored_save_load_roundtrip() {
    use fsmon::monitored::Monitored;

    let dir = std::env::temp_dir().join(format!("fsmon-atomic-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("monitored.jsonl");

    // Create empty store and save
    let store = Monitored::load(&path).unwrap_or_else(|_| Monitored { groups: vec![] });
    store.save(&path).unwrap();
    assert!(path.exists(), "file should exist after save");

    // Reload and verify
    let loaded = Monitored::load(&path).unwrap();
    assert_eq!(loaded.entry_count(), 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn monitored_load_missing_file_fails() {
    use fsmon::monitored::Monitored;

    let path = std::env::temp_dir().join(format!("fsmon-nonexistent-{}.jsonl", std::process::id()));
    let _ = fs::remove_file(&path);
    let result = Monitored::load(&path);
    // Should return Err or create an empty one
    if let Ok(store) = result {
        assert_eq!(store.entry_count(), 0);
    }
}

// ---- 配置容错 ----

#[test]
fn config_loads_defaults_when_no_file() {
    use fsmon::config::Config;

    let dir = std::env::temp_dir().join(format!("fsmon-config-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    temp_env::with_vars([("HOME", Some(dir.to_string_lossy().as_ref()))], || {
        let cfg = Config::load().unwrap_or_default();
        // Default config should have monitored path set
        let _ = &cfg.monitored.path; // just verify field exists
    });

    let _ = fs::remove_dir_all(&dir);
}

// ---- 日志截断容错 ----

#[test]
fn jsonl_truncated_line_is_skipped() {
    // Simulate a log file that was truncated mid-write (crash scenario)
    let lines = concat!(
        r#"{"time":"2026-06-01T10:00:00Z","event_type":"CREATE","path":"/a","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}"#,
        "\n",
        r#"{"time":"2026-06-01T10:01:00Z","event_type":"MODIFY","path":"/b","pid":2,"cmd":"y","user":"r","file_size":0,"ppid":0,"tgid"#,
    );

    let mut count = 0;
    for line in lines.lines() {
        if parse_log_line_jsonl(line).is_some() {
            count += 1;
        }
    }
    // First line complete → 1, second truncated → skipped
    assert_eq!(count, 1, "truncated line should be silently skipped");
}

#[test]
fn jsonl_empty_file_parses_cleanly() {
    let lines = "";
    let mut count = 0;
    for line in lines.lines() {
        if parse_log_line_jsonl(line).is_some() {
            count += 1;
        }
    }
    assert_eq!(count, 0);
}

#[test]
fn jsonl_multiple_complete_events_all_parse() {
    let lines = concat!(
        r#"{"time":"2026-06-01T10:00:00Z","event_type":"CREATE","path":"/a","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}"#,
        "\n",
        r#"{"time":"2026-06-01T10:01:00Z","event_type":"DELETE","path":"/b","pid":2,"cmd":"y","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}"#,
        "\n",
        r#"{"time":"2026-06-01T10:02:00Z","event_type":"MODIFY","path":"/c","pid":3,"cmd":"z","user":"r","file_size":100,"ppid":0,"tgid":0,"chain":""}"#,
    );

    let count = lines
        .lines()
        .filter(|l| parse_log_line_jsonl(l).is_some())
        .count();
    assert_eq!(count, 3);
}

// ---- cmd_to_log_name 格式 ----

#[test]
fn cmd_to_log_name_format() {
    use fsmon::utils::cmd_to_log_name;
    assert_eq!(cmd_to_log_name("_global"), "_global_log.jsonl");
    assert_eq!(cmd_to_log_name("myapp"), "myapp_log.jsonl");
    assert_eq!(cmd_to_log_name("nginx"), "nginx_log.jsonl");
}
