use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::utils::parse_time;
use crate::{FileEvent, parse_log_line_jsonl};

const SCAN_BACK_BYTES: u64 = 4096;

pub struct Query {
    log_dir: PathBuf,
    paths: Option<Vec<PathBuf>>,
    since: Option<String>,
    until: Option<String>,
}

impl Query {
    pub fn new(
        log_dir: PathBuf,
        paths: Option<Vec<PathBuf>>,
        since: Option<String>,
        until: Option<String>,
    ) -> Self {
        Self {
            log_dir,
            paths,
            since,
            until,
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

    /// Resolve the list of log files to query.
    /// If paths is Some, resolve each path to its log filename.
    /// If paths is None, scan log_dir for all `*.jsonl` log files.
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

        // Scan directory for all *.jsonl files (log files)
        if !self.log_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&self.log_dir)
            .with_context(|| format!("Failed to read log directory {}", self.log_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }

        files.sort();

        Ok(files)
    }

    /// Read the next non-empty JSONL line from the reader at current position.
    /// Returns (line_bytes, total_bytes_consumed) or None at EOF.
    fn read_next_line(reader: &mut BufReader<File>) -> Result<Option<(Vec<u8>, usize)>> {
        loop {
            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }
            let trimmed = std::str::from_utf8(&line).unwrap_or("").trim();
            if !trimmed.is_empty() {
                return Ok(Some((line, bytes_read)));
            }
            // Skip empty lines
        }
    }

    fn read_events_from(
        &self,
        log_file: &Path,
        since_time: Option<DateTime<Utc>>,
        until_time: Option<DateTime<Utc>>,
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

        // Seek to start_offset and read JSONL lines within [start_offset, end_offset]
        reader.seek(SeekFrom::Start(start_offset))?;
        let mut offset = start_offset;

        loop {
            if offset >= end_offset {
                break;
            }

            let (line_bytes, line_len) = match Self::read_next_line(&mut reader)? {
                Some(b) => b,
                None => break,
            };
            offset += line_len as u64;

            let line_str = match std::str::from_utf8(&line_bytes) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };
            if line_str.is_empty() {
                continue;
            }

            let event: FileEvent = match parse_log_line_jsonl(line_str) {
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

            events.push(event);
        }

        Ok(events)
    }

    /// Seek to a byte offset in the file and extract the timestamp from the
    /// nearest complete JSONL line at or before `offset`.
    fn seek_and_parse_time(
        &self,
        reader: &mut BufReader<File>,
        offset: u64,
    ) -> Option<(DateTime<Utc>, u64)> {
        // Try progressively larger scan-back windows (up to 256KB)
        // to handle JSONL lines larger than the default 4KB scan-back.
        let mut scan_back = SCAN_BACK_BYTES;
        loop {
            let read_start = offset.saturating_sub(scan_back);

            reader.seek(SeekFrom::Start(read_start)).ok()?;

            // Skip to the start of the next complete line
            if read_start > 0 {
                let mut found_newline = false;
                let mut byte = [0u8; 1];
                loop {
                    if reader.read_exact(&mut byte).is_err() {
                        break; // EOF, larger scan_back needed
                    }
                    if byte[0] == b'\n' {
                        found_newline = true;
                        break;
                    }
                }
                if !found_newline {
                    // Hit EOF without finding newline — retry with larger window
                    let next = scan_back.saturating_mul(2);
                    if next >= offset || next > 256 * 1024 {
                        return None;
                    }
                    scan_back = next;
                    continue;
                }
            }

            // Read one complete line
            let mut line = String::new();
            reader.read_line(&mut line).ok()?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            let event: FileEvent = parse_log_line_jsonl(trimmed)?;
            return Some((event.time, offset));
        }
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

    /// Expand a byte offset backward by up to `max_lines` lines to catch
    /// minor out-of-order entries near the boundary.
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
            return self.expand_offset_backward_from_start(reader, file_len, max_lines);
        }

        let avg_line_bytes: u64 = 200;
        let mut buf_size: u64 = (max_lines as u64)
            .saturating_mul(avg_line_bytes)
            .max(SCAN_BACK_BYTES);

        loop {
            let scan_start = offset.saturating_sub(buf_size);

            reader.seek(SeekFrom::Start(scan_start))?;
            let mut ring_buf = vec![0u64; max_lines];
            let mut ring_idx = 0usize;
            let mut ring_count = 0usize;
            let mut pos = scan_start;

            // Skip to the start of the next complete line if not at file start
            if scan_start > 0 {
                let mut byte = [0u8; 1];
                loop {
                    let bytes = reader.read(&mut byte)?;
                    if bytes == 0 {
                        break;
                    }
                    pos += 1;
                    if pos > offset {
                        return Ok(0);
                    }
                    if byte[0] == b'\n' {
                        break;
                    }
                }
            }

            // Track line start positions
            loop {
                if pos >= offset {
                    break;
                }
                ring_buf[ring_idx % max_lines] = pos;
                ring_idx += 1;
                ring_count += 1;

                // Read one line
                let mut line = Vec::new();
                let bytes_read = reader.read_until(b'\n', &mut line)?;
                if bytes_read == 0 {
                    break;
                }
                pos += bytes_read as u64;
            }

            if ring_count >= max_lines {
                return Ok(ring_buf[ring_idx % max_lines]);
            }

            if scan_start == 0 {
                if ring_count == 0 {
                    return Ok(0);
                }
                return Ok(ring_buf[0]);
            }

            let new_buf_size = buf_size.saturating_mul(2);
            if new_buf_size >= offset {
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
            ring_buf[ring_idx % max_lines] = pos;
            ring_idx += 1;
            ring_count += 1;

            // Read one line
            let mut line = Vec::new();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                break;
            }
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

    fn output_events(&self, events: &[FileEvent]) -> Result<()> {
        if events.is_empty() {
            println!("No matching events found");
            return Ok(());
        }

        for event in events {
            println!("{}", event.to_jsonl_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventType;

    fn make_event(time: DateTime<Utc>, size: u64, pid: u32) -> FileEvent {
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

    #[test]
    fn test_read_events_basic() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("fsmon_test_query_basic");
        std::fs::create_dir_all(&dir).unwrap();
        let log_name = crate::utils::path_to_log_name(Path::new("/tmp/test"));
        let log_path = dir.join(&log_name);

        let e1 = make_event(Utc::now(), 100, 1);
        let e2 = make_event(Utc::now(), 200, 2);

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", &e1.to_jsonl_string()).unwrap();
        writeln!(f, "{}", &e2.to_jsonl_string()).unwrap();

        let q = Query::new(dir.clone(), Some(vec![PathBuf::from("/tmp/test")]), None, None);
        let events = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(events.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_with_since_filter() {
        use std::io::Write;
        use chrono::TimeZone;

        let dir = std::env::temp_dir().join("fsmon_test_query_since");
        std::fs::create_dir_all(&dir).unwrap();
        let log_name = crate::utils::path_to_log_name(Path::new("/tmp/test"));
        let log_path = dir.join(&log_name);

        let old = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let new = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let e1 = make_event(old, 100, 1);
        let e2 = make_event(new, 200, 2);

        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "{}", &e1.to_jsonl_string()).unwrap();
        writeln!(f, "{}", &e2.to_jsonl_string()).unwrap();

        let cutoff = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let q = Query::new(dir.clone(), Some(vec![PathBuf::from("/tmp/test")]), None, None);
        let events = q.read_events_from(&log_path, Some(cutoff), None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time, new);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
