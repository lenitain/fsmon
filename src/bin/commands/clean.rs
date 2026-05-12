use anyhow::Result;
use chrono::Utc;
use fsmon::clean::clean_logs;
use fsmon::config::Config;
use fsmon::monitored::CMD_GLOBAL;
use fsmon::utils::{SizeFilter, parse_size_filter};
use fsmon::{DEFAULT_KEEP_DAYS, DEFAULT_MAX_SIZE, TimeOp, TimeFilter, parse_time_filter};

use crate::CleanArgs;

pub async fn cmd_clean(args: CleanArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // CMD is required. Use '_global' for the global log.
    let cmd = args.cmd.as_deref().ok_or_else(|| {
        anyhow::anyhow!("CMD is required. Use '{}' for the global log.", CMD_GLOBAL)
    })?;

    // Time filter: CLI > config > default
    let time_filter: TimeFilter = if let Some(ref t) = args.time {
        parse_time_filter(t)?
    } else if let Some(days) = cfg.logging.keep_days {
        TimeFilter {
            op: TimeOp::Gt,
            time: Utc::now() - chrono::Duration::days(days as i64),
        }
    } else {
        TimeFilter {
            op: TimeOp::Gt,
            time: Utc::now() - chrono::Duration::days(DEFAULT_KEEP_DAYS as i64),
        }
    };

    let max_size_filter: Option<SizeFilter> = args
        .size
        .clone()
        .or(cfg.logging.size.clone())
        .or_else(|| Some(DEFAULT_MAX_SIZE.to_string()))
        .map(|s| parse_size_filter(&s))
        .transpose()?;

    clean_logs(
        &cfg.logging.path,
        cmd,
        Some(time_filter),
        max_size_filter,
        args.dry_run,
    )
    .await?;
    Ok(())
}
