use anyhow::Result;
use fsmon::config::Config;
use fsmon::socket::{self, SocketCmd};
use serde_json;

pub fn cmd_health() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let socket_path = cfg.socket.path.clone();
    let cmd = SocketCmd::Health;

    match socket::send_cmd(&socket_path, &cmd) {
        Ok(resp) => {
            let output =
                serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "Failed to serialize response".to_string());
            println!("{}", output);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            return Err(anyhow::anyhow!(e.to_string()));
        }
    }
    Ok(())
}
