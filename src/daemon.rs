use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::utils::process_exists;

fn get_runtime_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "fsmon", "fsmon")
        .and_then(|dirs| dirs.runtime_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonStatus {
    Running {
        pid: u32,
        paths: Vec<PathBuf>,
        log_file: PathBuf,
        start_time: DateTime<Utc>,
        event_count: u64,
        memory_usage: u64,
    },
    Stopped,
}

pub struct Daemon;

impl Daemon {
    pub fn new() -> Self {
        Self
    }

    pub async fn status(&self) -> Result<DaemonStatus> {
        let pid_file = get_runtime_dir().join("fsmon.pid");
        let config_file = get_runtime_dir().join("fsmon.json");

        if !pid_file.exists() || !config_file.exists() {
            return Ok(DaemonStatus::Stopped);
        }

        let pid_str = fs::read_to_string(&pid_file)?;
        let pid: u32 = pid_str.trim().parse()?;

        // Check if process exists
        if !process_exists(pid) {
            return Ok(DaemonStatus::Stopped);
        }

        // Read config
        let config_str = fs::read_to_string(&config_file)?;
        let config: serde_json::Value = serde_json::from_str(&config_str)?;

        let paths: Vec<PathBuf> = config["paths"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| v.as_str().map(PathBuf::from))
            .collect();

        let log_file = config["log_file"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h: PathBuf| h.join(".fsmon").join("history.log"))
                    .unwrap_or_else(|| PathBuf::from("history.log"))
            });

        let start_time = config["start_time"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Count events in log file
        let event_count = if log_file.exists() {
            fs::File::open(&log_file)
                .map(|f| BufReader::new(f).lines().filter_map(Result::ok).filter(|l| !l.is_empty()).count() as u64)
                .unwrap_or(0)
        } else {
            0
        };

        // Get memory usage (simplified)
        let memory_usage = Self::get_memory_usage(pid).unwrap_or(0);

        Ok(DaemonStatus::Running {
            pid,
            paths,
            log_file,
            start_time,
            event_count,
            memory_usage,
        })
    }

    pub async fn stop(&self, force: bool) -> Result<()> {
        let pid_file = get_runtime_dir().join("fsmon.pid");
        let config_file = get_runtime_dir().join("fsmon.json");

        if !pid_file.exists() {
            println!("fsmon daemon is not running");
            return Ok(());
        }

        let pid_str = fs::read_to_string(&pid_file)?;
        let pid: u32 = pid_str.trim().parse()?;

        if !process_exists(pid) {
            println!("fsmon daemon is not running (stale PID file)");
            fs::remove_file(&pid_file)?;
            return Ok(());
        }

        // Send signal to process
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let result = unsafe { libc::kill(pid as i32, signal) };

        if result == 0 {
            if force {
                println!("fsmon daemon (PID: {}) force stopped", pid);
            } else {
                println!("fsmon daemon (PID: {}) stopped successfully", pid);
            }
        } else {
            eprintln!("Failed to stop fsmon daemon (PID: {})", pid);
        }

        // Cleanup PID file
        let _ = fs::remove_file(&pid_file);
        let _ = fs::remove_file(config_file);

        Ok(())
    }

    fn get_memory_usage(pid: u32) -> Result<u64> {
        let status_path = format!("/proc/{}/status", pid);
        let content = fs::read_to_string(&status_path)?;

        for line in content.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    return Ok(parts[1].parse::<u64>()? * 1024); // Convert kB to bytes
                }
            }
        }

        Ok(0)
    }
}
