use anyhow::{Context, Result};
use std::process;

pub fn cmd_init() -> Result<()> {
    fsmon::config::Config::init_dirs()?;
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

    let status = std::process::Command::new(&shell)
        .current_dir(&dir)
        .status()
        .with_context(|| format!("Failed to start shell: {}", shell))?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        process::exit(code);
    }

    Ok(())
}
