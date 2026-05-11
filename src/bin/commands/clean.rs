use anyhow::Result;
use chrono::Utc;
use fsmon::config::Config;
use fsmon::utils::{parse_size_filter, SizeFilter};
use fsmon::{SizeOp, clean_logs, parse_time_filter, TimeFilter, DEFAULT_KEEP_DAYS, DEFAULT_MAX_SIZE};

use crate::CleanArgs;

pub async fn cmd_clean(args: CleanArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let paths = if args.path.is_empty() {
        None
    } else {
        Some(args.path.clone())
    };

    // Time filter: CLI > config > default
    let time_filter: TimeFilter = if let Some(ref t) = args.time {
        parse_time_filter(t)?
    } else if let Some(days) = cfg.logging.keep_days {
        TimeFilter {
            op: SizeOp::Gt,
            time: Utc::now() - chrono::Duration::days(days as i64),
        }
    } else {
        TimeFilter {
            op: SizeOp::Gt,
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
        paths.as_deref(),
        Some(time_filter),
        max_size_filter,
        args.dry_run,
    )
    .await?;
    Ok(())
}
