use anyhow::Result;
use fsmon::common::socket::{self, SocketCmd};

/// Query daemon health status.
pub fn cmd_health() -> Result<()> {
    let socket_path = socket::socket_path();
    let cmd = SocketCmd::Health;

    match socket::send_cmd(&socket_path, &cmd) {
        Ok(resp) => {
            let output = serde_json::to_string_pretty(&resp)
                .unwrap_or_else(|_| "Failed to serialize response".to_string());
            println!("{}", output);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            return Err(anyhow::anyhow!(e.to_string()));
        }
    }
    Ok(())
}
