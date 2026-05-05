use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::config;

const SERVICE_NAME: &str = "fsmon";

fn get_template_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{}@.service", SERVICE_NAME))
}

fn get_instance_config_path(name: &str) -> PathBuf {
    PathBuf::from(config::INSTANCE_CONFIG_DIR).join(format!("fsmon-{}.toml", name))
}

const SERVICE_TEMPLATE: &str = r#"[Unit]
Description=fsmon filesystem monitor (%i)
After=network.target

[Service]
Type=simple
ExecStart=EXEC_START_PLACEHOLDER monitor --instance %i
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

# Security hardening
NoNewPrivileges=yes
ProtectSystem={PROTECT_SYSTEM}
ProtectHome={PROTECT_HOME}
ReadWritePaths={READ_WRITE_PATHS}
PrivateTmp={PRIVATE_TMP}

[Install]
WantedBy=multi-user.target
"#;

pub fn install(
    force: bool,
    protect_system: Option<&str>,
    protect_home: Option<&str>,
    read_write_paths: Option<&[String]>,
    private_tmp: Option<&str>,
) -> Result<()> {
    if !is_root() {
        bail!("Installation requires root privileges. Please run with sudo.");
    }

    let service_file = get_template_path();
    if service_file.exists() {
        if force {
            println!("Template already exists, force mode enabled. Reinstalling...");
        } else {
            bail!(
                "Template already installed at {}. Use 'uninstall' first or '--force' to overwrite.",
                service_file.display()
            );
        }
    }

    let exe_path = env::current_exe()
        .context("Failed to detect current executable path")?
        .canonicalize()
        .context("Failed to resolve executable path")?;

    let protect_system_val = protect_system.unwrap_or("strict");
    let protect_home_val = protect_home.unwrap_or("read-only");
    let read_write_paths_val = read_write_paths
        .map(|v| v.join(" "))
        .unwrap_or_else(|| "/var/log".to_string());
    let private_tmp_val = private_tmp.unwrap_or("yes");

    let service_content = SERVICE_TEMPLATE
        .replace("EXEC_START_PLACEHOLDER", &exe_path.display().to_string())
        .replace("{PROTECT_SYSTEM}", protect_system_val)
        .replace("{PROTECT_HOME}", protect_home_val)
        .replace("{READ_WRITE_PATHS}", &read_write_paths_val)
        .replace("{PRIVATE_TMP}", private_tmp_val);

    fs::write(&service_file, &service_content)
        .with_context(|| format!("Failed to write service file to {}", service_file.display()))?;

    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!("Service template installed to {}", service_file.display());
    println!("Binary path: {}", exe_path.display());
    println!("Usage: systemctl enable fsmon@INSTANCE_NAME --now");
    println!("       systemctl start fsmon@INSTANCE_NAME");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if !is_root() {
        bail!("Uninstallation requires root privileges. Please run with sudo.");
    }
    uninstall_inner()
}

fn uninstall_inner() -> Result<()> {
    let service_file = get_template_path();
    if !service_file.exists() {
        return Ok(());
    }

    let _ = fs::remove_file(&service_file)
        .with_context(|| format!("Failed to remove service file {}", service_file.display()));

    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!(
        "Service template uninstalled from {}",
        service_file.display()
    );
    println!(
        "Note: Running instances were not stopped. Run 'systemctl stop fsmon@<name>' for each."
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn enable_instance(
    name: &str,
    paths: &[PathBuf],
    output: Option<&PathBuf>,
    min_size: Option<&str>,
    types: Option<&str>,
    exclude: Option<&str>,
    all_events: bool,
    recursive: bool,
    force: bool,
) -> Result<()> {
    if !is_root() {
        bail!("Enabling instance requires root privileges. Please run with sudo.");
    }

    let template_path = get_template_path();
    if !template_path.exists() {
        bail!(
            "Service template not found at {}. Run 'fsmon install' first.",
            template_path.display()
        );
    }

    // Default log path if not specified: /var/log/fsmon/{name}.log
    let output = output
        .cloned()
        .or_else(|| Some(PathBuf::from("/var/log/fsmon").join(format!("{}.log", name))));

    // Build instance config
    let instance_cfg = crate::config::InstanceConfig {
        paths: paths.to_vec(),
        output,
        min_size: min_size.map(String::from),
        types: types.map(String::from),
        exclude: exclude.map(String::from),
        all_events: if all_events { Some(true) } else { None },
        recursive: if recursive { Some(true) } else { None },
    };

    crate::config::generate_instance_config(name, &instance_cfg, force)?;

    // Enable and start via systemd
    let unit_name = format!("{}@{}", SERVICE_NAME, name);

    let status = Command::new("systemctl")
        .args(["enable", &unit_name])
        .output()
        .with_context(|| format!("Failed to run systemctl enable {}", unit_name))?;
    if !status.status.success() {
        bail!(
            "systemctl enable {} failed:\n{}",
            unit_name,
            String::from_utf8_lossy(&status.stderr)
        );
    }
    println!("Enabled systemd unit: {}", unit_name);

    let status = Command::new("systemctl")
        .args(["start", &unit_name])
        .output()
        .with_context(|| format!("Failed to run systemctl start {}", unit_name))?;
    if !status.status.success() {
        // Non-fatal: print warning but don't fail
        eprintln!(
            "Warning: systemctl start {} failed:\n{}",
            unit_name,
            String::from_utf8_lossy(&status.stderr)
        );
    } else {
        println!("Started systemd unit: {}", unit_name);
    }

    Ok(())
}

pub fn disable_instance(name: &str) -> Result<()> {
    if !is_root() {
        bail!("Disabling instance requires root privileges. Please run with sudo.");
    }

    let unit_name = format!("{}@{}", SERVICE_NAME, name);

    let _ = Command::new("systemctl")
        .args(["stop", &unit_name])
        .output();

    let status = Command::new("systemctl")
        .args(["disable", &unit_name])
        .output()
        .with_context(|| format!("Failed to run systemctl disable {}", unit_name))?;
    if !status.status.success() {
        eprintln!(
            "Warning: systemctl disable {} failed:\n{}",
            unit_name,
            String::from_utf8_lossy(&status.stderr)
        );
    }
    println!("Disabled systemd unit: {}", unit_name);

    // Remove instance config
    let config_path = get_instance_config_path(name);
    if config_path.exists() {
        fs::remove_file(&config_path).with_context(|| {
            format!("Failed to remove instance config {}", config_path.display())
        })?;
        println!("Removed instance config: {}", config_path.display());
    }

    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
