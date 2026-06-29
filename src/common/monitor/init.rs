// Initialization methods extracted from Monitor::run() for readability.

use super::FsGroupKey;
use anyhow::{Context, Result, bail};
use fanotify_fid::consts::{
    FAN_CLASS_NOTIF, FAN_CLOEXEC, FAN_NONBLOCK, FAN_REPORT_DIR_FID, FAN_REPORT_FID, FAN_REPORT_NAME,
};
use fanotify_fid::prelude::*;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use super::{EventReceiver, EventSender, FileLogWriter, Monitor};
use crate::common::dir_cache;
use crate::common::fid_parser::{
    DIR_CACHE_CAP, FsGroup, chown_to_user, mark_directory_at, mark_recursive_with_depth,
    open_dir_safe,
};
use crate::common::filters::PathOptions;
use crate::common::monitored::PathEntry;
use crate::common::proc_cache;
use crate::common::utils::format_size;
use proc_connector::ProcConnector;

impl Monitor {
    /// Root privilege check. Bails if not root.
    pub(crate) fn check_root(&self) -> Result<()> {
        if nix::unistd::geteuid().as_raw() != 0 {
            let hint = if let Ok(exe) = std::env::current_exe() {
                if exe.to_string_lossy().contains(".cargo/bin") {
                    "\n\nHint: It looks like fsmon was installed via cargo install (~/.cargo/bin).\n\
                    sudo cannot find it because ~/.cargo/bin is not in sudo's secure_path.\n\
                    Please either:\n\
                      1. Copy to system path: sudo cp ~/.cargo/bin/fsmon /usr/local/bin/\n\
                      2. Or use full path: sudo ~/.cargo/bin/fsmon monitor ..."
                } else {
                    ""
                }
            } else {
                ""
            };

            bail!(
                "fanotify requires root privileges, please run with sudo{}",
                hint
            );
        }
        Ok(())
    }

    /// Initialize process store. Returns proc connector for event loop.
    pub(crate) fn init_process_cache(&mut self) -> Option<ProcConnector> {
        let proc_conn = proc_cache::try_create_connector();
        let store = proc_cache::DefaultStore::new(self.cache_config.proc_ttl_secs);
        let _ = proc_tree::snapshot(&store);
        self.proc.store = Some(store.clone());
        proc_conn
    }

    /// Initialize fanotify: compute masks, set up fs_groups, pending paths, inotify.
    pub(crate) fn init_fanotify(&mut self) -> Result<usize> {
        // Compute combined event mask from ALL cmd groups (OR over all entries)
        let combined_mask = self
            .monitored_entries
            .iter()
            .map(|(_, opts)| crate::common::fid_parser::path_mask_from_options(opts))
            .fold(0, |a, b| a | b);
        debug_log!(self.debug, "combined fanotify mask: {:#x}", combined_mask);

        // Collect canonical paths — non-existent paths go to pending_paths
        let mut keep_paths: Vec<PathBuf> = Vec::new();
        for path in std::mem::take(&mut self.paths) {
            if path.exists() {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                self.canonical_paths.push(canonical);
                keep_paths.push(path);
            } else {
                eprintln!(
                    "[INFO] Path '{}' does not exist yet — will start monitoring when created.",
                    path.display()
                );
                let pending_opts: Vec<PathOptions> = self
                    .monitored_entries
                    .iter()
                    .filter(|(p, _)| p == &path)
                    .map(|(_, o)| o.clone())
                    .collect();
                self.monitored_entries.retain(|(p, _)| p != &path);
                for opts in pending_opts {
                    self.inotify_state.pending_paths.push((
                        path.clone(),
                        PathEntry {
                            path: path.clone(),
                            recursive: Some(opts.recursive),
                            types: opts
                                .event_types
                                .as_ref()
                                .map(|v| v.iter().map(|t| t.to_string()).collect()),
                            size: opts
                                .size_filter
                                .map(|f| format!("{}{}", f.op(), format_size(f.bytes()))),
                            cmd: opts.cmd,
                            max_depth: opts.max_depth,
                            symlink_target: None,
                        },
                    ));
                }
            }
        }
        self.paths = keep_paths;
        // Initialize inotify for watching parent dirs of pending paths
        self.inotify_state.inotify = Some(inotify::Inotify::init().context("inotify_init")?);
        self.setup_inotify_watches();

        // Initialize per-filesystem fanotify fds.
        let mut fs_group_devs: std::collections::HashMap<u64, FsGroupKey> =
            std::collections::HashMap::new();
        for (i, canonical) in self.canonical_paths.iter().enumerate() {
            let path_mask = combined_mask;

            // Determine filesystem via st_dev
            let dev_id = std::fs::metadata(canonical)
                .ok()
                .map(|m| std::os::linux::fs::MetadataExt::st_dev(&m))
                .unwrap_or(0);

            // Try to reuse an existing FsGroup on the same filesystem
            if let Some(&key) = fs_group_devs.get(&dev_id) {
                // Same filesystem — just add inode mark
                let fan_fd = &self.fanotify.groups[key].fan_fd;
                let dir_fd = match open_dir_safe(canonical) {
                    Ok(fd) => fd,
                    Err(e) => {
                        eprintln!(
                            "[WARNING] Cannot open {} for marking: {:#}",
                            canonical.display(),
                            e
                        );
                        continue;
                    }
                };
                if let Err(e) = mark_directory_at(fan_fd, &dir_fd, path_mask) {
                    eprintln!(
                        "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                        canonical.display(),
                        fan_fd.as_raw_fd(),
                        e
                    );
                } else {
                    eprintln!(
                        "[INFO] Added {} (inode mark) on existing fd {}",
                        canonical.display(),
                        fan_fd.as_raw_fd()
                    );
                    let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
                    if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                        let max_depth = opts.and_then(|o| o.max_depth);
                        let _ = mark_recursive_with_depth(fan_fd, path_mask, canonical, max_depth);
                    }
                }
                self.fanotify.groups[key].ref_count += 1;
                self.fanotify
                    .path_to_group
                    .insert(self.paths[i].clone(), key);
                continue;
            }

            // New filesystem — create fanotify fd + mount fd
            let new_fd = fanotify_init(
                FAN_CLOEXEC
                    | FAN_NONBLOCK
                    | FAN_CLASS_NOTIF
                    | FAN_REPORT_FID
                    | FAN_REPORT_DIR_FID
                    | FAN_REPORT_NAME,
                (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
            )
            .with_context(|| {
                format!(
                    "fanotify_init failed for {} (requires Linux 5.9+ kernel)",
                    canonical.display()
                )
            })?;

            let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
            let recursive = opts.is_some_and(|o| o.recursive) && canonical.is_dir();
            let max_depth = opts.and_then(|o| o.max_depth);
            if self
                .add_mark_upward(&new_fd, path_mask, canonical, recursive, max_depth)
                .is_none()
            {
                drop(new_fd);
                continue;
            }

            // Open directory fd for open_by_handle_at
            let mount_fd = match Self::open_dir(canonical) {
                Ok(fd) => fd,
                Err(e) => {
                    eprintln!(
                        "[WARNING] Could not open directory fd for {}: {}",
                        canonical.display(),
                        e
                    );
                    drop(new_fd);
                    continue;
                }
            };

            let key = self.fanotify.groups.insert(FsGroup {
                dev_id,
                fan_fd: new_fd,
                mount_fd,
                ref_count: 1,
            });
            fs_group_devs.insert(dev_id, key);
            self.fanotify
                .path_to_group
                .insert(self.paths[i].clone(), key);
        }

        let fan_group_count = self.fanotify.groups.len();

        if fan_group_count > 0 {
            // Pre-cache directory handles (shared across fds)
            for (i, canonical) in self.canonical_paths.iter().enumerate() {
                if canonical.is_dir() {
                    let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
                    let recursive = opts.is_some_and(|o| o.recursive);
                    if recursive {
                        dir_cache::cache_recursive(&self.fanotify.dir_cache, canonical);
                    } else {
                        dir_cache::cache_dir_handle(&self.fanotify.dir_cache, canonical);
                    }
                }
            }
        } else if self.inotify_state.pending_paths.is_empty() {
            eprintln!(
                "No entries configured. Waiting for socket commands (use 'fsmon add <cmd> --path <path>')."
            );
        }

        Ok(fan_group_count)
    }

    /// Initialize logging: create log dir, chown, disk space check.
    pub(crate) fn init_logging(&self) -> Result<()> {
        // Ensure log directory exists and is owned by the original user
        if let Some(ref dir) = self.log_dir {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create log directory {}", dir.display()))?;
            match chown_to_user(dir) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!(
                        "[WARNING] Log directory '{}' is on a filesystem that does not support\n         ownership changes (e.g. vfat/exfat/NFS). Log files will remain owned by root.\n         Run 'sudo fsmon clean' if you cannot clean logs as a normal user.",
                        dir.display()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[WARNING] Could not chown log directory '{}': {}.\n         Log files may remain owned by root.",
                        dir.display(),
                        e
                    );
                }
            }
        }

        // Startup disk space check
        if let Some(ref threshold_str) = self.disk_min_free
            && let Some(ref dir) = self.log_dir
        {
            Self::check_disk_space(dir, threshold_str);
        }

        Ok(())
    }

    /// Print startup status: metrics, active paths, pending paths, cache stats.
    pub(crate) fn print_startup_status(&self, fan_group_count: usize) {
        println!("Starting file trace monitor...");

        // Initialize metrics counters
        self.metrics
            .set_monitored_paths(self.monitored_entries.len() as i64);
        self.metrics
            .set_pending_paths(self.inotify_state.pending_paths.len() as i64);
        self.metrics
            .set_reader_groups(self.fanotify.groups.len() as i64);

        if !self.canonical_paths.is_empty() {
            println!("Active paths ({} fd(s)):", fan_group_count);
            for (path, opts) in &self.monitored_entries {
                let label = match opts.cmd {
                    Some(ref name) => format!("[{}]", name),
                    None => "[global]".to_string(),
                };
                println!("  {} {}", label, path.display());
            }
        }
        if self.debug {
            debug_log!(
                self.debug,
                "monitored_entries ({} entries, full list):",
                self.monitored_entries.len()
            );
            for (i, (p, o)) in self.monitored_entries.iter().enumerate() {
                debug_log!(
                    self.debug,
                    "  [{}] {} cmd={} recursive={}",
                    i,
                    p.display(),
                    o.cmd.as_deref().unwrap_or("global"),
                    o.recursive
                );
            }
            debug_log!(self.debug, "--- cache stats ---");
            debug_log!(
                self.debug,
                "  dir_cache:        {}/{} entries",
                self.fanotify.dir_cache.entry_count(),
                DIR_CACHE_CAP
            );
            if let Some(ref s) = self.proc.store {
                debug_log!(self.debug, "  proc_store:       {} entries", s.len());
            }
            debug_log!(
                self.debug,
                "  file_size_cache:  {}/{} entries",
                self.file_size_cache.len(),
                self.file_size_cache.cap()
            );
        }
        if !self.inotify_state.pending_paths.is_empty() {
            println!("Pending paths (waiting for directory creation):");
            let mut by_cmd: std::collections::BTreeMap<Option<String>, Vec<&PathBuf>> =
                std::collections::BTreeMap::new();
            for (path, entry) in &self.inotify_state.pending_paths {
                let cmd = entry.cmd.as_deref().and_then(|c| {
                    if c == crate::common::monitored::CMD_GLOBAL {
                        None
                    } else {
                        Some(c.to_string())
                    }
                });
                by_cmd.entry(cmd).or_default().push(path);
            }
            for (cmd, paths) in &by_cmd {
                let label = match cmd {
                    Some(name) => format!("[{}]", name),
                    None => "[global]".to_string(),
                };
                for path in paths {
                    println!("  {} {}", label, path.display());
                }
            }
        }
    }

    /// Spawn reader tasks and file writer. Returns (event_rx, dir_cache).
    pub(crate) fn spawn_tasks(
        &mut self,
    ) -> (
        EventReceiver,
        moka::sync::Cache<fanotify_fid::types::HandleKey, std::path::PathBuf>,
    ) {
        // Spawn one reader task per FsGroup
        let (event_tx, event_rx) = match self.cache_config.channel_capacity {
            Some(cap) if cap > 0 => {
                let (tx, rx) = tokio::sync::mpsc::channel(cap);
                (EventSender::Bounded(tx), EventReceiver::Bounded(rx))
            }
            _ => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                (EventSender::Unbounded(tx), EventReceiver::Unbounded(rx))
            }
        };
        let dir_cache = self.fanotify.dir_cache.clone();

        // Shared state for live-add
        self.event_tx = Some(event_tx);
        self.fanotify.shared_dir_cache = Some(dir_cache.clone());

        let keys: Vec<_> = self.fanotify.groups.keys().collect();
        for key in keys {
            self.spawn_fd_reader(key);
        }

        // Spawn file writer task
        let fw_log_dir = self.log_dir.clone();
        let fw_debug = self.debug;
        let fw_local = self.local_time;
        let fw_metrics = self.metrics.clone();
        if let Some(fw_log_dir) = fw_log_dir
            && let Some(ref tx) = self.event_stream_tx
        {
            let fw_rx = tx.subscribe();
            let fw = FileLogWriter::new(fw_log_dir, fw_debug, fw_local, fw_metrics);
            tokio::spawn(async move {
                fw.run(fw_rx).await;
            });
        }

        (event_rx, dir_cache)
    }
}
