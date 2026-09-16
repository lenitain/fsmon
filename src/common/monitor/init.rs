// Initialization methods extracted from Monitor::run() for readability.

use super::FsGroupKey;
use crate::{debug_log, info_log};
use anyhow::{Context, Result};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use super::factory::{FanotifyFactory, GROUP_INIT_FLAGS, UNLIMITED_MARKS};
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

/// Degraded-mode opt-in. Without `CAP_SYS_ADMIN` the daemon refuses to start
/// unless the operator explicitly accepts losing pid attribution.
fn unprivileged_allowed() -> bool {
    std::env::var_os("FSMON_ALLOW_UNPRIVILEGED")
        .map(|v| v != "0")
        .unwrap_or(false)
}

impl Monitor {
    /// Verify that this process can create a *privileged* fanotify group.
    ///
    /// The kernel decides that itself with `capable(CAP_SYS_ADMIN)` inside
    /// `fanotify_init()`, so probing an admin-only init flag is more faithful
    /// than `capget()` — inside a user namespace `capget()` reports full
    /// capabilities while the group is still marked `FANOTIFY_UNPRIV`.
    ///
    /// Missing it means every event caused by another process gets
    /// `metadata.pid = 0` (fanotify_user.c), silently destroying the process
    /// attribution that is fsmon's entire point. Hence: loud failure, not a
    /// quiet degraded run (PRIVILEGE-SEPARATION-PLAN.md §6 阶段 1).
    pub(crate) fn check_privileges(&self) -> Result<()> {
        if self.privileged {
            return Ok(());
        }
        if unprivileged_allowed() {
            eprintln!(
                "[WARNING] Running WITHOUT CAP_SYS_ADMIN (FSMON_ALLOW_UNPRIVILEGED is set).\n\
                 \x20        The kernel will blank metadata.pid for events caused by other\n\
                 \x20        processes, so `pid`/`comm`/`cmd` attribution is degraded to 0/empty.\n\
                 \x20        Paths, event types and timestamps are still correct."
            );
            return Ok(());
        }
        Err(crate::common::privileges::PermanentStartupError::new(
            "fsmon requires CAP_SYS_ADMIN to create a privileged fanotify group.\n\
             Without it the kernel blanks the pid of events caused by other processes\n\
             (fanotify_user.c: `metadata.pid = 0`), so process attribution silently fails.\n\
             Start fsmon through the hardened systemd unit\n\
             (`AmbientCapabilities=CAP_SYS_ADMIN`, see 'fsmon init --service'),\n\
             or set FSMON_ALLOW_UNPRIVILEGED=1 to accept the degraded mode.",
        )
        .into())
    }

    /// Fork the privileged "fanotify factory" subprocess.
    ///
    /// Must run before [`Self::drop_privileges`]: the child inherits
    /// CAP_SYS_ADMIN at fork() time, the parent then sheds it.
    pub(crate) fn spawn_factory(&mut self) -> Result<()> {
        let factory = FanotifyFactory::spawn().context("forking the fanotify factory")?;
        self.fanotify.factory = Some(std::sync::Arc::new(factory));
        Ok(())
    }

    /// Drop the daemon's own privileges once the initial marks exist.
    pub(crate) fn drop_privileges(&self) -> Result<()> {
        crate::common::privileges::drop_privileges()
    }

    /// Group init flags. `FAN_UNLIMITED_MARKS` lifts the per-uid mark cap
    /// (plan §5.7) but is an admin-only flag, so it is only requested when
    /// the daemon actually holds CAP_SYS_ADMIN.
    pub(crate) fn group_init_flags(&self) -> u32 {
        if self.privileged {
            GROUP_INIT_FLAGS | UNLIMITED_MARKS
        } else {
            GROUP_INIT_FLAGS
        }
    }

    /// Initialize process tracking (event-driven, RUN-23). Returns the proc
    /// connector for the event loop. The tracker adopts a one-shot `/proc`
    /// baseline; events maintain it from then on.
    pub(crate) fn init_process_cache(&mut self) -> Option<ProcConnector> {
        let proc_conn = proc_cache::try_create_connector();
        let config = proc_tree::TrackerConfig {
            domain: proc_tree::DomainId(1),
            history: proc_tree::HistoryPolicy::Count(proc_cache::PROC_HISTORY_CAP),
            stop: proc_tree::StopPolicy::Continue,
        };
        let (tracker, src) = proc_cache::init_tracker(config, std::path::Path::new("/proc"));
        self.proc.tracker = Some(tracker);
        self.proc.source = Some(src);
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
                info_log!(
                    "Path '{}' does not exist yet — will start monitoring when created.",
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

        // Initialize per-filesystem fanotify fds via the privileged factory.
        let factory = self
            .fanotify
            .factory
            .clone()
            .context("fanotify factory has not been started")?;
        let init_flags = self.group_init_flags();
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
                if let Err(e) = mark_directory_at(&factory, fan_fd, &dir_fd, path_mask) {
                    eprintln!(
                        "[WARNING] Cannot inode-mark {} on fd {}: {:#}",
                        canonical.display(),
                        fan_fd.as_raw_fd(),
                        e
                    );
                } else {
                    info_log!(
                        "Added {} (inode mark) on existing fd {}",
                        canonical.display(),
                        fan_fd.as_raw_fd()
                    );
                    let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
                    if opts.is_some_and(|o| o.recursive) && canonical.is_dir() {
                        let max_depth = opts.and_then(|o| o.max_depth);
                        let _ = mark_recursive_with_depth(
                            &factory,
                            fan_fd,
                            path_mask,
                            canonical,
                            max_depth,
                            self.fanotify.shared_dir_cache.as_ref(),
                        );
                    }
                }
                self.fanotify.groups[key].ref_count += 1;
                self.fanotify
                    .path_to_group
                    .insert(self.paths[i].clone(), key);
                continue;
            }

            // New filesystem — ask the factory for a group and mark the root.
            let opts = self.paths.get(i).and_then(|p| self.first_opt_for_path(p));
            let recursive = opts.is_some_and(|o| o.recursive) && canonical.is_dir();
            let max_depth = opts.and_then(|o| o.max_depth);

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
            let new_fd = match factory.create_group_and_mark(&dir_fd, init_flags, path_mask) {
                Ok(fd) => fd,
                Err(e) => {
                    eprintln!(
                        "[WARNING] Cannot create fanotify group for {}: {:#}",
                        canonical.display(),
                        e
                    );
                    continue;
                }
            };
            // create_group_and_mark() already added the inode mark on `dir_fd`.
            info_log!(
                "Monitoring {} (inode mark) on fd {}",
                canonical.display(),
                new_fd.as_raw_fd()
            );
            if recursive {
                let _ = mark_recursive_with_depth(
                    &factory,
                    &new_fd,
                    path_mask,
                    canonical,
                    max_depth,
                    self.fanotify.shared_dir_cache.as_ref(),
                );
            }

            let key = self.fanotify.groups.insert(FsGroup {
                dev_id,
                fan_fd: new_fd,
                ref_count: 1,
            });
            fs_group_devs.insert(dev_id, key);
            self.fanotify
                .path_to_group
                .insert(self.paths[i].clone(), key);
        }

        let fan_group_count = self.fanotify.groups.len();

        if fan_group_count > 0 {
            // Directory handles are cached by the marking walk itself, from the
            // descriptors it opens — see mark_recursive_with_depth().  Only
            // non-recursive roots need caching here, because the walk starts at
            // depth 1 and skips depth 0 (the caller already marked that one).
            for (i, canonical) in self.canonical_paths.iter().enumerate() {
                if !canonical.is_dir() {
                    continue;
                }
                let recursive = self
                    .paths
                    .get(i)
                    .and_then(|p| self.first_opt_for_path(p))
                    .is_some_and(|o| o.recursive);
                if !recursive {
                    dir_cache::cache_dir_handle(&self.fanotify.dir_cache, canonical);
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
            if let Some(ref t) = self.proc.tracker {
                debug_log!(self.debug, "  proc_store:       {} live", t.live_count());
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
    pub(crate) fn spawn_tasks(&mut self) -> (EventReceiver, dir_cache::DirCache) {
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
