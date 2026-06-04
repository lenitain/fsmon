use crate::proc_cache::{ProcCache, PidTree};

/// Manages process cache and process tree.
pub struct ProcManager {
    /// Process info cache (PID → ProcInfo)
    pub proc_cache: Option<ProcCache>,
    /// Process tree (PID → parent PID + cmd)
    pub pid_tree: Option<PidTree>,
}

impl ProcManager {
    pub fn new() -> Self {
        Self {
            proc_cache: None,
            pid_tree: None,
        }
    }

    /// Set the process cache.
    pub fn set_proc_cache(&mut self, cache: ProcCache) {
        self.proc_cache = Some(cache);
    }

    /// Set the process tree.
    pub fn set_pid_tree(&mut self, tree: PidTree) {
        self.pid_tree = Some(tree);
    }

    /// Get a reference to the process cache.
    pub fn proc_cache(&self) -> Option<&ProcCache> {
        self.proc_cache.as_ref()
    }

    /// Get a reference to the process tree.
    pub fn pid_tree(&self) -> Option<&PidTree> {
        self.pid_tree.as_ref()
    }

    /// Get the number of entries in the process cache.
    pub fn proc_cache_entries(&self) -> u64 {
        self.proc_cache.as_ref().map_or(0, |c| c.entry_count() as u64)
    }

    /// Get the number of entries in the process tree.
    pub fn pid_tree_entries(&self) -> u64 {
        self.pid_tree.as_ref().map_or(0, |t| t.entry_count() as u64)
    }
}
