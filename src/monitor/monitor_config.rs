// Configuration struct for Monitor construction.
// Replaces the 13-parameter new() signature.

use crate::config::ResolvedCacheConfig;
use crate::filters::PathOptions;
use std::path::PathBuf;

/// Configuration for creating a Monitor instance.
pub struct MonitorConfig {
    pub paths_and_options: Vec<(PathBuf, PathOptions)>,
    pub log_dir: Option<PathBuf>,
    pub monitored_path: Option<PathBuf>,
    pub buffer_size: Option<usize>,
    pub socket_listener: Option<tokio::net::UnixListener>,
    pub debug: bool,
    pub cache_config: Option<ResolvedCacheConfig>,
    pub disk_min_free: Option<String>,
    pub sync_interval: Option<std::time::Duration>,
    pub subscribe_buf: Option<usize>,
    pub local_time: bool,
    pub metrics_interval: Option<u64>,
    pub watchdog_interval: Option<u64>,
}
