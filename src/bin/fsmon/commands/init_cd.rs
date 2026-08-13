use anyhow::{Context, Result, ensure};
use clap::CommandFactory;
use clap_complete::Shell;
use clap_complete_nushell::Nushell;
use std::path::{Path, PathBuf};
use std::process;

use fsmon::common::config::chown_to_original_user;
use fsmon::warning_log;

use crate::Cli;

/// 目标目录类型，用于 `fsmon cd` 命令
#[derive(Debug, Clone, Copy)]
pub enum CdTarget {
    /// 日志目录（默认）
    Log,
    /// 监控路径存储目录
    Monitored,
    /// 配置目录
    Config,
}

impl CdTarget {
    /// 从 bool 参数创建 CdTarget（向后兼容）
    pub fn from_args(monitored: bool, config: bool) -> Self {
        if config {
            CdTarget::Config
        } else if monitored {
            CdTarget::Monitored
        } else {
            CdTarget::Log
        }
    }
}

/// Initialize fsmon configuration and directories.
pub fn cmd_init(service: bool, completions: bool) -> Result<()> {
    fsmon::common::config::Config::init_dirs()?;
    if service || completions {
        eprintln!();
    }
    if service {
        install_service()?;
        if completions {
            eprintln!();
        }
    }
    if completions {
        generate_man_pages().context("Failed to generate man pages")?;
        eprintln!();
        generate_shell_completions().context("Failed to generate shell completions")?;
    }
    Ok(())
}

fn generate_man_pages() -> Result<()> {
    let home = fsmon::common::config::guess_home();
    let man_dir = PathBuf::from(format!("{}/.local/share/man/man1", home));
    std::fs::create_dir_all(&man_dir)
        .with_context(|| format!("Failed to create {}", man_dir.display()))?;
    chown_to_original_user(&man_dir);

    let base = man_dir.join("fsmon.1");
    if base.exists() {
        eprintln!("Exists man page: {}", base.display());
        return Ok(());
    }

    let cmd = Cli::command();

    let man = clap_mangen::Man::new(cmd.clone());
    let root_path = man
        .generate_to(&man_dir)
        .with_context(|| format!("Failed to write man page to {}", man_dir.display()))?;
    chown_to_original_user(&root_path);

    for sub in cmd.get_subcommands() {
        let man = clap_mangen::Man::new(sub.clone());
        let path = man.generate_to(&man_dir)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !stem.starts_with("fsmon-") {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("1");
            let new_path = path.with_file_name(format!("fsmon-{}.{}", stem, ext));
            std::fs::rename(&path, &new_path)?;
            chown_to_original_user(&new_path);
        } else {
            chown_to_original_user(&path);
        }
    }

    eprintln!("Generated man pages in {}", man_dir.display());
    Ok(())
}

fn service_template(binary: &str, home: &str, watchdog_sec: Option<u64>) -> String {
    let watchdog_line = match watchdog_sec {
        Some(secs) => format!("WatchdogSec={}", secs),
        None => String::new(),
    };
    format!(
        r"[Unit]
Description=fsmon - File System Change Monitor
Documentation=man:fsmon(1)
After=local-fs.target

[Service]
Type=notify
ExecStart={binary} daemon
Restart=always
RestartSec=5
RestartPreventExitStatus=2
StartLimitBurst=5
StartLimitIntervalSec=300
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
    let uid = fsmon::common::config::resolve_uid();
    let home = fsmon::common::config::resolve_home(uid)
        .context("Failed to resolve home directory")?
        .to_string_lossy()
        .to_string();

    // Load config to check watchdog settings
    let cfg = fsmon::common::config::Config::load()?;
    let watchdog_cfg = cfg.watchdog.as_ref();
    let watchdog_sec = watchdog_cfg.and_then(|w| {
        w.interval_secs.map(|interval| {
            let multiplier = w.multiplier.unwrap_or(2);
            interval * multiplier
        })
    });

    let content = service_template(&binary, &home, watchdog_sec);

    let service_path = Path::new("/etc/systemd/system/fsmon.service");
    if service_path.exists() {
        eprintln!("Exists systemd service: {}", service_path.display());
        eprintln!(
            "  (delete it first if you need to regenerate: sudo rm {})",
            service_path.display()
        );
        return Ok(());
    }

    fsmon::common::config::chown_to_original_user(
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
        warning_log!(
            "systemctl daemon-reload exited with status: {}",
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

fn generate_one_completion(dir: &Path, filename: &str, shell: Shell) -> Result<()> {
    let path = dir.join(filename);
    if path.exists() {
        eprintln!("Exists {} completions: {}", shell, path.display());
        return Ok(());
    }
    let mut file = std::fs::File::create(&path)?;
    clap_complete::generate(shell, &mut Cli::command(), "fsmon", &mut file);
    chown_to_original_user(&path);
    eprintln!("Generated {} completions: {}", shell, path.display());
    Ok(())
}

fn generate_one_completion_nushell(dir: &Path, filename: &str) -> Result<()> {
    let path = dir.join(filename);
    if path.exists() {
        eprintln!("Exists nushell completions: {}", path.display());
        return Ok(());
    }
    let mut file = std::fs::File::create(&path)?;
    clap_complete::generate(Nushell, &mut Cli::command(), "fsmon", &mut file);
    chown_to_original_user(&path);
    eprintln!("Generated nushell completions: {}", path.display());
    Ok(())
}

fn generate_shell_completions() -> Result<()> {
    let home = fsmon::common::config::guess_home();

    let fish_dir = PathBuf::from(format!("{}/.config/fish/completions", home));
    std::fs::create_dir_all(&fish_dir)
        .with_context(|| format!("Failed to create {}", fish_dir.display()))?;
    chown_to_original_user(&fish_dir);
    generate_one_completion(&fish_dir, "fsmon.fish", Shell::Fish)?;

    let bash_dir = PathBuf::from(format!("{}/.local/share/bash-completion/completions", home));
    std::fs::create_dir_all(&bash_dir)
        .with_context(|| format!("Failed to create {}", bash_dir.display()))?;
    chown_to_original_user(&bash_dir);
    generate_one_completion(&bash_dir, "fsmon", Shell::Bash)?;

    let zsh_dir = PathBuf::from(format!("{}/.local/share/zsh/site-functions", home));
    std::fs::create_dir_all(&zsh_dir)
        .with_context(|| format!("Failed to create {}", zsh_dir.display()))?;
    chown_to_original_user(&zsh_dir);
    generate_one_completion(&zsh_dir, "_fsmon", Shell::Zsh)?;

    let nu_dir = PathBuf::from(format!("{}/.local/share/nushell/completions", home));
    std::fs::create_dir_all(&nu_dir)
        .with_context(|| format!("Failed to create {}", nu_dir.display()))?;
    chown_to_original_user(&nu_dir);
    generate_one_completion_nushell(&nu_dir, "fsmon.nu")?;

    eprintln!(
        "Restart your shell or run 'exec fish' / 'exec zsh' / 'exec nu' / 'source ~/.bashrc' to activate."
    );
    Ok(())
}

/// Open a subshell in the monitored path, log directory,
/// 或配置目录。
pub fn cmd_cd(target: CdTarget) -> Result<()> {
    let mut cfg = fsmon::common::config::Config::load()?;
    cfg.resolve_paths()?;

    let dir = match target {
        CdTarget::Config => {
            // cd to the config directory (~/.config/fsmon/)
            fsmon::common::config::Config::user_path()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| {
                    let home = fsmon::common::config::guess_home();
                    PathBuf::from(format!("{}/.config/fsmon", home))
                })
        }
        CdTarget::Monitored => {
            // cd to the monitored store directory (where monitored.jsonl lives).
            // Mirror of -l which cds to the log directory.
            let store_file = cfg.monitored.path.clone();

            store_file
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| store_file.clone())
        }
        CdTarget::Log => {
            // -l: cd to log directory (identical to old `fsmon cd`)
            cfg.logging.path.unwrap_or_else(|| {
                let home = fsmon::common::config::guess_home();
                PathBuf::from(format!("{}/.local/state/fsmon", home))
            })
        }
    };

    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
        fsmon::common::config::chown_to_original_user(&dir);
        eprintln!("Created directory: {}", dir.display());
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let label = match target {
        CdTarget::Config => "config",
        CdTarget::Monitored => "monitored path",
        CdTarget::Log => "log",
    };
    eprintln!(
        "Entering fsmon {} directory (type 'exit' to return)...",
        label
    );
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
