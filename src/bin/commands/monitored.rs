use anyhow::Result;
use fsmon::common::config::Config;
use fsmon::common::monitored::{Monitored, CMD_GLOBAL};

/// List all monitored paths with their configuration.
pub fn cmd_monitored() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let store = Monitored::load(&cfg.monitored.path).unwrap_or_default();

    if store.groups.is_empty() {
        println!("No monitored paths.");
        return Ok(());
    }

    println!("=== Monitored Paths ===");
    println!();

    for group in &store.groups {
        let cmd_display = if group.cmd == CMD_GLOBAL {
            "_global (all processes)".to_string()
        } else {
            group.cmd.clone()
        };
        println!("Process: {}", cmd_display);

        for (path, params) in &group.paths {
            let mut parts = Vec::new();
            parts.push(format!("  {}", path.display()));

            let mut details = Vec::new();
            if let Some(recursive) = params.recursive {
                details.push(if recursive { "recursive" } else { "non-recursive" }.to_string());
            }
            if let Some(ref types) = params.types {
                if types.is_empty() {
                    details.push("no types".to_string());
                } else if types.len() <= 3 {
                    details.push(format!("types: {}", types.join(", ")));
                } else {
                    details.push(format!("types: {}... ({} total)", types[..3].join(", "), types.len()));
                }
            }
            if let Some(ref size) = params.size {
                details.push(format!("size: {}", size));
            }

            if !details.is_empty() {
                parts.push(format!(" ({})", details.join(", ")));
            }

            println!("{}", parts.join(""));
        }
        println!();
    }

    Ok(())
}

/// Output all monitored paths (one per line) — used by shell completion scripts.
pub fn cmd_list_monitored_paths() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let store = Monitored::load(&cfg.monitored.path).unwrap_or_default();
    for entry in store.flatten() {
        println!("{}", entry.path.display());
    }
    Ok(())
}
