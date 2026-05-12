use anyhow::Result;
use fsmon::config::Config;
use fsmon::monitored::Monitored;
use fsmon::socket::{self, SocketCmd};
use std::path::PathBuf;

pub fn cmd_remove(path: Option<PathBuf>, cmd: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Monitored::load(&cfg.monitored.path)?;

    // Match by (path, cmd) pair precisely
    let removed = if let Some(ref p) = path {
        // --path /home --cmd openclaw → exact (path, cmd) match
        // --path /home (no cmd)       → only match entries where cmd is None
        store.remove_entry(p, cmd.as_deref())
    } else if let Some(ref c) = cmd {
        // --cmd openclaw (no path) → only match process-only entries (path == cmd sentinel)
        store.remove_entry(&PathBuf::from(c.as_str()), Some(c.as_str()))
    } else {
        false
    };

    if !removed {
        eprintln!("Entry not found");
        return Ok(());
    }
    store.save(&cfg.monitored.path)?;
    eprintln!("Entry removed");

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
