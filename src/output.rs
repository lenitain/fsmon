use anyhow::Result;
use std::fs;
use std::io::Write;

use crate::{FileEvent, OutputFormat};

/// Output an event: stdout follows `format`, log file is always JSON
/// (not all formats are parseable by `fsmon query`, so JSON is the canonical file format)
pub fn output_event(
    event: &FileEvent,
    format: OutputFormat,
    output_file: &mut Option<fs::File>,
) -> Result<()> {
    match format {
        OutputFormat::Human => {
            let output = event.to_human_string();
            println!("{}", output);
            if let Some(file) = output_file {
                writeln!(file, "{}", serde_json::to_string(event)?)?;
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string(event)?;
            println!("{}", json);
            if let Some(file) = output_file {
                writeln!(file, "{}", json)?;
            }
        }
        OutputFormat::Csv => {
            let csv_line = event.to_csv_string();
            println!("{}", csv_line);
            if let Some(file) = output_file {
                writeln!(file, "{}", csv_line)?;
            }
        }
    }
    Ok(())
}
