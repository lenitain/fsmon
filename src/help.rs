pub enum HelpTopic {
    Root,
    Daemon,
    Add,
    Remove,
    Managed,
    Query,
    Clean,
    Install,
    Uninstall,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Daemon => "Run the fsmon daemon as a background service (managed by systemd)",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove a path from the monitoring list",
        HelpTopic::Managed => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
        HelpTopic::Install => "Install fsmon systemd service and generate default configuration",
        HelpTopic::Uninstall => "Uninstall fsmon systemd service",
    }
}

pub const fn long_about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "",
        HelpTopic::Daemon => {
            r#"Run the fsmon daemon as a background service (managed by systemd).

The daemon monitors all configured paths via fanotify and logs events.
Use 'fsmon add'/'fsmon remove' to manage paths dynamically without
restarting the daemon.

Systemd:
  sudo systemctl start fsmon
  sudo systemctl enable fsmon
  sudo systemctl status fsmon"#
        }
        HelpTopic::Add => {
            r#"Add a path to the monitoring list.

The path is added immediately if the daemon is running, and persisted
in /etc/fsmon/fsmon.toml for automatic monitoring on daemon restart.

Options:
  -r, --recursive     Watch subdirectories recursively
  -t, --types         Event types to monitor (comma-separated)
  -m, --min-size      Minimum file size change to report (e.g., 100MB, 1GB)
  -e, --exclude       Glob patterns to exclude (e.g., "*.tmp")
  --all-events        Monitor all 14 fanotify event types

Examples:
  fsmon add /var/www -r --types MODIFY,CREATE
  fsmon add /etc --types MODIFY --min-size 100KB
  fsmon add /tmp --all-events"#
        }
        HelpTopic::Remove => {
            r#"Remove a path from the monitoring list.

The path is removed immediately if the daemon is running.

Examples:
  fsmon remove /var/www"#
        }
        HelpTopic::Managed => {
            r#"List all monitored paths with their configuration.

Displays each path with its recursive flag, event type filters,
minimum size threshold, and exclusion patterns.

Examples:
  fsmon managed"#
        }
        HelpTopic::Query => {
            r#"Query historical file change events from log files.

Options:
  --log-file        Log file path (default: from /etc/fsmon/fsmon.toml)
  --since           Start time: relative (1h, 30m, 7d) or absolute
  --until           End time
  --pid             Filter by PID (comma-separated)
  --cmd             Filter by process name (wildcards: nginx*)
  --user            Filter by username (comma-separated)
  --types           Filter by event type (comma-separated)
  --min-size        Minimum size change
  --format          Output format: human, json, csv
  --sort            Sort by: time, size, pid

Examples:
  fsmon query --since 1h
  fsmon query --cmd nginx
  fsmon query --since 1h --cmd nginx --types MODIFY --format json"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

Options:
  --log-file        Log file path (default: from /etc/fsmon/fsmon.toml)
  --keep-days       Keep logs from last N days (default: 30)
  --max-size        Maximum log file size (e.g., 100MB, 1GB)
  --dry-run         Preview mode, don't actually delete

Examples:
  fsmon clean --keep-days 7
  fsmon clean --max-size 100MB --dry-run"#
        }
        HelpTopic::Install => {
            r#"Install fsmon systemd service and generate default configuration.

Creates /etc/systemd/system/fsmon.service with:
  - Runtime directory at /run/fsmon/ for unix socket
  - CAP_SYS_ADMIN capability for fanotify
  - Automatic restart on failure

Also creates /etc/fsmon/fsmon.toml if it doesn't exist.

Examples:
  sudo fsmon install
  sudo fsmon install --force    # Reinstall existing service"#
        }
        HelpTopic::Uninstall => {
            r#"Uninstall fsmon systemd service.

Removes /etc/systemd/system/fsmon.service.
Does NOT stop running instances — stop first with:
  sudo systemctl stop fsmon

Examples:
  sudo fsmon uninstall"#
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Management:
  fsmon add /var/www -r                 Add path with recursive monitoring
  fsmon remove /var/www                 Remove path
  fsmon managed                         List monitored paths

Query & Clean:
  fsmon query --since 1h                Events from last hour
  fsmon query --cmd nginx               Filter by process name
  fsmon clean --keep-days 7             Keep 7 days of logs

Daemon:
  sudo fsmon install                    Install systemd service
  sudo systemctl start fsmon            Start daemon
  fsmon add /var/www                    Add path (daemon picks it up live)
  sudo systemctl stop fsmon             Stop daemon
  sudo fsmon uninstall                  Remove systemd service"#
}
