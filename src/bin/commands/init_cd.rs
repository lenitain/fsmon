use anyhow::{Context, Result, ensure};
use std::path::Path;
use std::process;

pub fn cmd_init(service: bool) -> Result<()> {
    fsmon::config::Config::init_dirs()?;
    if service {
        install_service()?;
    }
    Ok(())
}

fn service_template(binary: &str, home: &str) -> String {
    format!(
        r#"[Unit]
Description=fsmon - File System Change Monitor
Documentation=man:fsmon(1)
After=local-fs.target

[Service]
Type=simple
ExecStart={binary} daemon
Restart=always
RestartSec=5
Environment=HOME={home}

[Install]
WantedBy=multi-user.target
"#,
        binary = binary,
        home = home,
    )
}

fn install_service() -> Result<()> {
    // Must be root to write to /etc/systemd/system/
    ensure!(
        nix::unistd::geteuid().is_root(),
        "Installing a systemd service requires root privileges.\n\
         Try: sudo fsmon init --service"
    );

    // Find the binary path
    let binary = std::env::current_exe()
        .context("Failed to determine fsmon binary path")?
        .to_string_lossy()
        .to_string();

    // Resolve the original user's home directory
    let (uid, _gid) = fsmon::config::resolve_uid_gid();
    let home = fsmon::config::resolve_home(uid)
        .context("Failed to resolve home directory")?
        .to_string_lossy()
        .to_string();

    let content = service_template(&binary, &home);

    let service_path = Path::new("/etc/systemd/system/fsmon.service");
    fsmon::config::chown_to_original_user(
        service_path
            .parent()
            .expect("/etc/systemd/system should exist"),
    );

    std::fs::write(service_path, content.as_bytes())
        .with_context(|| format!("Failed to write {}", service_path.display()))?;

    eprintln!("Created systemd service: {}", service_path.display());

    // Run systemctl daemon-reload
    let reload_status = process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .context("Failed to run systemctl daemon-reload")?;

    if !reload_status.success() {
        eprintln!(
            "[WARNING] systemctl daemon-reload exited with status: {}",
            reload_status
        );
    }

    eprintln!();
    eprintln!("Service installed. To start now:");
    eprintln!("  sudo systemctl enable --now fsmon");
    eprintln!();
    eprintln!("To check status:");
    eprintln!("  sudo systemctl status fsmon");
    eprintln!();
    eprintln!("To view logs:");
    eprintln!("  journalctl -u fsmon -f");

    Ok(())
}

pub fn cmd_cd() -> Result<()> {
    let mut cfg = fsmon::config::Config::load()?;
    cfg.resolve_paths()?;
    let dir = cfg.logging.path.clone();

    if !dir.exists() {
        eprintln!("Log directory does not exist yet. Run 'fsmon init' first.");
        process::exit(1);
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    eprintln!("Entering fsmon log directory (type 'exit' to return)...");
    eprintln!("  {}", dir.display());
    eprintln!();

    let status = process::Command::new(&shell)
        .current_dir(&dir)
        .status()
        .with_context(|| format!("Failed to start shell: {}", shell))?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        process::exit(code);
    }

    Ok(())
}
