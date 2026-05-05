use anyhow::Result;
use std::fs;
use std::io::Write;

use crate::{FileEvent, OutputFormat};

/// Output an event to the log file. Canonical format is TOML.
pub fn output_event(
    event: &FileEvent,
    format: OutputFormat,
    output_file: &mut Option<fs::File>,
) -> Result<()> {
    let line = match format {
        OutputFormat::Human | OutputFormat::Toml => event.to_toml_string(),
        OutputFormat::Csv => event.to_csv_string(),
    };

    if let Some(file) = output_file {
        writeln!(file, "{}", line)?;
        // Blank line separator between events
        writeln!(file)?;
    }
    Ok(())
}
