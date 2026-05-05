use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;

const SERVICE_NAME: &str = "fsmon";

fn get_service_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{SERVICE_NAME}.service"))
}

const SERVICE_TEMPLATE: &str = r#"[Unit]
Description=fsmon filesystem monitor
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=EXEC_START_PLACEHOLDER daemon
Restart=on-failure
RestartPreventExitStatus=78
RestartSec=5
RuntimeDirectory=fsmon
RuntimeDirectoryMode=0755
StandardOutput=journal
StandardError=journal
CapabilityBoundingSet=CAP_SYS_ADMIN
AmbientCapabilities=CAP_SYS_ADMIN

[Install]
WantedBy=multi-user.target
"#;

pub fn install(force: bool) -> Result<()> {
    if !is_root() {
        bail!("Installation requires root privileges. Please run with sudo.");
    }

    let service_file = get_service_path();
    if service_file.exists() {
        if force {
            println!(
                "Service already installed at {}, force mode enabled. Reinstalling...",
                service_file.display()
            );
        } else {
            bail!(
                "Service already installed at {}. Use 'uninstall' first or '--force' to overwrite.",
                service_file.display()
            );
        }
    }

    let exe_path = env::current_exe()
        .context("Failed to detect current executable path")?
        .canonicalize()
        .context("Failed to resolve executable path")?;

    let service_content = SERVICE_TEMPLATE
        .replace("EXEC_START_PLACEHOLDER", &exe_path.display().to_string());

    fs::write(&service_file, &service_content)
        .with_context(|| format!("Failed to write service file to {}", service_file.display()))?;

    // Create /etc/fsmon/ if not exists
    let config_dir = Config::default_config_path()
        .parent()
        .context("Config path has no parent")?
        .to_path_buf();
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create config directory {}", config_dir.display()))?;

    // Generate default config if not exists
    Config::generate_default()?;

    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!("Service installed to {}", service_file.display());
    println!("fsmon path: {}", exe_path.display());
    println!("Usage:");
    println!("  sudo systemctl enable fsmon --now");
    println!("  sudo systemctl start fsmon");
    println!("  sudo systemctl stop fsmon");
    println!("  sudo systemctl status fsmon");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if !is_root() {
        bail!("Uninstallation requires root privileges. Please run with sudo.");
    }
    uninstall_inner()
}

fn uninstall_inner() -> Result<()> {
    let service_file = get_service_path();
    if !service_file.exists() {
        return Ok(());
    }

    fs::remove_file(&service_file)
        .with_context(|| format!("Failed to remove service file {}", service_file.display()))?;

    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!("Service uninstalled from {}", service_file.display());
    println!(
        "Note: The running service was not stopped. Run 'systemctl stop fsmon' to stop it."
    );
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
