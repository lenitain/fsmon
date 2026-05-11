use anyhow::Result;
use fsmon::config::Config;
use fsmon::utils::path_to_log_name;
use std::path::PathBuf;

/// Resolve the log file path for given paths.
///
/// Pure computation — hashes each path (FNV-1a, deterministic) and
/// appends the log directory. No I/O beyond loading the tiny config.
/// Outputs one path per line.
pub fn cmd_p2l(paths: Vec<PathBuf>) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    for path in paths {
        let log_path = cfg.logging.path.join(path_to_log_name(&path));
        println!("{}", log_path.display());
    }
    Ok(())
}
