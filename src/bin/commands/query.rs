use anyhow::Result;
use fsmon::config::Config;
use fsmon::monitored::CMD_GLOBAL;
use fsmon::query::Query;
use fsmon::{TimeFilter, parse_time_filter};

use crate::QueryArgs;

pub async fn cmd_query(args: QueryArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    // CMD is required. Use '_global' to query all global events.
    let cmd = args.cmd.as_deref().ok_or_else(|| {
        anyhow::anyhow!("CMD is required. Use '{}' for global events.", CMD_GLOBAL)
    })?;

    let path_filters = if args.path.is_empty() {
        None
    } else {
        Some(args.path.clone())
    };

    // Parse time filters
    let time_filters: Vec<TimeFilter> = args
        .time
        .iter()
        .map(|s| parse_time_filter(s))
        .collect::<Result<Vec<_>>>()?;

    let query = Query::new(
        cfg.logging.path,
        Some(cmd.to_string()),
        path_filters,
        time_filters,
    );

    query.execute().await?;
    Ok(())
}
