//! Proc Connector Process Cache + Process Tree
//!
//! Delegates to `proc-tree` crate for core process tree logic.
//! Provides moka-based storage adapters and proc-connector integration.

use std::time::Duration;

use moka::sync::Cache;
use proc_connector::{NetlinkMessageIter, ProcConnector, ProcEvent as PcEvent};

// Re-export proc-tree public types
pub use proc_tree::{PidNode, ProcEvent, ProcInfo, ProcessLink, read_proc_start_time_ns};

// ---- Moka-backed trait adapters ----

/// Adapter: moka `Cache<u32, PidNode>` → `TreeStore`.
struct TreeRef<'a>(&'a Cache<u32, PidNode>);

impl proc_tree::TreeStore for TreeRef<'_> {
    fn get_node(&self, pid: u32) -> Option<PidNode> {
        self.0.get(&pid)
    }
    fn insert_node(&self, pid: u32, node: PidNode) {
        self.0.insert(pid, node);
    }
    fn all_pids(&self) -> Vec<u32> {
        self.0.iter().map(|(k, _)| *k).collect()
    }
}

/// Adapter: moka `Cache<u32, ProcInfo>` → `CacheStore`.
struct CacheRef<'a>(&'a Cache<u32, ProcInfo>);

impl proc_tree::CacheStore for CacheRef<'_> {
    fn get_info(&self, pid: u32) -> Option<ProcInfo> {
        self.0.get(&pid)
    }
    fn insert_info(&self, pid: u32, info: ProcInfo) {
        self.0.insert(pid, info);
    }
}

// ---- Type aliases (backward compat) ----

/// Shared PID → ProcInfo cache (thread-safe, bounded, TTL-based eviction).
pub type ProcCache = Cache<u32, ProcInfo>;

/// Shared process tree: PID → parent PID + cmd (bounded, TTL-based eviction).
pub type PidTree = Cache<u32, PidNode>;

// ---- Constants ----

/// Capacity for process info cache.
pub const PROC_CACHE_CAP: u64 = 65536;

/// TTL for process info entries. Exited processes are evicted after this time.
pub const PROC_CACHE_TTL_SECS: u64 = 600;

/// Capacity for process tree cache.
pub const PID_TREE_CAP: u64 = 65536;

/// TTL for process tree entries.
pub const PID_TREE_TTL_SECS: u64 = 600;

/// Parameters for process caches (ProcCache and PidTree).
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

/// Create a ProcCache with explicit capacity and TTL overrides.
pub fn new_cache_with(params: CacheParams) -> ProcCache {
    Cache::builder()
        .max_capacity(params.capacity)
        .time_to_live(Duration::from_secs(params.ttl_secs))
        .build()
}

/// Create a PidTree with explicit capacity and TTL overrides.
pub fn new_pid_tree_with(params: CacheParams) -> PidTree {
    Cache::builder()
        .max_capacity(params.capacity)
        .time_to_live(Duration::from_secs(params.ttl_secs))
        .build()
}

/// Snapshot all existing processes from /proc on daemon start.
pub fn snapshot_process_tree(tree: &PidTree, cache: &ProcCache) {
    proc_tree::snapshot(&TreeRef(tree), &CacheRef(cache));
}

/// Check if `pid` is a descendant of any process whose cmd == `target_cmd`.
pub fn is_descendant(tree: &PidTree, pid: u32, target_cmd: &str) -> bool {
    proc_tree::is_descendant(&TreeRef(tree), pid, target_cmd)
}

/// Build a chain string from the process tree.
/// Format: "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
pub fn build_chain(tree: &PidTree, cache: &ProcCache, pid: u32) -> String {
    proc_tree::build_chain_string(&TreeRef(tree), &CacheRef(cache), pid)
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
    proc_tree::handle_events(&TreeRef(tree), &CacheRef(cache), &events);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_cache_insert_and_get() {
        let cache = new_cache_with(CacheParams::default());
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
        let tree = new_pid_tree_with(CacheParams::default());
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
        let tree = new_pid_tree_with(CacheParams::default());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        assert!(!is_descendant(&tree, 99999, "systemd"));
    }

    #[test]
    fn test_is_descendant_cycle() {
        let tree = new_pid_tree_with(CacheParams::default());
        tree.insert(1, PidNode { ppid: 2, cmd: "a".into() });
        tree.insert(2, PidNode { ppid: 3, cmd: "b".into() });
        tree.insert(3, PidNode { ppid: 1, cmd: "c".into() });
        assert!(!is_descendant(&tree, 1, "nginx"));
    }

    #[test]
    fn test_build_chain_cycle() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert(1, PidNode { ppid: 2, cmd: "a".into() });
        tree.insert(2, PidNode { ppid: 3, cmd: "b".into() });
        tree.insert(3, PidNode { ppid: 1, cmd: "c".into() });
        cache.insert(1, ProcInfo { cmd: "a".into(), user: "u".into(), ppid: 2, tgid: 1, start_time_ns: 0 });
        cache.insert(2, ProcInfo { cmd: "b".into(), user: "u".into(), ppid: 3, tgid: 2, start_time_ns: 0 });
        cache.insert(3, ProcInfo { cmd: "c".into(), user: "u".into(), ppid: 1, tgid: 3, start_time_ns: 0 });
        let chain = build_chain(&tree, &cache, 1);
        assert!(!chain.is_empty());
        assert!(chain.starts_with("1|"));
    }

    #[test]
    fn test_build_chain_from_tree() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1, start_time_ns: 0 });
        tree.insert(100, PidNode { ppid: 1, cmd: "openclaw".into() });
        cache.insert(100, ProcInfo { cmd: "openclaw".into(), user: "root".into(), ppid: 1, tgid: 100, start_time_ns: 0 });
        tree.insert(101, PidNode { ppid: 100, cmd: "sh".into() });
        cache.insert(101, ProcInfo { cmd: "sh".into(), user: "root".into(), ppid: 100, tgid: 101, start_time_ns: 0 });
        tree.insert(102, PidNode { ppid: 101, cmd: "touch".into() });
        cache.insert(102, ProcInfo { cmd: "touch".into(), user: "root".into(), ppid: 101, tgid: 102, start_time_ns: 0 });

        let chain = build_chain(&tree, &cache, 102);
        assert_eq!(chain, "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root");
    }

    #[test]
    fn test_build_chain_single() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        tree.insert(1, PidNode { ppid: 0, cmd: "systemd".into() });
        cache.insert(1, ProcInfo { cmd: "systemd".into(), user: "root".into(), ppid: 0, tgid: 1, start_time_ns: 0 });

        let chain = build_chain(&tree, &cache, 1);
        assert_eq!(chain, "1|systemd|root");
    }

    #[test]
    fn test_snapshot_pid1() {
        let tree = new_pid_tree_with(CacheParams::default());
        let cache = new_cache_with(CacheParams::default());
        snapshot_process_tree(&tree, &cache);
        assert!(tree.contains_key(&1), "PID 1 should exist after snapshot");
        if let Some(node) = tree.get(&1) {
            assert!(!node.cmd.is_empty(), "PID 1 should have a cmd");
            assert_eq!(node.ppid, 0, "PID 1's ppid should be 0");
        }
        assert!(cache.contains_key(&1), "PID 1 should exist in proc cache after snapshot");
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
