use anyhow::Result;
use fsmon::config::Config;
use fsmon::query::Query;
use fsmon::{TimeFilter, parse_time_filter};

use crate::QueryArgs;

pub async fn cmd_query(args: QueryArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let paths = if args.path.is_empty() {
        None
    } else {
        Some(args.path.clone())
    };

    // Parse time filters
    let time_filters: Vec<TimeFilter> = args.time.iter()
        .map(|s| parse_time_filter(s))
        .collect::<Result<Vec<_>>>()?;

    let query = Query::new(
        cfg.logging.path,
        paths,
        time_filters,
    );

    query.execute().await?;
    Ok(())
}
