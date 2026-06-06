use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::utils::{TimeFilter, TimeFilterExt, cmd_to_log_name};
use crate::{FileEvent, parse_log_line_jsonl};

const SCAN_BACK_BYTES: u64 = 4096;

/// Query engine for searching historical file change events.
pub struct Query {
    log_dir: PathBuf,
    /// Cmd name to filter by (None = read all log files).
    cmd_filter: Option<String>,
    /// Path prefix filters applied to event.path (None = no path filter).
    path_filters: Option<Vec<PathBuf>>,
    time_filters: Vec<TimeFilter>,
    /// Output timestamps in local time instead of UTC.
    local_time: bool,
}

impl Query {
    pub fn new(
        log_dir: PathBuf,
        cmd_filter: Option<String>,
        path_filters: Option<Vec<PathBuf>>,
        time_filters: Vec<TimeFilter>,
        local_time: bool,
    ) -> Self {
        Self {
            log_dir,
            cmd_filter,
            path_filters,
            time_filters,
            local_time,
        }
    }

    /// Get a reference to the path filters.
    pub fn path_filters(&self) -> Option<&Vec<PathBuf>> {
        self.path_filters.as_ref()
    }

    pub async fn execute(&self) -> Result<()> {
        // Resolve which log files to read
        let log_files = self.resolve_log_files()?;

        if log_files.is_empty() {
            println!("No matching log files found");
            return Ok(());
        }

        // Build since/until from time filters
        let since_time = self.extract_since();
        let until_time = self.extract_until();

        // Read events from each file
        let mut all_events = Vec::new();
        for log_file in &log_files {
            let events = self.read_events_from(log_file, since_time, until_time)?;
            all_events.extend(events);
        }

        // Apply path filters on event.path
        if let Some(ref path_filters) = self.path_filters {
            all_events.retain(|event| path_filters.iter().any(|pf| event.path.starts_with(pf)));
        }

        // Output (time order preserved from log files)
        self.output_events(&all_events)?;

        Ok(())
    }

    /// Extract a lower-bound (since) time from filters with > or >= operators.
    fn extract_since(&self) -> Option<DateTime<Utc>> {
        let mut since = None;
        for f in &self.time_filters {
            if f.is_lower_bound() {
                let candidate = f.time;
                if since.is_none_or(|s| candidate > s) {
                    since = Some(candidate);
                }
            }
        }
        since
    }

    /// Extract an upper-bound (until) time from filters with < or <= operators.
    fn extract_until(&self) -> Option<DateTime<Utc>> {
        let mut until = None;
        for f in &self.time_filters {
            if f.is_upper_bound() {
                let candidate = f.time;
                if until.is_none_or(|u| candidate < u) {
                    until = Some(candidate);
                }
            }
        }
        until
    }

    /// Read events from a single log file within the time range, with binary search.
    pub fn read_events_from(
        &self,
        log_path: &Path,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<FileEvent>> {
        let file = File::open(log_path)
            .with_context(|| format!("Failed to open log file {}", log_path.display()))?;
        let file_len = file.metadata()?.len();

        if file_len == 0 {
            return Ok(Vec::new());
        }

        // Use binary search to find start position
        let start_pos = if let Some(since_time) = since {
            self.find_first_event_after(file_len, log_path, since_time)?
        } else {
            0
        };

        // Read from start_pos to end
        let mut reader = BufReader::new(
            File::open(log_path)
                .with_context(|| format!("Failed to open log file {}", log_path.display()))?,
        );
        reader.seek(SeekFrom::Start(start_pos))?;

        let mut events = Vec::new();
        let mut line = String::new();

        while reader.read_line(&mut line)? > 0 {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && let Some(event) = parse_log_line_jsonl(trimmed)
            {
                // Apply time filters
                let pass = self.time_filters.iter().all(|f| f.matches(event.time));
                if pass {
                    // Check until bound before push (event consumed by push)
                    if let Some(u) = until
                        && event.time > u
                    {
                        break;
                    }
                    events.push(event);
                }
            }
            line.clear();
        }

        Ok(events)
    }

    /// Binary search for the position of the first event at or after `since`.
    pub fn find_first_event_after(
        &self,
        file_len: u64,
        log_path: &Path,
        since: DateTime<Utc>,
    ) -> Result<u64> {
        let file = File::open(log_path)
            .with_context(|| format!("Failed to open log file {}", log_path.display()))?;
        let mut reader = BufReader::new(file);

        let mut low: u64 = 0;
        let mut high: u64 = file_len;

        while low < high {
            let mid = low + (high - low) / 2;

            // Scan back to find a complete line (start of JSON object)
            let scan_start = mid.saturating_sub(SCAN_BACK_BYTES);

            let mut buf = vec![0u8; (mid - scan_start) as usize];
            reader.seek(SeekFrom::Start(scan_start))?;
            reader.read_exact(&mut buf)?;

            // Find the line that contains or starts after `mid`
            let content = String::from_utf8_lossy(&buf);
            let line_start = content.rfind('\n').map(|p| p + 1).unwrap_or(0);
            let adjusted_pos = scan_start + line_start as u64;
            reader.seek(SeekFrom::Start(adjusted_pos))?;

            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                high = mid;
                continue;
            }

            let trimmed = line.trim();
            let event_time = if !trimmed.is_empty() {
                parse_log_line_jsonl(trimmed).map(|e| e.time)
            } else {
                None
            };

            match event_time {
                Some(t) if t < since => {
                    low = mid + 1;
                }
                Some(_) => {
                    high = mid;
                }
                None => {
                    // Invalid line — skip forward
                    low = mid + 1;
                }
            }
        }

        Ok(low)
    }

    /// Execute changes query: dedup by path (keep latest), sort by time desc, output JSONL.
    pub async fn execute_changes(&self) -> Result<()> {
        let log_files = self.resolve_log_files()?;

        if log_files.is_empty() {
            println!("No matching log files found");
            return Ok(());
        }

        let since_time = self.extract_since();
        let until_time = self.extract_until();

        // Read events from each file, dedup by path (keep latest event per path)
        let mut latest_by_path: std::collections::HashMap<PathBuf, FileEvent> =
            std::collections::HashMap::new();

        for log_file in &log_files {
            let events = self.read_events_from(log_file, since_time, until_time)?;
            for event in events {
                // Apply path filters
                if let Some(ref path_filters) = self.path_filters
                    && !path_filters.iter().any(|pf| event.path.starts_with(pf))
                {
                    continue;
                }

                match latest_by_path.entry(event.path.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if event.time > entry.get().time {
                            entry.insert(event);
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(event);
                    }
                }
            }
        }

        // Sort by time descending (newest first)
        let mut all_events: Vec<FileEvent> = latest_by_path.into_values().collect();
        all_events.sort_by_key(|b| std::cmp::Reverse(b.time));

        // Output JSONL
        self.output_events(&all_events)?;
        Ok(())
    }

    /// Output events as JSONL to stdout, respecting local_time preference.
    fn output_events(&self, events: &[FileEvent]) -> Result<()> {
        for event in events {
            let line = if self.local_time {
                event.to_jsonl_string_local()
            } else {
                event.to_jsonl_string()
            };
            println!("{}", line);
        }
        Ok(())
    }

    /// Resolve which log files to read based on cmd_filter.
    pub fn resolve_log_files(&self) -> Result<Vec<PathBuf>> {
        let log_dir = &self.log_dir;

        if !log_dir.exists() {
            return Ok(Vec::new());
        }

        Ok(if let Some(ref cmd) = self.cmd_filter {
            // Specific cmd → read its log file
            let log_path = log_dir.join(cmd_to_log_name(cmd));
            if log_path.exists() {
                vec![log_path]
            } else {
                Vec::new()
            }
        } else {
            // No cmd filter → list all *_log.jsonl files
            let mut files = Vec::new();
            for entry in fs::read_dir(log_dir)? {
                let entry = entry?;
                let path = entry.path();
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if fname.ends_with("_log.jsonl") && path.is_file() {
                    files.push(path);
                }
            }
            files.sort();
            files
        })
    }
}
