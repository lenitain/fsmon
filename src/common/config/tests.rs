use super::*;
use std::fs;
use temp_env;

fn unique_home_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = std::process::id();
    let thread = std::thread::current().id();
    std::env::temp_dir().join(format!("fsmon_home_test_{}_{:?}_{}", id, thread, n))
}

/// Mutex to prevent concurrent env var manipulation across tests
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Override HOME for test isolation.
/// Uses catch_unwind to prevent mutex poisoning on panic.
fn with_isolated_home(f: impl FnOnce(&Path)) {
    let lock = ENV_LOCK.lock().unwrap();
    let dir = unique_home_dir();
    let _ = fs::remove_dir_all(&dir);
    let home_val = dir.to_string_lossy().to_string();

    temp_env::with_vars(
        [
            ("HOME", Some(home_val.as_str())),
            ("XDG_CONFIG_HOME", None::<&str>),
            ("SUDO_UID", None::<&str>),
        ],
        || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
            let _ = fs::remove_dir_all(&dir);
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        },
    );

    drop(lock);
}

#[test]
fn test_load_returns_default_when_no_file() {
    with_isolated_home(|_| {
        let cfg = Config::load().unwrap();
        assert_eq!(
            cfg.monitored.path.to_string_lossy(),
            "~/.local/share/fsmon/monitored.jsonl"
        );
        assert_eq!(
            cfg.logging.path,
            Some(PathBuf::from("~/.local/state/fsmon"))
        );
    });
}

#[test]
fn test_load_reads_existing_file() {
    with_isolated_home(|_| {
        // Write a config file
        let config_path = Config::path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let content = r#"[monitored]
path = "/custom/monitored.jsonl"

[logging]
path = "/custom/logs"
"#;
        fs::write(&config_path, content).unwrap();

        let cfg = Config::load().unwrap();
        assert_eq!(cfg.monitored.path, PathBuf::from("/custom/monitored.jsonl"));
        assert_eq!(cfg.logging.path, Some(PathBuf::from("/custom/logs")));
    });
}

#[test]
fn test_load_invalid_config_returns_error_for_bad_toml() {
    with_isolated_home(|_| {
        let config_path = Config::path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        // Write invalid TOML (not comment-only)
        fs::write(&config_path, "garbage [[[").unwrap();

        // Should error
        assert!(Config::load().is_err());

        // File should be untouched
        let content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(content.trim(), "garbage [[[");
    });
}

#[test]
fn test_load_empty_file_returns_defaults() {
    with_isolated_home(|_| {
        let config_path = Config::path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        // Empty file or comment-only → return defaults (same as no file)
        fs::write(&config_path, "").unwrap();
        let cfg = Config::load().unwrap();
        assert_eq!(
            cfg.monitored.path.to_string_lossy(),
            "~/.local/share/fsmon/monitored.jsonl"
        );
    });
}

#[test]
fn test_resolve_paths_expands_tilde() {
    with_isolated_home(|home| {
        let mut cfg = Config::default();
        cfg.logging.path = Some(PathBuf::from("~/.local/state/fsmon"));
        cfg.resolve_paths().unwrap();

        let home_str = home.to_string_lossy();
        assert!(
            cfg.monitored.path.to_string_lossy().starts_with(&*home_str),
            "monitored.path should start with home dir: {} vs {}",
            cfg.monitored.path.display(),
            home_str
        );
        assert!(
            cfg.logging
                .path
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .starts_with(&*home_str),
            "logging.path should start with home dir"
        );
    });
}

#[test]
fn test_config_path_uses_xdg_config_home() {
    let _lock = ENV_LOCK.lock().unwrap();

    temp_env::with_vars(
        [
            ("XDG_CONFIG_HOME", Some("/custom/xdg/config")),
            ("HOME", Some("/home/test")),
        ],
        || {
            let path = Config::path();
            assert!(
                path.to_string_lossy()
                    .contains("/custom/xdg/config/fsmon/fsmon.toml")
            );

            temp_env::with_var_unset("XDG_CONFIG_HOME", || {
                let path = Config::path();
                assert!(
                    path.to_string_lossy()
                        .contains("/home/test/.config/fsmon/fsmon.toml")
                );
            });
        },
    );
}

#[test]
fn test_init_dirs_creates_directories() {
    with_isolated_home(|home| {
        Config::init_dirs().unwrap();

        let log_dir = home.join(".local/state/fsmon");
        let monitored_dir = home.join(".local/share/fsmon");
        let config_file = home.join(".config/fsmon/fsmon.toml");

        assert!(
            !log_dir.exists(),
            "log dir should not exist (init only creates config)"
        );
        assert!(
            !monitored_dir.exists(),
            "monitored dir should not exist (init only creates config)"
        );
        assert!(
            config_file.exists(),
            "config file should be created by init"
        );
        // Config should load from the new file (comment-only → defaults)
        let cfg = Config::load().unwrap();
        assert_eq!(
            cfg.monitored.path.to_string_lossy(),
            "~/.local/share/fsmon/monitored.jsonl"
        );
    });
}

#[test]
fn test_init_dirs_uses_config_when_present() {
    with_isolated_home(|home| {
        let config_path = Config::path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        // Write config with default paths
        fs::write(
            &config_path,
            r#"[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
path = "~/.local/state/fsmon"
"#,
        )
        .unwrap();

        Config::init_dirs().unwrap();

        let log_dir = home.join(".local/state/fsmon");
        let monitored_dir = home.join(".local/share/fsmon");
        assert!(
            !log_dir.exists(),
            "log dir should not exist (init only creates config)"
        );
        assert!(
            !monitored_dir.exists(),
            "monitored dir should not exist (init only creates config)"
        );
    });
}

#[test]
fn test_init_dirs_uses_custom_config_paths() {
    with_isolated_home(|home| {
        let config_path = Config::path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let custom_log = home.join("my_logs");
        let custom_monitored_dir = home.join("my_data");
        let _custom_monitored_file = custom_monitored_dir.join("paths.jsonl");
        let content = format!(
            r#"[monitored]
path = "{}/my_data/paths.jsonl"

[logging]
path = "{}/my_logs"
"#,
            home.to_string_lossy(),
            home.to_string_lossy(),
        );
        fs::write(&config_path, content).unwrap();

        Config::init_dirs().unwrap();

        assert!(!custom_log.exists(), "init only creates config, not dirs");
        assert!(
            !custom_monitored_dir.exists(),
            "init only creates config, not dirs"
        );
    });
}

#[test]
fn test_resolve_uid_no_sudo() {
    // Without SUDO_UID, resolve_uid returns our own UID
    let _lock = ENV_LOCK.lock().unwrap();
    temp_env::with_var_unset("SUDO_UID", || {
        let uid = resolve_uid();
        assert_eq!(uid, nix::unistd::geteuid().as_raw());
    });
}

#[test]
fn test_expand_tilde_basic() {
    assert_eq!(
        expand_tilde(Path::new("~/foo/bar"), "/home/user"),
        PathBuf::from("/home/user/foo/bar")
    );
    assert_eq!(
        expand_tilde(Path::new("~"), "/home/user"),
        PathBuf::from("/home/user")
    );
    assert_eq!(
        expand_tilde(Path::new("/absolute/path"), "/home/user"),
        PathBuf::from("/absolute/path")
    );
}

#[test]
fn test_cache_config_defaults() {
    let r = ResolvedCacheConfig::default();
    assert_eq!(r.dir_capacity, crate::common::fid_parser::DIR_CACHE_CAP);
    assert_eq!(
        r.dir_ttl_secs,
        crate::common::fid_parser::DIR_CACHE_TTL_SECS
    );
    assert_eq!(
        r.file_size_capacity,
        crate::common::fid_parser::FILE_SIZE_CACHE_CAP
    );
    assert_eq!(
        r.proc_ttl_secs,
        crate::common::proc_cache::PROC_STORE_TTL_SECS
    );
    assert_eq!(r.buffer_size, 4096 * 8);
}

#[test]
fn test_cache_config_resolve_with_cli_override() {
    // Config empty, CLI overrides → CLI values win
    let cfg = CacheConfig {
        dir_capacity: None,
        dir_ttl_secs: None,
        file_size_capacity: None,
        proc_ttl_secs: None,
        channel_capacity: None,
        subscribe_buf: None,
        buffer_size: None,
    };
    let cli = CliCacheOverride {
        dir_capacity: Some(50000),
        dir_ttl_secs: Some(7200),
        file_size_capacity: Some(5000),
        proc_ttl_secs: Some(300),
        buffer_size: Some(65536),
        channel_capacity: None,
        subscribe_buf: None,
    };
    let r = cfg.resolve_with_cli(&cli);
    assert_eq!(r.dir_capacity, 50000);
    assert_eq!(r.dir_ttl_secs, 7200);
    assert_eq!(r.file_size_capacity, 5000);
    assert_eq!(r.proc_ttl_secs, 300);
    assert_eq!(r.buffer_size, 65536);
}

#[test]
fn test_cache_config_resolve_config_over_default() {
    // Config has values, CLI empty → config values win
    let cfg = CacheConfig {
        dir_capacity: Some(200000),
        dir_ttl_secs: None,
        file_size_capacity: Some(20000),
        proc_ttl_secs: None,
        channel_capacity: None,
        subscribe_buf: None,
        buffer_size: None,
    };
    let cli = CliCacheOverride::default();
    let r = cfg.resolve_with_cli(&cli);
    assert_eq!(r.dir_capacity, 200000);
    assert_eq!(
        r.dir_ttl_secs,
        crate::common::fid_parser::DIR_CACHE_TTL_SECS
    );
    assert_eq!(r.file_size_capacity, 20000);
    assert_eq!(
        r.proc_ttl_secs,
        crate::common::proc_cache::PROC_STORE_TTL_SECS
    );
}

#[test]
fn test_cache_config_cli_highest_priority() {
    // Both config and CLI have values → CLI wins
    let cfg = CacheConfig {
        dir_capacity: Some(50000),
        dir_ttl_secs: Some(100),
        file_size_capacity: Some(500),
        proc_ttl_secs: Some(50),
        channel_capacity: None,
        subscribe_buf: None,
        buffer_size: None,
    };
    let cli = CliCacheOverride {
        dir_capacity: Some(99999),
        dir_ttl_secs: None,
        file_size_capacity: Some(999),
        proc_ttl_secs: None,
        buffer_size: None,
        channel_capacity: None,
        subscribe_buf: None,
    };
    let r = cfg.resolve_with_cli(&cli);
    assert_eq!(r.dir_capacity, 99999); // CLI wins
    assert_eq!(r.dir_ttl_secs, 100); // Config (CLI didn't set)
    assert_eq!(r.file_size_capacity, 999); // CLI wins
    assert_eq!(r.proc_ttl_secs, 50); // Config (CLI didn't set)
}

#[test]
fn test_cache_config_toml_parsing() {
    // Verify that the TOML config can be parsed with [cache] section
    let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[cache]
dir_capacity = 123456
dir_ttl_secs = 7200
file_size_capacity = 5000
proc_ttl_secs = 300
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    let cache = cfg.cache.expect("cache section should be parsed");
    assert_eq!(cache.dir_capacity, Some(123456));
    assert_eq!(cache.dir_ttl_secs, Some(7200));
    assert_eq!(cache.file_size_capacity, Some(5000));
    assert_eq!(cache.proc_ttl_secs, Some(300));
}
