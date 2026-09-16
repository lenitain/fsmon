//! Startup privilege validation integration tests.
//!
//! fsmon refuses to start without `CAP_SYS_ADMIN`, because the kernel would
//! then mark the fanotify group `FANOTIFY_UNPRIV` and blank `metadata.pid`
//! for every event caused by another process — silently destroying the
//! process attribution that is fsmon's whole purpose.
//!
//! That condition is permanent: no amount of restarting grants a missing
//! capability. The systemd unit therefore sets `RestartPreventExitStatus=2`
//! and `Restart=always`, so this failure MUST exit with code 2. Exiting 1
//! makes systemd retry `StartLimitBurst` times before giving up.
//!
//! These tests run the real binary. They are skipped when the test runner is
//! root, because then the daemon legitimately has the capability.

use std::process::Command;

/// Path to the compiled fsmon binary next to the test executable.
fn fsmon_bin() -> String {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // test binary name
    path.pop(); // deps/
    path.push("fsmon");
    path.to_string_lossy().to_string()
}

/// Run `fsmon daemon` with an isolated HOME/runtime dir so the singleton lock
/// and config of any real daemon cannot interfere.
fn run_daemon_isolated(
    home: &std::path::Path,
    runtime: &std::path::Path,
    allow_unprivileged: bool,
) -> std::process::Output {
    let mut cmd = Command::new(fsmon_bin());
    cmd.arg("daemon")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", runtime);
    if allow_unprivileged {
        cmd.env("FSMON_ALLOW_UNPRIVILEGED", "1");
    } else {
        cmd.env_remove("FSMON_ALLOW_UNPRIVILEGED");
    }
    cmd.output().expect("failed to execute fsmon daemon")
}

/// A missing capability is a permanent configuration error, not a transient
/// one. `RestartPreventExitStatus=2` in the generated unit depends on it.
#[test]
fn daemon_exits_2_when_cap_sys_admin_is_missing() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: running as root, the daemon legitimately has CAP_SYS_ADMIN");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let output = run_daemon_isolated(home.path(), runtime.path(), false);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CAP_SYS_ADMIN"),
        "expected the error to name the missing capability, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing capability can never be fixed by a restart, so the daemon \
         must exit 2 (RestartPreventExitStatus). stderr: {stderr}"
    );
}

/// The explicit opt-in must still work, so operators who accept the degraded
/// mode are not blocked by the guard.
#[test]
fn daemon_starts_in_degraded_mode_when_explicitly_allowed() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: running as root, this asserts the *unprivileged* path");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let mut child = Command::new(fsmon_bin())
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("FSMON_ALLOW_UNPRIVILEGED", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fsmon daemon");

    // Give it time to get past startup, then confirm it is still alive.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let alive = child.try_wait().expect("try_wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "daemon should keep running in degraded mode");
}
