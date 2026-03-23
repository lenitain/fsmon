use anyhow::Result;
use chrono::Utc;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::{FileEvent, OutputFormat};
use crate::utils::get_process_info;

pub struct Monitor {
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<String>>,
    exclude: Option<String>,
    output: Option<PathBuf>,
    format: OutputFormat,
}

impl Monitor {
    pub fn new(
        paths: Vec<PathBuf>,
        min_size: Option<i64>,
        event_types: Option<Vec<String>>,
        exclude: Option<String>,
        output: Option<PathBuf>,
        format: OutputFormat,
    ) -> Self {
        Self {
            paths,
            min_size,
            event_types,
            exclude,
            output,
            format,
        }
    }

    pub async fn run(self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<FileEvent>(1000);
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();

        // Handle Ctrl+C
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            r.store(false, Ordering::SeqCst);
        });

        // Setup file watcher
        let mut watcher: RecommendedWatcher = Watcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.try_send(process_event(event));
                }
            },
            Config::default(),
        )?;

        // Watch all paths
        for path in &self.paths {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            watcher.watch(&canonical, RecursiveMode::Recursive)?;
        }

        // Setup output file if specified
        let mut output_file = if let Some(ref path) = self.output {
            let parent = path.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(parent)?;
            Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            )
        } else {
            None
        };

        println!("Starting file trace monitor...");
        println!("Monitoring paths: {}", self.paths.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "));
        println!("Press Ctrl+C to stop\n");

        while running.load(Ordering::SeqCst) {
            match timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Some(event)) => {
                    if self.should_output(&event) {
                        self.output_event(&event, &mut output_file).await?;
                    }
                }
                _ => continue,
            }
        }

        println!("\nStopping file trace monitor...");
        Ok(())
    }

    pub async fn run_daemon(self) -> Result<()> {
        // Create PID file
        let pid_file = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join("fsmon.pid");

        if pid_file.exists() {
            let pid_str = fs::read_to_string(&pid_file)?;
            let pid: u32 = pid_str.trim().parse()?;
            if process_exists(pid) {
                println!("fsmon daemon already running (PID: {})", pid);
                return Ok(());
            }
        }

        // Write PID file
        fs::write(&pid_file, process::id().to_string())?;

        // Create log directory
        let log_file = self.output.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h: PathBuf| h.join(".fsmon").join("history.log"))
                .unwrap_or_else(|| PathBuf::from("history.log"))
        });

        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // Save daemon config
        let config_file = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join("fsmon.json");
        let config = serde_json::json!({
            "paths": self.paths,
            "log_file": log_file,
            "start_time": Utc::now().to_rfc3339(),
        });
        fs::write(&config_file, serde_json::to_string_pretty(&config)?)?;

        println!("fsmon daemon started (PID: {}), log file: {}",
            process::id(),
            log_file.display()
        );

        // Run the monitor
        self.run().await?;

        // Cleanup
        let _ = fs::remove_file(&pid_file);
        let _ = fs::remove_file(&config_file);

        Ok(())
    }

    fn should_output(&self, event: &FileEvent) -> bool {
        // Check event type filter
        if let Some(ref types) = self.event_types {
            if !types.contains(&event.event_type) {
                return false;
            }
        }

        // Check min size filter
        if let Some(min) = self.min_size {
            if event.size_change.abs() < min {
                return false;
            }
        }

        // Check exclude pattern
        if let Some(ref exclude) = self.exclude {
            if let Ok(pattern) = regex::Regex::new(&exclude.replace("*", ".*")) {
                if pattern.is_match(&event.path.to_string_lossy()) {
                    return false;
                }
            }
        }

        true
    }

    async fn output_event(
        &self,
        event: &FileEvent,
        output_file: &mut Option<fs::File>,
    ) -> Result<()> {
        match self.format {
            OutputFormat::Human => {
                let output = event.to_human_string();
                println!("{}", output);

                // Also write to file if specified
                if let Some(file) = output_file {
                    writeln!(file, "{}", serde_json::to_string(event)?)?;
                }
            }
            OutputFormat::Json => {
                let json = serde_json::to_string(event)?;
                println!("{}", json);

                if let Some(file) = output_file {
                    writeln!(file, "{}", json)?;
                }
            }
            OutputFormat::Csv => {
                let csv = format!(
                    "{},{},{},{},{},{},{}",
                    event.time.to_rfc3339(),
                    event.event_type,
                    event.path.display(),
                    event.pid,
                    event.cmd,
                    event.user,
                    event.size_change
                );
                println!("{}", csv);

                if let Some(file) = output_file {
                    writeln!(file, "{}", serde_json::to_string(event)?)?;
                }
            }
        }
        Ok(())
    }
}

fn process_event(event: Event) -> FileEvent {
    let event_type = match event.kind {
        EventKind::Create(_) => "CREATE",
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => "RENAME",
        EventKind::Modify(_) => "MODIFY",
        EventKind::Remove(_) => "DELETE",
        _ => "UNKNOWN",
    }.to_string();

    let path = event.paths.first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("unknown"));

    // Get process info
    let (pid, cmd, user) = get_process_info(&path);

    // Try to get file size change
    let size_change = if let Ok(metadata) = fs::metadata(&path) {
        metadata.len() as i64
    } else {
        0
    };

    FileEvent {
        time: Utc::now(),
        event_type,
        path,
        pid,
        cmd,
        user,
        size_change,
    }
}

fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

use std::process;
