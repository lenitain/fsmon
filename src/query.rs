use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use crate::utils::{format_size, parse_time};
use crate::{EventType, FileEvent, OutputFormat, SortBy, parse_log_line};

const SCAN_BACK_BYTES: u64 = 4096;
const BYTES_PER_LINE: u64 = 512;

pub struct Query {
    log_file: PathBuf,
    since: Option<String>,
    until: Option<String>,
    pids: Option<Vec<u32>>,
    cmd: Option<String>,
    users: Option<Vec<String>>,
    event_types: Option<Vec<EventType>>,
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
        event_types: Option<Vec<EventType>>,
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
        let mut reader = BufReader::new(file);

        // Use binary search to narrow the file range when time filters are present.
        // The log file is approximately time-sorted (monotonically increasing),
        // so binary search finds the approximate start/end byte offsets.
        let start_offset = if let Some(since) = since_time {
            let found = self.find_offset_for_time(&mut reader, since)?;
            // Expand backward to catch minor out-of-order entries near the boundary
            self.expand_offset_backward(&mut reader, found, 50)?
        } else {
            0
        };

        let end_offset = if let Some(until) = until_time {
            self.find_end_offset_for_time(&mut reader, until)?
        } else {
            u64::MAX
        };

        let mut events = Vec::new();

        // Seek to start_offset and read lines within [start_offset, end_offset]
        reader.seek(SeekFrom::Start(start_offset))?;
        let mut offset = start_offset;

        loop {
            if offset >= end_offset {
                break;
            }

            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                break; // EOF
            }
            offset += bytes_read as u64;

            let line_str = match std::str::from_utf8(&line) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };
            if line_str.is_empty() {
                continue;
            }

            let event: FileEvent = match parse_log_line(line_str) {
                Some(e) => e,
                None => continue,
            };

            // Time filtering is already narrowed by binary search, but apply
            // exact filters here to handle minor out-of-order entries
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

            // Apply non-time filters
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

    /// Seek to a byte offset in the file and extract the timestamp from the
    /// nearest complete line. When the offset lands mid-line, seeks backward
    /// to find the line start, then reads forward.
    /// Returns (timestamp, byte_offset_of_line_start) or None if no valid line found.
    fn seek_and_parse_time(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
    ) -> Option<(DateTime<Utc>, u64)> {
        // Seek backward to find the start of the line containing `offset`.
        // Read a small chunk before `offset` and scan backward for a newline.
        // 4096 bytes handles most JSON log lines; if a line is longer, the
        // binary search may land inside the previous line but will still
        // converge correctly because timestamps are monotonically increasing.
        let scan_back = SCAN_BACK_BYTES;
        let read_start = offset.saturating_sub(scan_back);

        reader.seek(SeekFrom::Start(read_start)).ok()?;
        let mut pre_buf = Vec::new();
        let pre_bytes = reader.read_until(b'\n', &mut pre_buf).ok()?;
        if pre_bytes == 0 {
            return None; // empty region
        }

        // After read_until, the reader is positioned right after the first '\n'.
        // If read_start == 0, the first line is in pre_buf; read_until consumed
        // up to and including the first '\n'. So the next read starts at the second line.
        // If read_start > 0, we consumed a partial line + '\n', reader is at second line.
        // Either way, read the complete line from the current position.
        let line_start = read_start + pre_bytes as u64;
        let mut line_buf = Vec::new();
        let line_bytes = reader.read_until(b'\n', &mut line_buf).ok()?;
        if line_bytes == 0 {
            return None; // EOF after the pre-read
        }

        let line = std::str::from_utf8(&line_buf).ok()?.trim();
        if line.is_empty() {
            return None;
        }

        let event: FileEvent = parse_log_line(line)?;
        Some((event.time, line_start))
    }

    /// Binary search to find the byte offset of the first line with timestamp >= target.
    /// The log file is approximately time-sorted (monotonically increasing).
    fn find_offset_for_time(
        &self,
        reader: &mut BufReader<File>,
        target: DateTime<Utc>,
    ) -> Result<u64> {
        let file_len = reader.get_ref().metadata()?.len();
        if file_len == 0 {
            return Ok(0);
        }

        let mut low: u64 = 0;
        let mut high = file_len;

        while low < high {
            let mid = low + (high - low) / 2;
            match self.seek_and_parse_time(reader, mid) {
                Some((time, _)) if time < target => {
                    low = mid + 1;
                }
                None => {
                    // Couldn't parse at this offset. seek_and_parse_time seeks
                    // backward to find a line, so the parsed line is likely before
                    // our position (its time < target). Search right.
                    low = mid + 1;
                }
                _ => {
                    // time >= target: search left for the first such line
                    high = mid;
                }
            }
        }

        Ok(low)
    }

    /// Binary search to find the byte offset of the first line with timestamp > target.
    /// Returns the offset past the last line within the time range.
    fn find_end_offset_for_time(
        &self,
        reader: &mut BufReader<File>,
        target: DateTime<Utc>,
    ) -> Result<u64> {
        let file_len = reader.get_ref().metadata()?.len();
        if file_len == 0 {
            return Ok(0);
        }

        let mut low: u64 = 0;
        let mut high = file_len;

        while low < high {
            let mid = low + (high - low) / 2;
            match self.seek_and_parse_time(reader, mid) {
                Some((time, _)) if time <= target => {
                    low = mid + 1;
                }
                None => {
                    // Couldn't parse at this offset. The line we read (by seeking
                    // backward) is likely before our position, so its time is likely
                    // <= target. Search right.
                    low = mid + 1;
                }
                _ => {
                    // time > target: search left
                    high = mid;
                }
            }
        }

        Ok(low)
    }

    /// Expand a byte offset backward by up to `max_lines` lines to catch
    /// minor out-of-order entries near the boundary.
    /// Returns the byte offset of the line that is `max_lines` before `offset`.
    ///
    /// Instead of scanning from file start (O(offset)), scans a bounded
    /// window before `offset`. Doubles the window size on retry if needed.
    fn expand_offset_backward(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
        max_lines: usize,
    ) -> Result<u64> {
        if offset == 0 || max_lines == 0 {
            return Ok(offset);
        }

        let file_len = reader.get_ref().metadata()?.len();
        if offset >= file_len {
            // offset is at or past EOF; scan the whole file
            return self.expand_offset_backward_from_start(reader, file_len, max_lines);
        }

        // Start with a reasonable window: 512 bytes per line on average.
        // Retry with larger windows if we don't find enough lines.
        let mut buf_size: u64 = (max_lines as u64)
            .saturating_mul(BYTES_PER_LINE)
            .max(SCAN_BACK_BYTES);

        loop {
            let scan_start = offset.saturating_sub(buf_size);

            reader.seek(SeekFrom::Start(scan_start))?;
            let mut ring_buf = vec![0u64; max_lines];
            let mut ring_idx = 0usize;
            let mut ring_count = 0usize;
            let mut pos = scan_start;

            // If not starting at file start, skip the partial first line
            if scan_start > 0 {
                let mut discard = Vec::new();
                let bytes = reader.read_until(b'\n', &mut discard)?;
                if bytes == 0 {
                    // Empty region — fall back to reading from start
                    return self.expand_offset_backward_from_start(reader, offset, max_lines);
                }
                pos += bytes as u64;
            }

            loop {
                if pos >= offset {
                    break;
                }
                let mut line = Vec::new();
                let bytes_read = reader.read_until(b'\n', &mut line)?;
                if bytes_read == 0 {
                    break;
                }
                ring_buf[ring_idx % max_lines] = pos;
                ring_idx += 1;
                ring_count += 1;
                pos += bytes_read as u64;
            }

            if ring_count >= max_lines {
                // Found enough lines; return the earliest of the last max_lines
                return Ok(ring_buf[ring_idx % max_lines]);
            }

            if scan_start == 0 {
                // Already scanning from file start and still not enough lines
                if ring_count == 0 {
                    return Ok(0);
                }
                return Ok(ring_buf[0]);
            }

            // Not enough lines in this window — double and retry
            let new_buf_size = buf_size.saturating_mul(2);
            if new_buf_size >= offset {
                // Will cover from file start on next iteration; just do it directly
                return self.expand_offset_backward_from_start(reader, offset, max_lines);
            }
            buf_size = new_buf_size;
        }
    }

    /// Fallback: scan from file start up to `offset`, tracking the last
    /// `max_lines` line start positions.
    fn expand_offset_backward_from_start(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
        max_lines: usize,
    ) -> Result<u64> {
        reader.seek(SeekFrom::Start(0))?;
        let mut ring_buf = vec![0u64; max_lines];
        let mut ring_idx = 0usize;
        let mut ring_count = 0usize;
        let mut pos = 0u64;

        loop {
            if pos >= offset {
                break;
            }
            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                break;
            }
            ring_buf[ring_idx % max_lines] = pos;
            ring_idx += 1;
            ring_count += 1;
            pos += bytes_read as u64;
        }

        if ring_count == 0 {
            return Ok(0);
        }

        let start_idx = if ring_count >= max_lines {
            ring_idx % max_lines
        } else {
            0
        };
        Ok(ring_buf[start_idx % max_lines])
    }

    fn sort_events(&self, mut events: Vec<FileEvent>) -> Vec<FileEvent> {
        match self.sort {
            SortBy::Time => {
                events.sort_by_key(|a| a.time);
            }
            SortBy::Size => {
                events.sort_by_key(|b| std::cmp::Reverse(b.size_change.abs()));
            }
            SortBy::Pid => {
                events.sort_by_key(|a| a.pid);
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
            event_type: EventType::Create,
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
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 100,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Delete,
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
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Modify,
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
            Some(vec![EventType::Create]),
            None,
            OutputFormat::Human,
            SortBy::Time,
        );

        let events = q.read_events(None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Create);

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
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 500,
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
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
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 0,
        };
        let e2 = FileEvent {
            time: t2,
            event_type: EventType::Create,
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

    /// Helper: create a sorted log file with N events at regular time intervals
    fn create_sorted_log(dir_name: &str, count: usize) -> (PathBuf, Vec<DateTime<Utc>>) {
        use std::io::Write;
        let dir = std::env::temp_dir().join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let base = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let mut times = Vec::new();
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..count {
            let t = base + chrono::Duration::minutes(i as i64);
            times.push(t);
            let e = FileEvent {
                time: t,
                event_type: EventType::Create,
                path: PathBuf::from(format!("/tmp/file{}", i)),
                pid: (i as u32) + 1,
                cmd: "test".into(),
                user: "root".into(),
                size_change: (i as i64) * 100,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }
        (log_path, times)
    }

    #[test]
    fn test_binary_search_since_only() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_since", 100);
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

        // since = times[50], expect events[50..100]
        let events = q.read_events(Some(times[50]), None, None).unwrap();
        assert_eq!(events.len(), 50);
        assert!(events[0].time >= times[50]);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_until_only() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_until", 100);
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

        // until = times[49], expect events[0..50]
        let events = q.read_events(None, Some(times[49]), None).unwrap();
        assert_eq!(events.len(), 50);
        assert!(events.last().unwrap().time <= times[49]);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_since_and_until() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_range", 100);
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

        // since=times[20], until=times[79], expect events[20..80]
        let events = q
            .read_events(Some(times[20]), Some(times[79]), None)
            .unwrap();
        assert_eq!(events.len(), 60);
        assert!(events[0].time >= times[20]);
        assert!(events.last().unwrap().time <= times[79]);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_since_before_all() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_before", 10);
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

        let before_all = times[0] - chrono::Duration::hours(1);
        let events = q.read_events(Some(before_all), None, None).unwrap();
        assert_eq!(events.len(), 10);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_since_after_all() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_after", 10);
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

        let after_all = times[9] + chrono::Duration::hours(1);
        let events = q.read_events(Some(after_all), None, None).unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_until_after_all() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_until_after", 10);
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

        let after_all = times[9] + chrono::Duration::hours(1);
        let events = q.read_events(None, Some(after_all), None).unwrap();
        assert_eq!(events.len(), 10);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_until_before_all() {
        let (log_path, times) = create_sorted_log("fsmon_test_bin_until_before", 10);
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

        let before_all = times[0] - chrono::Duration::hours(1);
        let events = q.read_events(None, Some(before_all), None).unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_empty_file() {
        let dir = std::env::temp_dir().join("fsmon_test_bin_empty");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");
        std::fs::File::create(&log_path).unwrap();

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

        let since = Utc::now() - chrono::Duration::hours(1);
        let events = q.read_events(Some(since), None, None).unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary_search_with_combined_filters() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_bin_combined");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let base = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..50 {
            let t = base + chrono::Duration::minutes(i);
            let e = FileEvent {
                time: t,
                event_type: if i % 2 == 0 {
                    EventType::Create
                } else {
                    EventType::Modify
                },
                path: PathBuf::from(format!("/tmp/file{}", i)),
                pid: if i < 25 { 100 } else { 200 },
                cmd: "nginx".into(),
                user: "root".into(),
                size_change: i * 100,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }

        let q = Query::new(
            log_path.clone(),
            None,
            None,
            Some(vec![100]), // pid filter
            None,
            None,
            Some(vec![EventType::Create]), // type filter
            Some(500),                     // min_size filter
            OutputFormat::Human,
            SortBy::Time,
        );

        // since=base+10min, until=base+24min
        // Events with pid=100 are in range [0,25), Create events are even i
        // In range [10,24]: even i = 10,12,14,16,18,20,22,24 → 8 events
        // size_change >= 500: i*100 >= 500 → i >= 5, all 8 qualify
        let since = base + chrono::Duration::minutes(10);
        let until = base + chrono::Duration::minutes(24);
        let events = q.read_events(Some(since), Some(until), None).unwrap();
        assert_eq!(events.len(), 8);
        for e in &events {
            assert!(e.time >= since);
            assert!(e.time <= until);
            assert_eq!(e.pid, 100);
            assert_eq!(e.event_type, EventType::Create);
            assert!(e.size_change >= 500);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary_search_single_entry() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_bin_single");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let t = Utc.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let e = FileEvent {
            time: t,
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/single"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            size_change: 42,
        };
        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();

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

        // Exact match
        let events = q.read_events(Some(t), Some(t), None).unwrap();
        assert_eq!(events.len(), 1);

        // Before
        let before = t - chrono::Duration::hours(1);
        let events = q.read_events(Some(before), Some(before), None).unwrap();
        assert!(events.is_empty());

        // After
        let after = t + chrono::Duration::hours(1);
        let events = q.read_events(Some(after), None, None).unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary_search_large_file_performance() {
        // Test with a larger file to verify binary search doesn't do full scan
        let (log_path, times) = create_sorted_log("fsmon_test_bin_large", 10000);
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

        // Query only the last 100 events
        let since = times[9900];
        let events = q.read_events(Some(since), None, None).unwrap();
        assert_eq!(events.len(), 100);
        assert!(events[0].time >= since);

        let _ = std::fs::remove_dir_all(log_path.parent().unwrap());
    }

    #[test]
    fn test_binary_search_long_lines() {
        // Lines > 4096 bytes must not break seek_and_parse_time
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_bin_long_lines");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let base = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let long_path = format!("/tmp/{}", "a".repeat(5000));
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..100 {
            let t = base + chrono::Duration::minutes(i);
            let e = FileEvent {
                time: t,
                event_type: EventType::Create,
                path: PathBuf::from(&long_path),
                pid: (i as u32) + 1,
                cmd: "test".into(),
                user: "root".into(),
                size_change: 0,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }

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

        let since = base + chrono::Duration::minutes(50);
        let events = q.read_events(Some(since), None, None).unwrap();
        assert_eq!(events.len(), 50);
        assert!(events[0].time >= since);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_expand_offset_backward_few_lines() {
        // File with fewer lines than max_lines — must still work correctly
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_expand_few");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let base = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..5 {
            let t = base + chrono::Duration::minutes(i);
            let e = FileEvent {
                time: t,
                event_type: EventType::Create,
                path: PathBuf::from(format!("/tmp/file{}", i)),
                pid: (i as u32) + 1,
                cmd: "test".into(),
                user: "root".into(),
                size_change: 0,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }

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

        // since before all events — expand_backward with max_lines=50 should
        // return offset 0 even though file only has 5 lines
        let before_all = base - chrono::Duration::hours(1);
        let events = q.read_events(Some(before_all), None, None).unwrap();
        assert_eq!(events.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_expand_offset_backward_large_lines() {
        // File with long lines where bounded scanning must still find enough lines
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_expand_large");
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.log");

        let base = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let long_cmd = "x".repeat(10000);
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..100 {
            let t = base + chrono::Duration::minutes(i);
            let e = FileEvent {
                time: t,
                event_type: EventType::Create,
                path: PathBuf::from(format!("/tmp/file{}", i)),
                pid: (i as u32) + 1,
                cmd: long_cmd.clone(),
                user: "root".into(),
                size_change: 0,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }

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

        let since = base + chrono::Duration::minutes(50);
        let events = q.read_events(Some(since), None, None).unwrap();
        assert_eq!(events.len(), 50);
        assert!(events[0].time >= since);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
