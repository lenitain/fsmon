use anyhow::{Result, bail};
use fsmon::config::Config;
use fsmon::managed::{Managed, PathEntry};
use fsmon::socket::{self, SocketCmd};
use path_clean::PathClean;
use std::path::PathBuf;

use crate::AddArgs;

pub fn cmd_add(args: AddArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let process_name = args.cmd.clone();

    // Require at least --path or --cmd
    if args.path.is_none() && process_name.is_none() {
        bail!("At least one of --path or --cmd is required");
    }

    // Resolve path if provided
    let path = if let Some(ref raw_path) = args.path {
        let path_str = raw_path.to_string_lossy();
        if path_str.contains('\0') {
            bail!("Invalid path: contains null byte");
        }

        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let expanded = fsmon::config::expand_tilde(raw_path, &home);
        let cleaned = expanded.clean();

        let resolved = match cleaned.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                if cleaned.components().count() == 0 {
                    bail!(
                        "Invalid path (empty after normalization): {}",
                        raw_path.display()
                    );
                }
                eprintln!("[Note] path does not exist yet — will start monitoring when created.");
                cleaned
            }
        };

        let log_dir_canon = cfg
            .logging
            .path
            .canonicalize()
            .unwrap_or_else(|_| cfg.logging.path.clone());
        if log_dir_canon.starts_with(&resolved) {
            bail!(
                "Cannot monitor '{}': log directory '{}' is inside this path — \
                 would cause infinite recursion on every log write.\n\
                 Tip: use a different logging.dir or add a more specific path",
                raw_path.display(),
                cfg.logging.path.display()
            );
        }

        Some(resolved)
    } else {
        None
    };

    let mut store = Managed::load(&cfg.managed.path)?;

    // Check for duplicates if path is specified
    if let Some(ref path) = path {
        if store.get(path, process_name.as_deref()).is_some() {
            eprintln!(
                "[Note] '{}' is already monitored — new parameters will replace the existing configuration.",
                path.display()
            );
        }

        // Check for monitoring overlap
        for entry in &store.entries {
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
        Some(vec![
            "ACCESS".into(),
            "MODIFY".into(),
            "CLOSE_WRITE".into(),
            "CLOSE_NOWRITE".into(),
            "OPEN".into(),
            "OPEN_EXEC".into(),
            "ATTRIB".into(),
            "CREATE".into(),
            "DELETE".into(),
            "DELETE_SELF".into(),
            "MOVED_FROM".into(),
            "MOVED_TO".into(),
            "MOVE_SELF".into(),
            "FS_ERROR".into(),
        ])
    } else {
        Some(args.types.clone())
    };
    let size_val = args.size.clone();
    let exclude = if args.exclude.is_empty() {
        None
    } else {
        Some(args.exclude.clone())
    };
    let exclude_cmd = if args.exclude_cmd.is_empty() {
        None
    } else {
        Some(args.exclude_cmd.clone())
    };
    let recursive = if args.recursive {
        Some(true)
    } else {
        Some(false)
    };
    let entry = PathEntry {
        path: path.clone().unwrap_or_else(|| {
            PathBuf::from(process_name.as_ref().map(|s| s.as_str()).unwrap_or(""))
        }),
        recursive,
        types: types.clone(),
        size: size_val.clone(),
        exclude: exclude.clone(),
        exclude_cmd: exclude_cmd.clone(),
        cmd: process_name.clone(),
    };

    store.add_entry(entry.clone());
    store.save(&cfg.managed.path)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    let result = socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "add".to_string(),
            path,
            recursive,
            types,
            size: size_val,
            exclude,
            exclude_cmd,
            track_cmd: process_name,
        },
    );

    let entry_json = serde_json::to_string(&entry).expect("PathEntry serialization");
    match result {
        Ok(resp) if resp.ok => {
            println!("Entry added into managed");
            println!("{}", entry_json);
        }
        Ok(resp) => {
            if resp.error_kind == Some(fsmon::socket::ErrorKind::Permanent) {
                let mut store = Managed::load(&cfg.managed.path)?;
                store.remove_entry(&entry.path, entry.cmd.as_deref());
                store.save(&cfg.managed.path)?;
                eprintln!("Error: {}", resp.error.unwrap_or_default());
            } else {
                println!("Entry added into managed");
                println!("{}", entry_json);
                eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
            }
        }
        Err(_) => {
            println!("Entry added into managed");
            println!("{}", entry_json);
            eprintln!("Daemon is not running — will be monitored after daemon restart.");
        }
    }
    Ok(())
}
