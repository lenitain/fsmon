pub enum HelpTopic {
    Root,
    Daemon,
    Init,
    Cd,
    Add,
    Remove,
    Monitored,
    Query,
    Clean,
    Changes,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Daemon => "Run the fsmon daemon (requires sudo for fanotify)",
        HelpTopic::Init => "Create the config file (directories created on first use)",
        HelpTopic::Cd => "Open a subshell in the monitored path or log directory",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove one or more paths from the monitoring list",
        HelpTopic::Monitored => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
        HelpTopic::Changes => "Show the most recent event per path (deduplicated changes)",
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
  sudo fsmon daemon &                     Start daemon in background
  sudo fsmon daemon --debug               Enable debug output
  sudo fsmon daemon --disk-min-free 10%       Warn when disk < 10% free
  sudo fsmon daemon --sync-interval 5         fdatasync log files every 5s
  sudo fsmon daemon --local-time              Use local timezone in timestamps
  sudo fsmon daemon --buffer-size 65536       Fanotify read buffer
  sudo fsmon daemon --channel-capacity 1024   Event channel cap (default: unbounded)
  sudo fsmon daemon --subscribe-buf 8192      Subscribe broadcast buffer
  sudo fsmon daemon --cache-dir-cap 200000    Override dir_cache capacity
  fsmon add openclaw --path /home -r          Track openclaw on /home (recursive)
  fsmon monitored                             List monitored paths
  fsmon query _global -t '>1h'             Events from last hour

For systemd integration:
  sudo fsmon init --service             Install systemd service (auto-start on crash)
  sudo systemctl start fsmon            Start via systemd
  journalctl -u fsmon -f               View daemon logs

Config:           ~/.config/fsmon/fsmon.toml
Monitored:        ~/.local/share/fsmon/monitored.jsonl (configurable via [monitored].path)
Log dir:          ~/.local/state/fsmon/ (configurable via [logging].path)
Socket:           /tmp/fsmon-<UID>.sock (configurable via [socket].path)"#
        }
        HelpTopic::Init => {
            r#"Create the config file only (chezmoi-style).

Creates:
  ~/.config/fsmon/fsmon.toml  Reference config (defaults apply without modification)

Directories are created on first use:
  - Monitored dir: by 'fsmon add' on first run
  - Log dir: by 'fsmon daemon' or 'fsmon cd -l' on first run

With --service, also installs a systemd service for automatic crash recovery:
  sudo fsmon init --service

Examples:
  fsmon init"#
        }
        HelpTopic::Cd => {
            r#"Open a subshell in the monitored path or log directory.

Spawns a new shell (using $SHELL, fallback /bin/sh).
Type 'exit' to return to the original directory.

Examples:
  fsmon cd -l                     Enter log directory in subshell
  fsmon cd -m                     Enter monitored store directory"#
        }
        HelpTopic::Add => {
            r#"Add a path or process to the monitoring list.

The entry is added immediately if the daemon is running, and persisted
in the monitored paths database for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

USAGE:
  fsmon add [CMD] [OPTIONS]

ARGS:
  <CMD>   Process name to track (process tree + ancestry chain).
          Enables process tree tracking: fork/exec children are auto-included.
          When specified, matching events include a `chain` field.
          Use '_global' to monitor all events on a path (no process tracking).

Options:
  --path <PATH>           Filesystem path to monitor
  -r, --recursive         Watch subdirectories recursively
  -t, --types             Event types to monitor (repeatable; use "all" for all 14 types)
  -s, --size             Size filter with operator (required: >=, >, <=, <, =)
                          e.g. >1MB, >=500KB, <100MB, =0
Examples:
  fsmon add openclaw --path /home -r           Track openclaw on /home (recursive)
  fsmon add _global --path /home -r            Monitor /home (all processes)
  fsmon add nginx --path /var/log/nginx        Track nginx on /var/log/nginx
  fsmon add _global --path /home --types MODIFY --types CREATE  Filter by event types
  fsmon add _global --path /home --types all                   All 14 event types
  fsmon add _global --path /home -s '>=1MB'                    Minimum file size change"#
        }
        HelpTopic::Remove => {
            r#"Remove one or more paths from the monitoring list.

Without --path, removes the entire cmd group.
With --path, removes only the specified paths. Multiple paths are atomic:
all must exist, or nothing is removed.

USAGE:
  fsmon remove [CMD] [--path <PATH>...]

ARGS:
  <CMD>   Cmd group to remove (positional). Use '_global' for global monitoring.

Options:
  --path <PATH>    Path(s) to remove from the cmd group (repeatable)

Examples:
  fsmon remove _global               Remove entire global cmd group
  fsmon remove openclaw              Remove the entire openclaw cmd group
  fsmon remove openclaw --path /a    Remove /a from openclaw group
  fsmon remove _global --path /a --path /b  Remove /a, /b from global group (atomic)"#
        }
        HelpTopic::Monitored => {
            r#"List all monitored paths with their configuration.

Displays each path with its recursive flag, event type filters,
size threshold, path/cmd exclusion patterns.

Examples:
  fsmon monitored"#
        }
        HelpTopic::Query => {
            r#"Query historical file change events from log files.

Output is JSONL (one JSON object per line), pipe to jq for custom filtering.

USAGE:
  fsmon query [CMD] [OPTIONS]

ARGS:
  <CMD>   Cmd group to query (positional). Use '_global' for global events.
          Log files are named by cmd: `{cmd}_log.jsonl` or `_global_log.jsonl`.

Options:
  -p, --path        Path prefix filter(s) applied to event.path. Repeatable.
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
  fsmon query _global                All global events
  fsmon query openclaw               Events from openclaw cmd group
  fsmon query _global --path /home   Global events under /home
  fsmon query nginx --path /var/www  Nginx events under /var/www
  fsmon query _global -t '>1h'       Events from last hour"#
        }
        HelpTopic::Clean => {
            r#"Clean a log file for a specific cmd group, retain by time or size.

Defaults: keep_days=30, size=>=1GB (from fsmon.toml [logging] section or code fallback).
CLI args override config. Daemon does not auto-clean; use cron/systemd timer.

USAGE:
  fsmon clean <CMD> [OPTIONS]

ARGS:
  <CMD>   Cmd group to clean (positional). Use '_global' for the global log.

Options:
  -t, --time        Time filter with operator (e.g. >30d — keep newer than 30 days)
  -s, --size        Size limit for log file truncation with operator (e.g. >500MB, >=1GB).
                          Operator required: >=, >, <=, <, =
  --dry-run         Preview mode, don't actually delete

Alternatively, clean the log files directly with standard Unix tools:
  truncate --size 100M ~/.local/state/fsmon/*_log.jsonl
  for f in ~/.local/state/fsmon/*_log.jsonl; do tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"; done
  find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
(Note: native fsmon clean uses accurate JSONL parsing and is safer for large files)

Examples:
  fsmon clean _global                Clean global log with defaults (>=30d)
  fsmon clean openclaw -t '>7d'     Keep last 7 days of openclaw events
  fsmon clean nginx --dry-run        Preview nginx log cleaning"#
        }
        HelpTopic::Changes => {
            r#"Show the most recent event per path — a deduplicated summary of file changes.

Output is JSONL (same format as `query`), but with duplicate paths collapsed:
only the latest event for each unique path is shown, sorted by time descending.

This is the easiest way to answer "what files changed since last deploy?"

USAGE:
  fsmon changes [CMD] [OPTIONS]

ARGS:
  <CMD>   Cmd group to query (positional). Use '_global' for global events.

Options:
  -p, --path        Path prefix filter(s). Repeatable.
  -t, --time        Time filter with operator (repeatable).
                    >1h  — events newer than 1 hour ago
                    <2026-05-01 — events before a date
                    Combine both for a range: -t '>1h' -t '<now'

Examples:
  fsmon changes _global                Latest event per path in global log
  fsmon changes nginx -t '>1h'        Latest nginx file changes in last hour
  fsmon changes _global -p /etc -t '>24h'  What changed in /etc since yesterday
  fsmon changes _global -t '>2026-05-25 08:00'  What changed since last deploy
  fsmon changes _global | wc -l          Count of changed files"#
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Setup (no sudo needed):
  fsmon init                        Create config file (directories created on first use)
  sudo fsmon init --service         Also install systemd service (auto-start on crash)
  fsmon cd -l                       Open subshell in log directory
  fsmon cd -m                       Open subshell in monitored store directory

Daemon (requires sudo):
  sudo fsmon daemon &               Start daemon in background
  sudo systemctl start fsmon        Start via systemd (if installed)
  sudo systemctl stop fsmon         Stop via systemd
  journalctl -u fsmon -f           View daemon logs via systemd
  kill %1                           Stop daemon (or Ctrl+C)

Management (no sudo needed):
  fsmon add openclaw --path /home -r   Track openclaw on /home (recursive)
  fsmon add _global --path /home       Monitor /home (all processes)
  fsmon remove _global                 Remove entire global cmd group
  fsmon remove openclaw              Remove entire openclaw cmd group
  fsmon monitored                   List monitored paths

Query (stdout JSONL, pipe to jq):
  fsmon query _global -t '>1h'             Events from last hour
  fsmon query _global | jq 'select(.cmd == "nginx")'  Custom filter

Clean (config defaults: keep_days=30, size=>=1GB):
  fsmon clean _global               Clean global log (keep >30d)
  fsmon clean openclaw -t '>7d'    Keep last 7 days of openclaw
  fsmon clean nginx --dry-run       Preview nginx log cleaning

Config:  ~/.config/fsmon/fsmon.toml (created by fsmon init, defaults apply without modification)
Monitor: ~/.local/share/fsmon/monitored.jsonl (configurable via [monitored].path)
Logs:    ~/.local/state/fsmon/*_log.jsonl (configurable via [logging].path)

3 data exit points:
  ① JSONL log files (on by default, configurable via [logging].path)
  ② Unix socket subscribe — real-time JSONL stream (examples/)
  ③ Unix socket admin — add/remove/list/health (examples/)
"#
}
