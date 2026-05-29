use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
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
Type=notify
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
    let uid = fsmon::config::resolve_uid();
    let home = fsmon::config::resolve_home(uid)
        .context("Failed to resolve home directory")?
        .to_string_lossy()
        .to_string();

    let content = service_template(&binary, &home);

    let service_path = Path::new("/etc/systemd/system/fsmon.service");
    if service_path.exists() {
        eprintln!("Exists systemd service: {}", service_path.display());
        eprintln!("  (delete it first if you need to regenerate: sudo rm {})", service_path.display());
        return Ok(());
    }

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

pub fn cmd_cd(monitored: bool) -> Result<()> {
    let dir = if monitored {
        // cd to first configured monitored path
        let mut cfg = fsmon::config::Config::load()?;
        cfg.resolve_paths()?;
        let store = fsmon::monitored::Monitored::load(&cfg.monitored.path)?;
        let entries = store.flatten();
        let first = entries.first().ok_or_else(|| {
            anyhow::anyhow!(
                "No monitored paths configured. Add one first: fsmon add <cmd> --path <dir>"
            )
        })?;
        first.path.clone()
    } else {
        // -l: cd to log directory (identical to old `fsmon cd`)
        let mut cfg = fsmon::config::Config::load()?;
        cfg.resolve_paths()?;
        cfg.logging.path.unwrap_or_else(|| {
            let home = fsmon::config::guess_home();
            PathBuf::from(format!("{}/.local/state/fsmon", home))
        })
    };

    if !dir.exists() {
        std::fs::create_dir_all(&dir).with_context(|| {
            format!("Failed to create directory: {}", dir.display())
        })?;
        fsmon::config::chown_to_original_user(&dir);
        eprintln!("Created directory: {}", dir.display());
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let label = if monitored { "monitored path" } else { "log" };
    eprintln!("Entering fsmon {} directory (type 'exit' to return)...", label);
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
