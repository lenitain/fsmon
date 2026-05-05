use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SERVICE_NAME: &str = "fsmon";

fn get_template_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{}@.service", SERVICE_NAME))
}

const SERVICE_TEMPLATE: &str = r#"[Unit]
Description=fsmon filesystem monitor (%i)
After=network.target

[Service]
Type=simple
ExecStart=EXEC_START_PLACEHOLDER daemon
Restart=on-failure
RestartPreventExitStatus=78
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

    // Service runs the fsmon daemon
    let cli_path = exe_path.clone();

    let protect_system_val = protect_system.unwrap_or("strict");
    let protect_home_val = protect_home.unwrap_or("read-only");
    let read_write_paths_val = read_write_paths
        .map(|v| v.join(" "))
        .unwrap_or_else(|| "/var/log".to_string());
    let private_tmp_val = private_tmp.unwrap_or("yes");

    let service_content = SERVICE_TEMPLATE
        .replace("EXEC_START_PLACEHOLDER", &cli_path.display().to_string())
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
    println!("fsmon path: {}", cli_path.display());
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

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
