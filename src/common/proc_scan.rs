//! One-shot `/proc` scanning wrapper (event-driven migration, RUN-23):
//! used only for the bootstrap baseline — never a poll loop.

use proc_tree::{
    procf::{ProcScanConfig, scan},
    SnapshotEntry, SnapshotMeta,
};

/// Scan `/proc` once and return `(entries, meta)` for bootstrap.
pub fn scan_once(proc_root: &std::path::Path) -> (Vec<SnapshotEntry>, SnapshotMeta) {
    let result = scan(&ProcScanConfig {
        proc_root: proc_root.to_path_buf(),
        ..Default::default()
    });
    (result.entries, result.meta)
}
