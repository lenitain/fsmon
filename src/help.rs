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
  fsmon add /path/to/watch  Add a path (no sudo needed)
  fsmon managed             List monitored paths
  fsmon query --since 1h    Query events

Config:           ~/.config/fsmon/config.toml
Store:            ~/.local/share/fsmon/store.toml (managed by add/remove)
Log dir:          ~/.local/state/fsmon/ (one .toml file per monitored path)
Socket:           /tmp/fsmon-<UID>.sock"#
        }
        HelpTopic::Add => {
            r#"Add a path to the monitoring list.

The path is added immediately if the daemon is running, and persisted
in ~/.config/fsmon/config.toml for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

Options:
  -r, --recursive     Watch subdirectories recursively
  -t, --types         Event types to monitor (comma-separated)
  -m, --min-size      Minimum file size change to report (e.g., 100MB, 1GB)
  -e, --exclude       Glob patterns to exclude (e.g., "*.tmp")
  --all-events        Monitor all 14 fanotify event types

Examples:
  fsmon add /path/to/project -r --types MODIFY,CREATE
  fsmon add /etc --types MODIFY --min-size 100KB
  fsmon add /tmp --all-events"#
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
minimum size threshold, and exclusion patterns.

Examples:
  fsmon managed"#
        }
        HelpTopic::Query => {
            r#"Query historical file change events from log files.

Options:
  --path            Path(s) to query. Repeatable. Default: all.
                    Examples: --path /tmp --path /var/log
  --since           Start time: relative (1h, 30m, 7d) or absolute
  --until           End time
  --pid             Filter by PID (comma-separated)
  --cmd             Filter by process name (wildcards: nginx*)
  --user            Filter by username (comma-separated)
  --types           Filter by event type (comma-separated)
  --min-size        Minimum size change
  --sort            Sort by: time, size, pid

Examples:
  fsmon query --since 1h
  fsmon query --path /tmp --since 1h
  fsmon query --path /tmp --cmd nginx"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

Options:
  --path            Path(s) to clean. Repeatable. Default: all.
  --keep-days       Keep logs from last N days (default: 30)
  --max-size        Maximum log file size (e.g., 100MB, 1GB)
  --dry-run         Preview mode, don't actually delete

Examples:
  fsmon clean --keep-days 7
  fsmon clean --path /tmp --max-size 100MB --dry-run"#
        }
        HelpTopic::Generate => {
            r#"Generate a default configuration file at ~/.config/fsmon/config.toml.

The config file defines infrastructure paths (store, log dir, socket).
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
  fsmon add /path -r                Add path with recursive monitoring
  fsmon remove /path                Remove path
  fsmon managed                     List monitored paths

Query & Clean:
  fsmon query --since 1h            Events from last hour
  fsmon query --path /tmp           Events for a specific path
  fsmon clean --keep-days 7         Keep 7 days of logs

Config: ~/.config/fsmon/config.toml
Store:  ~/.local/share/fsmon/store.toml
Logs:   ~/.local/state/fsmon/*.toml"#
}
