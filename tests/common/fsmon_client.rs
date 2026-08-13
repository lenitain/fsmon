//! CLI client helpers for fsmon integration tests.
//!
//! Wraps `fsmon` binary invocations and returns parsed output.

#![allow(dead_code)]

use fsmon::common::FileEvent;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Resolve the path to the `fsmon` binary in the Cargo target directory.
/// Works for both `cargo test` (debug) and `cargo test --release`.
pub fn fsmon_binary() -> PathBuf {
    let current_exe = std::env::current_exe().expect("current_exe");
    // current_exe is roughly target/{debug|release}/deps/test-binary-xxx
    let target_dir = current_exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir");
    target_dir.join("fsmon")
}

/// Run `fsmon` with the given arguments and return the full Output.
pub fn run_fsmon(args: &[&str]) -> Output {
    Command::new(fsmon_binary())
        .args(args)
        .output()
        .expect("failed to run fsmon binary")
}

/// Run `fsmon` and panic if it fails. Returns stdout as String.
pub fn run_fsmon_success(args: &[&str]) -> String {
    let out = run_fsmon(args);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "fsmon {:?} failed (status {}):\n{}",
            args, out.status, stderr
        );
    }
    String::from_utf8(out.stdout).expect("valid utf8 stdout")
}

/// Run `fsmon` and return stdout + stderr combined on failure (useful for
/// tests that expect non-zero exit).
pub fn run_fsmon_raw(args: &[&str]) -> (bool, String, String) {
    let out = run_fsmon(args);
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap_or_default(),
        String::from_utf8(out.stderr).unwrap_or_default(),
    )
}

/// Parse `fsmon monitored` output (human-readable format) into Vec<(process, path)>.
pub fn parse_monitored_output(stdout: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut current_process = None;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(stripped) = trimmed.strip_prefix("Process: ") {
            current_process = Some(stripped.to_string());
        } else if trimmed.starts_with('/')
            && let Some(ref process) = current_process
        {
            // Extract path (remove optional details in parentheses)
            let path_end = trimmed.find(" (").unwrap_or(trimmed.len());
            let path = trimmed[..path_end].to_string();
            result.push((process.clone(), path));
        }
    }

    result
}

/// Parse `fsmon query` / `fsmon changes` output (JSONL FileEvent lines).
pub fn parse_query_output(stdout: &str) -> Vec<FileEvent> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSONL in query output"))
        .collect()
}
