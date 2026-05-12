use anyhow::Result;
use fsmon::config::Config;
use fsmon::monitored::Monitored;

pub fn cmd_monitored() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let store = Monitored::load(&cfg.monitored.path)
        .unwrap_or_default();

    for group in &store.groups {
        println!("{}", serde_json::to_string(group).expect("CmdGroup serialization"));
    }

    Ok(())
}

/// Output all monitored paths (one per line) — used by shell completion scripts.
pub fn cmd_list_monitored_paths() -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;
    let store = Monitored::load(&cfg.monitored.path)
        .unwrap_or_default();
    for entry in store.flatten() {
        println!("{}", entry.path.display());
    }
    Ok(())
}
