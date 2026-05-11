use anyhow::Result;
use clap::{Parser, Subcommand};
use fsmon::help::{self, HelpTopic};
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

#[derive(Subcommand)]
pub enum Commands {
    /// Run the fsmon daemon (requires sudo for fanotify)
    #[command(about = help::about(HelpTopic::Daemon), long_about = help::long_about(HelpTopic::Daemon))]
    Daemon,

    /// Add a path to the monitoring list
    #[command(about = help::about(HelpTopic::Add), long_about = help::long_about(HelpTopic::Add))]
    Add(AddArgs),

    /// Remove one or more paths from the monitoring list
    #[command(about = help::about(HelpTopic::Remove), long_about = help::long_about(HelpTopic::Remove))]
    Remove {
        /// Path(s) to remove. Multiple paths can be specified.
        paths: Vec<PathBuf>,
    },

    /// List all monitored paths with their configuration
    #[command(about = help::about(HelpTopic::Managed), long_about = help::long_about(HelpTopic::Managed))]
    Managed,

    /// Query historical file change events
    #[command(about = help::about(HelpTopic::Query), long_about = help::long_about(HelpTopic::Query))]
    Query(QueryArgs),

    /// Clean historical log files
    #[command(about = help::about(HelpTopic::Clean), long_about = help::long_about(HelpTopic::Clean))]
    Clean(CleanArgs),

    /// Initialize log and managed data directories
    #[command(about = help::about(HelpTopic::Init), long_about = help::long_about(HelpTopic::Init))]
    Init,

    /// Print the log directory path
    #[command(about = help::about(HelpTopic::Cd), long_about = help::long_about(HelpTopic::Cd))]
    Cd,

    /// Resolve the log file path for one or more paths
    #[command(about = help::about(HelpTopic::P2l), long_about = help::long_about(HelpTopic::P2l))]
    P2l {
        /// Path(s) to resolve. Multiple paths can be specified.
        paths: Vec<PathBuf>,
    },

    /// List managed paths (one per line, for shell completion use)
    #[command(hide = true)]
    ListManagedPaths,
}

#[derive(Parser)]
pub struct AddArgs {
    /// Path to monitor
    pub path: PathBuf,

    /// Watch subdirectories recursively
    #[arg(short)]
    pub recursive: bool,

    /// Event types to monitor (repeatable; use "all" for all 14 types). Controls kernel mask.
    #[arg(short, long, value_name = "TYPE")]
    pub types: Vec<String>,

    /// Size filter with operator (required: >=, >, <=, <, =). e.g. >1MB, >=500KB, <100MB, =0
    #[arg(short, long, value_name = "SIZE")]
    pub size: Option<String>,

    /// Path glob patterns to exclude (repeatable, prefix ! to invert)
    #[arg(short, long, value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Process names to exclude (glob, repeatable, prefix ! to invert)
    #[arg(long, value_name = "PATTERN")]
    pub exclude_cmd: Vec<String>,
}

#[derive(Parser)]
pub struct QueryArgs {
    /// Path(s) to query. Repeatable. Default: all.
    #[arg(short, long, value_name = "PATH")]
    pub path: Vec<PathBuf>,
    /// Time filter with operator (repeatable: >1h for since, <2026-05-01 for until)
    #[arg(short, long, value_name = "FILTER")]
    pub time: Vec<String>,
}

#[derive(Parser)]
pub struct CleanArgs {
    /// Path(s) to clean. Repeatable. Default: all.
    #[arg(short, long, value_name = "PATH")]
    pub path: Vec<PathBuf>,
    /// Time filter with operator (e.g. >30d — delete entries older than 30 days)
    #[arg(long, value_name = "FILTER")]
    pub time: Option<String>,
    /// Size limit for log file truncation with operator (e.g. >500MB, >=1GB)
    #[arg(short, long)]
    pub size: Option<String>,
    #[arg(short, long)]
    pub dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    commands::run(cli.command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ---- AddArgs CLI parsing ----

    #[test]
    fn test_add_no_flags() {
        let args = AddArgs::try_parse_from(&["add", "/tmp"]).unwrap();
        assert_eq!(args.path, PathBuf::from("/tmp"));
        assert!(args.types.is_empty());
        assert!(args.exclude.is_empty());
        assert!(args.exclude_cmd.is_empty());
        assert!(!args.recursive);
        assert!(args.size.is_none());
    }

    #[test]
    fn test_add_types_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "--types", "MODIFY", "--types", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_types_short() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "-t", "MODIFY", "-t", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_types_all_long() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "--types", "all"]).unwrap();
        assert_eq!(args.types, vec!["all"]);
    }

    #[test]
    fn test_add_types_mixed() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "-t", "MODIFY", "--types", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_exclude_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "--exclude", "*.tmp", "--exclude", "*.log",
        ]).unwrap();
        assert_eq!(args.exclude, vec!["*.tmp", "*.log"]);
    }

    #[test]
    fn test_add_exclude_short() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "-e", "*.tmp", "-e", "*.log",
        ]).unwrap();
        assert_eq!(args.exclude, vec!["*.tmp", "*.log"]);
    }

    #[test]
    fn test_add_exclude_invert() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "--exclude", "!*.py",
        ]).unwrap();
        assert_eq!(args.exclude, vec!["!*.py"]);
    }

    #[test]
    fn test_add_exclude_cmd_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "--exclude-cmd", "rsync", "--exclude-cmd", "apt",
        ]).unwrap();
        assert_eq!(args.exclude_cmd, vec!["rsync", "apt"]);
    }

    #[test]
    fn test_add_exclude_cmd_short_not_applicable() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "--exclude-cmd", "nginx",
        ]).unwrap();
        assert_eq!(args.exclude_cmd, vec!["nginx"]);
    }

    #[test]
    fn test_add_recursive_short() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "-r"]).unwrap();
        assert!(args.recursive);
    }

    #[test]
    fn test_add_size_short() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "-s", "1GB"]).unwrap();
        assert_eq!(args.size, Some("1GB".into()));
    }

    #[test]
    fn test_add_size_long() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "--size", "100MB"]).unwrap();
        assert_eq!(args.size, Some("100MB".into()));
    }

    #[test]
    fn test_add_size_with_operator() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "-s", ">=1MB"]).unwrap();
        assert_eq!(args.size, Some(">=1MB".into()));

        let args = AddArgs::try_parse_from(&["add", "/tmp", "--size", "<500KB"]).unwrap();
        assert_eq!(args.size, Some("<500KB".into()));

        let args = AddArgs::try_parse_from(&["add", "/tmp", "-s", "=0"]).unwrap();
        assert_eq!(args.size, Some("=0".into()));
    }

    #[test]
    fn test_add_size_decimal_and_negative() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "-s", "1.5KB"]).unwrap();
        assert_eq!(args.size, Some("1.5KB".into()));

        let args = AddArgs::try_parse_from(&["add", "/tmp", "--size", ">-1KB"]).unwrap();
        assert_eq!(args.size, Some(">-1KB".into()));
    }

    #[test]
    fn test_add_size_case_insensitive_unit() {
        let args = AddArgs::try_parse_from(&["add", "/tmp", "-s", "1mb"]).unwrap();
        assert_eq!(args.size, Some("1mb".into()));

        let args = AddArgs::try_parse_from(&["add", "/tmp", "--size", "100Kb"]).unwrap();
        assert_eq!(args.size, Some("100Kb".into()));
    }

    #[test]
    fn test_add_all_flags() {
        let args = AddArgs::try_parse_from(&[
            "add", "/tmp",
            "-r",
            "-t", "MODIFY", "--types", "CREATE",
            "-e", "*.tmp", "--exclude", "*.log",
            "--exclude-cmd", "rsync",
            "-s", "1KB",
        ]).unwrap();
        assert_eq!(args.path, PathBuf::from("/tmp"));
        assert!(args.recursive);
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
        assert_eq!(args.exclude, vec!["*.tmp", "*.log"]);
        assert_eq!(args.exclude_cmd, vec!["rsync"]);
        assert_eq!(args.size, Some("1KB".into()));
    }

    // ---- QueryArgs CLI parsing ----

    #[test]
    fn test_query_no_flags() {
        let args = QueryArgs::try_parse_from(&["query"]).unwrap();
        assert!(args.path.is_empty());
        assert!(args.time.is_empty());
    }

    #[test]
    fn test_query_path_long() {
        let args = QueryArgs::try_parse_from(&[
            "query",
            "--path", "/tmp", "--path", "/home",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp"), PathBuf::from("/home")]);
    }

    #[test]
    fn test_query_path_short() {
        let args = QueryArgs::try_parse_from(&[
            "query",
            "-p", "/tmp", "-p", "/home",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp"), PathBuf::from("/home")]);
    }

    #[test]
    fn test_query_time_since() {
        let args = QueryArgs::try_parse_from(&["query", "-t", ">1h"]).unwrap();
        assert_eq!(args.time, vec![">1h".to_string()]);
    }

    #[test]
    fn test_query_time_until() {
        let args = QueryArgs::try_parse_from(&["query", "--time", "<2026-05-01"]).unwrap();
        assert_eq!(args.time, vec!["<2026-05-01".to_string()]);
    }

    #[test]
    fn test_query_time_repeatable() {
        let args = QueryArgs::try_parse_from(&[
            "query",
            "--time", ">1h", "--time", "<now",
        ]).unwrap();
        assert_eq!(args.time, vec![">1h".to_string(), "<now".to_string()]);
    }

    #[test]
    fn test_query_time_with_path() {
        let args = QueryArgs::try_parse_from(&[
            "query",
            "-p", "/tmp",
            "-t", ">1h",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp")]);
        assert_eq!(args.time, vec![">1h".to_string()]);
    }

    // ---- CleanArgs CLI parsing ----

    #[test]
    fn test_clean_no_flags() {
        let args = CleanArgs::try_parse_from(&["clean"]).unwrap();
        assert!(args.path.is_empty());
        assert!(args.time.is_none());
        assert!(args.size.is_none());
        assert!(!args.dry_run);
    }

    #[test]
    fn test_clean_path_long() {
        let args = CleanArgs::try_parse_from(&[
            "clean",
            "--path", "/tmp", "--path", "/var/log",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")]);
    }

    #[test]
    fn test_clean_path_short() {
        let args = CleanArgs::try_parse_from(&[
            "clean",
            "-p", "/tmp", "-p", "/var/log",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")]);
    }

    #[test]
    fn test_clean_time() {
        let args = CleanArgs::try_parse_from(&["clean", "--time", ">30d"]).unwrap();
        assert_eq!(args.time, Some(">30d".into()));
    }

    #[test]
    fn test_clean_size_short() {
        let args = CleanArgs::try_parse_from(&["clean", "-s", "500MB"]).unwrap();
        assert_eq!(args.size, Some("500MB".into()));
    }

    #[test]
    fn test_clean_size_long() {
        let args = CleanArgs::try_parse_from(&["clean", "--size", ">=1GB"]).unwrap();
        assert_eq!(args.size, Some(">=1GB".into()));
    }

    #[test]
    fn test_clean_dry_run_long() {
        let args = CleanArgs::try_parse_from(&["clean", "--dry-run"]).unwrap();
        assert!(args.dry_run);
    }

    #[test]
    fn test_clean_all_flags() {
        let args = CleanArgs::try_parse_from(&[
            "clean",
            "-p", "/tmp", "--path", "/var/log",
            "--time", ">30d",
            "-s", ">=100MB",
            "--dry-run",
        ]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")]);
        assert_eq!(args.time, Some(">30d".into()));
        assert_eq!(args.size, Some(">=100MB".into()));
        assert!(args.dry_run);
    }

    // ---- Remove command (positional paths) ----

    #[test]
    fn test_remove_single_path() {
        let cli = Cli::try_parse_from(&["fsmon", "remove", "/tmp"]).unwrap();
        let paths = match cli.command {
            Commands::Remove { paths } => paths,
            _ => panic!("expected Remove"),
        };
        assert_eq!(paths, vec![PathBuf::from("/tmp")]);
    }

    #[test]
    fn test_remove_multiple_paths() {
        let cli = Cli::try_parse_from(&["fsmon", "remove", "/tmp", "/home", "/var/log"]).unwrap();
        let paths = match cli.command {
            Commands::Remove { paths } => paths,
            _ => panic!("expected Remove"),
        };
        assert_eq!(paths, vec![
            PathBuf::from("/tmp"),
            PathBuf::from("/home"),
            PathBuf::from("/var/log"),
        ]);
    }

    #[test]
    fn test_remove_empty_ok() {
        let cli = Cli::try_parse_from(&["fsmon", "remove"]).unwrap();
        let paths = match cli.command {
            Commands::Remove { paths } => paths,
            _ => panic!("expected Remove"),
        };
        assert!(paths.is_empty());
    }
}
