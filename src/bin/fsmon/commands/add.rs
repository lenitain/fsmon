use anyhow::{Result, bail};
use fsmon::common::config::Config;
use fsmon::common::monitored::{CMD_GLOBAL, Monitored, PathEntry};
use fsmon::common::socket::{self, SocketCmd, SocketError, SocketResponse};
use std::path::PathBuf;

use fsmon::common::security;
use crate::AddArgs;

/// Add a path to the monitoring list.
pub fn cmd_add(args: AddArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    Config::ensure_monitored_dir()?;

    // CMD is required. Use '_global' for global monitoring.
    let process_name = args.cmd.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "CMD is required. Use '{}' for global monitoring.",
            CMD_GLOBAL
        )
    })?;
    let process_name = Some(process_name.to_string());

    // Require at least --path
    if args.path.is_none() {
        bail!("At least one of --path or a process name is required");
    }

    // Reject tracking the fsmon daemon itself — its events are excluded
    // by PID filter, so --cmd fsmon would never match anything.
    if process_name.as_deref() == Some("fsmon") {
        bail!(
            "Cannot monitor 'fsmon' process: fsmon daemon's own events are excluded \
                 from monitoring.\n\
                 Tip: use a different process name, or omit the process name to capture all events."
        );
    }

    // Resolve path if provided
    let path = if let Some(ref raw_path) = args.path {
        let path_str = raw_path.to_string_lossy();
        if path_str.contains('\0') {
            bail!("Invalid path: contains null byte");
        }

        let resolved = super::resolve_path_arg(raw_path);

        // Validate path against security blacklist (F-014/019)
        if let Err(e) = security::check_path_allowed(&resolved, &[]) {
            bail!("{}", e);
        }
        let exists = resolved.exists();
        if !exists {
            eprintln!("[Note] path does not exist yet — will start monitoring when created.");
        }

        if let Some(ref log_path) = cfg.logging.path {
            let log_dir_canon = log_path.canonicalize().unwrap_or_else(|_| log_path.clone());
            if args.recursive && log_dir_canon.starts_with(&resolved) || log_dir_canon == resolved {
                bail!(
                    "Cannot monitor '{}': {}\n\
                     Tip: use a path outside the log directory, or use a different logging.path",
                    raw_path.display(),
                    if log_dir_canon == resolved {
                        "this path is the log directory itself".to_string()
                    } else {
                        format!("log directory '{}' is inside this path", log_path.display())
                    }
                );
            }
        }
        // NOTE: Socket paths (lock.sock, daemon.sock) under /run/user/<UID>/fsmon/
        // do NOT need the same guard as the log directory above.  Reasons:
        //   1. Unix domain socket I/O (connect/send/recv) does NOT generate
        //      fanotify filesystem events — only bind() and unlink() do.
        //   2. bind/unlink are one-shot operations at daemon startup/shutdown,
        //      not a continuous write loop like log appending.
        //   3. Even if those rare events are captured, the daemon PID filter
        //      (events.rs: `event_pid == self.daemon_pid`) skips them.
        //   4. Moving socket files into a subdirectory was a cosmetic cleanup;
        //      the old flat layout (/run/user/<UID>/fsmon.sock) was equally safe
        //      for the same reasons.
        Some(resolved)
    } else {
        None
    };

    let mut store = Monitored::load(&cfg.monitored.path)?;

    // Check for duplicates if path is specified
    if let Some(ref path) = path {
        if store.get(path, process_name.as_deref()).is_some() {
            let cmd_info = match process_name.as_deref() {
                Some(cmd) => format!(" with cmd {}", cmd),
                None => " (without cmd)".to_string(),
            };
            eprintln!(
                "[Note] '{}{}' is already monitored — new parameters will replace the existing configuration.",
                path.display(),
                cmd_info,
            );
        }

        // Check for monitoring overlap
        for entry in &store.flatten() {
            let e_recursive = entry.recursive.unwrap_or(false);
            if e_recursive && path.starts_with(&entry.path) && *path != entry.path {
                eprintln!(
                    "[Note] '{}' is under recursively monitored path '{}' — events already covered.",
                    path.display(),
                    entry.path.display()
                );
            }
            if args.recursive && entry.path.starts_with(path) && entry.path != *path {
                eprintln!(
                    "[Note] already monitored path '{}' is under new recursive path '{}' — events already covered.",
                    entry.path.display(),
                    path.display()
                );
            }
        }
    }

    let types: Option<Vec<String>> = if args.types.is_empty() {
        None
    } else if args.types.iter().any(|s| s.eq_ignore_ascii_case("all")) {
        Some(
            fsmon::common::EventType::ALL
                .iter()
                .map(|t| t.to_string())
                .collect(),
        )
    } else {
        // Validate each event type
        for t in &args.types {
            let _ = t
                .parse::<fsmon::common::EventType>()
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        Some(args.types.clone())
    };
    let size_val = args.size.clone();
    let recursive = if args.recursive {
        Some(true)
    } else {
        Some(false)
    };
    let entry = PathEntry {
        path: path
            .clone()
            .unwrap_or_else(|| PathBuf::from(process_name.as_deref().unwrap_or(""))),
        cmd: process_name.clone(),
        recursive,
        types: types.clone(),
        size: size_val.clone(),
    };

    store.add_entry(entry.clone());
    store.save(&cfg.monitored.path)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = socket::socket_path();
    let result = socket::send_cmd(
        &socket_path,
        &SocketCmd::Add {
            path: path.clone().unwrap_or_default(),
            recursive,
            types,
            size: size_val,
            track_cmd: process_name,
        },
    );

    match result {
        Ok(SocketResponse::Ok) => {
            println!("Entry added: {}", entry.path.display());
        }
        Ok(resp) => {
            // Unexpected response type
            println!("Entry added: {}", entry.path.display());
            eprintln!("Unexpected response from daemon: {:?}", resp);
        }
        Err(SocketError::Permanent(msg)) => {
            let mut store = Monitored::load(&cfg.monitored.path)?;
            store.remove_entry(&entry.path, entry.cmd.as_deref());
            store.save(&cfg.monitored.path)?;
            eprintln!("Error: {}", msg);
        }
        Err(SocketError::Transient(_)) => {
            println!("Entry added: {}", entry.path.display());
        }
    }
    Ok(())
}
