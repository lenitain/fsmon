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
        let types_str = entry
            .types
            .as_ref()
            .map(|v| v.join(","))
            .unwrap_or_else(|| "-".to_string());
        let recursive_str = if entry.recursive.unwrap_or(false) {
            "recursive"
        } else {
            "non-recursive"
        };
        let size_str = entry.size.as_deref().unwrap_or("-");
        let exclude_str = entry.exclude.as_ref().map(|v| v.join(",")).as_deref().unwrap_or("-").to_string();
        let exclude_cmd_str = entry.exclude_cmd.as_ref().map(|v| v.join(",")).as_deref().unwrap_or("-").to_string();
        println!(
            "{} | types={} | {} | size={} | exclude-path={} | exclude-cmd={}",
            entry.path.display(),
            types_str,
            recursive_str,
            size_str,
            exclude_str,
            exclude_cmd_str,
        );
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
