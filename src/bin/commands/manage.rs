use anyhow::Result;
use fsmon::config::Config;
use fsmon::monitored::Monitored;

pub fn cmd_monitored() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let entries = Monitored::load(&cfg.monitored.path)
        .map(|s| s.entries)
        .unwrap_or_default();

    for entry in &entries {
        println!("{}", serde_json::to_string(entry).expect("PathEntry serialization"));
    }

    Ok(())
}

/// Output all monitored paths (one per line) — used by shell completion scripts.
pub fn cmd_list_monitored_paths() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let entries = Monitored::load(&cfg.monitored.path)
        .map(|s| s.entries)
        .unwrap_or_default();
    for entry in &entries {
        println!("{}", entry.path.display());
    }
    Ok(())
}
