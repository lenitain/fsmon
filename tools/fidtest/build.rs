//! Detect which fanotify-fid version this harness was resolved against.
//!
//! The harness is deliberately a single source file that demonstrates both the
//! old and the new behaviour: against 0.7.0 it shows that info record types
//! 4/5/6/7/10/12 are unreachable from the public API, and against 0.7.1 or
//! later it shows the parsed values.  Those code paths cannot coexist in one
//! compilation (0.7.0 has no `pidfd()` to call), so the dependency's *resolved*
//! version decides which one is compiled.
//!
//! Reading `Cargo.lock` rather than parsing the requirement string means a path
//! dependency on a local checkout is handled correctly too.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Extract the version of `fanotify-fid` recorded in a `Cargo.lock`.
fn resolved_version(lock: &str) -> Option<String> {
    let mut in_package = false;
    for line in lock.lines() {
        let line = line.trim();
        if line.starts_with("[[package]]") {
            in_package = false;
        } else if line == "name = \"fanotify-fid\"" {
            in_package = true;
        } else if in_package && line.starts_with("version = ") {
            return line
                .trim_start_matches("version = ")
                .trim_matches('"')
                .to_string()
                .into();
        }
    }
    None
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(fanotify_fid_has_records)");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lock_path = manifest_dir.join("Cargo.lock");

    // Info record types 4/5/6/7/10/12 became reachable in 0.7.1 — the release
    // that made the parser honour records it previously dropped.  The releases
    // before it are known exactly, so matching them by name is enough and
    // nothing else has to be parsed.
    //
    // Deliberately NOT phrased as ">= 0.7.1": a checkout of this repository can
    // report any version, and a version we cannot read should not silently
    // select the stale branch.
    const PREDATES_RECORDS: [&str; 3] = ["0.7.0", "0.6.0", "0.5.0"];

    let has_records = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|lock| resolved_version(&lock))
        .is_none_or(|v| !PREDATES_RECORDS.contains(&v.as_str()));

    if has_records {
        println!("cargo:rustc-cfg=fanotify_fid_has_records");
    }
}
