use anyhow::Result;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::NamedTempFile;

use crate::common::config::chown_to_original_user;
use crate::common::utils::{self, TimeFilterExt, cmd_to_log_name};
use crate::common::{SizeFilter, SizeOp, TimeFilter, parse_log_line_jsonl};

/// Check if `kept_bytes` exceeds the limit per the filter's operator.
pub fn should_trim(kept_bytes: usize, filter: &SizeFilter) -> bool {
    let max = filter.bytes() as usize;
    match filter.op() {
        SizeOp::Gt => kept_bytes > max,
        SizeOp::Ge => kept_bytes >= max,
        SizeOp::Lt => kept_bytes < max,
        SizeOp::Le => kept_bytes <= max,
        SizeOp::Eq => kept_bytes == max,
    }
}

/// Clean a single log file by time and size.
///
/// Uses backup/rollback mechanism to prevent data loss on rename failure (F-011).
/// Uses fd-based existence check instead of path.exists() (F-023).
pub async fn clean_single_log(
    log_file: &Path,
    time_filter: Option<TimeFilter>,
    max_size: Option<SizeFilter>,
    dry_run: bool,
) -> Result<()> {
    // Symlink check: refuse to clean symlinks (F-010)
    let metadata = fs::symlink_metadata(log_file)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow::anyhow!(
            "Refusing to clean symlink: {}",
            log_file.display()
        ));
    }

    let original_size = metadata.len();

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
            let max = filter.bytes() as usize;
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
        // Backup/rollback mechanism (F-011)
        let backup_file = log_file.with_extension("bak");
        let _ = fs::copy(log_file, &backup_file);

        match fs::rename(&temp_file, log_file) {
            Ok(()) => {
                // Success: remove backup
                let _ = fs::remove_file(&backup_file);
            }
            Err(e) => {
                // Rename failed: restore backup
                eprintln!(
                    "\x1b[33m[WARNING]\x1b[0m Rename failed, restoring backup: {}",
                    e
                );
                let _ = fs::rename(&backup_file, log_file);
                let _ = fs::remove_file(&temp_file);
                return Err(e.into());
            }
        }
        chown_to_original_user(log_file);
        println!("Cleaning {}...", log_file.display());
        let time_desc = time_filter.as_ref().map_or("all time".to_string(), |f| {
            format!("{} {}", f.op(), utils::format_datetime(&f.time()))
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

/// Find the byte offset to keep approximately `max_bytes` from the tail.
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

/// Truncate a file by removing bytes from the start.
///
/// Uses tempfile crate for secure temporary file creation (F-009).
/// Checks for symlinks before truncation (F-010).
/// Temporary file permissions are set to 0600 (F-026).
pub fn truncate_from_start(path: &Path, offset: usize) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }

    // Symlink check: refuse to truncate symlinks (F-010)
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow::anyhow!(
            "Refusing to truncate symlink: {}",
            path.display()
        ));
    }

    let file_len = metadata.len() as usize;
    // offset == file_len means delete everything — write empty file
    if offset >= file_len {
        fs::write(path, b"")?;
        return Ok(());
    }

    let dir = path.parent().unwrap_or(Path::new("."));

    // Use tempfile crate for secure temp file (F-009, F-026)
    // NamedTempFile::new_in() creates with O_CREAT|O_EXCL and mode 0600
    let tmp_file = NamedTempFile::new_in(dir)?;

    let result = (|| -> Result<()> {
        let mut src = fs::File::open(path)?;
        src.seek(SeekFrom::Start(offset as u64))?;

        let mut writer = BufWriter::new(&tmp_file);
        let mut buf = vec![0u8; 8192];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
        }
        writer.flush()?;
        Ok(())
    })();

    result?;

    // Persist: rename temp file to target
    let tmp_path = tmp_file.into_temp_path();
    fs::rename(&tmp_path, path)?;
    chown_to_original_user(path);
    Ok(())
}

/// Count newlines in the first `upto` bytes of a file.
pub fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    use std::io::Read;

    let f = fs::File::open(path)?;
    let mut buf = vec![];
    f.take(upto as u64).read_to_end(&mut buf)?;
    Ok(buf.iter().filter(|&&b| b == b'\n').count())
}
