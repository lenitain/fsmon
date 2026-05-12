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
        /// Process name scope (positional). Without --path, removes the entire cmd group.
        #[arg(value_name = "CMD")]
        cmd: Option<String>,
        /// Path(s) to remove from the cmd group (repeatable). Without cmd, operates on the null group.
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

    /// Initialize log and monitored data directories
    #[command(about = help::about(HelpTopic::Init), long_about = help::long_about(HelpTopic::Init))]
    Init,

    /// Print the log directory path
    #[command(about = help::about(HelpTopic::Cd), long_about = help::long_about(HelpTopic::Cd))]
    Cd,

    /// List monitored paths (one per line, for shell completion use)
    #[command(hide = true)]
    ListMonitoredPaths,
}

#[derive(Parser)]
pub struct AddArgs {
    /// Process name to track (process tree + ancestry chain). Positional argument.
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

    /*
    /// Path glob patterns to exclude (repeatable, prefix ! to invert)
    #[arg(short, long, value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Process names to exclude (glob, repeatable, prefix ! to invert)
    #[arg(long, value_name = "PATTERN")]
    pub exclude_cmd: Vec<String>,
    */
}

#[derive(Parser)]
pub struct QueryArgs {
    /// Cmd group to query (positional). Omit to query all cmd groups.
    #[arg(value_name = "CMD")]
    pub cmd: Option<String>,
    /// Path prefix filter(s) applied to event.path. Repeatable.
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
    fn test_add_positional_cmd() {
        let args = AddArgs::try_parse_from(&["add", "openclaw", "--path", "/home"]).unwrap();
        assert_eq!(args.cmd, Some("openclaw".to_string()));
        assert_eq!(args.path, Some(PathBuf::from("/home")));
    }

    #[test]
    fn test_add_positional_cmd_only() {
        let args = AddArgs::try_parse_from(&["add", "nginx"]).unwrap();
        assert_eq!(args.cmd, Some("nginx".to_string()));
        assert!(args.path.is_none());
    }

    #[test]
    fn test_add_path_only() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp"]).unwrap();
        assert_eq!(args.path, Some(PathBuf::from("/tmp")));
        assert!(args.cmd.is_none());
        assert!(args.types.is_empty());
        assert!(!args.recursive);
        assert!(args.size.is_none());
    }

    #[test]
    fn test_add_types_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "--types", "MODIFY", "--types", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_types_short() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "-t", "MODIFY", "-t", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_types_all_long() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "--types", "all"]).unwrap();
        assert_eq!(args.types, vec!["all"]);
    }

    #[test]
    fn test_add_types_mixed() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "-t", "MODIFY", "--types", "CREATE",
        ]).unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    /*
    #[test]
    fn test_add_exclude_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "--exclude", "*.tmp", "--exclude", "*.log",
        ]).unwrap();
        // exclude field removed
    }

    #[test]
    fn test_add_exclude_short() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "-e", "*.tmp", "-e", "*.log",
        ]).unwrap();
        // exclude field removed
    }

    #[test]
    fn test_add_exclude_invert() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "--exclude", "!*.py",
        ]).unwrap();
        // exclude field removed
    }

    #[test]
    fn test_add_exclude_cmd_long() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "--exclude-cmd", "rsync", "--exclude-cmd", "apt",
        ]).unwrap();
        // exclude_cmd field removed
        assert!(args.cmd.is_none());
    }

    #[test]
    fn test_add_exclude_cmd_short_not_applicable() {
        let args = AddArgs::try_parse_from(&[
            "add", "--path", "/tmp",
            "--exclude-cmd", "nginx",
        ]).unwrap();
        // exclude_cmd field removed
        assert!(args.cmd.is_none());
    }
    */

    #[test]
    fn test_add_recursive_short() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-r"]).unwrap();
        assert!(args.recursive);
        assert!(args.cmd.is_none());
    }

    #[test]
    fn test_add_size_short() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-s", "1GB"]).unwrap();
        assert_eq!(args.size, Some("1GB".into()));
    }

    #[test]
    fn test_add_size_long() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "--size", "100MB"]).unwrap();
        assert_eq!(args.size, Some("100MB".into()));
    }

    #[test]
    fn test_add_size_with_operator() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-s", ">=1MB"]).unwrap();
        assert_eq!(args.size, Some(">=1MB".into()));

        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "--size", "<500KB"]).unwrap();
        assert_eq!(args.size, Some("<500KB".into()));

        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-s", "=0"]).unwrap();
        assert_eq!(args.size, Some("=0".into()));
    }

    #[test]
    fn test_add_size_decimal_and_negative() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-s", "1.5KB"]).unwrap();
        assert_eq!(args.size, Some("1.5KB".into()));

        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "--size", ">-1KB"]).unwrap();
        assert_eq!(args.size, Some(">-1KB".into()));
    }

    #[test]
    fn test_add_size_case_insensitive_unit() {
        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "-s", "1mb"]).unwrap();
        assert_eq!(args.size, Some("1mb".into()));

        let args = AddArgs::try_parse_from(&["add", "--path", "/tmp", "--size", "100Kb"]).unwrap();
        assert_eq!(args.size, Some("100Kb".into()));
    }

    #[test]
    fn test_add_all_flags() {
        let args = AddArgs::try_parse_from(&[
            "add", "nginx", "--path", "/tmp",
            "-r",
            "-t", "MODIFY", "--types", "CREATE",
            "-s", "1KB",
        ]).unwrap();
        assert_eq!(args.cmd, Some("nginx".to_string()));
        assert_eq!(args.path, Some(PathBuf::from("/tmp")));
        assert!(args.recursive);
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
        assert_eq!(args.size, Some("1KB".into()));
    }

    #[test]
    fn test_add_positional_cmd_with_recursive() {
        let args = AddArgs::try_parse_from(&[
            "add", "openclaw", "--path", "/home", "-r",
        ]).unwrap();
        assert_eq!(args.cmd, Some("openclaw".to_string()));
        assert_eq!(args.path, Some(PathBuf::from("/home")));
        assert!(args.recursive);
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
    fn test_remove_path() {
        let cli = Cli::try_parse_from(&["fsmon", "remove", "--path", "/tmp"]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert!(cmd.is_none());
                assert_eq!(path, vec![PathBuf::from("/tmp")]);
            }
            _ => panic!("expected Remove"),
        };
    }

    #[test]
    fn test_remove_multi_path() {
        let cli = Cli::try_parse_from(&[
            "fsmon", "remove",
            "--path", "/tmp",
            "--path", "/home",
        ]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert!(cmd.is_none());
                assert_eq!(path, vec![
                    PathBuf::from("/tmp"),
                    PathBuf::from("/home"),
                ]);
            }
            _ => panic!("expected Remove"),
        };
    }

    #[test]
    fn test_remove_cmd() {
        // fsmon remove nginx (positional cmd)
        let cli = Cli::try_parse_from(&["fsmon", "remove", "nginx"]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert_eq!(cmd, Some("nginx".to_string()));
                assert!(path.is_empty());
            }
            _ => panic!("expected Remove"),
        };
    }

    #[test]
    fn test_remove_path_and_cmd() {
        // fsmon remove openclaw --path /tmp
        let cli = Cli::try_parse_from(&["fsmon", "remove", "openclaw", "--path", "/tmp"]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert_eq!(cmd, Some("openclaw".to_string()));
                assert_eq!(path, vec![PathBuf::from("/tmp")]);
            }
            _ => panic!("expected Remove"),
        };
    }

    #[test]
    fn test_remove_empty_ok() {
        // fsmon remove (no args) — valid parse, handler will error
        let cli = Cli::try_parse_from(&["fsmon", "remove"]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert!(cmd.is_none());
                assert!(path.is_empty());
            }
            _ => panic!("expected Remove"),
        };
    }

    // ---- Integration tests (no sudo needed) ----

    use fsmon::monitored::Monitored;
    use fsmon::config::Config;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Global mutex for tests that modify HOME env var.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Generate a unique temp directory path for test isolation.
    fn unique_temp_home() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "fsmon_integration_test_{}_{}",
            std::process::id(),
            n
        ))
    }

    /// Run a test with an isolated HOME directory.
    /// Creates `{home}/monitored` as a monitored path (exists, not parent of log dir).
    fn with_isolated_home(f: impl FnOnce(&Path, &Path)) {
        // Recover from poisoned mutex (previous test panic)
        let _lock = match ENV_LOCK.lock() {
            Ok(l) => l,
            Err(e) => e.into_inner(),
        };
        let dir = unique_temp_home();
        let _ = fs::remove_dir_all(&dir);
        let home_str = dir.to_string_lossy().to_string();

        // Create a monitored dir inside the temp home (not parent of log dir)
        let monitored_path = dir.join("monitored");
        fs::create_dir_all(&monitored_path).unwrap();

        temp_env::with_vars(
            &[
                ("HOME", Some(home_str.as_str())),
                ("XDG_CONFIG_HOME", None::<&str>),
                ("SUDO_UID", None::<&str>),
            ],
            || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    f(&dir, &monitored_path)
                }));
                let _ = fs::remove_dir_all(&dir);
                if let Err(e) = result {
                    std::panic::resume_unwind(e);
                }
            },
        );
    }

    /// Load the monitored store from the default path under the isolated home.
    fn load_store(_home: &Path) -> Monitored {
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        Monitored::load(&cfg.monitored.path).unwrap()
    }

    #[test]
    fn test_integration_add_path_only() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 1);
            assert!(store.get(mp, None).is_some());
        });
    }

    #[test]
    fn test_integration_add_with_cmd() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&[
                "add", "openclaw", "--path", p.as_ref(),
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 1);
            assert!(store.get(mp, Some("openclaw")).is_some());
        });
    }

    #[test]
    fn test_integration_add_with_types() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&[
                "add", "--path", p.as_ref(),
                "--types", "MODIFY",
                "--types", "CREATE",
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            let entry = store.get(mp, None).unwrap();
            let types = entry.types.unwrap();
            assert!(types.contains(&"MODIFY".to_string()));
            assert!(types.contains(&"CREATE".to_string()));
        });
    }

    #[test]
    fn test_integration_add_recursive() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&[
                "add", "--path", p.as_ref(), "-r",
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            let entry = store.get(mp, None).unwrap();
            assert_eq!(entry.recursive, Some(true));
        });
    }

    #[test]
    fn test_integration_add_and_remove_path() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            super::commands::cmd_remove(None, vec![mp.to_path_buf()]).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 0);
        });
    }

    #[test]
    fn test_integration_remove_entire_null_group() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            assert_eq!(load_store(home).entry_count(), 1);

            super::commands::cmd_remove(None, vec![]).unwrap();
            assert_eq!(load_store(home).entry_count(), 0);
        });
    }

    #[test]
    fn test_integration_remove_entire_cmd_group() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&[
                "add", "myapp", "--path", p.as_ref(),
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();
            assert_eq!(load_store(home).entry_count(), 1);

            super::commands::cmd_remove(Some("myapp".into()), vec![]).unwrap();
            assert_eq!(load_store(home).entry_count(), 0);
        });
    }

    #[test]
    fn test_integration_remove_path_from_cmd_group() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            // Same path in both myapp and null group
            let args = AddArgs::try_parse_from(&[
                "add", "myapp", "--path", p.as_ref(),
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            assert_eq!(load_store(home).entry_count(), 2);

            // Remove path only from myapp group
            super::commands::cmd_remove(
                Some("myapp".into()),
                vec![mp.to_path_buf()],
            ).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 1);
            assert!(store.get(mp, None).is_some());
            assert!(store.get(mp, Some("myapp")).is_none());
        });
    }

    #[test]
    fn test_integration_remove_multi_path_atomic_failure() {
        with_isolated_home(|_home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            // One path exists, one doesn't → should fail atomically
            let result = super::commands::cmd_remove(
                None,
                vec![mp.to_path_buf(), PathBuf::from("/nonexistent")],
            );
            assert!(result.is_err(), "should fail atomically");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("not being monitored"),
                "error should mention path not found, got: {}",
                err,
            );
        });
    }

    #[test]
    fn test_integration_remove_nonexistent_cmd_group() {
        with_isolated_home(|_home, _mp| {
            let result = super::commands::cmd_remove(Some("nonexistent".into()), vec![]);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_integration_add_to_both_null_and_cmd() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&[
                "add", "myapp", "--path", p.as_ref(),
            ]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 2);
            assert_eq!(store.groups.len(), 2);
        });
    }

}
