use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::utils::parse_time;
use crate::{EventType, FileEvent, OutputFormat, SortBy, parse_log_line};

const SCAN_BACK_BYTES: u64 = 4096;

pub struct Query {
    log_dir: PathBuf,
    paths: Option<Vec<PathBuf>>,
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
        log_dir: PathBuf,
        paths: Option<Vec<PathBuf>>,
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
            log_dir,
            paths,
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
        // Resolve which log files to read
        let log_files = self.resolve_log_files()?;

        if log_files.is_empty() {
            println!("No matching log files found");
            return Ok(());
        }

        // Parse time filters
        let since_time = self.since.as_ref().map(|s| parse_time(s)).transpose()?;
        let until_time = self.until.as_ref().map(|s| parse_time(s)).transpose()?;

        // Compile cmd regex if specified
        let cmd_regex = self
            .cmd
            .as_ref()
            .map(|c| Regex::new(&c.replace("*", ".*")))
            .transpose()?;

        // Read and filter events from each file
        let mut all_events = Vec::new();
        for log_file in &log_files {
            let events =
                self.read_events_from(log_file, since_time, until_time, cmd_regex.clone())?;
            all_events.extend(events);
        }

        // Sort events
        let sorted_events = self.sort_events(all_events);

        // Output
        self.output_events(&sorted_events)?;

        Ok(())
    }

    /// Resolve the list of log files to query.
    /// If paths is Some, resolve each path to its log filename.
    /// If paths is None, scan log_dir for all `*.toml` log files.
    fn resolve_log_files(&self) -> Result<Vec<PathBuf>> {
        if let Some(ref paths) = self.paths {
            let mut files = Vec::new();
            for path in paths {
                let log_path = self.log_dir.join(crate::utils::path_to_log_name(path));
                if log_path.exists() {
                    files.push(log_path);
                }
            }
            return Ok(files);
        }

        // Scan directory for all *.toml files (log files)
        if !self.log_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&self.log_dir)
            .with_context(|| format!("Failed to read log directory {}", self.log_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                files.push(path);
            }
        }

        files.sort();

        Ok(files)
    }

    /// Read the next TOML event block from the reader at current position.
    /// Each event is a TOML document (multi-line) separated by blank lines.
    /// Skips leading blank lines, then reads until a blank line or EOF.
    /// Returns (block_bytes, total_bytes_consumed) or None at EOF.
    fn read_next_block(reader: &mut BufReader<File>) -> Result<Option<(Vec<u8>, usize)>> {
        let mut block = Vec::new();
        let mut total_bytes = 0usize;

        // Skip leading blank lines, collect first content line
        loop {
            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }
            total_bytes += bytes_read;
            let trimmed = std::str::from_utf8(&line).unwrap_or("").trim();
            if !trimmed.is_empty() {
                // First content line — start of the block
                block.extend_from_slice(&line);
                break;
            }
            // Blank line, skip
        }

        // Read the rest of the TOML block until a blank line or EOF
        loop {
            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                break; // EOF
            }
            total_bytes += bytes_read;
            let trimmed = std::str::from_utf8(&line).unwrap_or("").trim();
            if trimmed.is_empty() {
                break; // blank line ends the block
            }
            block.extend_from_slice(&line);
        }

        if block.is_empty() {
            Ok(None)
        } else {
            Ok(Some((block, total_bytes)))
        }
    }

    fn read_events_from(
        &self,
        log_file: &Path,
        since_time: Option<DateTime<Utc>>,
        until_time: Option<DateTime<Utc>>,
        cmd_regex: Option<Regex>,
    ) -> Result<Vec<FileEvent>> {
        let file = File::open(log_file)
            .with_context(|| format!("Failed to open log file: {}", log_file.display()))?;
        let mut reader = BufReader::new(file);

        // Use binary search to narrow the file range when time filters are present.
        let start_offset = if let Some(since) = since_time {
            let found = self.find_offset_for_time(&mut reader, since)?;
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

        // Seek to start_offset and read TOML blocks within [start_offset, end_offset]
        reader.seek(SeekFrom::Start(start_offset))?;
        let mut offset = start_offset;

        loop {
            if offset >= end_offset {
                break;
            }

            let (block_bytes, block_len) = match Self::read_next_block(&mut reader)? {
                Some(b) => b,
                None => break,
            };
            offset += block_len as u64;

            let block_str = match std::str::from_utf8(&block_bytes) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };
            if block_str.is_empty() {
                continue;
            }

            let event: FileEvent = match parse_log_line(block_str) {
                Some(e) => e,
                None => continue,
            };

            // Apply time filters
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
                && event.file_size.abs() < min
            {
                continue;
            }

            events.push(event);
        }

        Ok(events)
    }

    /// Seek to a byte offset in the file and extract the timestamp from the
    /// nearest complete TOML event block at or before `offset`.
    fn seek_and_parse_time(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
    ) -> Option<(DateTime<Utc>, u64)> {
        let scan_back = SCAN_BACK_BYTES;
        let read_start = offset.saturating_sub(scan_back);

        reader.seek(SeekFrom::Start(read_start)).ok()?;

        // Skip forward until we find a blank line (block boundary) or EOF
        loop {
            let mut line = Vec::new();
            let bytes = reader.read_until(b'\n', &mut line).ok()?;
            if bytes == 0 {
                return None; // EOF
            }
            let s = std::str::from_utf8(&line).ok()?.trim();
            if s.is_empty() {
                break; // Found blank line, next content is a complete block
            }
        }

        // Read the complete TOML block
        let mut block = Vec::new();
        loop {
            let mut line = Vec::new();
            let bytes = reader.read_until(b'\n', &mut line).ok()?;
            if bytes == 0 {
                break; // EOF
            }
            let s = std::str::from_utf8(&line).ok()?.trim();
            if s.is_empty() {
                break; // blank line ends the block
            }
            block.extend_from_slice(&line);
        }

        let block_str = std::str::from_utf8(&block).ok()?.trim();
        if block_str.is_empty() {
            return None;
        }

        let event: FileEvent = parse_log_line(block_str)?;
        Some((event.time, offset))
    }

    /// Binary search to find the byte offset of the first line with timestamp >= target.
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
                    low = mid + 1;
                }
                _ => {
                    high = mid;
                }
            }
        }

        Ok(low)
    }

    /// Binary search to find the byte offset of the first line with timestamp > target.
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
                    low = mid + 1;
                }
                _ => {
                    high = mid;
                }
            }
        }

        Ok(low)
    }

    /// Expand a byte offset backward by up to `max_blocks` blocks to catch
    /// minor out-of-order entries near the boundary.
    fn expand_offset_backward(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
        max_blocks: usize,
    ) -> Result<u64> {
        if offset == 0 || max_blocks == 0 {
            return Ok(offset);
        }

        let file_len = reader.get_ref().metadata()?.len();
        if offset >= file_len {
            return self.expand_offset_backward_from_start(reader, file_len, max_blocks);
        }

        let avg_block_bytes: u64 = 420;
        let mut buf_size: u64 = (max_blocks as u64)
            .saturating_mul(avg_block_bytes)
            .max(SCAN_BACK_BYTES);

        loop {
            let scan_start = offset.saturating_sub(buf_size);

            reader.seek(SeekFrom::Start(scan_start))?;
            let mut ring_buf = vec![0u64; max_blocks];
            let mut ring_idx = 0usize;
            let mut ring_count = 0usize;
            let mut pos = scan_start;

            // Skip partial first block if not at file start
            if scan_start > 0 {
                let mut found_blank = false;
                loop {
                    let mut discard = Vec::new();
                    let bytes = reader.read_until(b'\n', &mut discard)?;
                    if bytes == 0 {
                        break;
                    }
                    pos += bytes as u64;
                    if pos > offset {
                        return Ok(0);
                    }
                    let s = std::str::from_utf8(&discard).unwrap_or("").trim();
                    if s.is_empty() {
                        found_blank = true;
                        break;
                    }
                }
                if !found_blank {
                    return self.expand_offset_backward_from_start(reader, offset, max_blocks);
                }
            }

            // Track complete blocks
            loop {
                if pos >= offset {
                    break;
                }
                ring_buf[ring_idx % max_blocks] = pos;
                ring_idx += 1;
                ring_count += 1;

                let mut found_content = false;
                loop {
                    let mut line = Vec::new();
                    let bytes_read = reader.read_until(b'\n', &mut line)?;
                    if bytes_read == 0 {
                        break;
                    }
                    pos += bytes_read as u64;
                    if pos > offset {
                        break;
                    }
                    let s = std::str::from_utf8(&line).unwrap_or("").trim();
                    if s.is_empty() {
                        break;
                    }
                    found_content = true;
                }
                if !found_content {
                    break;
                }
            }

            if ring_count >= max_blocks {
                return Ok(ring_buf[ring_idx % max_blocks]);
            }

            if scan_start == 0 {
                if ring_count == 0 {
                    return Ok(0);
                }
                return Ok(ring_buf[0]);
            }

            let new_buf_size = buf_size.saturating_mul(2);
            if new_buf_size >= offset {
                return self.expand_offset_backward_from_start(reader, offset, max_blocks);
            }
            buf_size = new_buf_size;
        }
    }

    /// Fallback: scan from file start up to `offset`, tracking the last
    /// `max_blocks` block start positions.
    fn expand_offset_backward_from_start(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
        max_blocks: usize,
    ) -> Result<u64> {
        reader.seek(SeekFrom::Start(0))?;
        let mut ring_buf = vec![0u64; max_blocks];
        let mut ring_idx = 0usize;
        let mut ring_count = 0usize;
        let mut pos = 0u64;

        loop {
            if pos >= offset {
                break;
            }
            ring_buf[ring_idx % max_blocks] = pos;
            ring_idx += 1;
            ring_count += 1;

            loop {
                let mut line = Vec::new();
                let bytes_read = reader.read_until(b'\n', &mut line)?;
                if bytes_read == 0 {
                    break;
                }
                pos += bytes_read as u64;
                let s = std::str::from_utf8(&line).unwrap_or("").trim();
                if s.is_empty() {
                    break;
                }
            }
        }

        if ring_count == 0 {
            return Ok(0);
        }

        let start_idx = if ring_count >= max_blocks {
            ring_idx % max_blocks
        } else {
            0
        };
        Ok(ring_buf[start_idx % max_blocks])
    }

    fn sort_events(&self, mut events: Vec<FileEvent>) -> Vec<FileEvent> {
        match self.sort {
            SortBy::Time => {
                events.sort_by_key(|a| a.time);
            }
            SortBy::Size => {
                events.sort_by_key(|b| std::cmp::Reverse(b.file_size.abs()));
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
            OutputFormat::Toml => {
                for event in events {
                    print!("{}", event.to_toml_string());
                    println!(); // blank line separator
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
            file_size: size,
            monitored_path: PathBuf::from("/watched"),
        }
    }

    fn make_query(sort: SortBy) -> Query {
        Query::new(
            PathBuf::from("/tmp/fsmon_logs"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            OutputFormat::Toml,
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
        assert_eq!(sorted[0].file_size.abs(), 5000);
        assert_eq!(sorted[1].file_size.abs(), 1000);
        assert_eq!(sorted[2].file_size.abs(), 100);
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
        assert_eq!(sorted[0].file_size, 42);
    }

    #[test]
    fn test_read_events_with_pid_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_query_pid");
        std::fs::create_dir_all(&dir).unwrap();
        let log_name = crate::utils::path_to_log_name(Path::new("/tmp/test_pid"));
        let log_path = dir.join(&log_name);

        let e1 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 100,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp/test_pid"),
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Delete,
            path: PathBuf::from("/tmp/b"),
            pid: 200,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp/test_pid"),
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", &e1.to_toml_string()).unwrap();
        writeln!(f, "{}", &e2.to_toml_string()).unwrap();

        let q = Query::new(
            dir.clone(),
            Some(vec![PathBuf::from("/tmp/test_pid")]),
            None,
            None,
            Some(vec![100]),
            None,
            None,
            None,
            None,
            OutputFormat::Toml,
            SortBy::Time,
        );

        let events = q.read_events_from(&log_path, None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_type_filter() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_query_type");
        std::fs::create_dir_all(&dir).unwrap();
        let log_name = crate::utils::path_to_log_name(Path::new("/tmp/test"));
        let log_path = dir.join(&log_name);

        let e1 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/a"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp/test"),
        };
        let e2 = FileEvent {
            time: Utc::now(),
            event_type: EventType::Modify,
            path: PathBuf::from("/tmp/b"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            monitored_path: PathBuf::from("/tmp/test"),
        };

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", &e1.to_toml_string()).unwrap();
        writeln!(f, "{}", &e2.to_toml_string()).unwrap();

        let q = Query::new(
            dir.clone(),
            Some(vec![PathBuf::from("/tmp/test")]),
            None,
            None,
            None,
            None,
            None,
            Some(vec![EventType::Create]),
            None,
            OutputFormat::Toml,
            SortBy::Time,
        );

        let events = q.read_events_from(&log_path, None, None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Create);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
