use anyhow::{Result, bail};
use fsmon::config::Config;
use fsmon::monitored::Monitored;
use fsmon::socket::{self, SocketCmd};
use std::path::PathBuf;

pub fn cmd_remove(cmd: Option<String>, paths: Vec<PathBuf>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Monitored::load(&cfg.monitored.path)?;

    let cmd_str = cmd.as_deref();
    match (cmd_str, paths.as_slice()) {
        // fsmon remove (no args) → error
        (None, &[]) => {
            bail!("Specify a cmd group name or --path to remove");
        }
        // fsmon remove bash (no --path) → remove entire cmd group
        (Some(c), &[]) => {
            if !store.remove_cmd_group(c) {
                bail!("Cmd group '{}' not found", c);
            }
        }
        // fsmon remove bash --path /a --path /b → remove from that cmd group
        // fsmon remove --path /a (no cmd) → remove from null cmd group
        (cmd_val, ps) => {
            // Atomic: check all paths exist first
            for p in ps {
                if !store.has_entry(p, cmd_val) {
                    if let Some(c) = cmd_val {
                        bail!("Path '{}' not found under cmd '{}'", p.display(), c);
                    } else {
                        bail!("Path '{}' is not being monitored (null cmd group)", p.display());
                    }
                }
            }
            let mut removed_any = false;
            for p in ps {
                if store.remove_entry(p, cmd_val) {
                    removed_any = true;
                }
            }
            if !removed_any {
                let label = cmd_val.unwrap_or("null");
                bail!("No entries removed (cmd group '{}')", label);
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
