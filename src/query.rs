use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::utils::{format_size, parse_time};
use crate::{FileEvent, OutputFormat, SortBy};

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
        let since_time = self.since.as_ref().map(|s| parse_time(s)).transpose()?;

        let until_time = self.until.as_ref().map(|s| parse_time(s)).transpose()?;

        // Compile cmd regex if specified
        let cmd_regex = self
            .cmd
            .as_ref()
            .map(|c| Regex::new(&c.replace("*", ".*")))
            .transpose()?;

        // Read and filter events
        let events = self.read_events(since_time, until_time, cmd_regex)?;

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
                && event.time < *since
            {
                continue;
            }

            if let Some(ref until) = until_time
                && event.time > *until
            {
                continue;
            }

            if let Some(ref pids) = self.pids
                && !pids.contains(&event.pid)
            {
                continue;
            }

            if let Some(ref regex) = cmd_regex
                && !regex.is_match(&event.cmd)
            {
                continue;
            }

            if let Some(ref users) = self.users
                && !users.contains(&event.user)
            {
                continue;
            }

            if let Some(ref types) = self.event_types
                && !types.contains(&event.event_type)
            {
                continue;
            }

            if let Some(min) = self.min_size
                && event.size_change.abs() < min
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_event(time: DateTime<Utc>, size: i64, pid: u32) -> FileEvent {
        FileEvent {
            time,
            event_type: "CREATE".to_string(),
            path: PathBuf::from("/tmp/test"),
            pid,
            cmd: "test".to_string(),
            user: "root".to_string(),
            size_change: size,
        }
    }

    fn make_query(sort: SortBy) -> Query {
        Query::new(
            PathBuf::from("/dev/null"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            sort,
        )
    }

    #[test]
    fn test_sort_events_by_time() {
        let t1 = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 9, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2024, 1, 1, 11, 0, 0).unwrap();

        let events = vec![
            make_event(t1, 100, 1),
            make_event(t2, 200, 2),
            make_event(t3, 50, 3),
        ];

        let q = make_query(SortBy::Time);
        let sorted = q.sort_events(events);
        assert_eq!(sorted[0].time, t2);
        assert_eq!(sorted[1].time, t1);
        assert_eq!(sorted[2].time, t3);
    }

    #[test]
    fn test_sort_events_by_size() {
        let t = Utc::now();
        let events = vec![
            make_event(t, 100, 1),
            make_event(t, -5000, 2),
            make_event(t, 1000, 3),
        ];

        let q = make_query(SortBy::Size);
        let sorted = q.sort_events(events);
        // Sort by absolute size descending
        assert_eq!(sorted[0].size_change.abs(), 5000);
        assert_eq!(sorted[1].size_change.abs(), 1000);
        assert_eq!(sorted[2].size_change.abs(), 100);
    }

    #[test]
    fn test_sort_events_by_pid() {
        let t = Utc::now();
        let events = vec![
            make_event(t, 100, 300),
            make_event(t, 200, 100),
            make_event(t, 50, 200),
        ];

        let q = make_query(SortBy::Pid);
        let sorted = q.sort_events(events);
        assert_eq!(sorted[0].pid, 100);
        assert_eq!(sorted[1].pid, 200);
        assert_eq!(sorted[2].pid, 300);
    }

    #[test]
    fn test_sort_events_empty() {
        let q = make_query(SortBy::Time);
        let sorted = q.sort_events(vec![]);
        assert!(sorted.is_empty());
    }

    #[test]
    fn test_sort_events_single() {
        let t = Utc::now();
        let events = vec![make_event(t, 42, 1)];
        let q = make_query(SortBy::Size);
        let sorted = q.sort_events(events);
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].size_change, 42);
    }

    #[test]
    fn test_read_events_with_pid_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_pid");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let e1 = FileEvent {
            time: Utc::now(),
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/a"),
            pid: 100,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: "DELETE".into(),
            path: PathBuf::from("/tmp/b"),
            pid: 200,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();

        let q = Query::new(
            log_path.clone(),
            None,
            None,
            Some(vec![100]),
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_type_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_type");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let e1 = FileEvent {
            time: Utc::now(),
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: "MODIFY".into(),
            path: PathBuf::from("/tmp/b"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();

        let q = Query::new(
            log_path.clone(),
            None,
            None,
            None,
            None,
            None,
            Some(vec!["CREATE".into()]),
            None,
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "CREATE");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_min_size_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_size");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let e1 = FileEvent {
            time: Utc::now(),
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 500,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/b"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 50,
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();

        let q = Query::new(
            log_path.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(100),
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].size_change, 500);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_time_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_time");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let t1 = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap();

        let e1 = FileEvent {
            time: t1,
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: t2,
            event_type: "CREATE".into(),
            path: PathBuf::from("/tmp/b"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();

        let since = Utc.with_ymd_and_hms(2024, 1, 1, 11, 0, 0).unwrap();
        let q = Query::new(
            log_path.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(Some(since), None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time, t2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_skips_invalid_json() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "invalid json line").unwrap();
        writeln!(f, "{{}}").unwrap();

        let q = Query::new(
            log_path.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(None, None, None).unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
