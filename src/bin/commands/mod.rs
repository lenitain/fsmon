use anyhow::Result;
use fsmon::EventType;
use fsmon::filters::PathOptions;
use fsmon::monitored::PathEntry;
use fsmon::utils::parse_size_filter;
use std::path::PathBuf;

mod add;
mod clean;
mod daemon;
mod init_cd;
mod monitored;
mod query;
mod remove;

pub use add::cmd_add;
pub use clean::cmd_clean;
pub use daemon::cmd_daemon;
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
            cache_stats_interval,
            buffer_size,
        } => {
            let cli_cache = fsmon::config::CliCacheOverride {
                dir_capacity: cache_dir_cap,
                dir_ttl_secs: cache_dir_ttl,
                file_size_capacity: cache_file_size,
                proc_ttl_secs: cache_proc_ttl,
                stats_interval_secs: cache_stats_interval,
                buffer_size,
            };
            cmd_daemon(debug, cli_cache).await_()
        }
        Add(args) => cmd_add(args),
        Remove { cmd, path } => cmd_remove(cmd, path),
        Monitored => cmd_monitored(),
        Query(args) => cmd_query(args).await_(),
        Clean(args) => cmd_clean(args).await_(),
        Init => cmd_init(),
        Cd => cmd_cd(),
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
        tokio::runtime::Runtime::new().unwrap().block_on(self)
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
    let size_filter = entry
        .size
        .as_ref()
        .map(|s| parse_size_filter(s))
        .transpose()?;
    let event_types = entry
        .types
        .as_ref()
        .map(|v| {
            v.iter()
                .map(|s| s.parse::<EventType>())
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let cmd = entry.cmd.as_deref().and_then(|c| {
        if c == fsmon::monitored::CMD_GLOBAL {
            None
        } else {
            Some(c.to_string())
        }
    });
    Ok(PathOptions {
        size_filter,
        event_types,
        recursive: entry.recursive.unwrap_or(false),
        cmd,
    })
}
