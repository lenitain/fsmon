use anyhow::Result;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::config::chown_to_original_user;
use crate::utils;
use crate::{SizeFilter, SizeOp, TimeFilter, parse_log_line_jsonl};

/// Check if `kept_bytes` exceeds the limit per the filter's operator.
fn should_trim(kept_bytes: usize, filter: &SizeFilter) -> bool {
    let max = filter.bytes as usize;
    match filter.op {
        SizeOp::Gt => kept_bytes > max,
        SizeOp::Ge => kept_bytes >= max,
        SizeOp::Lt => kept_bytes < max,
        SizeOp::Le => kept_bytes <= max,
        SizeOp::Eq => kept_bytes == max,
    }
}

/// Clean a single log file by time and size.
async fn clean_single_log(
    log_file: &Path,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    if !log_file.exists() {
        println!("Log file not found: {}", log_file.display());
        return Ok(());
    }

    let original_size = fs::metadata(log_file)?.len();

    let temp_file = log_file.with_extension("tmp");
    let mut time_deleted: u64 = 0;
    let mut kept_bytes: usize = 0;

    {
        let file = fs::File::open(log_file)?;
        let reader = BufReader::new(file);
        let writer = fs::File::create(&temp_file)?;
        let mut writer = BufWriter::new(writer);

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let (should_keep, event) = if let Some(event) = parse_log_line_jsonl(trimmed) {
                let passes_time = time_filter.as_ref().map_or(true, |f| {
                    match f.op {
                        SizeOp::Gt => event.time > f.time,
                        SizeOp::Ge => event.time >= f.time,
                        SizeOp::Lt => event.time < f.time,
                        SizeOp::Le => event.time <= f.time,
                        SizeOp::Eq => event.time == f.time,
                    }
                });
                (passes_time, Some(event))
            } else {
                (true, None)
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1; // +1 for newline
            } else if dry_run {
                if let Some(ev) = event {
                    println!("  [to-delete] {} | {} | {}",
                        ev.time.format("%Y-%m-%d %H:%M:%S"),
                        ev.event_type,
                        ev.path.display());
                }
                time_deleted += 1;
            } else {
                time_deleted += 1;
            }
        }
    }

    let size_deleted = if let Some(ref filter) = max_size {
        if should_trim(kept_bytes, filter) {
            let max = filter.bytes as usize;
            let trim_start = find_tail_offset(&temp_file, max)?;
            let dropped = count_lines(&temp_file, trim_start)?;
            truncate_from_start(&temp_file, trim_start)?;
            kept_bytes -= trim_start;
            dropped
        } else {
            0
        }
    } else {
        0
    };

    let total_deleted = time_deleted + size_deleted as u64;

    if dry_run {
        let _ = fs::remove_file(&temp_file);
        if total_deleted > 0 {
            println!("---");
            println!("Dry run: {} entries would be deleted (use --dry-run to preview)", total_deleted);
        } else {
            println!("Dry run: 0 entries match cleanup criteria");
        }
    } else {
        fs::rename(&temp_file, log_file)?;
        chown_to_original_user(log_file);
        println!("Cleaning {}...", log_file.display());
        let time_desc = time_filter.as_ref().map_or("all time".to_string(), |f| {
            format!("{} {}", f.op, utils::format_datetime(&f.time))
        });
        println!(
            "Deleted {} entries (time filter: {})",
            total_deleted, time_desc
        );
        println!(
            "Log file size reduced from {} to {}",
            utils::format_size(original_size as i64),
            utils::format_size(kept_bytes as i64)
        );
    }

    Ok(())
}

/// Clean log files by age and size.
///
/// If `paths` is Some, only clean matching log files for those paths.
/// If `paths` is None, clean all `*.jsonl` log files in `log_dir`.
pub async fn clean_logs(
    log_dir: &Path,
    paths: Option<&[PathBuf]>,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    if !log_dir.exists() {
        println!("Log directory not found: {}", log_dir.display());
        return Ok(());
    }

    if let Some(paths) = paths {
        for path in paths {
            let log_file = log_dir.join(crate::utils::path_to_log_name(path));
            clean_single_log(&log_file, time_filter, max_size, dry_run).await?;
        }
    } else {
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                clean_single_log(&path, time_filter, max_size, dry_run).await?;
            }
        }
    }

    Ok(())
}

fn find_tail_offset(path: &Path, max_bytes: usize) -> Result<usize> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let file_len = f.metadata()?.len() as usize;

    if file_len <= max_bytes {
        return Ok(0);
    }

    let target = file_len - max_bytes;         // we want to start here
    let scan_start = target.saturating_sub(4096);  // scan back up to 4KB
    let scan_len = file_len - scan_start;           // scan from scan_start to EOF

    f.seek(SeekFrom::Start(scan_start as u64))?;
    let mut buf = vec![0u8; scan_len];
    f.read_exact(&mut buf)?;

    // Find the LAST newline before (or at) `target`, so we keep ≈max_bytes
    // from the tail. If no newline found before target, look for the first
    // newline after target (fallback: keep a partial line).
    let target_rel = target - scan_start;
    let last_nl_before = buf[..target_rel].iter().rposition(|&b| b == b'\n');
    let first_nl_after = buf[target_rel..].iter().position(|&b| b == b'\n');

    let offset = match last_nl_before {
        Some(pos) => scan_start + pos + 1,  // keep after this newline
        None => match first_nl_after {
            Some(pos) => target + pos + 1,  // keep after next newline
            None => file_len,                // no newline at all — keep nothing
        },
    };
    Ok(offset)
}

fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }

    let file_len = fs::metadata(path)?.len() as usize;
    // offset == file_len means delete everything — write empty file
    if offset >= file_len {
        fs::write(path, b"")?;
        return Ok(());
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp_path = dir.join(format!(".fsmon_trunc_{}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut tmp = fs::File::create_new(&tmp_path)?;
        let mut src = fs::File::open(path)?;
        src.seek(SeekFrom::Start(offset as u64))?;

        let mut buf = vec![0u8; 8192];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            tmp.write_all(&buf[..n])?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;

    fs::rename(&tmp_path, path)?;
    chown_to_original_user(path);
    Ok(())
}

fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    use std::io::Read;

    let f = fs::File::open(path)?;
    let mut buf = vec![];
    f.take(upto as u64).read_to_end(&mut buf)?;
    Ok(buf.iter().filter(|&&b| b == b'\n').count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use chrono::Utc;
    use crate::{EventType, FileEvent, TimeFilter, SizeFilter, SizeOp};

    fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_count_lines_basic() {
        let dir = std::env::temp_dir().join("fsmon_test_count");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "line1\nline2\nline3\n");

        assert_eq!(count_lines(&path, 6).unwrap(), 1);
        assert_eq!(count_lines(&path, 12).unwrap(), 2);
        assert_eq!(count_lines(&path, 18).unwrap(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_lines_empty() {
        let dir = std::env::temp_dir().join("fsmon_test_count_empty");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "");

        assert_eq!(count_lines(&path, 0).unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_lines_no_trailing_newline() {
        let dir = std::env::temp_dir().join("fsmon_test_count_no_nl");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "line1\nline2");

        assert_eq!(count_lines(&path, 6).unwrap(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_small_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_small");
        fs::create_dir_all(&dir).unwrap();
        let path = create_test_file(&dir, "test.log", "short\n");

        assert_eq!(find_tail_offset(&path, 1000).unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tail_offset_large_file() {
        let dir = std::env::temp_dir().join("fsmon_test_tail_large");
        fs::create_dir_all(&dir).unwrap();

        let line = "aaa\n";
        let content = line.repeat(2000);
        let path = create_test_file(&dir, "test.log", &content);

        let offset = find_tail_offset(&path, 512).unwrap();
        assert!(offset > 0, "offset should be > 0 for large file");
        assert!(offset <= 8000, "offset should be within file");

        let full = fs::read_to_string(&path).unwrap();
        if offset > 0 {
            assert_eq!(
                full.as_bytes()[offset - 1],
                b'\n',
                "tail should start right after a newline"
            );
        }
        assert!(offset < content.len());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_by_time() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        let new_event = FileEvent {
            time: Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/new"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(time_filter), None, false))
            .unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "expected 1 event line, got {:?}", lines);
        let remaining = FileEvent::from_jsonl_str(lines[0]).unwrap();
        assert_eq!(remaining.path, PathBuf::from("/tmp/new"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_dry_run() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_dryrun");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        let old_event = FileEvent {
            time: Utc::now() - chrono::Duration::days(60),
            event_type: EventType::Create,
            path: PathBuf::from("/tmp/old"),
            pid: 1,
            cmd: "test".into(),
            user: "root".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };

        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
        }

        let original_content = fs::read_to_string(&log_path).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(time_filter), None, true))
            .unwrap();

        let after_content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original_content, after_content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_logs_nonexistent_file() {
        let path = PathBuf::from("/tmp/fsmon_nonexistent_dir_clean_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let time_filter = TimeFilter { op: SizeOp::Gt, time: cutoff };
        assert!(
            rt.block_on(clean_logs(&path, None, Some(time_filter), None, false))
                .is_ok()
        );
    }

    #[test]
    fn test_clean_logs_by_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_size");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");

        {
            let mut f = fs::File::create(&log_path).unwrap();
            for i in 0..100 {
                let event = FileEvent {
                    time: Utc::now(),
                    event_type: EventType::Create,
                    path: PathBuf::from(format!("/tmp/file{}", i)),
                    pid: 1,
                    cmd: "test".into(),
                    user: "root".into(),
                    file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
                };
                writeln!(f, "{}", event.to_jsonl_string()).unwrap();
            }
        }

        let original_size = fs::metadata(&log_path).unwrap().len();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let log_dir = log_path.parent().unwrap();
        rt.block_on(clean_logs(log_dir, None, None, Some(SizeFilter { op: SizeOp::Gt, bytes: 500 }), false))
            .unwrap();

        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size);

        let _ = fs::remove_dir_all(&dir);
    }

    // ---- should_trim unit tests ----

    #[test]
    fn test_should_trim_gt() {
        assert!(should_trim(100, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
        assert!(!should_trim(50, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Gt, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_ge() {
        assert!(should_trim(100, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Ge, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_lt() {
        assert!(should_trim(30, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
        assert!(!should_trim(50, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Lt, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_le() {
        assert!(should_trim(30, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Le, bytes: 50 }));
    }

    #[test]
    fn test_should_trim_eq() {
        assert!(should_trim(50, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
        assert!(!should_trim(100, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
        assert!(!should_trim(30, &SizeFilter { op: SizeOp::Eq, bytes: 50 }));
    }

    // ---- integration: size filter edge cases ----

    #[test]
    fn test_clean_size_filter_eq_zero_keeps_all() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_eq0");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let event = FileEvent {
                time: Utc::now(), event_type: EventType::Create,
                path: PathBuf::from("/f"), pid: 1,
                cmd: "t".into(), user: "r".into(),
                file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        let original = fs::read_to_string(&log_path).unwrap();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(
            log_dir, None, None,
            Some(SizeFilter { op: SizeOp::Eq, bytes: 0 }), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original, after, "=0 should NOT delete when file is non-empty");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_size_filter_gt_zero_deletes_all() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_gt0");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let event = FileEvent {
                time: Utc::now(), event_type: EventType::Create,
                path: PathBuf::from("/f"), pid: 1,
                cmd: "t".into(), user: "r".into(),
                file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(
            log_dir, None, None,
            Some(SizeFilter { op: SizeOp::Gt, bytes: 0 }), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert!(after.trim().is_empty(), ">0 should delete all content, got: {:?}", after);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_size_filter_lt_inverts() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_lt");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        {
            let mut f = fs::File::create(&log_path).unwrap();
            for i in 0..20 {
                let event = FileEvent {
                    time: Utc::now(), event_type: EventType::Create,
                    path: PathBuf::from(format!("/f{}", i)), pid: 1,
                    cmd: "t".into(), user: "r".into(),
                    file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
                };
                writeln!(f, "{}", event.to_jsonl_string()).unwrap();
            }
        }
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let size_filter = SizeFilter { op: SizeOp::Lt, bytes: 100000 };
        rt.block_on(clean_logs(
            log_dir, None, None, Some(size_filter), false,
        )).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert!(after.len() > 0, "should keep at least 0 bytes worth of content");
        assert!(after.len() <= 100000, "kept content should be ≤ 100000 bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- integration: time filter operators ----

    #[test]
    fn test_clean_time_filter_ge() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time_ge");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(10),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        let mid_event = FileEvent {
            time: now - chrono::Duration::days(5),
            event_type: EventType::Create, path: PathBuf::from("/mid"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        let new_event = FileEvent {
            time: now,
            event_type: EventType::Create, path: PathBuf::from("/new"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", mid_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }
        let cutoff = now - chrono::Duration::days(7);
        let tf = TimeFilter { op: SizeOp::Ge, time: cutoff };
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), None, false)).unwrap();
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, ">=7d should keep mid(5d) + new(0d)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_time_filter_le() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_time_le");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(10),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        let new_event = FileEvent {
            time: now,
            event_type: EventType::Create, path: PathBuf::from("/new"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
            writeln!(f, "{}", new_event.to_jsonl_string()).unwrap();
        }
        let cutoff = now - chrono::Duration::days(7);
        let tf = TimeFilter { op: SizeOp::Le, time: cutoff };
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), None, false)).unwrap();
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "<=7d should keep old(10d) only");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_no_time_filter_keeps_all() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_no_time");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        let old_event = FileEvent {
            time: now - chrono::Duration::days(100),
            event_type: EventType::Create, path: PathBuf::from("/old"),
            pid: 1, cmd: "t".into(), user: "r".into(),
            file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        };
        {
            let mut f = fs::File::create(&log_path).unwrap();
            writeln!(f, "{}", old_event.to_jsonl_string()).unwrap();
        }
        let original = fs::read_to_string(&log_path).unwrap();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, None, None, false)).unwrap();
        let after = fs::read_to_string(&log_path).unwrap();
        assert_eq!(original, after, "no time filter should keep all events");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_specific_path_only() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_specific");
        fs::create_dir_all(&dir).unwrap();
        let log_a = dir.join(crate::utils::path_to_log_name(Path::new("/a")));
        let log_b = dir.join(crate::utils::path_to_log_name(Path::new("/b")));
        {
            let mut f = fs::File::create(&log_a).unwrap();
            let event = FileEvent {
                time: Utc::now() - chrono::Duration::days(100),
                event_type: EventType::Create, path: PathBuf::from("/a/x"),
                pid: 1, cmd: "t".into(), user: "r".into(),
                file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
            };
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        {
            let mut f = fs::File::create(&log_b).unwrap();
            writeln!(f, "keep").unwrap();
        }
        let cutoff = Utc::now();
        let tf = TimeFilter { op: SizeOp::Gt, time: cutoff };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(&dir, Some(&[PathBuf::from("/a")]), Some(tf), None, false)).unwrap();
        let content_b = fs::read_to_string(&log_b).unwrap();
        assert_eq!(content_b.trim(), "keep", "log /b should be untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_both_time_and_size() {
        let dir = std::env::temp_dir().join("fsmon_test_clean_both");
        fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("test.jsonl");
        let now = Utc::now();
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let old = FileEvent {
                time: now - chrono::Duration::days(60),
                event_type: EventType::Create, path: PathBuf::from("/old"),
                pid: 1, cmd: "t".into(), user: "r".into(),
                file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
            };
            writeln!(f, "{}", old.to_jsonl_string()).unwrap();
            for i in 0..50 {
                let ev = FileEvent {
                    time: now,
                    event_type: EventType::Create, path: PathBuf::from(format!("/f{}", i)),
                    pid: 1, cmd: "t".into(), user: "r".into(),
                    file_size: 0,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
                };
                writeln!(f, "{}", ev.to_jsonl_string()).unwrap();
            }
        }
        let tf = TimeFilter { op: SizeOp::Gt, time: now - chrono::Duration::days(7) };
        let sf = SizeFilter { op: SizeOp::Gt, bytes: 2000 };
        let original_size = fs::metadata(&log_path).unwrap().len();
        let log_dir = log_path.parent().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(clean_logs(log_dir, None, Some(tf), Some(sf), false)).unwrap();
        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(new_size < original_size, "combined filters should reduce size (orig={}, new={})", original_size, new_size);
        assert!(new_size <= 2200, "should be trimmed to ~2000 bytes (newline-aligned), got {}", new_size);
        let _ = fs::remove_dir_all(&dir);
    }
}
