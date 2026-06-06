use clap::Parser;
use std::path::PathBuf;

/// Arguments for the `fsmon add` command.
#[derive(Parser, Clone)]
pub struct AddArgs {
    /// Process name to track (positional). Use '_global' for global monitoring.
    #[arg(value_name = "CMD")]
    pub cmd: Option<String>,

    /// Path to monitor
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Watch subdirectories recursively
    #[arg(short)]
    pub recursive: bool,

    /// Event types to monitor (repeatable; use "all" for all 14 types). Controls kernel mask.
    #[arg(short, long, value_name = "TYPE")]
    pub types: Vec<String>,

    /// Size filter with operator (required: >=, >, <=, <, =). e.g. >1MB, >=500KB, <100MB, =0
    #[arg(short, long, value_name = "SIZE")]
    pub size: Option<String>,
}

#[derive(Parser, Clone)]
pub struct QueryArgs {
    /// Cmd group to query (positional). Use '_global' for global events.
    #[arg(value_name = "CMD")]
    pub cmd: Option<String>,
    /// Path prefix filter(s) applied to event.path. Repeatable.
    #[arg(short, long, value_name = "PATH")]
    pub path: Vec<PathBuf>,
    /// Time filter with operator (repeatable: >1h for since, <2026-05-01 for until)
    #[arg(short, long, value_name = "FILTER")]
    pub time: Vec<String>,
}

#[derive(Parser, Clone)]
pub struct ChangesArgs {
    /// Cmd group to query (positional). Use '_global' for global events.
    #[arg(value_name = "CMD")]
    pub cmd: Option<String>,
    /// Path prefix filter(s) applied to event.path. Repeatable.
    #[arg(short, long, value_name = "PATH")]
    pub path: Vec<PathBuf>,
    /// Time filter with operator (repeatable: >1h for since, <2026-05-01 for until)
    #[arg(short, long, value_name = "FILTER")]
    pub time: Vec<String>,
}

#[derive(Parser, Clone)]
pub struct CleanArgs {
    /// Cmd group to clean (positional). Use '_global' for the global log.
    #[arg(value_name = "CMD")]
    pub cmd: Option<String>,
    /// Time filter with operator (e.g. >30d — delete entries older than 30 days)
    #[arg(short, long, value_name = "FILTER")]
    pub time: Option<String>,
    /// Size limit for log file truncation with operator (e.g. >500MB, >=1GB)
    #[arg(short, long)]
    pub size: Option<String>,
    /// Dry run — preview without modifying
    #[arg(short, long)]
    pub dry_run: bool,
}
