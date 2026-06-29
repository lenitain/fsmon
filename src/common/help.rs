/// Help topic for fsmon commands.
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

impl std::fmt::Debug for HelpTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HelpTopic::Root => write!(f, "Root"),
            HelpTopic::Daemon => write!(f, "Daemon"),
            HelpTopic::Init => write!(f, "Init"),
            HelpTopic::Cd => write!(f, "Cd"),
            HelpTopic::Add => write!(f, "Add"),
            HelpTopic::Remove => write!(f, "Remove"),
            HelpTopic::Monitored => write!(f, "Monitored"),
            HelpTopic::Query => write!(f, "Query"),
            HelpTopic::Clean => write!(f, "Clean"),
            HelpTopic::Changes => write!(f, "Changes"),
        }
    }
}

/// Get short description for a help topic.
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

/// Get detailed description for a help topic.
pub const fn long_about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "",
        HelpTopic::Daemon => {
            r"Monitors all configured paths via fanotify and logs events.
Use 'fsmon add'/'fsmon remove' to manage paths dynamically without restarting.

Examples:
  sudo fsmon daemon &                     Start daemon in background
  sudo fsmon daemon --debug               Enable debug output

For systemd integration:
  sudo fsmon init --service             Install systemd service
  sudo systemctl start fsmon            Start via systemd
  journalctl -u fsmon -f               View daemon logs

Config: ~/.config/fsmon/fsmon.toml
Logs:   ~/.local/state/fsmon/"
        }
        HelpTopic::Init => {
            r"Directories are created on first use:
  - Monitored dir: by 'fsmon add' on first run
  - Log dir: by 'fsmon daemon' or 'fsmon cd -l' on first run

With --service, also installs a systemd service:
  sudo fsmon init --service"
        }
        HelpTopic::Cd => {
            r"Spawns a new shell (using $SHELL, fallback /bin/sh) in the target directory.
Type 'exit' to return to the original directory.

Options:
  -m, --monitored    cd to the monitored store directory
  -l, --logging      cd to the log directory
  -c, --config       cd to the config directory (~/.config/fsmon/)

Examples:
  fsmon cd -l                       Open subshell in log directory
  fsmon cd -m                       Open subshell in monitored store directory
  fsmon cd -c                       Open subshell in config directory"
        }
        HelpTopic::Add => {
            r"The entry is added immediately if the daemon is running, and persisted
in the monitored paths database for automatic monitoring on daemon restart.

No sudo needed — store is updated immediately.

<CMD> enables process tree tracking: fork/exec children are auto-included.
Use '_global' to monitor all events on a path (no process tracking)."
        }
        HelpTopic::Remove => {
            r"Without --path, removes the entire cmd group.
With --path, removes only the specified paths. Multiple paths are atomic:
all must exist, or nothing is removed."
        }
        HelpTopic::Monitored => {
            r"Displays each path with its recursive flag, event type filters,
size threshold, path/cmd exclusion patterns."
        }
        HelpTopic::Query => {
            r"Output is JSONL (one JSON object per line), pipe to jq for custom filtering.

Native fsmon query uses binary search and is significantly faster on large logs."
        }
        HelpTopic::Clean => {
            r"Defaults: keep_days=30, size=>=1GB (from fsmon.toml or code fallback).
CLI args override config. Daemon does not auto-clean; use cron/systemd timer."
        }
        HelpTopic::Changes => {
            r"Same format as 'query', but with duplicate paths collapsed:
only the latest event for each unique path is shown, sorted by time descending."
        }
    }
}

/// Get help text displayed after command help.
pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed help

Setup (no sudo needed):
  fsmon init                        Create config file (directories created on first use)
  sudo fsmon init --service         Also install systemd service (auto-start on crash)
  fsmon cd -l                       Open subshell in log directory
  fsmon cd -m                       Open subshell in monitored store directory
  fsmon cd -c                       Open subshell in config directory

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
Socket:  /run/user/<UID>/fsmon/daemon.sock (hardcoded)

3 data exit points:
  ① JSONL log files (on by default, configurable via [logging].path)
  ② Unix socket subscribe — real-time JSONL stream (examples/)
  ③ Unix socket admin — add/remove/list/health (examples/)
"#
}
