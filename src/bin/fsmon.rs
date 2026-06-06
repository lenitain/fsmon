use anyhow::Result;
use clap::{Parser, Subcommand};
use fsmon::help::{self, HelpTopic};
pub use fsmon::{AddArgs, ChangesArgs, CleanArgs, QueryArgs};
use std::path::PathBuf;

mod commands;

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = help::about(HelpTopic::Root))]
#[command(after_help = help::after_help())]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// fsmon CLI commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Run the fsmon daemon (requires sudo for fanotify)
    #[command(about = help::about(HelpTopic::Daemon), long_about = help::long_about(HelpTopic::Daemon))]
    Daemon {
        /// Enable debug output (event matching, routing decisions)
        #[arg(short, long)]
        debug: bool,

        /// Directory handle cache capacity (default: 100000).
        /// Lower on memory-constrained systems; raise for large trees.
        #[arg(long, value_name = "N")]
        cache_dir_cap: Option<u64>,

        /// Directory handle cache TTL in seconds (default: 3600).
        #[arg(long, value_name = "SECS")]
        cache_dir_ttl: Option<u64>,

        /// File size cache capacity (default: 10000).
        /// Raise for high-file-volume workloads.
        #[arg(long, value_name = "N")]
        cache_file_size: Option<usize>,

        /// Process cache TTL in seconds (default: 600).
        /// Applies to both process info and process tree caches.
        #[arg(long, value_name = "SECS")]
        cache_proc_ttl: Option<u64>,

        /// Cache stats log interval in seconds in debug mode (default: 60).
        /// Set to 0 to disable periodic cache stats.
        #[arg(long, value_name = "SECS")]
        cache_stats_interval: Option<u64>,

        /// Fanotify read buffer size in bytes (default: 32768, min: 4096, max: 1048576).
        /// Raise for high-throughput event volumes.
        #[arg(long, value_name = "BYTES")]
        buffer_size: Option<usize>,

        /// Event channel capacity between reader tasks and the main loop.
        /// Default: unbounded. Set to a finite number (e.g. 1024) to cap
        /// memory under extreme event storms — reader tasks block when
        /// the buffer is full, with fanotify overflow as the final backstop.
        #[arg(long, value_name = "N")]
        channel_capacity: Option<usize>,

        /// Subscribe event stream buffer capacity (default: 4096).
        /// Number of events the broadcast channel can buffer for slow
        /// subscribers before dropping oldest. Raise for high-throughput
        /// workloads with many concurrent subscribers.
        #[arg(long, value_name = "N")]
        subscribe_buf: Option<usize>,

        /// Minimum free disk space before warning (e.g. "10%", "5GB").
        /// Default: no check. Only applies to the log directory filesystem.
        #[arg(long, value_name = "THRESHOLD")]
        disk_min_free: Option<String>,

        /// Log file sync interval in seconds (default: disabled).
        /// When set to N > 0, calls fdatasync on all dirty log files every N seconds.
        /// Prevents event loss on crash (kill -9, power loss) at the cost of
        /// ~10-50ms disk I/O per interval. Recommended: 5.
        #[arg(long, value_name = "SECS")]
        sync_interval: Option<u64>,

        /// Use local time instead of UTC in event timestamps.
        /// When set, timestamps show local timezone offset (e.g. +08:00)
        /// instead of Z suffix.
        #[arg(long)]
        local_time: bool,

        /// Metrics report interval in seconds (default: disabled).
        /// When set to N > 0, prints a one-line status report to stderr every N seconds.
        /// Report includes: uptime, RSS (MB), events processed/written,
        /// cache sizes, and reader task health.
        #[arg(long, value_name = "SECS")]
        metrics_interval: Option<u64>,

        /// systemd watchdog heartbeat interval in seconds (default: disabled).
        /// When set to N > 0, sends periodic WATCHDOG=1 notifications to systemd.
        #[arg(long, value_name = "SECS")]
        watchdog_interval: Option<u64>,

        /// Watchdog timeout multiplier (default: 2).
        /// WatchdogSec = watchdog_interval × multiplier.
        /// Recommended: 2-4. Higher = more tolerant of transient stalls.
        #[arg(long, value_name = "N")]
        watchdog_multiplier: Option<u64>,
    },

    /// Add a path to the monitoring list
    #[command(about = help::about(HelpTopic::Add), long_about = help::long_about(HelpTopic::Add))]
    Add(AddArgs),

    /// Remove one or more paths from the monitoring list
    #[command(about = help::about(HelpTopic::Remove), long_about = help::long_about(HelpTopic::Remove))]
    Remove {
        /// Cmd group to remove (positional). Use '_global' for global monitoring.
        #[arg(value_name = "CMD")]
        cmd: Option<String>,
        /// Path(s) to remove from the cmd group (repeatable).
        #[arg(long, value_name = "PATH")]
        path: Vec<PathBuf>,
    },

    /// List all monitored paths with their configuration
    #[command(about = help::about(HelpTopic::Monitored), long_about = help::long_about(HelpTopic::Monitored))]
    Monitored,

    /// Query historical file change events
    #[command(about = help::about(HelpTopic::Query), long_about = help::long_about(HelpTopic::Query))]
    Query(QueryArgs),

    /// Clean historical log files
    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean(CleanArgs),

    /// Show most recent event per path (deduplicated changes)
    #[command(about = help::about(HelpTopic::Changes), long_about = help::long_about(HelpTopic::Changes))]
    Changes(ChangesArgs),

    /// Create the config file. Directories are created on first use by
    /// other commands (monitored: fsmon add; logs: fsmon daemon / fsmon cd).
    /// With --service, also create a systemd service file.
    #[command(about = help::about(HelpTopic::Init), long_about = help::long_about(HelpTopic::Init))]
    Init {
        /// Also create a systemd service file at /etc/systemd/system/fsmon.service
        #[arg(long)]
        service: bool,
    },

    /// Open a subshell in the monitored path or log directory
    #[command(about = help::about(HelpTopic::Cd), long_about = help::long_about(HelpTopic::Cd))]
    Cd {
        /// cd to the monitored store directory
        #[arg(
            short = 'm',
            long,
            conflicts_with = "logging",
            required_unless_present = "logging"
        )]
        monitored: bool,
        /// cd to the log directory (same as old `fsmon cd`)
        #[arg(
            short = 'l',
            long,
            conflicts_with = "monitored",
            required_unless_present = "monitored"
        )]
        logging: bool,
    },

    /// Query daemon health status from the running daemon
    #[command(about = "Query daemon health status")]
    Health,

    /// List monitored paths (one per line, for shell completion use)
    #[command(hide = true)]
    ListMonitoredPaths,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    commands::run(cli.command)
}

#[cfg(test)]
#[path = "tests/cli_parsing_tests.rs"]
mod tests;
