//! Shared helpers for fsmon integration tests.

pub mod fsmon_client;
pub mod fsmon_daemon;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Create a unique temporary directory under the system temp folder.
///
/// Pattern: `{temp_dir}/fsmon-test-{tag}-{nanos_since_epoch}`
pub fn unique_tmp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("fsmon-test-{}-{}", tag, nanos))
}
