use anyhow::Result;
use fsmon::config::Config;
use fsmon::utils::path_to_log_name;
use std::path::PathBuf;

/// Resolve the log file path for a given path.
///
/// Pure computation — hashes the path (FNV-1a, deterministic) and
/// appends the log directory. No I/O beyond loading the tiny config.
pub fn cmd_log_path(path: PathBuf) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let log_path = cfg.logging.path.join(path_to_log_name(&path));
    println!("{}", log_path.display());
    Ok(())
}
