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

use std::sync::Arc;

use dashmap::DashMap;
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
}

/// Shared PID → ProcInfo cache (thread-safe).
pub type ProcCache = Arc<DashMap<u32, ProcInfo>>;

pub fn new_cache() -> ProcCache {
    Arc::new(DashMap::new())
}

// ---- PidTree (new) ----

/// A node in the process tree. cmd starts empty (from Fork) and fills on Exec.
#[derive(Clone, Debug)]
pub struct PidNode {
    pub ppid: u32,
    pub cmd: String,
}

/// Shared process tree: PID → parent PID + cmd.
pub type PidTree = Arc<DashMap<u32, PidNode>>;

pub fn new_pid_tree() -> PidTree {
    Arc::new(DashMap::new())
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
        tree.insert(pid, PidNode { ppid, cmd });
    }
}

/// Check if `pid` is a descendant of any process whose cmd == `target_cmd`.
/// Walks up the tree via ppid until hitting root (pid=1, pid=0, or self-loop).
pub fn is_descendant(tree: &PidTree, pid: u32, target_cmd: &str) -> bool {
    let mut current = pid;
    loop {
        if let Some(node) = tree.get(&current) {
            if node.cmd == target_cmd {
                return true;
            }
            if node.ppid == 0 || current == node.ppid {
                break;
            }
            current = node.ppid;
        } else {
            break;
        }
    }
    false
}

/// Build a chain string from the process tree.
/// Format: "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
/// Falls back to reading /proc if a PID is not in the tree.
pub fn build_chain(tree: &PidTree, cache: &ProcCache, pid: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = pid;
    loop {
        // Try tree first for ppid, then cache for user
        let (ppid, cmd, user) = if let Some(node) = tree.get(&current) {
            let user = cache.get(&current)
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
            let cmd = status.lines()
                .find(|l| l.starts_with("Name:"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let ppid = status.lines()
                .find(|l| l.starts_with("PPid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let user = status.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|uid_str| uid_str.parse::<u32>().ok())
                .and_then(|uid| uid_to_username(uid))
                .unwrap_or_else(|| "unknown".to_string());
            (ppid, cmd, user)
        };

        parts.push(format!("{}|{}|{}", current, cmd, user));
        if ppid == 0 || current == ppid {
            break;
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
            eprintln!("[WARNING] Failed to create proc connector: {e}. \
                       Process tree tracking will be unavailable.");
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
pub fn handle_proc_events(
    cache: &ProcCache,
    tree: &PidTree,
    data: &[u8],
    n: usize,
) -> bool {
    let mut processed = false;
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(ProcEvent::Exec { pid, .. })) => {
                let cmd = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let (user, ppid, tgid) = read_proc_info(pid)
                    .unwrap_or_else(|| ("unknown".to_string(), 0, 0));

                cache.insert(pid, ProcInfo { cmd: cmd.clone(), user, ppid, tgid });

                // Also update PidTree with the resolved cmd/ppid
                tree.insert(pid, PidNode { ppid, cmd });

                processed = true;
            }
            Ok(Some(ProcEvent::Fork { child_pid, parent_pid, .. })) => {
                // Pre-populate tree: we know the parent but not cmd yet
                tree.insert(child_pid, PidNode { ppid: parent_pid, cmd: String::new() });
                processed = true;
            }
            Ok(Some(ProcEvent::Exit { .. })) => {
                // Keep the node — it's still valid for historical chain lookups
                // of events that happened before this process exited.
                processed = true;
            }
            Ok(Some(_)) => {} // Uid, Gid, Sid, etc. — ignore
            Ok(None) => {}     // Control message (NLMSG_NOOP, NLMSG_DONE, NLMSG_ERROR-ACK)
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
            let uid: u32 = val.split_whitespace().nth(0)?.parse().ok()?;
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
        let cache: ProcCache = Arc::new(DashMap::new());
        cache.insert(12345, ProcInfo {
            cmd: "test_process".into(), user: "testuser".into(), ppid: 1, tgid: 12345,
        });
        let info = cache.get(&12345).unwrap();
        assert_eq!(info.cmd, "test_process");
        assert_eq!(info.ppid, 1);
        assert_eq!(info.tgid, 12345);
    }

    #[test]
    fn test_is_descendant() {
        let tree: PidTree = Arc::new(DashMap::new());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        tree.insert(100, PidNode { ppid: 1, cmd: "openclaw".into() });
        tree.insert(101, PidNode { ppid: 100, cmd: "sh".into() });
        tree.insert(102, PidNode { ppid: 101, cmd: String::new() }); // Fork, no Exec yet

        assert!(is_descendant(&tree, 102, "openclaw"));
        assert!(is_descendant(&tree, 101, "openclaw"));
        assert!(is_descendant(&tree, 100, "openclaw"));
        assert!(!is_descendant(&tree, 102, "nginx"));
        assert!(!is_descendant(&tree, 1, "openclaw"));
    }

    #[test]
    fn test_is_descendant_unknown_pid() {
        let tree: PidTree = Arc::new(DashMap::new());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        assert!(!is_descendant(&tree, 99999, "systemd"));
    }

    #[test]
    fn test_build_chain_from_tree() {
        let tree: PidTree = Arc::new(DashMap::new());
        let cache: ProcCache = Arc::new(DashMap::new());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1 });
        tree.insert(100, PidNode { ppid: 1, cmd: "openclaw".into() });
        cache.insert(100, ProcInfo { cmd: "openclaw".into(), user: "root".into(), ppid: 1, tgid: 100 });
        tree.insert(101, PidNode { ppid: 100, cmd: "sh".into() });
        cache.insert(101, ProcInfo { cmd: "sh".into(), user: "root".into(), ppid: 100, tgid: 101 });
        tree.insert(102, PidNode { ppid: 101, cmd: "touch".into() });
        cache.insert(102, ProcInfo { cmd: "touch".into(), user: "root".into(), ppid: 101, tgid: 102 });

        let chain = build_chain(&tree, &cache, 102);
        assert_eq!(chain, "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root");
    }

    #[test]
    fn test_build_chain_single() {
        let tree: PidTree = Arc::new(DashMap::new());
        let cache: ProcCache = Arc::new(DashMap::new());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1 });

        let chain = build_chain(&tree, &cache, 1);
        assert_eq!(chain, "1|systemd|root");
    }

    #[test]
    fn test_snapshot_pid1() {
        // PID 1 always exists on Linux
        let tree: PidTree = Arc::new(DashMap::new());
        snapshot_process_tree(&tree);
        assert!(tree.contains_key(&1), "PID 1 should exist after snapshot");
        if let Some(node) = tree.get(&1) {
            assert!(!node.cmd.is_empty(), "PID 1 should have a cmd");
            assert_eq!(node.ppid, 0, "PID 1's ppid should be 0");
        }
    }
}
