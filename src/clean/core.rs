use anyhow::Result;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::config::chown_to_original_user;
use crate::utils::{self, TimeFilterExt, cmd_to_log_name};
use crate::{SizeFilter, SizeOp, TimeFilter, parse_log_line_jsonl};

/// Check if `kept_bytes` exceeds the limit per the filter's operator.
pub fn should_trim(kept_bytes: usize, filter: &SizeFilter) -> bool {
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
pub async fn clean_single_log(
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
                let passes_time = time_filter.as_ref().is_none_or(|f| f.matches(event.time));
                (passes_time, Some(event))
            } else {
                (true, None)
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1; // +1 for newline
            } else if dry_run {
                if let Some(ev) = event {
                    println!(
                        "  [to-delete] {} | {} | {}",
                        ev.time.format("%Y-%m-%d %H:%M:%S"),
                        ev.event_type,
                        ev.path.display()
                    );
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
            println!(
                "Dry run: {} entries would be deleted (use --dry-run to preview)",
                total_deleted
            );
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
    cmd: &str,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    if !log_dir.exists() {
        println!("Log directory not found: {}", log_dir.display());
        return Ok(());
    }

    let log_file = log_dir.join(cmd_to_log_name(cmd));
    clean_single_log(&log_file, time_filter, max_size, dry_run).await
}

pub fn find_tail_offset(path: &Path, max_bytes: usize) -> Result<usize> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let file_len = f.metadata()?.len() as usize;

    if file_len <= max_bytes {
        return Ok(0);
    }

    let target = file_len - max_bytes; // we want to start here
    let scan_start = target.saturating_sub(4096); // scan back up to 4KB
    let scan_len = file_len - scan_start; // scan from scan_start to EOF

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
        Some(pos) => scan_start + pos + 1, // keep after this newline
        None => match first_nl_after {
            Some(pos) => target + pos + 1, // keep after next newline
            None => file_len,              // no newline at all — keep nothing
        },
    };
    Ok(offset)
}

pub fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
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

pub fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    use std::io::Read;

    let f = fs::File::open(path)?;
    let mut buf = vec![];
    f.take(upto as u64).read_to_end(&mut buf)?;
    Ok(buf.iter().filter(|&&b| b == b'\n').count())
}
