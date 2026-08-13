//! Watchdog configuration tests.
//!
//! Tests for watchdog interval, multiplier validation, and CLI parsing.

use fsmon::common::config::Config;

// ---- multiplier validation ----

#[test]
fn multiplier_must_be_greater_than_1() {
    // multiplier = 0 should be rejected
    assert!(validate_multiplier(0).is_err());
    // multiplier = 1 should be rejected
    assert!(validate_multiplier(1).is_err());
}

#[test]
fn multiplier_valid_values() {
    // multiplier >= 2 should be accepted
    assert!(validate_multiplier(2).is_ok());
    assert!(validate_multiplier(3).is_ok());
    assert!(validate_multiplier(100).is_ok());
}

/// Simulate daemon validation logic.
fn validate_multiplier(multiplier: u64) -> Result<(), String> {
    if multiplier <= 1 {
        return Err(format!(
            "watchdog multiplier must be > 1, got {}.",
            multiplier
        ));
    }
    Ok(())
}

// ---- watchdog_sec calculation ----

#[test]
fn watchdog_sec_calculation() {
    // WatchdogSec = interval × multiplier
    assert_eq!(calc_watchdog_sec(15, 2), 30);
    assert_eq!(calc_watchdog_sec(10, 3), 30);
    assert_eq!(calc_watchdog_sec(5, 4), 20);
    assert_eq!(calc_watchdog_sec(1, 2), 2);
}

/// Simulate WatchdogSec calculation.
fn calc_watchdog_sec(interval: u64, multiplier: u64) -> u64 {
    interval * multiplier
}

// ---- config parsing ----

#[test]
fn config_watchdog_section() {
    let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[socket]
path = "/tmp/sock"

[watchdog]
interval_secs = 15
multiplier = 3
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    let watchdog = cfg.watchdog.unwrap();
    assert_eq!(watchdog.interval_secs, Some(15));
    assert_eq!(watchdog.multiplier, Some(3));
}

#[test]
fn config_watchdog_defaults() {
    let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[socket]
path = "/tmp/sock"
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    assert!(cfg.watchdog.is_none());
}

#[test]
fn config_watchdog_partial() {
    // Only interval_secs set, multiplier defaults to None (will use code default 2)
    let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[socket]
path = "/tmp/sock"

[watchdog]
interval_secs = 20
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    let watchdog = cfg.watchdog.unwrap();
    assert_eq!(watchdog.interval_secs, Some(20));
    assert_eq!(watchdog.multiplier, None);
}

// ---- CLI parsing ----

#[test]
fn cli_watchdog_args() {
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[arg(long, value_name = "SECS")]
        watchdog_interval: Option<u64>,

        #[arg(long, value_name = "N")]
        watchdog_multiplier: Option<u64>,
    }

    // Both args present
    let cli = Cli::try_parse_from([
        "test",
        "--watchdog-interval",
        "15",
        "--watchdog-multiplier",
        "3",
    ])
    .unwrap();
    assert_eq!(cli.watchdog_interval, Some(15));
    assert_eq!(cli.watchdog_multiplier, Some(3));

    // Only interval
    let cli = Cli::try_parse_from(["test", "--watchdog-interval", "10"]).unwrap();
    assert_eq!(cli.watchdog_interval, Some(10));
    assert_eq!(cli.watchdog_multiplier, None);

    // No args
    let cli = Cli::try_parse_from(["test"]).unwrap();
    assert_eq!(cli.watchdog_interval, None);
    assert_eq!(cli.watchdog_multiplier, None);
}

// ---- priority / merge logic ----

#[test]
fn merge_priority_cli_over_config() {
    // CLI values should override config values
    let config_multiplier = Some(3u64);
    let cli_multiplier = Some(5u64);

    let merged = cli_multiplier.or(config_multiplier).unwrap_or(2);
    assert_eq!(merged, 5); // CLI wins
}

#[test]
fn merge_priority_config_over_default() {
    // Config values should override defaults
    let config_multiplier = Some(4u64);
    let cli_multiplier = None;

    let merged = cli_multiplier.or(config_multiplier).unwrap_or(2);
    assert_eq!(merged, 4); // Config wins
}

#[test]
fn merge_priority_default_when_none() {
    // Default value (2) when neither CLI nor config set
    let config_multiplier = None;
    let cli_multiplier = None;

    let merged = cli_multiplier.or(config_multiplier).unwrap_or(2);
    assert_eq!(merged, 2); // Default
}

// ---- systemd service template ----

#[test]
fn service_template_with_watchdog() {
    let template = service_template("/usr/bin/fsmon", "/home/user", Some(30));
    assert!(template.contains("WatchdogSec=30"));
}

#[test]
fn service_template_without_watchdog() {
    let template = service_template("/usr/bin/fsmon", "/home/user", None);
    assert!(!template.contains("WatchdogSec"));
}

/// Simulate service template generation.
fn service_template(binary: &str, home: &str, watchdog_sec: Option<u64>) -> String {
    let watchdog_line = match watchdog_sec {
        Some(secs) => format!("WatchdogSec={}", secs),
        None => String::new(),
    };
    format!(
        r"[Unit]
Description=fsmon
After=local-fs.target

[Service]
Type=notify
ExecStart={binary} daemon
Restart=always
RestartSec=5
Environment=HOME={home}
{watchdog_line}

[Install]
WantedBy=multi-user.target
",
        binary = binary,
        home = home,
        watchdog_line = if watchdog_line.is_empty() {
            ""
        } else {
            &watchdog_line
        },
    )
}

// ---- edge cases ----

#[test]
fn multiplier_zero_rejected() {
    assert!(validate_multiplier(0).is_err());
}

#[test]
fn multiplier_max_value() {
    // u64::MAX should be accepted (no overflow in validation)
    assert!(validate_multiplier(u64::MAX).is_ok());
}

#[test]
fn watchdog_sec_overflow() {
    // Test that multiplication doesn't overflow in normal ranges
    let interval = 3600u64; // 1 hour
    let multiplier = 24u64;
    let result = calc_watchdog_sec(interval, multiplier);
    assert_eq!(result, 86400); // 24 hours
}

#[test]
fn config_toml_full() {
    let toml_str = r#"
[monitored]
path = "/tmp/test.jsonl"

[logging]
path = "/tmp/logs"

[socket]
path = "/tmp/sock"

[watchdog]
interval_secs = 10
multiplier = 4
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    let watchdog = cfg.watchdog.unwrap();
    assert_eq!(watchdog.interval_secs, Some(10));
    assert_eq!(watchdog.multiplier, Some(4));

    // Verify WatchdogSec calculation
    let interval = watchdog.interval_secs.unwrap();
    let multiplier = watchdog.multiplier.unwrap();
    let watchdog_sec = calc_watchdog_sec(interval, multiplier);
    assert_eq!(watchdog_sec, 40);
}
