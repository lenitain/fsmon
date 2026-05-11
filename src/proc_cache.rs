//! Proc Connector Process Cache
//!
//! Listens to process exec events via Linux netlink proc connector,
//! caches PID -> (cmd, user) mapping immediately when process executes.
//! Solves the problem where short-lived processes (touch, rm, mv etc.)
//! cause /proc/{pid} to be unreadable when fanotify events arrive.
//!
//! Uses the safe `proc-connector` crate — no raw `libc` netlink FFI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use proc_connector::{ProcConnector, ProcEvent};

use crate::utils::uid_to_username;

// ---- Public Types ----

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub cmd: String,
    pub user: String,
}

pub type ProcCache = Arc<DashMap<u32, ProcInfo>>;

/// Start proc connector listener thread, returns shared cache and a readiness flag.
/// The flag is set to `true` once netlink subscription succeeds, so callers can
/// avoid a fixed sleep and instead poll the flag with a timeout.
pub fn start_proc_listener() -> (ProcCache, Arc<AtomicBool>) {
    let cache: ProcCache = Arc::new(DashMap::new());
    let cache_clone = cache.clone();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();

    std::thread::Builder::new()
        .name("proc-connector".into())
        .spawn(move || {
            if let Err(e) = run_listener(cache_clone, ready_clone) {
                eprintln!("proc connector listener failed: {}", e);
            }
        })
        .ok();

    (cache, ready)
}

// ---- Internal Implementation ----

fn run_listener(cache: ProcCache, ready: Arc<AtomicBool>) -> anyhow::Result<()> {
    // ProcConnector::new() handles socket creation, bind, and subscribe — all safe
    let conn = ProcConnector::new()
        .map_err(|e| anyhow::anyhow!("ProcConnector::new: {}", e))?;

    // Signal readiness: subscription done, safe to process fanotify events
    ready.store(true, Ordering::Release);

    // Receive loop with timeout (1s). Short timeout so thread can be joinable
    // and doesn't busy-loop.
    let mut buf = vec![0u8; 4096];
    loop {
        match conn.recv_timeout(&mut buf, Duration::from_secs(1)) {
            Ok(Some(ProcEvent::Exec { pid, .. })) => {
                // Process just exec()'d, /proc/{pid} must exist
                let cmd = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let user = read_proc_uid(pid).unwrap_or_else(|| "unknown".to_string());

                cache.insert(pid, ProcInfo { cmd, user });
            }
            Ok(Some(_)) => {
                // Non-Exec event (Fork, Exit, Uid…), ignore — we only cache Exec
            }
            Ok(None) => {
                // Timeout — loop back, check for shutdown
            }
            Err(proc_connector::Error::Interrupted) => {
                // Signal interrupted, retry
                continue;
            }
            Err(proc_connector::Error::Overrun) => {
                eprintln!("[WARNING] proc connector overrun — some exec events may have been lost");
            }
            Err(e) => {
                eprintln!("proc connector recv error: {}", e);
                // Non-recoverable error, exit
                break;
            }
        }
    }

    Ok(())
}

fn read_proc_uid(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let uid: u32 = status
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    uid_to_username(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_cache_insert_and_get() {
        let cache: ProcCache = Arc::new(DashMap::new());
        cache.insert(
            12345,
            ProcInfo {
                cmd: "test_process".to_string(),
                user: "testuser".to_string(),
            },
        );

        let info = cache.get(&12345);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.cmd, "test_process");
        assert_eq!(info.user, "testuser");
    }

    #[test]
    fn test_proc_cache_missing_pid() {
        let cache: ProcCache = Arc::new(DashMap::new());
        assert!(cache.get(&99999).is_none());
    }

    #[test]
    fn test_proc_cache_overwrite() {
        let cache: ProcCache = Arc::new(DashMap::new());
        cache.insert(
            1,
            ProcInfo {
                cmd: "old".into(),
                user: "a".into(),
            },
        );
        cache.insert(
            1,
            ProcInfo {
                cmd: "new".into(),
                user: "b".into(),
            },
        );

        let info = cache.get(&1).unwrap();
        assert_eq!(info.cmd, "new");
        assert_eq!(info.user, "b");
    }

    #[test]
    fn test_proc_cache_concurrent_access() {
        use std::thread;

        let cache: ProcCache = Arc::new(DashMap::new());
        let mut handles = vec![];

        for i in 0..10 {
            let cache_clone = cache.clone();
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    let pid = (i * 100 + j) as u32;
                    cache_clone.insert(
                        pid,
                        ProcInfo {
                            cmd: format!("proc_{}", pid),
                            user: "test".into(),
                        },
                    );
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(cache.len(), 1000);
    }

    // ---- Integration tests (require sudo) ----

    #[test]
    #[ignore]
    fn test_proc_connector_create() {
        let conn = ProcConnector::new();
        assert!(conn.is_ok(), "Should be able to create ProcConnector with root");
        // Dropped → unsubscribe + close automatically
    }

    #[test]
    #[ignore]
    fn test_proc_listener_receives_events() {
        let (cache, _ready) = start_proc_listener();

        // Spawn a short-lived process that will trigger PROC_EVENT_EXEC
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Create a subprocess
        let mut child = std::process::Command::new("echo")
            .arg("test")
            .spawn()
            .unwrap();
        child.wait().unwrap();

        // Wait for event to be cached
        std::thread::sleep(std::time::Duration::from_millis(200));

        // The proc connector should have captured the exec event for our process
        // Note: due to timing, this might not always capture the exact pid,
        // but it should have captured some events
        assert!(
            !cache.is_empty(),
            "Proc cache should have received some events"
        );
    }
}
