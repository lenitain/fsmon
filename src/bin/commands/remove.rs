use anyhow::{Result, bail};
use fsmon::common::config::Config;
use fsmon::common::monitored::{CMD_GLOBAL, Monitored};
use fsmon::common::socket::{self, SocketCmd};
use std::path::PathBuf;

/// Remove one or more paths from the monitoring list.
pub fn cmd_remove(cmd: Option<String>, paths: Vec<PathBuf>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // Resolve relative paths to absolute (shared helper)
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .map(|p| super::resolve_path_arg(&p))
        .collect();

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
            eprintln!("Entry removed: [{}]", cmd_str);
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
                    eprintln!("Entry removed: {}", p.display());
                }
            }
            if !removed_any {
                bail!("No entries removed (cmd group '{}')", cmd_str);
            }
        }
    }

    store.save(&cfg.monitored.path)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = socket::socket_path();
    for p in &paths {
        if socket::send_cmd(
            &socket_path,
            &SocketCmd::Remove {
                path: p.clone(),
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
