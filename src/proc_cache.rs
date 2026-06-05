//! Proc Connector Process Cache + Process Tree
//!
//! Delegates to `proc-tree` crate for core process tree logic.
//! Uses `DefaultTree`/`DefaultCache` (HashMap + Mutex + TTL) as storage.

use proc_connector::{NetlinkMessageIter, ProcConnector, ProcEvent as PcEvent};

// Re-export proc-tree public types
pub use proc_tree::{
    DefaultCache, DefaultTree, PidNode, ProcEvent, ProcInfo, ProcessLink,
    read_proc_start_time_ns,
};

// ---- Type aliases ----

/// Shared process tree: PID → parent PID + cmd.
pub type PidTree = DefaultTree;

/// Shared PID → ProcInfo cache.
pub type ProcCache = DefaultCache;

// ---- Constants ----

pub const PROC_CACHE_CAP: u64 = 65536;
pub const PROC_CACHE_TTL_SECS: u64 = 600;
pub const PID_TREE_CAP: u64 = 65536;
pub const PID_TREE_TTL_SECS: u64 = 600;

pub struct CacheParams {
    pub capacity: u64,
    pub ttl_secs: u64,
}

impl Default for CacheParams {
    fn default() -> Self {
        Self {
            capacity: PROC_CACHE_CAP,
            ttl_secs: PROC_CACHE_TTL_SECS,
        }
    }
}

pub fn new_cache_with(params: CacheParams) -> ProcCache {
    ProcCache::new(params.capacity, params.ttl_secs)
}

pub fn new_pid_tree_with(params: CacheParams) -> PidTree {
    PidTree::new(params.capacity, params.ttl_secs)
}

pub fn snapshot_process_tree(tree: &PidTree, cache: &ProcCache) {
    proc_tree::snapshot(tree, cache);
}

pub fn is_descendant(tree: &PidTree, pid: u32, target_cmd: &str) -> bool {
    proc_tree::is_descendant(tree, pid, target_cmd)
}

pub fn build_chain(tree: &PidTree, cache: &ProcCache, pid: u32) -> String {
    proc_tree::build_chain_string(tree, cache, pid)
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
/// Converts raw proc-connector events to proc-tree events and delegates.
pub fn handle_proc_events(cache: &ProcCache, tree: &PidTree, data: &[u8], n: usize) -> bool {
    let mut events: Vec<ProcEvent> = Vec::new();
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(PcEvent::Exec {
                pid, timestamp_ns, ..
            })) => {
                events.push(ProcEvent::Exec { pid, timestamp_ns });
            }
            Ok(Some(PcEvent::Fork {
                child_pid,
                parent_pid,
                timestamp_ns,
                ..
            })) => {
                events.push(ProcEvent::Fork {
                    child_pid,
                    parent_pid,
                    timestamp_ns,
                });
            }
            Ok(Some(PcEvent::Exit { pid, .. })) => {
                events.push(ProcEvent::Exit { pid });
            }
            Ok(Some(_)) => {} // Uid, Gid, Sid, etc. — ignore
            Ok(None) => {}    // Control message
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
    if events.is_empty() {
        return false;
    }
    proc_tree::handle_events(tree, cache, &events);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_tree::{CacheStore, TreeStore};

    #[test]
    fn test_proc_cache_insert_and_get() {
        let cache = new_cache_with(CacheParams::default());
        cache.insert_info(
            12345,
            ProcInfo {
                cmd: "test_process".into(),
                user: "testuser".into(),
                ppid: 1,
                tgid: 12345,
                start_time_ns: 0,
            },
        );
        let info = cache.get_info(12345).unwrap();
        assert_eq!(info.cmd, "test_process");
        assert_eq!(info.ppid, 1);
        assert_eq!(info.tgid, 12345);
    }

    #[test]
    fn test_is_descendant() {
        let tree = new_pid_tree_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 0, cmd: "systemd".into() });
        tree.insert_node(100, PidNode { ppid: 1, cmd: "openclaw".into() });
        tree.insert_node(101, PidNode { ppid: 100, cmd: "sh".into() });
        tree.insert_node(102, PidNode { ppid: 101, cmd: String::new() });

        assert!(is_descendant(&tree, 102, "openclaw"));
        assert!(is_descendant(&tree, 101, "openclaw"));
        assert!(is_descendant(&tree, 100, "openclaw"));
        assert!(!is_descendant(&tree, 102, "nginx"));
        assert!(!is_descendant(&tree, 1, "openclaw"));
    }

    #[test]
    fn test_is_descendant_unknown_pid() {
        let tree = new_pid_tree_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 0, cmd: "systemd".into() });
        assert!(!is_descendant(&tree, 99999, "systemd"));
    }

    #[test]
    fn test_is_descendant_cycle() {
        let tree = new_pid_tree_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 2, cmd: "a".into() });
        tree.insert_node(2, PidNode { ppid: 3, cmd: "b".into() });
        tree.insert_node(3, PidNode { ppid: 1, cmd: "c".into() });
        assert!(!is_descendant(&tree, 1, "nginx"));
    }

    #[test]
    fn test_build_chain_cycle() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 2, cmd: "a".into() });
        tree.insert_node(2, PidNode { ppid: 3, cmd: "b".into() });
        tree.insert_node(3, PidNode { ppid: 1, cmd: "c".into() });
        cache.insert_info(1, ProcInfo { cmd: "a".into(), user: "u".into(), ppid: 2, tgid: 1, start_time_ns: 0 });
        cache.insert_info(2, ProcInfo { cmd: "b".into(), user: "u".into(), ppid: 3, tgid: 2, start_time_ns: 0 });
        cache.insert_info(3, ProcInfo { cmd: "c".into(), user: "u".into(), ppid: 1, tgid: 3, start_time_ns: 0 });
        let chain = build_chain(&tree, &cache, 1);
        assert!(!chain.is_empty());
        assert!(chain.starts_with("1|"));
    }

    #[test]
    fn test_build_chain_from_tree() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert_info(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1, start_time_ns: 0 });
        tree.insert_node(100, PidNode { ppid: 1, cmd: "openclaw".into() });
        cache.insert_info(100, ProcInfo { cmd: "openclaw".into(), user: "root".into(), ppid: 1, tgid: 100, start_time_ns: 0 });
        tree.insert_node(101, PidNode { ppid: 100, cmd: "sh".into() });
        cache.insert_info(101, ProcInfo { cmd: "sh".into(), user: "root".into(), ppid: 100, tgid: 101, start_time_ns: 0 });
        tree.insert_node(102, PidNode { ppid: 101, cmd: "touch".into() });
        cache.insert_info(102, ProcInfo { cmd: "touch".into(), user: "root".into(), ppid: 101, tgid: 102, start_time_ns: 0 });

        let chain = build_chain(&tree, &cache, 102);
        assert_eq!(chain, "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root");
    }

    #[test]
    fn test_build_chain_single() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert_node(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert_info(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1, start_time_ns: 0 });

        let chain = build_chain(&tree, &cache, 1);
        assert_eq!(chain, "1|systemd|root");
    }

    #[test]
    fn test_snapshot_pid1() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        snapshot_process_tree(&tree, &cache);
        assert!(tree.contains_key(1), "PID 1 should exist after snapshot");
        if let Some(node) = tree.get_node(1) {
            assert!(!node.cmd.is_empty(), "PID 1 should have a cmd");
            assert_eq!(node.ppid, 0, "PID 1's ppid should be 0");
        }
        assert!(cache.contains_key(1), "PID 1 should exist in proc cache after snapshot");
    }

    #[test]
    fn test_read_proc_start_time_ns_pid1() {
        let ns = read_proc_start_time_ns(1);
        assert!(ns > 0, "PID 1 start_time_ns should be > 0, got {ns}");
    }

    #[test]
    fn test_read_proc_start_time_ns_nonexistent() {
        let ns = read_proc_start_time_ns(0x7FFFFFFF);
        assert_eq!(ns, 0, "non-existent PID should return 0, got {ns}");
    }
}
