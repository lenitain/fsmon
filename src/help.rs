pub enum HelpTopic {
    Root,
    Daemon,
    Init,
    Cd,
    Add,
    Remove,
    Managed,
    Query,
    Clean,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Daemon => "Run the fsmon daemon (requires sudo for fanotify)",
        HelpTopic::Init => "Initialize log and managed data directories",
        HelpTopic::Cd => "Print the log directory path",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove a path from the monitoring list",
        HelpTopic::Managed => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
    }
}

pub const fn long_about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "",
        HelpTopic::Daemon => {
            r#"Run the fsmon daemon as a foreground process (requires sudo for fanotify).

The daemon monitors all configured paths via fanotify and logs events.
Use 'fsmon add'/'fsmon remove' to manage paths dynamically without
restarting the daemon.

Usage:
  sudo fsmon daemon &       Start daemon in background
  fsmon add /path --types all       All 14 event types
  fsmon add /path --exclude-cmd 'rsync'  Exclude by process name
  fsmon managed                       List monitored paths
  fsmon query --since 1h    Query events

Config:           ~/.config/fsmon/fsmon.toml
Managed:          ~/.local/share/fsmon/managed.jsonl (configurable via [managed].file)
Log dir:          ~/.local/state/fsmon/ (configurable via [logging].dir)
Socket:           /tmp/fsmon-<UID>.sock (configurable via [socket].path)"#
        }
        HelpTopic::Init => {
            r#"Initialize fsmon data directories (chezmoi-style).

Creates the default log directory and managed data directory.
Config file at ~/.config/fsmon/fsmon.toml is optional — defaults
apply without it.

Created:
  ~/.local/state/fsmon/     Event log storage
  ~/.local/share/fsmon/     Managed paths database
  ~/.config/fsmon/          Config directory (for optional fsmon.toml)

Examples:
  fsmon init"#
        }
        HelpTopic::Cd => {
            r#"Print the log directory path.

Useful for quickly navigating to the event logs:
  cd $(fsmon cd)

Examples:
  fsmon cd                       Print log directory path
  cd $(fsmon cd) && ls           Navigate to logs"#
        }
        HelpTopic::Add => {
            r#"Add a path to the monitoring list.

The path is added immediately if the daemon is running, and persisted
in the managed paths database for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

Options:
  -r, --recursive         Watch subdirectories recursively
  -t, --types             Event types to monitor (repeatable; use "all" for all 14 types)
  -s, --size             Size filter with comparison operator (e.g. >1MB, >=500KB, <100MB)
                          Note: -s in add means size filter; -s in clean means size limit
  -e, --exclude           Path regex patterns to exclude (repeatable, prefix ! to invert)
  --exclude-cmd           Process name regex patterns to exclude (repeatable, prefix ! to invert)

Regex syntax:
  --exclude '\.tmp$' --exclude '\.log$'   Exclude .tmp and .log files
  --exclude '!.*\.py$'                      Only track .py files, exclude all others
  --exclude-cmd 'rsync' --exclude-cmd 'apt'  Exclude rsync and apt processes
  --exclude-cmd '!nginx|python'              Only track nginx and python processes
  Standard Rust regex syntax supported.
  prefix ! to invert (only valid as the first pattern)

Examples:
  fsmon add /path/to/project -r                 Default: 8 event types
  fsmon add /path --types MODIFY --types CREATE  Only these 2 types
  fsmon add /path --types all                   All 14 event types
  fsmon add /etc --size '>=100KB'
  fsmon add /var/log --exclude-cmd 'rsync'
  fsmon add /tmp --exclude-cmd 'nginx'"#
        }
        HelpTopic::Remove => {
            r#"Remove a path from the monitoring list.

The path is removed immediately if the daemon is running.

Examples:
  fsmon remove /path/to/watch"#
        }
        HelpTopic::Managed => {
            r#"List all monitored paths with their configuration.

Displays each path with its recursive flag, event type filters,
size threshold, path/cmd exclusion patterns.

Examples:
  fsmon managed"#
        }
        HelpTopic::Query => {
            r#"Query historical file change events from log files.

Output is JSONL (one JSON object per line), pipe to jq for custom filtering.

Options:
  -p, --path        Path(s) to query. Repeatable. Default: all.
  -s, --since       Start time: relative (1h, 30m, 7d) or absolute
  -u, --until       End time

Examples:
  fsmon query --since 1h
  fsmon query --path /tmp --since 1h
  fsmon query --since 1h | jq 'select(.cmd == "nginx")'
  fsmon query | jq -s 'sort_by(.file_size)[]'"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

Defaults: keep_days=30, size=1GB (from fsmon.toml [logging] section or code fallback).
CLI args override config. Daemon does not auto-clean; use cron/systemd timer.

Options:
  --path            Path(s) to clean. Repeatable. Default: all.
  --keep-days       Keep logs from last N days
  --size            Size limit for log file truncation (e.g. >500MB, >=1GB) (short: -s)
                          Note: -m in clean means size limit; -s in add means size filter
  --dry-run         Preview mode, don't actually delete

Examples:
  fsmon clean                       Use config defaults
  fsmon clean --keep-days 7         Override retention
  fsmon clean --path /tmp --dry-run Preview without deleting"#
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Setup (no sudo needed):
  fsmon init                        Create log and managed directories
  cd $(fsmon cd)                    Navigate to log directory

Daemon (requires sudo):
  sudo fsmon daemon &               Start daemon in background
  kill %1                           Stop daemon (or Ctrl+C)

Management (no sudo needed):
  fsmon add /path -r                Add path (recursive, default 8 types)
  fsmon add /path --exclude-cmd 'rsync'  Exclude by process name
  fsmon remove /path                Remove path
  fsmon managed                     List monitored paths

Query (stdout JSONL, pipe to jq):
  fsmon query --since 1h            Events from last hour
  fsmon query | jq 'select(.cmd == "nginx")'  Custom filter

Clean (config defaults: keep_days=30, size=1GB):
  fsmon clean                       Clean all logs
  fsmon clean --keep-days 7         Override retention
  fsmon clean --dry-run             Preview without deleting

Config: ~/.config/fsmon/fsmon.toml (optional — defaults without it)
Managed: ~/.local/share/fsmon/managed.jsonl (configurable via [managed].file)
Logs:   ~/.local/state/fsmon/*_log.jsonl (configurable via [logging].dir)"#
}
