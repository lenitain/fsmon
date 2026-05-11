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
    P2l,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Daemon => "Run the fsmon daemon (requires sudo for fanotify)",
        HelpTopic::Init => "Initialize log and managed data directories",
        HelpTopic::Cd => "Open a subshell in the log directory",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove one or more paths from the monitoring list",
        HelpTopic::Managed => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
        HelpTopic::P2l => "Path to log filename (pure hash, no I/O)",
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
  fsmon query -t '>1h'    Query events from last hour

Config:           ~/.config/fsmon/fsmon.toml
Managed:          ~/.local/share/fsmon/managed.jsonl (configurable via [managed].path)
Log dir:          ~/.local/state/fsmon/ (configurable via [logging].path)
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

Examples:
  fsmon init"#
        }
        HelpTopic::Cd => {
            r#"Open a subshell in the log directory.

Spawns a new shell (using $SHELL, fallback /bin/sh) inside the
log directory. Type 'exit' to return to the original directory.

Examples:
  fsmon cd                       Enter log directory in subshell
  fsmon cd && ls                 List log files, then exit"#
        }
        HelpTopic::Add => {
            r#"Add a path to the monitoring list.

The path is added immediately if the daemon is running, and persisted
in the managed paths database for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

Options:
  -r, --recursive         Watch subdirectories recursively
  -t, --types             Event types to monitor (repeatable; use "all" for all 14 types)
  -s, --size             Size filter with operator (required: >=, >, <=, <, =)
                          e.g. >1MB, >=500KB, <100MB, =0
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
            r#"Remove one or more paths from the monitoring list.

Paths are removed immediately if the daemon is running.

Examples:
  fsmon remove /path/to/watch
  fsmon remove /path/a /path/b /path/c"#
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
  -t, --time        Time filter with operator (repeatable).
                    >1h  — events newer than 1 hour ago (since)
                    <2026-05-01 — events before a date (until)
                    Combine both for a range: -t '>1h' -t '<now'

Alternatively, query the log files directly with standard Unix tools:
  cat ~/.local/state/fsmon/*_log.jsonl | jq 'select(.cmd == "nginx")'
  grep '"event_type":"MODIFY"' ~/.local/state/fsmon/*_log.jsonl
  tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
(Note: native fsmon query uses binary search and is significantly faster on large logs)

Examples:
  fsmon query -t '>1h'
  fsmon query --path /tmp -t '>1h'
  fsmon query -t '>1h' -t '<now' | jq 'select(.cmd == "nginx")'
  fsmon query | jq -s 'sort_by(.file_size)[]'"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

Defaults: keep_days=30, size=>=1GB (from fsmon.toml [logging] section or code fallback).
CLI args override config. Daemon does not auto-clean; use cron/systemd timer.

Options:
  --path            Path(s) to clean. Repeatable. Default: all.
  --time            Time filter with operator (e.g. >30d — keep newer than 30 days)
  --size            Size limit for log file truncation with operator (e.g. >500MB, >=1GB).
                          Operator required: >=, >, <=, <, = (short: -s)
  --dry-run         Preview mode, don't actually delete

Alternatively, clean the log files directly with standard Unix tools:
  truncate --size 100M ~/.local/state/fsmon/*_log.jsonl
  for f in ~/.local/state/fsmon/*_log.jsonl; do tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"; done
  find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
(Note: native fsmon clean uses accurate JSONL parsing and is safer for large files)

Examples:
  fsmon clean                       Use config defaults (>=30d)
  fsmon clean --time '>7d'          Keep last 7 days
  fsmon clean --path /tmp --dry-run Preview without deleting"#
        }
        HelpTopic::P2l => {
            r#"Resolve the log file path for one or more paths.

Log files are named by FNV-1a hash of the path (to avoid Linux's 255-byte
filename limit). This command computes the hash and outputs the full log
file path — pure computation, no I/O beyond loading the tiny config file.

Multiple paths can be specified; each result is printed on its own line.

Use it to pipe or tail logs for a specific path:
  tail -f "$(fsmon p2l /home/user/projects)"
  cat "$(fsmon p2l /tmp)" | jq '.'

Examples:
  fsmon p2l /home/user/projects
  fsmon p2l /tmp /var/log /etc"#
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Setup (no sudo needed):
  fsmon init                        Create log and managed directories
  fsmon cd                          Open subshell in log directory

Daemon (requires sudo):
  sudo fsmon daemon &               Start daemon in background
  kill %1                           Stop daemon (or Ctrl+C)

Management (no sudo needed):
  fsmon add /path -r                Add path (recursive, default 8 types)
  fsmon add /path --exclude-cmd 'rsync'  Exclude by process name
  fsmon remove /path                Remove path(s), multiple paths OK
  fsmon managed                     List monitored paths
  fsmon p2l /path1 /path2      Resolve log file path(s)

Query (stdout JSONL, pipe to jq):
  fsmon query -t '>1h'             Events from last hour
  fsmon query | jq 'select(.cmd == "nginx")'  Custom filter
  cat ~/.local/state/fsmon/*_log.jsonl | jq ...  Or direct pipe (slower)

Clean (config defaults: keep_days=30, size=>=1GB):
  fsmon clean                       Clean all logs (keep >30d)
  fsmon clean --time '>7d'         Keep last 7 days
  fsmon clean --dry-run             Preview without deleting
  tail -500 ...                     Or direct Unix tools (slower)

Config: ~/.config/fsmon/fsmon.toml (optional — defaults without it)
Managed: ~/.local/share/fsmon/managed.jsonl (configurable via [managed].path)
Logs:   ~/.local/state/fsmon/*_log.jsonl (configurable via [logging].path)"#
}
