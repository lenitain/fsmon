use anyhow::Result;
use std::fs;
use std::io::Write;

use crate::{FileEvent, OutputFormat};

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
            let csv = format!(
                "{},{},{},{},{},{},{}",
                event.time.to_rfc3339(),
                event.event_type,
                event.path.display(),
                event.pid,
                event.cmd,
                event.user,
                event.size_change
            );
            println!("{}", csv);
            if let Some(file) = output_file {
                writeln!(file, "{}", serde_json::to_string(event)?)?;
            }
        }
    }
    Ok(())
}
