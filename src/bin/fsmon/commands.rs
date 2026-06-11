use anyhow::Result;
use fsmon::common::filters::PathOptions;
use fsmon::common::monitored::PathEntry;
use std::path::PathBuf;

mod add;
mod changes;
mod clean;
mod daemon;
mod health;
mod init_cd;
mod monitored;
mod query;
mod remove;

pub use add::cmd_add;
pub use changes::cmd_changes;
pub use clean::cmd_clean;
pub use daemon::{DaemonOptions, cmd_daemon};
pub use health::cmd_health;
pub use init_cd::{cmd_cd, cmd_init};
pub use monitored::{cmd_list_monitored_paths, cmd_monitored};
pub use query::cmd_query;
pub use remove::cmd_remove;

/// Top-level dispatch: match command enum and call the corresponding handler.
pub fn run(command: crate::Commands) -> Result<()> {
    use crate::Commands::*;
    match command {
        Daemon {
            debug,
            cache_dir_cap,
            cache_dir_ttl,
            cache_file_size,
            cache_proc_ttl,
            cache_buffer,
            cache_channel,
            cache_subscribe,
            logging_disk_free,
            logging_local_time,
            metrics_interval,
            watchdog_interval,
            watchdog_multiplier,
        } => {
            let cli_cache = fsmon::common::config::CliCacheOverride {
                dir_capacity: cache_dir_cap,
                dir_ttl_secs: cache_dir_ttl,
                file_size_capacity: cache_file_size,
                proc_ttl_secs: cache_proc_ttl,
                buffer_size: cache_buffer,
                channel_capacity: cache_channel,
                subscribe_buf: cache_subscribe,
            };
            cmd_daemon(DaemonOptions {
                debug,
                cli_cache,
                disk_min_free: logging_disk_free,
                local_time: logging_local_time,
                metrics_interval,
                watchdog_interval,
                watchdog_multiplier,
            })
            .await_()
        }
        Add(args) => cmd_add(args),
        Remove { cmd, path } => cmd_remove(cmd, path),
        Monitored => cmd_monitored(),
        Query(args) => cmd_query(args).await_(),
        Changes(args) => cmd_changes(args).await_(),
        Clean(args) => cmd_clean(args).await_(),
        Init { service } => cmd_init(service),
        Cd { monitored, config, .. } => cmd_cd(monitored, config),
        Health => cmd_health(),
        ListMonitoredPaths => cmd_list_monitored_paths(),
    }
}

/// Convert async functions to sync by running in a new runtime.
/// Used because `run()` is called from sync `main()`.
trait AsyncPolyfill {
    type Output;
    fn await_(self) -> Self::Output;
}

impl<T> AsyncPolyfill for T
where
    T: std::future::Future<Output = Result<()>>,
{
    type Output = Result<()>;
    fn await_(self) -> Self::Output {
        tokio::runtime::Runtime::new()
            .map_err(|e| anyhow::anyhow!(e))?
            .block_on(self)
    }
}

/// Convert `PathEntry` list to `(PathBuf, PathOptions)` pairs.
pub fn parse_path_entries(entries: &[PathEntry]) -> Result<Vec<(PathBuf, PathOptions)>> {
    let mut result = Vec::new();
    for entry in entries {
        let opts = parse_path_options(entry)?;
        result.push((entry.path.clone(), opts));
    }
    Ok(result)
}

/// Convert a single `PathEntry` to `PathOptions`.
pub fn parse_path_options(entry: &PathEntry) -> Result<PathOptions> {
    PathOptions::try_from(entry)
}

/// Resolve a user-provided path to absolute.
/// Tilde expansion → path clean → canonicalize if exists,
/// otherwise join relative paths with cwd.
pub fn resolve_path_arg(raw: &std::path::Path) -> std::path::PathBuf {
    use path_clean::PathClean;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = fsmon::common::config::expand_tilde(raw, &home);
    let cleaned = expanded.clean();
    match cleaned.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            if cleaned.is_relative() {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&cleaned))
                    .unwrap_or(cleaned)
            } else {
                cleaned
            }
        }
    }
}
