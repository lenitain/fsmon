use anyhow::{Result, bail};
use fsmon::config::Config;
use fsmon::managed::{Managed, PathEntry};
use fsmon::socket::{self, SocketCmd};
use path_clean::PathClean;

use crate::AddArgs;

pub fn cmd_add(args: AddArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // 1. Reject null bytes (would crash C FFI)
    let path_str = args.path.to_string_lossy();
    if path_str.contains('\0') {
        bail!("Invalid path: contains null byte");
    }

    // 2. Expand tilde, then clean/normalize (resolve ., .. without touching fs)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = fsmon::config::expand_tilde(&args.path, &home);
    let cleaned = expanded.clean();

    // 3. Canonicalize for existing paths (resolves symlinks); use cleaned for non-existing
    let path = match cleaned.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            if cleaned.components().count() == 0 {
                bail!("Invalid path (empty after normalization): {}", args.path.display());
            }
            eprintln!("[Note] path does not exist yet — will start monitoring when created.");
            cleaned
        }
    };

    let log_dir_canon = cfg.logging.path.canonicalize().unwrap_or_else(|_| cfg.logging.path.clone());
    if log_dir_canon.starts_with(&path) {
        bail!(
            "Cannot monitor '{}': log directory '{}' is inside this path — \
             would cause infinite recursion on every log write.\n\
             Tip: use a different logging.dir or add a more specific path",
            args.path.display(),
            cfg.logging.path.display()
        );
    }

    let mut store = Managed::load(&cfg.managed.path)?;

    // 4. Check if already monitored
    if store.get(&path).is_some() {
        eprintln!(
            "[Note] '{}' is already monitored — new parameters will replace the existing configuration.",
            path.display()
        );
    }

    // 5. Check for monitoring overlap with existing entries
    let new_recursive = args.recursive;
    for entry in &store.entries {
        let ep = &entry.path;
        let e_recursive = entry.recursive.unwrap_or(false);
        // New path is a subdirectory of an existing recursive path
        if e_recursive && path.starts_with(ep) && path != *ep {
            eprintln!(
                "[Note] '{}' is under recursively monitored path '{}' — events already covered.",
                path.display(),
                ep.display()
            );
        }
        // New path is recursive and covers an existing monitored path
        if new_recursive && ep.starts_with(&path) && *ep != path {
            eprintln!(
                "[Note] already monitored path '{}' is under new recursive path '{}' — events already covered.",
                ep.display(),
                path.display()
            );
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
    let exclude = if args.exclude.is_empty() { None } else { Some(args.exclude.clone()) };
    let exclude_cmd = if args.exclude_cmd.is_empty() { None } else { Some(args.exclude_cmd.clone()) };
    let recursive = if args.recursive { Some(true) } else { None };
    let process_name = args.cmd.clone();

    store.add_entry(PathEntry {
        path: path.clone(),
        recursive,
        types: types.clone(),
        size: size_val.clone(),
        exclude: exclude.clone(),
        exclude_cmd: exclude_cmd.clone(),
        cmd: process_name.clone(),
    });

    store.save(&cfg.managed.path)?;

    // Try live update via socket (non-fatal if fails)
    let socket_path = cfg.socket.path.clone();
    let result = socket::send_cmd(
        &socket_path,
        &SocketCmd {
            cmd: "add".to_string(),
            path: Some(path.clone()),
            recursive,
            types,
            size: size_val,
            exclude,
            exclude_cmd,
            track_cmd: process_name,
        },
    );

    match result {
        Ok(resp) if resp.ok => {
            println!("Path added: {}", path.display());
            println!("Daemon updated live");
        }
        Ok(resp) => {
            let is_permanent = resp.error_kind == Some(fsmon::socket::ErrorKind::Permanent);
            if is_permanent {
                // Revert store save — the error will persist after restart
                let mut store = Managed::load(&cfg.managed.path)?;
                store.remove_entry(&path);
                store.save(&cfg.managed.path)?;
                eprintln!("Error: {}", resp.error.unwrap_or_default());
            } else {
                println!("Path added: {}", path.display());
                eprintln!("Daemon error: {}", resp.error.unwrap_or_default());
                eprintln!("Path will be monitored after daemon restart");
            }
        }
        Err(_) => {
            println!("Path added: {}", path.display());
            eprintln!("Daemon not running — path will be monitored after daemon restart.");
        }
    }
    Ok(())
}
