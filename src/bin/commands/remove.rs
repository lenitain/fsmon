use anyhow::Result;
use fsmon::config::Config;
use fsmon::managed::Managed;
use fsmon::socket::{self, SocketCmd};
use path_clean::PathClean;
use std::path::PathBuf;

pub fn cmd_remove(raw: PathBuf) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // Normalize path: expand tilde, clean (., ..), resolve symlinks.
    // Must match the normalization done by cmd_add, so store.remove_entry finds the entry.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = fsmon::config::expand_tilde(&raw, &home);
    let cleaned = expanded.clean();
    let path = cleaned.canonicalize().unwrap_or(cleaned);

    let mut store = Managed::load(&cfg.managed.path)?;

    if !store.remove_entry(&path) {
        eprintln!("No monitored path: {}", path.display());
        std::process::exit(1);
    }

    store.save(&cfg.managed.path)?;
    println!("Path removed: {}", path.display());

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "remove".to_string(),
            path: Some(path),
            recursive: None,
            types: None,
            size: None,
            exclude: None,
            exclude_cmd: None,
            track_cmd: None,
        },
    ) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            eprintln!("Change will apply after daemon restart");
        }
        Err(_) => {
            // daemon not running — store already saved, change applies on restart
        }
    }
    Ok(())
}
