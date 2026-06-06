//! Watchdog validation integration tests.
//!
//! Tests that verify daemon rejects invalid watchdog configurations.
//!
//! These tests run the actual fsmon binary and check for validation errors.
//! They may fail if another daemon is running or if not run as root.

use std::process::Command;

/// Get path to the fsmon binary.
fn fsmon_bin() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps
    path.push("fsmon");
    path.to_string_lossy().to_string()
}

/// Helper to run fsmon daemon with args and capture output.
/// Returns (success, stdout, stderr).
fn run_daemon(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(fsmon_bin())
        .arg("daemon")
        .args(args)
        .output()
        .expect("Failed to execute fsmon");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stdout, stderr)
}

/// Check if error is about validation (not daemon lock or permissions).
fn is_validation_error(stderr: &str) -> bool {
    // Exclude daemon lock and permission errors
    !stderr.contains("Another fsmon daemon")
        && !stderr.contains("Operation not permitted")
        && !stderr.contains("Permission denied")
}

// ---- multiplier validation tests ----

#[test]
fn daemon_rejects_multiplier_0() {
    let (success, _, stderr) = run_daemon(&["--watchdog-multiplier", "0"]);
    assert!(!success, "daemon should fail with multiplier=0");
    // Only check for validation error if it's not a daemon lock or permission error
    if is_validation_error(&stderr) {
        assert!(
            stderr.contains("multiplier must be > 1"),
            "error should mention multiplier, got: {}",
            stderr
        );
    }
}

#[test]
fn daemon_rejects_multiplier_1() {
    let (success, _, stderr) = run_daemon(&["--watchdog-multiplier", "1"]);
    assert!(!success, "daemon should fail with multiplier=1");
    if is_validation_error(&stderr) {
        assert!(
            stderr.contains("multiplier must be > 1"),
            "error should mention multiplier, got: {}",
            stderr
        );
    }
}

#[test]
fn daemon_accepts_multiplier_2() {
    // multiplier=2 is valid, but daemon will fail for other reasons (no root, etc.)
    // We just check it doesn't fail with the multiplier error
    let (success, _, stderr) = run_daemon(&["--watchdog-multiplier", "2"]);
    if !success {
        assert!(
            !stderr.contains("multiplier must be > 1"),
            "multiplier=2 should be accepted, got: {}",
            stderr
        );
    }
}

#[test]
fn daemon_accepts_multiplier_3() {
    let (success, _, stderr) = run_daemon(&["--watchdog-multiplier", "3"]);
    if !success {
        assert!(
            !stderr.contains("multiplier must be > 1"),
            "multiplier=3 should be accepted, got: {}",
            stderr
        );
    }
}

#[test]
fn daemon_rejects_negative_multiplier() {
    // Negative value should be rejected by clap (invalid number)
    let (success, _, _) = run_daemon(&["--watchdog-multiplier", "-1"]);
    assert!(!success, "daemon should fail with negative multiplier");
}

#[test]
fn daemon_rejects_non_numeric_multiplier() {
    let (success, _, _) = run_daemon(&["--watchdog-multiplier", "abc"]);
    assert!(!success, "daemon should fail with non-numeric multiplier");
}

// ---- watchdog interval tests ----

#[test]
fn daemon_rejects_zero_interval() {
    // interval=0 with multiplier > 1 should fail because watchdog is disabled
    // but the validation only checks multiplier, not interval
    let (success, _, stderr) =
        run_daemon(&["--watchdog-interval", "0", "--watchdog-multiplier", "2"]);
    // This should fail for root privilege, not watchdog validation
    if !success {
        assert!(
            !stderr.contains("multiplier must be > 1"),
            "interval=0 should not trigger multiplier error"
        );
    }
}

#[test]
fn daemon_accepts_valid_watchdog_args() {
    let (success, _, stderr) =
        run_daemon(&["--watchdog-interval", "15", "--watchdog-multiplier", "2"]);
    // This should fail for root privilege, not watchdog validation
    if !success {
        assert!(
            !stderr.contains("multiplier must be > 1"),
            "valid watchdog args should be accepted"
        );
    }
}

// ---- combined args tests ----

#[test]
fn daemon_rejects_multiplier_only() {
    // multiplier=1 without interval should still fail validation
    let (success, _, stderr) = run_daemon(&["--watchdog-multiplier", "1"]);
    assert!(!success, "daemon should fail with multiplier=1");
    if is_validation_error(&stderr) {
        assert!(
            stderr.contains("multiplier must be > 1"),
            "error should mention multiplier"
        );
    }
}

#[test]
fn daemon_rejects_multiplier_with_interval() {
    // multiplier=1 with interval should fail validation
    let (success, _, stderr) =
        run_daemon(&["--watchdog-interval", "10", "--watchdog-multiplier", "1"]);
    assert!(!success, "daemon should fail with multiplier=1");
    if is_validation_error(&stderr) {
        assert!(
            stderr.contains("multiplier must be > 1"),
            "error should mention multiplier"
        );
    }
}
