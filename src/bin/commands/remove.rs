use anyhow::{Result, bail};
use fsmon::config::Config;
use fsmon::monitored::Monitored;
use fsmon::socket::{self, SocketCmd};
use std::path::PathBuf;

pub fn cmd_remove(paths: Vec<PathBuf>, cmd: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Monitored::load(&cfg.monitored.path)?;

    match (cmd.as_deref(), paths.as_slice()) {
        // --cmd bash (no --path) → remove entire cmd group
        (Some(c), &[]) => {
            if !store.remove_cmd_group(c) {
                bail!("Cmd group '{}' not found", c);
            }
        }
        // --cmd bash --path /a --path /b → remove specific paths from that cmd group
        (Some(c), ps) => {
            // Atomic: check all paths exist first
            for p in ps {
                if !store.has_entry(p, Some(c)) {
                    bail!("Path '{}' not found under cmd '{}'", p.display(), c);
                }
            }
            let mut removed_any = false;
            for p in ps {
                if store.remove_entry(p, Some(c)) {
                    removed_any = true;
                }
            }
            if !removed_any {
                bail!("No entries removed");
            }
        }
        // --path /a --path /b (no --cmd) → remove paths from all groups
        (None, ps) => {
            // Atomic: pre-check that ALL paths exist in ANY group
            for p in ps {
                if !store.has_entry(p, None) {
                    bail!("Path '{}' is not being monitored", p.display());
                }
            }
            let mut removed_any = false;
            for p in ps {
                if store.remove_entry(p, None) {
                    removed_any = true;
                }
            }
            if !removed_any {
                bail!("No entries removed");
            }
        }
    }

    store.save(&cfg.monitored.path)?;
    eprintln!("Entry removed");

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    for p in &paths {
        if let Err(_) = socket::send_cmd(
            &socket_path,
            &SocketCmd {
                cmd: "remove".to_string(),
                path: Some(p.clone()),
                recursive: None,
                types: None,
                size: None,
                track_cmd: cmd.clone(),
            },
        ) {
            // daemon not running — OK, will take effect on restart
            break;
        }
    }
    Ok(())
}
