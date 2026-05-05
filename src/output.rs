use anyhow::Result;
use std::fs;
use std::io::Write;

use crate::{FileEvent, OutputFormat};

/// Output an event to the log file. Canonical format is JSON.
pub fn output_event(
    event: &FileEvent,
    format: OutputFormat,
    output_file: &mut Option<fs::File>,
) -> Result<()> {
    let line = match format {
        OutputFormat::Human => serde_json::to_string(event)?,
        OutputFormat::Json => serde_json::to_string(event)?,
        OutputFormat::Csv => event.to_csv_string(),
    };

    if let Some(file) = output_file {
        writeln!(file, "{}", line)?;
    }
    Ok(())
}
