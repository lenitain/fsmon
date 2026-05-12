use anyhow::{Context, Result};
use fsmon::config::Config;
use fsmon::DaemonLock;
use fsmon::monitor::Monitor;
use fsmon::monitored::Monitored;
use std::fs;
use std::path::Path;

use super::parse_path_entries;

pub async fn cmd_daemon(debug: bool) -> Result<()> {
    // Acquire singleton lock first — only one daemon instance allowed
    let (uid, _gid) = fsmon::config::resolve_uid_gid();
    let _lock = DaemonLock::acquire(uid)?;

    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    eprintln!("Config loaded:");
    eprintln!("  Monitored path database:  {}", cfg.monitored.path.display());
    eprintln!("  Event logs:     {}", cfg.logging.path.display());
    eprintln!("  Command socket: {}", cfg.socket.path.display());

    let store = Monitored::load(&cfg.monitored.path)?;

    let socket_path = cfg.socket.path.clone();

    // Create parent directories for socket
    if socket_path.exists() {
        fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let socket_listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind socket at {}", socket_path.display()))?;

    // Set socket permissions to 0666 so any user can send commands
    set_socket_permissions(&socket_path)?;

    // Chown store parent dir to the original user (daemon runs as root)
    let (uid, gid) = fsmon::config::resolve_uid_gid();
    if let Some(parent) = cfg.monitored.path.parent() {
        chown_path(parent, uid, gid);
    }

    let paths_and_options = parse_path_entries(&store.flatten())?;

    let store_path = cfg.monitored.path.clone();
    let mut monitor = Monitor::new(
        paths_and_options,
        Some(cfg.logging.path.clone()),
        Some(store_path),
        None,
        Some(socket_listener),
        debug,
    )?;

    if !store.is_empty() {
        for group in &store.groups {
            let cmd_label = if group.cmd == fsmon::monitored::CMD_GLOBAL {
                "[global]".to_string()
            } else {
                format!("[{}]", group.cmd)
            };
            eprintln!("  {} ({} path(s)):", cmd_label, group.paths.len());
            for path in group.paths.keys() {
                eprintln!("    {}", path.display());
            }
        }
    }

    monitor.run().await?;
    Ok(())
}

/// Chown a path to the given uid:gid (daemon runs as root, needs to give files back to the user).
pub fn chown_path(path: &Path, uid: u32, gid: u32) {
    if let Ok(cpath) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        let _ = nix::unistd::chown(
            cpath.as_c_str(),
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        );
    }
}

/// Set socket permissions to 0666 so non-root users can communicate with the daemon.
pub fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = fs::Permissions::from_mode(0o666);
    fs::set_permissions(path, perm)
        .with_context(|| format!("Failed to set socket permissions on {}", path.display()))?;
    Ok(())
}
