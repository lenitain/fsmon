use anyhow::Result;
use fsmon::config::Config;
use fsmon::managed::Managed;
use fsmon::socket::{self, SocketCmd};
use std::path::PathBuf;

pub fn cmd_remove(path: Option<PathBuf>, cmd: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Managed::load(&cfg.managed.path)?;

    let removed = if let Some(ref p) = path {
        store.remove_entry(p, cmd.as_deref())
    } else if let Some(ref c) = cmd {
        let len_before = store.entries.len();
        store.entries.retain(|e| e.cmd.as_deref() != Some(c.as_str()));
        store.entries.len() < len_before
    } else {
        false
    };

    if !removed {
        eprintln!("Entry not found");
        return Ok(());
    }
    store.save(&cfg.managed.path)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    match socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "remove".to_string(),
            path,
            recursive: None,
            types: None,
            size: None,
            exclude: None,
            exclude_cmd: None,
            track_cmd: cmd,
        },
    ) {
        Ok(resp) if resp.ok => {
            println!("Daemon updated live");
        }
        Ok(resp) => {
            eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
        }
        Err(_) => {}
    }
    Ok(())
}
