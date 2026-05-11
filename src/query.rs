use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::utils::{TimeFilter, SizeOp};
use crate::{FileEvent, parse_log_line_jsonl};

const SCAN_BACK_BYTES: u64 = 4096;

pub struct Query {
    log_dir: PathBuf,
    paths: Option<Vec<PathBuf>>,
    time_filters: Vec<TimeFilter>,
}

impl Query {
    pub fn new(
        log_dir: PathBuf,
        paths: Option<Vec<PathBuf>>,
        time_filters: Vec<TimeFilter>,
    ) -> Self {
        Self {
            log_dir,
            paths,
            time_filters,
        }
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
            let events =
                self.read_events_from(log_file, since_time, until_time)?;
            all_events.extend(events);
        }

        // Output (time order preserved from log files)
        self.output_events(&all_events)?;

        Ok(())
    }

    /// Extract a lower-bound (since) time from filters with > or >= operators.
    fn extract_since(&self) -> Option<DateTime<Utc>> {
        let mut since = None;
        for f in &self.time_filters {
            match f.op {
                SizeOp::Gt | SizeOp::Ge => {
                    let candidate = f.time;
                    if since.map_or(true, |s| candidate > s) {
                        since = Some(candidate);
                    }
                }
                _ => {}
            }
        }
        since
    }

    /// Extract an upper-bound (until) time from filters with < or <= operators.
    fn extract_until(&self) -> Option<DateTime<Utc>> {
        let mut until = None;
        for f in &self.time_filters {
            match f.op {
                SizeOp::Lt | SizeOp::Le => {
                    let candidate = f.time;
                    if until.map_or(true, |u| candidate < u) {
                        until = Some(candidate);
                    }
                }
                _ => {}
            }
        }
        until
    }

    /// Read events from a single log file within the time range, with binary search.
    fn read_events_from(
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
        reader.seek(SeekFrom::Start(start_pos as u64))?;

        let mut events = Vec::new();
        let mut line = String::new();

        while reader.read_line(&mut line)? > 0 {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if let Some(event) = parse_log_line_jsonl(trimmed) {
                    // Apply time filters
                    let pass = self.time_filters.iter().all(|f| {
                        match f.op {
                            SizeOp::Gt => event.time > f.time,
                            SizeOp::Ge => event.time >= f.time,
                            SizeOp::Lt => event.time < f.time,
                            SizeOp::Le => event.time <= f.time,
                            SizeOp::Eq => event.time == f.time,
                        }
                    });
                    if pass {
                        // Check until bound before push (event consumed by push)
                        if let Some(u) = until {
                            if event.time > u {
                                break;
                            }
                        }
                        events.push(event);
                    }
                }
            }
            line.clear();
        }

        Ok(events)
    }

    /// Binary search for the position of the first event at or after `since`.
    fn find_first_event_after(
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
            let scan_start = if mid < SCAN_BACK_BYTES {
                0
            } else {
                mid - SCAN_BACK_BYTES
            };

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

    /// Output events as JSONL to stdout
    fn output_events(&self, events: &[FileEvent]) -> Result<()> {
        for event in events {
            println!("{}", event.to_jsonl_string());
        }
        Ok(())
    }

    /// Resolve which log files to read
    fn resolve_log_files(&self) -> Result<Vec<PathBuf>> {
        let log_dir = &self.log_dir;

        if !log_dir.exists() {
            return Ok(Vec::new());
        }

        Ok(if let Some(ref paths) = self.paths {
            paths
                .iter()
                .map(|p| log_dir.join(crate::utils::path_to_log_name(p)))
                .filter(|p| p.exists())
                .collect()
        } else {
            let mut files = Vec::new();
            for entry in fs::read_dir(log_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    files.push(path);
                }
            }
            files.sort();
            files
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventType;
    use chrono::Utc;
    use std::io::Write;

    fn create_log_file(dir: &Path, events: &[FileEvent]) -> PathBuf {
        let path = dir.join("test.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for event in events {
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        path
    }

    #[test]
    fn test_read_events_basic() {
        let dir = std::env::temp_dir().join("fsmon_query_test_basic");
        fs::create_dir_all(&dir).unwrap();

        let events = vec![
            FileEvent {
                time: Utc::now(),
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/test"),
                pid: 100,
                cmd: "touch".into(),
                user: "root".into(),
                file_size: 0,
                monitored_path: PathBuf::from("/tmp"),
            },
            FileEvent {
                time: Utc::now(),
                event_type: EventType::Modify,
                path: PathBuf::from("/tmp/test"),
                pid: 200,
                cmd: "vim".into(),
                user: "root".into(),
                file_size: 100,
                monitored_path: PathBuf::from("/tmp"),
            },
        ];

        let log_path = create_log_file(&dir, &events);
        let q = Query::new(dir.clone(), None, vec![]);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, vec![]);
        let result = q.read_events_from(&log_path, None, None).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].pid, 100);
        assert_eq!(result[1].pid, 200);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_since_filter() {
        let dir = std::env::temp_dir().join("fsmon_query_test_since");
        fs::create_dir_all(&dir).unwrap();

        let now = Utc::now();
        let old_time = now - chrono::Duration::hours(2);
        let recent_time = now - chrono::Duration::minutes(30);

        let events = vec![
            FileEvent {
                time: old_time,
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/old"),
                pid: 100,
                cmd: "test".into(),
                user: "root".into(),
                file_size: 0,
                monitored_path: PathBuf::from("/tmp"),
            },
            FileEvent {
                time: recent_time,
                event_type: EventType::Modify,
                path: PathBuf::from("/tmp/recent"),
                pid: 200,
                cmd: "test".into(),
                user: "root".into(),
                file_size: 50,
                monitored_path: PathBuf::from("/tmp"),
            },
        ];

        let log_path = create_log_file(&dir, &events);
        let since = now - chrono::Duration::hours(1);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, vec![]);
        let result = q.read_events_from(&log_path, Some(since), None).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pid, 200);

        let _ = fs::remove_dir_all(&dir);
    }
}
