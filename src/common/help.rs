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
    Health,
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
            HelpTopic::Health => write!(f, "Health"),
        }
    }
}

/// Get short description for a help topic.
pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => {
            "\x1b[33mNote:\x1b[0m If installed via 'cargo install', copy to system path for sudo compatibility:\n  \x1b[32msudo cp ~/.cargo/bin/fsmon /usr/local/bin/\x1b[0m\n\nConfig:  ~/.config/fsmon/fsmon.toml (created by 'fsmon init')\nMonitor: ~/.local/share/fsmon/monitored.jsonl\nLogs:    ~/.local/state/fsmon/\nSocket:  /run/user/<UID>/fsmon/daemon.sock"
        }
        HelpTopic::Daemon => "Run the fsmon daemon (requires sudo for fanotify)",
        HelpTopic::Init => "Create the config file (directories created on first use)",
        HelpTopic::Cd => "Open a subshell in the monitored path or log directory",
        HelpTopic::Add => "Add a path to the monitoring list",
        HelpTopic::Remove => "Remove one or more paths from the monitoring list",
        HelpTopic::Monitored => "List all monitored paths with their configuration",
        HelpTopic::Query => "Query historical file change events from log files",
        HelpTopic::Clean => "Clean historical log files, retain by time or size",
        HelpTopic::Changes => "Show the most recent event per path (deduplicated changes)",
        HelpTopic::Health => "Query daemon health status",
    }
}

/// Get detailed description for a help topic.
pub const fn long_about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => {
            "Lightweight high-performance file change tracking tool.\n\n\x1b[33mSetup (no sudo needed):\x1b[0m\n  \x1b[32mfsmon init\x1b[0m                        Create config file\n  \x1b[32msudo fsmon init --service\x1b[0m         Also install systemd service\n  \x1b[32mfsmon cd -l\x1b[0m                       Open subshell in log directory\n  \x1b[32mfsmon cd -m\x1b[0m                       Open subshell in monitored store directory\n  \x1b[32mfsmon cd -c\x1b[0m                       Open subshell in config directory\n\n\x1b[33mDaemon (requires sudo):\x1b[0m\n  \x1b[32msudo fsmon daemon &\x1b[0m               Start daemon in background\n  \x1b[32msudo systemctl start fsmon\x1b[0m        Start via systemd\n  \x1b[32mjournalctl -u fsmon -f\x1b[0m           View daemon logs\n  \x1b[32mkill %1\x1b[0m                           Stop daemon (or Ctrl+C)\n\n\x1b[33mManagement (no sudo needed):\x1b[0m\n  \x1b[32mfsmon add <cmd> --path /home -r\x1b[0m   Track cmd on /home (recursive)\n  \x1b[32mfsmon add _global --path /home\x1b[0m   Monitor /home (all processes)\n  \x1b[32mfsmon remove <cmd>\x1b[0m                Remove cmd group\n  \x1b[32mfsmon monitored\x1b[0m                   List monitored paths\n\n\x1b[33mQuery (stdout JSONL, pipe to jq):\x1b[0m\n  \x1b[32mfsmon query <cmd> -t '>1h'\x1b[0m       Events from last hour\n  \x1b[32mfsmon query <cmd> | jq '.'\x1b[0m       Pretty print events\n\n\x1b[33mClean (config defaults: keep_days=30, size>=1GB):\x1b[0m\n  \x1b[32mfsmon clean <cmd>\x1b[0m                 Clean cmd log\n  \x1b[32mfsmon clean <cmd> -t '>7d'\x1b[0m      Keep last 7 days"
        }
        HelpTopic::Daemon => {
            "Monitors all configured paths via fanotify and logs events.\nUse 'fsmon add'/'fsmon remove' to manage paths dynamically without restarting.\n\nExamples:\n  sudo fsmon daemon &                     Start daemon in background\n  sudo fsmon daemon --debug               Enable debug output\n\nFor systemd integration:\n  sudo fsmon init --service             Install systemd service\n  sudo systemctl start fsmon            Start via systemd\n  journalctl -u fsmon -f               View daemon logs\n\nConfig: ~/.config/fsmon/fsmon.toml\nLogs:   ~/.local/state/fsmon/"
        }
        HelpTopic::Init => {
            "Directories are created on first use:\n  - Monitored dir: by 'fsmon add' on first run\n  - Log dir: by 'fsmon daemon' or 'fsmon cd -l' on first run\n\nWith --service, also installs a systemd service:\n  sudo fsmon init --service"
        }
        HelpTopic::Cd => {
            "Spawns a new shell (using $SHELL, fallback /bin/sh) in the target directory.\nType 'exit' to return to the original directory.\n\nOptions:\n  -m, --monitored    cd to the monitored store directory\n  -l, --logging      cd to the log directory\n  -c, --config       cd to the config directory (~/.config/fsmon/)\n\nExamples:\n  fsmon cd -l                       Open subshell in log directory\n  fsmon cd -m                       Open subshell in monitored store directory\n  fsmon cd -c                       Open subshell in config directory"
        }
        HelpTopic::Add => {
            "The entry is added immediately if the daemon is running, and persisted\nin the monitored paths database for automatic monitoring on daemon restart.\n\nNo sudo needed — store is updated immediately.\n\n<CMD> enables process tree tracking: fork/exec children are auto-included.\nUse '_global' to monitor all events on a path (no process tracking)."
        }
        HelpTopic::Remove => {
            "Without --path, removes the entire cmd group.\nWith --path, removes only the specified paths. Multiple paths are atomic:\nall must exist, or nothing is removed."
        }
        HelpTopic::Monitored => {
            "Displays each path with its recursive flag, event type filters,\nsize threshold, path/cmd exclusion patterns."
        }
        HelpTopic::Query => {
            "Output is JSONL (one JSON object per line), pipe to jq for custom filtering.\n\nNative fsmon query uses binary search and is significantly faster on large logs."
        }
        HelpTopic::Clean => {
            "Defaults: keep_days=30, size=>=1GB (from fsmon.toml or code fallback).\nCLI args override config. Daemon does not auto-clean; use cron/systemd timer."
        }
        HelpTopic::Changes => {
            "Same format as 'query', but with duplicate paths collapsed:\nonly the latest event for each unique path is shown, sorted by time descending."
        }
        HelpTopic::Health => {
            "Queries the running daemon's health status via the Unix socket.\n\nReturns daemon uptime, memory usage, and monitored path count.\nRequires the daemon to be running."
        }
    }
}
