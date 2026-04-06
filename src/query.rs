use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::{FileEvent, OutputFormat, SortBy};
use crate::utils::{format_size, parse_time};

pub struct Query {
    log_file: PathBuf,
    since: Option<String>,
    until: Option<String>,
    pids: Option<Vec<u32>>,
    cmd: Option<String>,
    users: Option<Vec<String>>,
    event_types: Option<Vec<String>>,
    min_size: Option<i64>,
    format: OutputFormat,
    sort: SortBy,
}

impl Query {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        log_file: PathBuf,
        since: Option<String>,
        until: Option<String>,
        pids: Option<Vec<u32>>,
        cmd: Option<String>,
        users: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
        min_size: Option<i64>,
        format: OutputFormat,
        sort: SortBy,
    ) -> Self {
        Self {
            log_file,
            since,
            until,
            pids,
            cmd,
            users,
            event_types,
            min_size,
            format,
            sort,
        }
    }

    pub async fn execute(&self) -> Result<()> {
        // Parse time filters
        let since_time = self.since.as_ref()
            .map(|s| parse_time(s))
            .transpose()?;

        let until_time = self.until.as_ref()
            .map(|s| parse_time(s))
            .transpose()?;

        // Compile cmd regex if specified
        let cmd_regex = self.cmd.as_ref()
            .map(|c| Regex::new(&c.replace("*", ".*")))
            .transpose()?;

        // Read and filter events
        let events = self.read_events(
            since_time,
            until_time,
            cmd_regex,
        )?;

        // Sort events
        let sorted_events = self.sort_events(events);

        // Output
        self.output_events(&sorted_events)?;

        Ok(())
    }

    fn read_events(
        &self,
        since_time: Option<DateTime<Utc>>,
        until_time: Option<DateTime<Utc>>,
        cmd_regex: Option<Regex>,
    ) -> Result<Vec<FileEvent>> {
        let file = File::open(&self.log_file)
            .with_context(|| format!("Failed to open log file: {}", self.log_file.display()))?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            // Try to parse as JSON
            let event: FileEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Apply filters
            if let Some(ref since) = since_time
                && event.time < *since {
                continue;
            }

            if let Some(ref until) = until_time
                && event.time > *until {
                continue;
            }

            if let Some(ref pids) = self.pids
                && !pids.contains(&event.pid) {
                continue;
            }

            if let Some(ref regex) = cmd_regex
                && !regex.is_match(&event.cmd) {
                continue;
            }

            if let Some(ref users) = self.users
                && !users.contains(&event.user) {
                continue;
            }

            if let Some(ref types) = self.event_types
                && !types.contains(&event.event_type) {
                continue;
            }

            if let Some(min) = self.min_size
                && event.size_change.abs() < min {
                continue;
            }

            events.push(event);
        }

        Ok(events)
    }

    fn sort_events(&self, mut events: Vec<FileEvent>) -> Vec<FileEvent> {
        match self.sort {
            SortBy::Time => {
                events.sort_by(|a, b| a.time.cmp(&b.time));
            }
            SortBy::Size => {
                events.sort_by(|a, b| b.size_change.abs().cmp(&a.size_change.abs()));
            }
            SortBy::Pid => {
                events.sort_by(|a, b| a.pid.cmp(&b.pid));
            }
        }
        events
    }

    fn output_events(&self, events: &[FileEvent]) -> Result<()> {
        if events.is_empty() {
            println!("No matching events found");
            return Ok(());
        }

        match self.format {
            OutputFormat::Human => {
                for event in events {
                    println!("{}", event.to_human_string());
                }
            }
            OutputFormat::Json => {
                for event in events {
                    println!("{}", serde_json::to_string(event)?);
                }
            }
            OutputFormat::Csv => {
                println!("time,type,path,pid,cmd,user,size_change,size_change_str");
                for event in events {
                    let size_human = format_size(event.size_change);
                    let size_prefix = if event.size_change >= 0 { "+" } else { "" };
                    println!(
                        "{},{},{},{},{},{},{},{}{}",
                        event.time.to_rfc3339(),
                        event.event_type,
                        event.path.display(),
                        event.pid,
                        event.cmd,
                        event.user,
                        event.size_change,
                        size_prefix,
                        size_human
                    );
                }
            }
        }

        Ok(())
    }
}
