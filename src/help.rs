pub enum HelpTopic {
    Root,
    Daemon,
    Add,
    Remove,
    Managed,
    Query,
    Clean,
    Generate,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Daemon => "Run the fsmon daemon (requires sudo for fanotify)",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove a path from the monitoring list",
        HelpTopic::Managed => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
        HelpTopic::Generate => "Generate a default configuration file",
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
  fsmon add /path --exclude-cmd rsync  Exclude by process name
  fsmon managed                       List monitored paths
  fsmon query --since 1h    Query events

Config:           ~/.config/fsmon/config.toml
Managed:          ~/.local/share/fsmon/managed.jsonl (configurable via [managed].file)
Log dir:          ~/.local/state/fsmon/ (configurable via [logging].dir)
Socket:           /tmp/fsmon-<UID>.sock (configurable via [socket].path)"#
        }
        HelpTopic::Add => {
            r#"Add a path to the monitoring list.

The path is added immediately if the daemon is running, and persisted
in ~/.config/fsmon/config.toml for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

Options:
  -r, --recursive         Watch subdirectories recursively
  -t, --types             Event types to monitor (comma-separated; use "all" for all 14 types)
  -m, --min-size          Minimum file size change to report (e.g., 100MB, 1GB)
  -e, --exclude           Glob patterns to exclude (use | for multiple, prefix ! to invert)
  --exclude-cmd           Process names to exclude (glob, use | for multiple, prefix ! to invert)

Examples:
  fsmon add /path/to/project -r                 Default: 8 event types
  fsmon add /path --types MODIFY,CREATE         Only these 2 types
  fsmon add /path --types all                   All 14 event types
  fsmon add /etc --types MODIFY --min-size 100KB
  fsmon add /var/log --exclude-cmd "rsync|apt"
  fsmon add /tmp --exclude-cmd nginx"#
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
  -S, --since       Start time: relative (1h, 30m, 7d) or absolute
  -U, --until       End time

Examples:
  fsmon query --since 1h
  fsmon query --path /tmp --since 1h
  fsmon query --since 1h | jq 'select(.cmd == "nginx")'
  fsmon query | jq -s 'sort_by(.file_size)[]'"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

Defaults come from config.toml [logging] section (keep_days=30, max_size="1GB").
CLI args override config overrides code defaults.

Options:
  --path            Path(s) to clean. Repeatable. Default: all.
  --keep-days       Keep logs from last N days
  --max-size        Maximum log file size (e.g., 100MB, 1GB)
  --dry-run         Preview mode, don't actually delete

Examples:
  fsmon clean                       Use config defaults
  fsmon clean --keep-days 7         Override retention
  fsmon clean --path /tmp --dry-run Preview without deleting"#
        }
        HelpTopic::Generate => {
            r#"Generate a default configuration file at ~/.config/fsmon/config.toml.

The config includes safety nets (keep_days=30, max_size="1GB")
that prevent disk overflow even if you never run 'fsmon clean'.

Monitored paths are managed separately via 'fsmon add'/'fsmon remove'.
The daemon also auto-generates a default config if none exists when started.

Examples:
  fsmon generate"#
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Daemon (requires sudo):
  sudo fsmon daemon &               Start daemon in background
  kill %1                           Stop daemon (or Ctrl+C)

Management (no sudo needed):
  fsmon add /path -r                Add path (recursive, default 8 types)
  fsmon add /path --exclude-cmd rsync  Exclude by process name
  fsmon remove /path                Remove path
  fsmon managed                     List monitored paths

Query (stdout JSONL, pipe to jq):
  fsmon query --since 1h            Events from last hour
  fsmon query | jq 'select(.cmd == "nginx")'  Custom filter

Clean (config defaults: keep_days=30, max_size=1GB):
  fsmon clean                       Clean all logs
  fsmon clean --keep-days 7         Override retention
  fsmon clean --dry-run             Preview without deleting

Config: ~/.config/fsmon/config.toml
Managed: ~/.local/share/fsmon/managed.jsonl (configurable via [managed].file)
Logs:   ~/.local/state/fsmon/*_log.jsonl (configurable via [logging].dir)"#
}
