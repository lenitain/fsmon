//! Proc Connector Process Cache + Process Tree
//!
//! Two data structures:
//! - **ProcCache**: PID → {cmd, user, ppid, tgid} from Exec events (existing)
//! - **PidTree**: PID → {ppid, cmd} for ancestor lookups (new, handles Fork/Exec/Exit)
//!
//! The PidTree is populated from three sources:
//! 1. Startup snapshot: `/proc/*/status` → seed existing processes
//! 2. Fork events: parent→child relationship (no cmd yet)
//! 3. Exec events: update cmd for the child

use std::time::Duration;

use moka::sync::Cache;
use proc_connector::{NetlinkMessageIter, ProcConnector, ProcEvent};

use crate::utils::uid_to_username;

// ---- ProcCache (existing) ----

/// Cached process info: command name, user, ppid, tgid.
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub cmd: String,
    pub user: String,
    pub ppid: u32,
    pub tgid: u32,
    pub start_time_ns: u64,
}

/// Capacity for process info cache.
/// Covers typical active PID ranges with headroom.
const PROC_CACHE_CAP: u64 = 65536;

/// TTL for process info entries. Exited processes are evicted after this time.
const PROC_CACHE_TTL_SECS: u64 = 600;

/// Capacity for process tree cache.
const PID_TREE_CAP: u64 = 65536;

/// TTL for process tree entries.
const PID_TREE_TTL_SECS: u64 = 600;

/// Shared PID → ProcInfo cache (thread-safe, bounded, TTL-based eviction).
pub type ProcCache = Cache<u32, ProcInfo>;

pub fn new_cache() -> ProcCache {
    Cache::builder()
        .max_capacity(PROC_CACHE_CAP)
        .time_to_live(Duration::from_secs(PROC_CACHE_TTL_SECS))
        .build()
}

// ---- PidTree ----

/// A node in the process tree. cmd starts empty (from Fork) and fills on Exec.
#[derive(Clone, Debug)]
pub struct PidNode {
    pub ppid: u32,
    pub cmd: String,
    pub start_time_ns: u64,
}

/// Shared process tree: PID → parent PID + cmd (bounded, TTL-based eviction).
pub type PidTree = Cache<u32, PidNode>;

pub fn new_pid_tree() -> PidTree {
    Cache::builder()
        .max_capacity(PID_TREE_CAP)
        .time_to_live(Duration::from_secs(PID_TREE_TTL_SECS))
        .build()
}

/// Snapshot all existing processes from /proc on daemon start.
/// Reads `/proc/*/status` to seed the tree with current PIDs and their ppid/cmd.
pub fn snapshot_process_tree(tree: &PidTree) {
    let dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[WARNING] Cannot read /proc for process tree snapshot: {e}");
            return;
        }
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let status = match std::fs::read_to_string(format!("/proc/{}/status", pid)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut ppid = 0u32;
        let mut cmd = String::new();
        for line in status.lines() {
            if let Some(val) = line.strip_prefix("PPid:") {
                ppid = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = line.strip_prefix("Name:") {
                cmd = val.trim().to_string();
            }
        }
        tree.insert(pid, PidNode { ppid, cmd, start_time_ns: 0 });
    }
}

pub fn read_proc_start_time_ns(pid: u32) -> u64 {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let after_comm = match stat.rfind(") ") {
        Some(pos) => pos + 2,
        None => return 0,
    };
    let mut rest = &stat[after_comm..];
    for _ in 0..19 {
        if let Some(pos) = rest.find(' ') {
            rest = &rest[pos + 1..];
        } else {
            return 0;
        }
    }
    let starttime_jiffies: u64 = match rest.split_whitespace().next() {
        Some(s) => s.parse().unwrap_or(0),
        None => return 0,
    };
    if starttime_jiffies == 0 {
        return 0;
    }
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return 0;
    }
    (starttime_jiffies as u128 * 1_000_000_000 / clk_tck as u128) as u64
}

/// Check if `pid` is a descendant of any process whose cmd == `target_cmd`.
/// Walks up the tree via ppid until hitting root (pid=1, pid=0, self-loop, or cycle).
pub fn is_descendant(tree: &PidTree, pid: u32, target_cmd: &str) -> bool {
    let mut current = pid;
    let mut visited = std::collections::HashSet::new();
    while let Some(node) = tree.get(&current) {
        if !visited.insert(current) {
            break; // cycle detected
        }
        if node.cmd == target_cmd {
            return true;
        }
        if node.ppid == 0 || current == node.ppid {
            break;
        }
        current = node.ppid;
    }
    false
}

/// Build a chain string from the process tree.
/// Format: "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
/// Falls back to reading /proc if a PID is not in the tree.
pub fn build_chain(tree: &PidTree, cache: &ProcCache, pid: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = pid;
    let mut visited = std::collections::HashSet::new();
    loop {
        // Try tree first for ppid, then cache for user
        let (ppid, cmd, user) = if let Some(node) = tree.get(&current) {
            let user = cache
                .get(&current)
                .map(|info| info.user.clone())
                .unwrap_or_else(|| "unknown".to_string());
            (node.ppid, node.cmd.clone(), user)
        } else {
            // Fallback to /proc/{pid}/status
            let status = match std::fs::read_to_string(format!("/proc/{}/status", current)) {
                Ok(s) => s,
                Err(_) => {
                    parts.push(format!("{}|unknown|unknown", current));
                    break;
                }
            };
            let cmd = status
                .lines()
                .find(|l| l.starts_with("Name:"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let ppid = status
                .lines()
                .find(|l| l.starts_with("PPid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let user = status
                .lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|uid_str| uid_str.parse::<u32>().ok())
                .and_then(uid_to_username)
                .unwrap_or_else(|| "unknown".to_string());
            (ppid, cmd, user)
        };

        parts.push(format!("{}|{}|{}", current, cmd, user));
        if ppid == 0 || current == ppid {
            break;
        }
        if !visited.insert(current) {
            break; // cycle detected
        }
        current = ppid;
    }
    parts.join(";")
}

// ---- Proc Connector ----

pub fn try_create_connector() -> Option<ProcConnector> {
    let conn = match ProcConnector::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[WARNING] Failed to create proc connector: {e}. \
                       Process tree tracking will be unavailable."
            );
            return None;
        }
    };
    if let Err(e) = conn.set_nonblocking() {
        eprintln!("[WARNING] Failed to set proc connector non-blocking: {e}");
        return None;
    }
    Some(conn)
}

/// Process proc connector events.
/// Handles Exec (update ProcCache + PidTree cmd), Fork (insert PidTree),
/// and Exit (optional, no cleanup needed for correct lookups).
pub fn handle_proc_events(cache: &ProcCache, tree: &PidTree, data: &[u8], n: usize) -> bool {
    let mut processed = false;
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(ProcEvent::Exec { pid, timestamp_ns, .. })) => {
                let cmd = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let (user, ppid, tgid) =
                    read_proc_info(pid).unwrap_or_else(|| ("unknown".to_string(), 0, 0));

                cache.insert(
                    pid,
                    ProcInfo {
                        cmd: cmd.clone(),
                        user,
                        ppid,
                        tgid,
                        start_time_ns: timestamp_ns,
                    },
                );

                // Also update PidTree with the resolved cmd/ppid
                tree.insert(
                    pid,
                    PidNode {
                        ppid,
                        cmd,
                        start_time_ns: timestamp_ns,
                    },
                );

                processed = true;
            }
            Ok(Some(ProcEvent::Fork {
                child_pid,
                parent_pid,
                timestamp_ns,
                ..
            })) => {
                // Pre-populate tree: we know the parent but not cmd yet
                tree.insert(
                    child_pid,
                    PidNode {
                        ppid: parent_pid,
                        cmd: String::new(),
                        start_time_ns: timestamp_ns,
                    },
                );
                processed = true;
            }
            Ok(Some(ProcEvent::Exit { .. })) => {
                // Keep the node — it's still valid for historical chain lookups
                // of events that happened before this process exited.
                processed = true;
            }
            Ok(Some(_)) => {} // Uid, Gid, Sid, etc. — ignore
            Ok(None) => {}    // Control message (NLMSG_NOOP, NLMSG_DONE, NLMSG_ERROR-ACK)
            Err(proc_connector::Error::Overrun) => {
                eprintln!("[WARNING] proc connector overrun — some exec events may have been lost");
            }
            Err(proc_connector::Error::Truncated) => {
                eprintln!("[WARNING] proc connector truncated message, continuing...");
            }
            Err(e) => {
                eprintln!("proc connector parse error: {e}");
            }
        }
    }
    processed
}

fn read_proc_info(pid: u32) -> Option<(String, u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let mut user = String::new();
    let mut ppid = 0u32;
    let mut tgid = 0u32;
    for line in status.lines() {
        if let Some(val) = line.strip_prefix("Uid:") {
            let uid: u32 = val.split_whitespace().next()?.parse().ok()?;
            user = uid_to_username(uid).unwrap_or_else(|| "unknown".to_string());
        } else if let Some(val) = line.strip_prefix("PPid:") {
            ppid = val.trim().parse().ok()?;
        } else if let Some(val) = line.strip_prefix("Tgid:") {
            tgid = val.trim().parse().ok()?;
        }
    }
    Some((user, ppid, tgid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_cache_insert_and_get() {
        let cache = new_cache();
        cache.insert(
            12345,
            ProcInfo {
                cmd: "test_process".into(),
                user: "testuser".into(),
                ppid: 1,
                tgid: 12345,
            start_time_ns: 0,
            },
        );
        let info = cache.get(&12345).unwrap();
        assert_eq!(info.cmd, "test_process");
        assert_eq!(info.ppid, 1);
        assert_eq!(info.tgid, 12345);
    }

    #[test]
    fn test_is_descendant() {
        let tree = new_pid_tree();
        tree.insert(
            1,
            PidNode {
                ppid: 0,
                cmd: "systemd".into(),
            start_time_ns: 0,
            },
        );
        tree.insert(
            100,
            PidNode {
                ppid: 1,
                cmd: "openclaw".into(),
            start_time_ns: 0,
            },
        );
        tree.insert(
            101,
            PidNode {
                ppid: 100,
                cmd: "sh".into(),
            start_time_ns: 0,
            },
        );
        tree.insert(
            102,
            PidNode {
                ppid: 101,
                cmd: String::new(),
            start_time_ns: 0,
            },
        ); // Fork, no Exec yet

        assert!(is_descendant(&tree, 102, "openclaw"));
        assert!(is_descendant(&tree, 101, "openclaw"));
        assert!(is_descendant(&tree, 100, "openclaw"));
        assert!(!is_descendant(&tree, 102, "nginx"));
        assert!(!is_descendant(&tree, 1, "openclaw"));
    }

    #[test]
    fn test_is_descendant_unknown_pid() {
        let tree = new_pid_tree();
        tree.insert(
            1,
            PidNode {
                ppid: 0,
                cmd: "systemd".into(),
            start_time_ns: 0,
            },
        );
        assert!(!is_descendant(&tree, 99999, "systemd"));
    }

    #[test]
    fn test_is_descendant_cycle() {
        // Complex cycle: A→B→C→A. is_descendant must not infinite-loop.
        let tree = new_pid_tree();
        tree.insert(1, PidNode { ppid: 2, cmd: "a".into(), start_time_ns: 0 });
        tree.insert(2, PidNode { ppid: 3, cmd: "b".into(), start_time_ns: 0 });
        tree.insert(3, PidNode { ppid: 1, cmd: "c".into(), start_time_ns: 0 });
        // Should detect cycle and return false (no matching cmd)
        assert!(!is_descendant(&tree, 1, "nginx"));
    }

    #[test]
    fn test_build_chain_cycle() {
        // Complex cycle: 1→2→3→1. build_chain must not infinite-loop.
        let tree = new_pid_tree();
        let cache = new_cache();
        tree.insert(1, PidNode { ppid: 2, cmd: "a".into(), start_time_ns: 0 });
        tree.insert(2, PidNode { ppid: 3, cmd: "b".into(), start_time_ns: 0 });
        tree.insert(3, PidNode { ppid: 1, cmd: "c".into(), start_time_ns: 0 });
        cache.insert(1, ProcInfo { cmd: "a".into(), user: "u".into(), ppid: 2, tgid: 1, start_time_ns: 0 });
        cache.insert(2, ProcInfo { cmd: "b".into(), user: "u".into(), ppid: 3, tgid: 2, start_time_ns: 0 });
        cache.insert(3, ProcInfo { cmd: "c".into(), user: "u".into(), ppid: 1, tgid: 3, start_time_ns: 0 });
        let chain = build_chain(&tree, &cache, 1);
        // Should produce partial chain without infinite loop
        assert!(!chain.is_empty());
        assert!(chain.starts_with("1|"));
    }

    #[test]
    fn test_build_chain_from_tree() {
        let tree = new_pid_tree();
        let cache = new_cache();
        tree.insert(
            1,
            PidNode {
                ppid: 0,
                cmd: "systemd".into(),
            start_time_ns: 0,
            },
        );
        cache.insert(
            1,
            ProcInfo {
                cmd: "systemd".into(),
                user: "root".into(),
                ppid: 0,
                tgid: 1,
            start_time_ns: 0,
            },
        );
        tree.insert(
            100,
            PidNode {
                ppid: 1,
                cmd: "openclaw".into(),
            start_time_ns: 0,
            },
        );
        cache.insert(
            100,
            ProcInfo {
                cmd: "openclaw".into(),
                user: "root".into(),
                ppid: 1,
                tgid: 100,
            start_time_ns: 0,
            },
        );
        tree.insert(
            101,
            PidNode {
                ppid: 100,
                cmd: "sh".into(),
            start_time_ns: 0,
            },
        );
        cache.insert(
            101,
            ProcInfo {
                cmd: "sh".into(),
                user: "root".into(),
                ppid: 100,
                tgid: 101,
            start_time_ns: 0,
            },
        );
        tree.insert(
            102,
            PidNode {
                ppid: 101,
                cmd: "touch".into(),
            start_time_ns: 0,
            },
        );
        cache.insert(
            102,
            ProcInfo {
                cmd: "touch".into(),
                user: "root".into(),
                ppid: 101,
                tgid: 102,
            start_time_ns: 0,
            },
        );

        let chain = build_chain(&tree, &cache, 102);
        assert_eq!(
            chain,
            "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
        );
    }

    #[test]
    fn test_build_chain_single() {
        let tree = new_pid_tree();
        let cache = new_cache();
        tree.insert(
            1,
            PidNode {
                ppid: 0,
                cmd: "systemd".into(),
            start_time_ns: 0,
            },
        );
        cache.insert(
            1,
            ProcInfo {
                cmd: "systemd".into(),
                user: "root".into(),
                ppid: 0,
                tgid: 1,
            start_time_ns: 0,
            },
        );

        let chain = build_chain(&tree, &cache, 1);
        assert_eq!(chain, "1|systemd|root");
    }

    #[test]
    fn test_snapshot_pid1() {
        // PID 1 always exists on Linux
        let tree = new_pid_tree();
        snapshot_process_tree(&tree);
        assert!(tree.contains_key(&1), "PID 1 should exist after snapshot");
        if let Some(node) = tree.get(&1) {
            assert!(!node.cmd.is_empty(), "PID 1 should have a cmd");
            assert_eq!(node.ppid, 0, "PID 1\'s ppid should be 0");
        }
    }

    #[test]
    fn test_read_proc_start_time_ns_pid1() {
        // PID 1 always exists on Linux — should have a non-zero start time.
        let ns = read_proc_start_time_ns(1);
        assert!(ns > 0, "PID 1 start_time_ns should be > 0, got {ns}");
    }

    #[test]
    fn test_read_proc_start_time_ns_nonexistent() {
        // A non-existent PID should return 0.
        // Use PID 0x7FFFFFFF (max valid PID on most systems is lower).
        let ns = read_proc_start_time_ns(0x7FFFFFFF);
        assert_eq!(ns, 0, "non-existent PID should return 0, got {ns}");
    }
}
