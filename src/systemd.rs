use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SERVICE_NAME: &str = "fsmon";
const SERVICE_TEMPLATE: &str = r#"[Unit]
Description=fsmon filesystem monitor
After=network.target

[Service]
Type=simple
ExecStart=EXEC_START_PLACEHOLDER
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

fn get_service_file_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{}.service", SERVICE_NAME))
}

pub fn install(
    monitor_paths: &[PathBuf],
    log_file: Option<&PathBuf>,
    force: bool,
    protect_system: Option<&str>,
    protect_home: Option<&str>,
    read_write_paths: Option<&[String]>,
    private_tmp: Option<&str>,
) -> Result<()> {
    // Check if running as root
    if !is_root() {
        bail!("Installation requires root privileges. Please run with sudo.");
    }

    let service_file = get_service_file_path();
    if service_file.exists() {
        if force {
            // Force mode: uninstall existing service first
            println!("Service already exists, force mode enabled. Reinstalling...");
            uninstall_inner()?;
        } else {
            bail!(
                "Service already installed at {}. Use 'uninstall' first or '--force' to overwrite.",
                service_file.display()
            );
        }
    }

    // Detect current binary path
    let exe_path = env::current_exe()
        .context("Failed to detect current executable path")?
        .canonicalize()
        .context("Failed to resolve executable path")?;

    // Build ExecStart command
    let mut exec_args = vec![exe_path.display().to_string(), "monitor".to_string()];
    for path in monitor_paths {
        exec_args.push(path.display().to_string());
    }
    if let Some(log) = log_file {
        exec_args.push("-o".to_string());
        exec_args.push(log.display().to_string());
    }

    // Generate service file with detected binary path and security options
    let exec_start = exec_args.join(" ");
    let protect_system = protect_system.unwrap_or("strict");
    let protect_home = protect_home.unwrap_or("read-only");
    let read_write_paths = read_write_paths
        .map(|v| v.join(" "))
        .unwrap_or_else(|| "/var/log".to_string());
    let private_tmp = private_tmp.unwrap_or("yes");

    let service_content = SERVICE_TEMPLATE
        .replace("EXEC_START_PLACEHOLDER", &exec_start)
        .replace("{PROTECT_SYSTEM}", protect_system)
        .replace("{PROTECT_HOME}", protect_home)
        .replace("{READ_WRITE_PATHS}", &read_write_paths)
        .replace("{PRIVATE_TMP}", private_tmp);
    fs::write(&service_file, &service_content)
        .with_context(|| format!("Failed to write service file to {}", service_file.display()))?;

    // Reload systemd
    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!("Service installed to {}", service_file.display());
    println!("Binary path: {}", exe_path.display());
    println!("To start: systemctl start {}", SERVICE_NAME);
    println!("To enable on boot: systemctl enable {}", SERVICE_NAME);
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if !is_root() {
        bail!("Uninstallation requires root privileges. Please run with sudo.");
    }

    uninstall_inner()
}

fn uninstall_inner() -> Result<()> {
    let service_file = get_service_file_path();
    if !service_file.exists() {
        return Ok(());
    }

    // Stop and disable if running
    let _ = Command::new("systemctl")
        .args(["stop", SERVICE_NAME])
        .output();
    let _ = Command::new("systemctl")
        .args(["disable", SERVICE_NAME])
        .output();

    // Remove service file
    fs::remove_file(&service_file)
        .with_context(|| format!("Failed to remove service file {}", service_file.display()))?;

    // Reload systemd
    Command::new("systemctl")
        .args(["daemon-reload"])
        .output()
        .context("Failed to reload systemd daemon")?;

    println!("Service uninstalled");
    Ok(())
}

pub fn status() -> Result<String> {
    let output = Command::new("systemctl")
        .args(["is-active", SERVICE_NAME])
        .output()
        .context("Failed to check service status")?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn stop() -> Result<()> {
    if !is_root() {
        bail!("Stopping service requires root privileges. Please run with sudo.");
    }

    Command::new("systemctl")
        .args(["stop", SERVICE_NAME])
        .output()
        .context("Failed to stop service")?;

    println!("Service stopped");
    Ok(())
}

pub fn start() -> Result<()> {
    if !is_root() {
        bail!("Starting service requires root privileges. Please run with sudo.");
    }

    Command::new("systemctl")
        .args(["start", SERVICE_NAME])
        .output()
        .context("Failed to start service")?;

    println!("Service started");
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
