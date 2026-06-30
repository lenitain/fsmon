use crate::{green, yellow};

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
            concat!(
                yellow!("Note:"),
                " If installed via 'cargo install', copy to system path for sudo compatibility:\n",
                "  ",
                green!("sudo cp ~/.cargo/bin/fsmon /usr/local/bin/"),
                "\n\nConfig:  ~/.config/fsmon/fsmon.toml (created by 'fsmon init')",
                "\nMonitor: ~/.local/share/fsmon/monitored.jsonl",
                "\nLogs:    ~/.local/state/fsmon/",
                "\nSocket:  /run/user/<UID>/fsmon/daemon.sock"
            )
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
            concat!(
                "Lightweight high-performance file change tracking tool.\n\n",
                yellow!("Setup (no sudo needed):"),
                "\n  ",
                green!("fsmon init"),
                "                        Create config file\n  ",
                green!("sudo fsmon init --service"),
                "         Also install systemd service\n  ",
                green!("fsmon cd -l"),
                "                       Open subshell in log directory\n  ",
                green!("fsmon cd -m"),
                "                       Open subshell in monitored store directory\n  ",
                green!("fsmon cd -c"),
                "                       Open subshell in config directory\n\n",
                yellow!("Daemon (requires sudo):"),
                "\n  ",
                green!("sudo fsmon daemon &"),
                "               Start daemon in background\n  ",
                green!("sudo systemctl start fsmon"),
                "        Start via systemd\n  ",
                green!("journalctl -u fsmon -f"),
                "           View daemon logs\n  ",
                green!("kill %1"),
                "                           Stop daemon (or Ctrl+C)\n\n",
                yellow!("Management (no sudo needed):"),
                "\n  ",
                green!("fsmon add <cmd> --path /home -r"),
                "   Track cmd on /home (recursive)\n  ",
                green!("fsmon add _global --path /home"),
                "   Monitor /home (all processes)\n  ",
                green!("fsmon remove <cmd>"),
                "                Remove cmd group\n  ",
                green!("fsmon monitored"),
                "                   List monitored paths\n\n",
                yellow!("Query (stdout JSONL, pipe to jq):"),
                "\n  ",
                green!("fsmon query <cmd> -t '>1h'"),
                "       Events from last hour\n  ",
                green!("fsmon query <cmd> | jq '.'"),
                "       Pretty print events\n\n",
                yellow!("Clean (config defaults: keep_days=30, size>=1GB):"),
                "\n  ",
                green!("fsmon clean <cmd>"),
                "                 Clean cmd log\n  ",
                green!("fsmon clean <cmd> -t '>7d'"),
                "      Keep last 7 days"
            )
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
