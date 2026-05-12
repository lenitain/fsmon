use anyhow::{Result, bail};
use fsmon::config::Config;
use fsmon::monitored::{CMD_GLOBAL, Monitored};
use fsmon::socket::{self, SocketCmd};
use std::path::PathBuf;

pub fn cmd_remove(cmd: Option<String>, paths: Vec<PathBuf>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Monitored::load(&cfg.monitored.path)?;

    // CMD is required. Use '_global' for global monitoring.
    let cmd_str = cmd.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "CMD is required. Use '{}' for global monitoring.",
            CMD_GLOBAL
        )
    })?;

    match paths.as_slice() {
        // fsmon remove bash (no --path) → remove entire cmd group
        &[] => {
            if !store.remove_cmd_group(Some(cmd_str)) {
                bail!("Cmd group '{}' not found", cmd_str);
            }
        }
        // fsmon remove bash --path /a --path /b → remove from that cmd group
        ps => {
            // Atomic: check all paths exist first
            for p in ps {
                if !store.has_entry(p, Some(cmd_str)) {
                    bail!("Path '{}' not found under cmd '{}'", p.display(), cmd_str);
                }
            }
            let mut removed_any = false;
            for p in ps {
                if store.remove_entry(p, Some(cmd_str)) {
                    removed_any = true;
                }
            }
            if !removed_any {
                bail!("No entries removed (cmd group '{}')", cmd_str);
            }
        }
    }

    store.save(&cfg.monitored.path)?;
    eprintln!("Entry removed");

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    for p in &paths {
        if socket::send_cmd(
            &socket_path,
            &SocketCmd {
                cmd: "remove".to_string(),
                path: Some(p.clone()),
                recursive: None,
                types: None,
                size: None,
                track_cmd: Some(cmd_str.to_string()),
            },
        )
        .is_err()
        {
            break;
        }
    }
    Ok(())
}
