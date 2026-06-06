// Tests that require binary-internal types (Cli, Commands) or binary-internal
// functions (commands::cmd_add, cmd_remove, cmd_clean).

use super::*;
use clap::Parser;

// ---- DaemonArgs CLI parsing ----

#[test]
fn test_daemon_sync_interval() {
    let cli = Cli::try_parse_from(["fsmon", "daemon", "--sync-interval", "5"]).unwrap();
    match cli.command {
        Commands::Daemon { sync_interval, .. } => {
            assert_eq!(sync_interval, Some(5));
        }
        _ => panic!("expected Daemon"),
    }
}

#[test]
fn test_daemon_sync_interval_default() {
    let cli = Cli::try_parse_from(["fsmon", "daemon"]).unwrap();
    match cli.command {
        Commands::Daemon { sync_interval, .. } => {
            assert_eq!(sync_interval, None);
        }
        _ => panic!("expected Daemon"),
    }
}

// ---- Remove command (positional paths) ----

#[test]
fn test_remove_path() {
    let cli = Cli::try_parse_from(["fsmon", "remove", "--path", "/tmp"]).unwrap();
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
        Cli::try_parse_from(["fsmon", "remove", "--path", "/tmp", "--path", "/home"]).unwrap();
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
    let cli = Cli::try_parse_from(["fsmon", "remove", "nginx"]).unwrap();
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
    let cli = Cli::try_parse_from(["fsmon", "remove", "openclaw", "--path", "/tmp"]).unwrap();
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
    let cli = Cli::try_parse_from(["fsmon", "remove"]).unwrap();
    match cli.command {
        Commands::Remove { cmd, path } => {
            assert!(cmd.is_none());
            assert!(path.is_empty());
        }
        _ => panic!("expected Remove"),
    };
}

// ---- Cd CLI parsing ----

#[test]
fn test_cd_logging() {
    let cli = Cli::try_parse_from(["fsmon", "cd", "-l"]).unwrap();
    match cli.command {
        Commands::Cd { monitored, logging } => {
            assert!(!monitored);
            assert!(logging);
        }
        _ => panic!("expected Cd"),
    };
}

#[test]
fn test_cd_monitored() {
    let cli = Cli::try_parse_from(["fsmon", "cd", "-m"]).unwrap();
    match cli.command {
        Commands::Cd { monitored, logging } => {
            assert!(monitored);
            assert!(!logging);
        }
        _ => panic!("expected Cd"),
    };
}

#[test]
fn test_cd_no_args_error() {
    let result = Cli::try_parse_from(["fsmon", "cd"]);
    assert!(result.is_err(), "cd with no args should error");
}

#[test]
fn test_cd_both_args_error() {
    let result = Cli::try_parse_from(["fsmon", "cd", "-m", "-l"]);
    assert!(result.is_err(), "cd with both -m and -l should error");
}

// ---- Integration tests (require commands module) ----

use fsmon::common::config::Config;
use fsmon::common::monitored::Monitored;
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
fn with_isolated_home(f: impl FnOnce(&Path, &Path)) {
    let _lock = match ENV_LOCK.lock() {
        Ok(l) => l,
        Err(e) => e.into_inner(),
    };
    let dir = unique_temp_home();
    let _ = fs::remove_dir_all(&dir);
    let home_str = dir.to_string_lossy().to_string();

    let monitored_path = dir.join("monitored");
    fs::create_dir_all(&monitored_path).unwrap();

    temp_env::with_vars(
        [
            ("HOME", Some(home_str.as_str())),
            ("XDG_CONFIG_HOME", None::<&str>),
            ("SUDO_UID", None::<&str>),
        ],
        || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir, &monitored_path)));
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "openclaw", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from([
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref(), "-r"]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "myapp", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "myapp", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        assert_eq!(load_store(home).entry_count(), 2);

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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();

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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        let args = AddArgs::try_parse_from(["add", "myapp", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "fsmon", "--path", p.as_ref()]).unwrap();
        let result = super::commands::cmd_add(args);
        assert!(result.is_err(), "fsmon cmd should fail");
    });
}

#[test]
fn test_integration_add_duplicate_replaces() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref(), "-r"]).unwrap();
        super::commands::cmd_add(args).unwrap();
        assert_eq!(load_store(home).entry_count(), 1);

        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref(), "-s", ">1MB"])
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();

        let result = super::commands::cmd_remove(Some("wrong_cmd".into()), vec![mp.to_path_buf()]);
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
        let args = AddArgs::try_parse_from(["add", "_global", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        let args = AddArgs::try_parse_from(["add", "app_a", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        let args = AddArgs::try_parse_from(["add", "app_b", "--path", p.as_ref()]).unwrap();
        super::commands::cmd_add(args).unwrap();
        assert_eq!(load_store(home).entry_count(), 3);

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
    use fsmon::common::query::Query;
    let q = Query::new(PathBuf::from("/nonexistent"), None, None, vec![], false);
    assert!(q.resolve_log_files().unwrap().is_empty());
}

#[test]
fn test_integration_query_cmd_no_log_file() {
    with_isolated_home(|_home, _mp| {
        use fsmon::common::query::Query;
        let q = Query::new(
            PathBuf::from("/nonexistent_log_dir"),
            Some("ghost".into()),
            None,
            vec![],
            false,
        );
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
    with_isolated_home(|home, _mp| {
        use std::io::Write;
        let log_dir = {
            let mut cfg = fsmon::common::config::Config::load().unwrap();
            cfg.logging.path = Some(std::path::PathBuf::from("~/.local/state/fsmon"));
            cfg.resolve_paths().unwrap();
            cfg.logging.path.unwrap()
        };
        fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join(fsmon::common::utils::cmd_to_log_name("_global"));
        {
            let mut f = fs::File::create(&log_path).unwrap();
            use chrono::Utc;
            let ts = Utc::now();
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

        {
            use fsmon::common::query::Query;
            let q = Query::new(log_dir.clone(), Some("_global".into()), None, vec![], false);
            let files = q.resolve_log_files().unwrap();
            assert_eq!(files.len(), 1, "should find _global_log.jsonl");
        }

        let store = load_store(home);
        assert_eq!(store.entry_count(), 0);

        let _ = fs::remove_dir_all(home);
    });
}
