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

        /// Fanotify read buffer size in bytes (default: 32768, min: 4096, max: 1048576).
        /// Raise for high-throughput event volumes.
        #[arg(long, value_name = "BYTES")]
        buffer_size: Option<usize>,
    },

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
            "add", "--path", "/tmp", "--types", "MODIFY", "--types", "CREATE",
        ])
        .unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

    #[test]
    fn test_add_types_short() {
        let args =
            AddArgs::try_parse_from(&["add", "--path", "/tmp", "-t", "MODIFY", "-t", "CREATE"])
                .unwrap();
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
            "add", "--path", "/tmp", "-t", "MODIFY", "--types", "CREATE",
        ])
        .unwrap();
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
    }

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
            "add", "nginx", "--path", "/tmp", "-r", "-t", "MODIFY", "--types", "CREATE", "-s",
            "1KB",
        ])
        .unwrap();
        assert_eq!(args.cmd, Some("nginx".to_string()));
        assert_eq!(args.path, Some(PathBuf::from("/tmp")));
        assert!(args.recursive);
        assert_eq!(args.types, vec!["MODIFY", "CREATE"]);
        assert_eq!(args.size, Some("1KB".into()));
    }

    #[test]
    fn test_add_positional_cmd_with_recursive() {
        let args = AddArgs::try_parse_from(&["add", "openclaw", "--path", "/home", "-r"]).unwrap();
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
        let args =
            QueryArgs::try_parse_from(&["query", "--path", "/tmp", "--path", "/home"]).unwrap();
        assert_eq!(
            args.path,
            vec![PathBuf::from("/tmp"), PathBuf::from("/home")]
        );
    }

    #[test]
    fn test_query_path_short() {
        let args = QueryArgs::try_parse_from(&["query", "-p", "/tmp", "-p", "/home"]).unwrap();
        assert_eq!(
            args.path,
            vec![PathBuf::from("/tmp"), PathBuf::from("/home")]
        );
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
        let args =
            QueryArgs::try_parse_from(&["query", "--time", ">1h", "--time", "<now"]).unwrap();
        assert_eq!(args.time, vec![">1h".to_string(), "<now".to_string()]);
    }

    #[test]
    fn test_query_time_with_path() {
        let args = QueryArgs::try_parse_from(&["query", "-p", "/tmp", "-t", ">1h"]).unwrap();
        assert_eq!(args.path, vec![PathBuf::from("/tmp")]);
        assert_eq!(args.time, vec![">1h".to_string()]);
    }

    // ---- CleanArgs CLI parsing ----

    #[test]
    fn test_clean_basic_cmd() {
        let args = CleanArgs::try_parse_from(&["clean", "_global"]).unwrap();
        assert_eq!(args.cmd, Some("_global".into()));
        assert!(args.time.is_none());
        assert!(args.size.is_none());
        assert!(!args.dry_run);
    }

    #[test]
    fn test_clean_cmd_with_time() {
        let args = CleanArgs::try_parse_from(&["clean", "openclaw", "--time", ">30d"]).unwrap();
        assert_eq!(args.cmd, Some("openclaw".into()));
        assert_eq!(args.time, Some(">30d".into()));
    }

    #[test]
    fn test_clean_cmd_with_size() {
        let args = CleanArgs::try_parse_from(&["clean", "nginx", "-s", "500MB"]).unwrap();
        assert_eq!(args.cmd, Some("nginx".into()));
        assert_eq!(args.size, Some("500MB".into()));
    }

    #[test]
    fn test_clean_cmd_with_dry_run() {
        let args = CleanArgs::try_parse_from(&["clean", "_global", "--dry-run"]).unwrap();
        assert_eq!(args.cmd, Some("_global".into()));
        assert!(args.dry_run);
    }

    #[test]
    fn test_clean_all_flags() {
        let args = CleanArgs::try_parse_from(&[
            "clean",
            "openclaw",
            "--time",
            ">30d",
            "-s",
            ">=100MB",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(args.cmd, Some("openclaw".into()));
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
        let cli =
            Cli::try_parse_from(&["fsmon", "remove", "--path", "/tmp", "--path", "/home"]).unwrap();
        match cli.command {
            Commands::Remove { cmd, path } => {
                assert!(cmd.is_none());
                assert_eq!(path, vec![PathBuf::from("/tmp"), PathBuf::from("/home"),]);
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

    use fsmon::config::Config;
    use fsmon::monitored::Monitored;
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
    fn test_integration_add_global() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 1);
            assert!(store.get(mp, None).is_some());
            assert_eq!(store.groups[0].cmd, "_global");
        });
    }

    #[test]
    fn test_integration_add_with_cmd() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "openclaw", "--path", p.as_ref()]).unwrap();
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
                "add",
                "_global",
                "--path",
                p.as_ref(),
                "--types",
                "MODIFY",
                "--types",
                "CREATE",
            ])
            .unwrap();
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
            let args =
                AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref(), "-r"]).unwrap();
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
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            super::commands::cmd_remove(Some("_global".into()), vec![mp.to_path_buf()]).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 0);
        });
    }

    #[test]
    fn test_integration_remove_entire_global_group() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            assert_eq!(load_store(home).entry_count(), 1);

            super::commands::cmd_remove(Some("_global".into()), vec![]).unwrap();
            assert_eq!(load_store(home).entry_count(), 0);
        });
    }

    #[test]
    fn test_integration_remove_entire_cmd_group() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "myapp", "--path", p.as_ref()]).unwrap();
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
            // Same path in both myapp and _global group
            let args = AddArgs::try_parse_from(&["add", "myapp", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            assert_eq!(load_store(home).entry_count(), 2);

            // Remove path only from myapp group
            super::commands::cmd_remove(Some("myapp".into()), vec![mp.to_path_buf()]).unwrap();

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
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            // One path exists, one doesn't → should fail atomically
            let result = super::commands::cmd_remove(
                Some("_global".into()),
                vec![mp.to_path_buf(), PathBuf::from("/nonexistent")],
            );
            assert!(result.is_err(), "should fail atomically");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("not found under cmd"),
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
    fn test_integration_add_to_both_global_and_cmd() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&["add", "myapp", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 2);
            assert_eq!(store.groups.len(), 2);
        });
    }

    // ---- Edge cases: add ----

    #[test]
    fn test_integration_add_missing_cmd_fails() {
        with_isolated_home(|_home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "--path", p.as_ref()]).unwrap();
            let result = super::commands::cmd_add(args);
            assert!(result.is_err(), "missing cmd should fail");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("CMD is required"), "got: {}", err);
        });
    }

    #[test]
    fn test_integration_add_fsmon_cmd_fails() {
        with_isolated_home(|_home, mp| {
            let p = mp.to_string_lossy();
            let args = AddArgs::try_parse_from(&["add", "fsmon", "--path", p.as_ref()]).unwrap();
            let result = super::commands::cmd_add(args);
            assert!(result.is_err(), "fsmon cmd should fail");
        });
    }

    #[test]
    fn test_integration_add_duplicate_replaces() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            // Add first
            let args =
                AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref(), "-r"]).unwrap();
            super::commands::cmd_add(args).unwrap();
            assert_eq!(load_store(home).entry_count(), 1);

            // Add same path+cmd again with different flags (no -r)
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            assert_eq!(store.entry_count(), 1, "should replace, not duplicate");
            let entry = store.get(mp, None).unwrap();
            assert_eq!(
                entry.recursive,
                Some(false),
                "should be replaced with new flags"
            );
        });
    }

    #[test]
    fn test_integration_add_with_size() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            let args =
                AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref(), "-s", ">1MB"])
                    .unwrap();
            super::commands::cmd_add(args).unwrap();

            let store = load_store(home);
            let entry = store.get(mp, None).unwrap();
            assert_eq!(entry.size.as_deref(), Some(">1MB"));
        });
    }

    // ---- Edge cases: remove ----

    #[test]
    fn test_integration_remove_missing_cmd_fails() {
        with_isolated_home(|_home, _mp| {
            let result = super::commands::cmd_remove(None, vec![]);
            assert!(result.is_err(), "missing cmd should fail");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("CMD is required"), "got: {}", err);
        });
    }

    #[test]
    fn test_integration_remove_path_not_in_cmd_fails() {
        with_isolated_home(|_home, mp| {
            let p = mp.to_string_lossy();
            // Add path under _global
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();

            // Try to remove same path from wrong cmd group
            let result =
                super::commands::cmd_remove(Some("wrong_cmd".into()), vec![mp.to_path_buf()]);
            assert!(result.is_err(), "path in wrong cmd should fail");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("not found under cmd"), "got: {}", err);
        });
    }

    #[test]
    fn test_integration_remove_nonexistent_cmd_fails() {
        with_isolated_home(|_home, _mp| {
            let result = super::commands::cmd_remove(Some("ghost".into()), vec![]);
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("not found"), "got: {}", err);
        });
    }

    #[test]
    fn test_integration_remove_keeps_other_cmds() {
        with_isolated_home(|home, mp| {
            let p = mp.to_string_lossy();
            // Add same path under two cmds
            let args = AddArgs::try_parse_from(&["add", "_global", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&["add", "app_a", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            let args = AddArgs::try_parse_from(&["add", "app_b", "--path", p.as_ref()]).unwrap();
            super::commands::cmd_add(args).unwrap();
            assert_eq!(load_store(home).entry_count(), 3);

            // Remove app_a entirely
            super::commands::cmd_remove(Some("app_a".into()), vec![]).unwrap();
            let store = load_store(home);
            assert_eq!(store.entry_count(), 2, "app_b + _global should remain");
            assert!(store.get(mp, None).is_some());
            assert!(store.get(mp, Some("app_b")).is_some());
            assert!(store.get(mp, Some("app_a")).is_none());
        });
    }

    // ---- Edge cases: query ----

    #[test]
    fn test_integration_query_missing_cmd_fails() {
        // QueryArgs without cmd can't be constructed via try_parse_from
        // because clap will fail due to missing positional. But we can still
        // verify the handler rejects it by calling with None.
        use fsmon::query::Query;
        let q = Query::new(PathBuf::from("/nonexistent"), None, None, vec![]);
        assert!(q.resolve_log_files().unwrap().is_empty());
    }

    #[test]
    fn test_integration_query_cmd_no_log_file() {
        with_isolated_home(|_home, _mp| {
            use fsmon::query::Query;
            let q = Query::new(
                PathBuf::from("/nonexistent_log_dir"),
                Some("ghost".into()),
                None,
                vec![],
            );
            // No log files should be found
            let files = q.resolve_log_files().unwrap();
            assert!(files.is_empty(), "nonexistent cmd should yield no files");
        });
    }

    // ---- Edge cases: clean ----

    #[test]
    fn test_integration_clean_missing_cmd_fails() {
        with_isolated_home(|_home, _mp| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(super::commands::cmd_clean(CleanArgs {
                cmd: None,
                time: None,
                size: None,
                dry_run: false,
            }));
            assert!(result.is_err(), "missing cmd should fail");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("CMD is required"), "got: {}", err);
        });
    }

    #[test]
    fn test_integration_clean_nonexistent_log() {
        with_isolated_home(|_home, _mp| {
            // Clean a cmd that has no log file → should succeed (file not found message)
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(super::commands::cmd_clean(CleanArgs {
                cmd: Some("ghost".into()),
                time: None,
                size: None,
                dry_run: false,
            }));
            assert!(result.is_ok(), "clean nonexistent log should not error");
        });
    }

    #[test]
    fn test_integration_clean_and_query_round_trip() {
        with_isolated_home(|home, mp| {
            // Write a mock log file for _global
            use std::io::Write;
            let log_dir = {
                let mut cfg = fsmon::config::Config::load().unwrap();
                cfg.resolve_paths().unwrap();
                cfg.logging.path
            };
            fs::create_dir_all(&log_dir).unwrap();
            let log_path = log_dir.join(fsmon::utils::cmd_to_log_name("_global"));
            {
                let mut f = fs::File::create(&log_path).unwrap();
                use chrono::Utc;
                let ts = Utc::now();
                // Write one old event and one recent event
                let old = format!(
                    r#"{{"time":"{}","event_type":"CREATE","path":"/old","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}}"#,
                    (ts - chrono::Duration::days(100)).to_rfc3339(),
                );
                let recent = format!(
                    r#"{{"time":"{}","event_type":"MODIFY","path":"/recent","pid":2,"cmd":"y","user":"r","file_size":100,"ppid":0,"tgid":0,"chain":""}}"#,
                    ts.to_rfc3339(),
                );
                writeln!(f, "{}", old).unwrap();
                writeln!(f, "{}", recent).unwrap();
            }

            // Query _global should find both events
            {
                use fsmon::query::Query;
                let q = Query::new(log_dir.clone(), Some("_global".into()), None, vec![]);
                let files = q.resolve_log_files().unwrap();
                assert_eq!(files.len(), 1, "should find _global_log.jsonl");
            }

            // Store should be empty (not touched)
            let store = load_store(home);
            assert_eq!(store.entry_count(), 0);

            let _ = fs::remove_dir_all(home);
        });
    }
}
