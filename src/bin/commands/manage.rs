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
        println!("{}", serde_json::to_string(entry).expect("PathEntry serialization"));
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
