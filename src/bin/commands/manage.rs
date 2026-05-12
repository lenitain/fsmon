use anyhow::Result;
use fsmon::config::Config;
use fsmon::managed::Managed;

pub fn cmd_managed() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let entries = Managed::load(&cfg.managed.path)
        .map(|s| s.entries)
        .unwrap_or_default();

    for entry in &entries {
        println!("{}", entry.path.display());
        println!("  recursive:     {}", if entry.recursive.unwrap_or(false) { "yes" } else { "no" });
        if let Some(ref types) = entry.types {
            println!("  types:         {}", types.join(", "));
        }
        if let Some(ref size) = entry.size {
            println!("  size:          {}", size);
        }
        if let Some(ref exclude) = entry.exclude {
            println!("  exclude:       {}", exclude.join(", "));
        }
        if let Some(ref exclude_cmd) = entry.exclude_cmd {
            println!("  exclude-cmd:   {}", exclude_cmd.join(", "));
        }
        if let Some(ref cmd) = entry.cmd {
            println!("  cmd:           {} (process tree tracking)", cmd);
        }
        println!();
    }

    Ok(())
}

/// Output all managed paths (one per line) — used by shell completion scripts.
pub fn cmd_list_managed_paths() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let entries = Managed::load(&cfg.managed.path)
        .map(|s| s.entries)
        .unwrap_or_default();
    for entry in &entries {
        println!("{}", entry.path.display());
    }
    Ok(())
}
