use anyhow::Result;
use fsmon::config::Config;
use fsmon::socket::{self, SocketCmd};

pub fn cmd_health() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let socket_path = cfg.socket.path.clone();
    let cmd = SocketCmd {
        cmd: "health".to_string(),
        path: None,
        recursive: None,
        types: None,
        size: None,
        track_cmd: None,
    };

    let resp = socket::send_cmd(&socket_path, &cmd)?;
    let output = toml::to_string(&resp).unwrap_or_else(|_| "Failed to serialize response".to_string());
    println!("{}", output);
    Ok(())
}
