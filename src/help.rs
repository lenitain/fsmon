pub enum HelpTopic {
    Root,
    Monitor,
    Query,
    Install,
    Uninstall,
    Enable,
    Disable,
    Clean,
    Generate,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Monitor => "Real-time file change monitoring",
        HelpTopic::Query => "Query historical monitoring logs",
        HelpTopic::Install => "Install systemd service template",
        HelpTopic::Uninstall => "Uninstall systemd service template",
        HelpTopic::Enable => "Enable a monitoring instance",
        HelpTopic::Disable => "Disable a monitoring instance",
        HelpTopic::Clean => "Clean historical logs",
        HelpTopic::Generate => "Generate a default config file",
    }
}

pub const fn long_about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "",
        HelpTopic::Monitor => {
            r#"Monitor filesystem events on specified paths, output fanotify raw events in real-time.

[Event Types]
  Default: 8 core change events (CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF)
  --all-events: Enable all 14 fanotify events (includes ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR)

[Systemd Service]
  Use 'fsmon install' to set up systemd service template
  Use 'fsmon enable <name>' to create and start an instance
  Use 'systemctl start/stop/status fsmon@<name>' to manage instances

[Instance Mode]
  --instance NAME  Load config from /etc/fsmon/fsmon-{NAME}.toml (systemd template mode)

[Examples]
  fsmon monitor /etc --types MODIFY              # Investigate config file changes
  fsmon monitor --instance var-log               # Run with instance config
  fsmon monitor / --all-events                   # Enable all 14 event types
  fsmon monitor ~/project --recursive            # Recursively monitor project directory
  fsmon monitor /tmp --min-size 100MB            # Track large file creation
  fsmon monitor /var/log --format json           # JSON format output (log file is always JSON)"#
        }
        HelpTopic::Query => {
            r#"Query historical file change events from log files, supports multiple filter conditions and sorting.

[Time Filtering]
  --since   Start time: relative (1h, 30m, 7d) or absolute ("2024-05-01 10:00")
  --until   End time
  
[Process Filtering]
  --pid     Filter by process ID (multiple supported: 1234,5678)
  --cmd     Filter by process name (wildcard support: nginx*, python)
  --user    Filter by username (multiple supported: root,admin)

[Event Filtering]
  --types     Filter by event type (ACCESS,MODIFY,CREATE,DELETE,...)
  --min-size  Filter by size change (e.g., 100MB, 1GB)

[Examples]
  fsmon query                              # Query default log (~/.fsmon/history.log)
  fsmon query --since 1h                   # Last 1 hour
  fsmon query --cmd nginx                  # Only nginx operations
  fsmon query --since 1h --cmd java --types MODIFY --min-size 100MB  # Combined filters
  fsmon query --format json --sort size    # JSON terminal output, sorted by size"#
        }
        HelpTopic::Install => {
            r#"Install fsmon systemd service template (fsmon@.service).

Uses systemd template units — one service file manages multiple instances.
After install, use 'fsmon enable <name>' to create individual instances.

[Service Configuration]
  - Creates /etc/systemd/system/fsmon@.service
  - Auto-restart on failure
  - Logs to systemd journal
  - Security hardening via ProtectSystem, ProtectHome, PrivateTmp

[Workflow]
  fsmon install                           # One-time: install template
  fsmon enable web --paths /var/www       # Create + start an instance
  fsmon enable db --paths /var/lib/data   # Another instance
  systemctl status fsmon@web              # Check instance status
  journalctl -u fsmon@web                 # View instance logs
  fsmon disable web                       # Stop + remove instance
  fsmon uninstall                         # Remove template

[Examples]
  fsmon install                                          # Install template
  fsmon install --force --protect-system no             # Reinstall with relaxed security"#
        }
        HelpTopic::Uninstall => {
            r#"Uninstall fsmon systemd service template (fsmon@.service).

[Actions]
  - Removes service template file
  - Reloads systemd daemon
  - Does NOT stop running instances (do that first with 'fsmon disable <name>')

[Examples]
  fsmon uninstall"#
        }
        HelpTopic::Enable => {
            r#"Create and start a new fsmon monitoring instance.

Generates an instance config at /etc/fsmon/fsmon-{NAME}.toml,
then runs systemctl enable --now fsmon@{NAME}.

[Prerequisites]
  Run 'fsmon install' first to install the systemd template.

[Examples]
  fsmon enable web --paths /var/www -o /var/log/fsmon/web.log
  fsmon enable db --paths /var/lib/postgresql --types MODIFY
  fsmon enable logs --paths /var/log --recursive --force"#
        }
        HelpTopic::Disable => {
            r#"Stop and disable a fsmon monitoring instance.

Stops the systemd unit, disables it, and removes the instance config file.

[Examples]
  fsmon disable web"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

[Cleanup Strategy]
  --keep-days   Keep logs from last N days (default: 30 days)
  --max-size    Limit maximum log file size (e.g., 100MB, 1GB)
  --dry-run     Preview mode, don't actually delete

[Examples]
  fsmon clean --keep-days 7           # Keep 7 days of logs
  fsmon clean --max-size 100MB        # Limit logs to 100MB
  fsmon clean --keep-days 7 --dry-run # Preview without deleting"#
        }
        HelpTopic::Generate => {
            "Generate a commented default configuration file.\n\
Generates a TOML config file at ~/.config/fsmon/config.toml (XDG config path).\n\
\n\
[Config Search Order]\n\
  1. ~/.fsmon/config.toml        (legacy)\n\
  2. ~/.config/fsmon/config.toml (XDG)\n\
  3. /etc/fsmon/config.toml      (system-wide)\n\
\n\
[Examples]\n\
  fsmon generate                  # Generate config (fails if exists)\n\
  fsmon generate --force          # Overwrite existing config"
        }
    }
}

pub const fn after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed command info

Examples:
  fsmon monitor /var/log                      # Basic monitoring
  fsmon monitor /etc --types MODIFY          # Investigate config file changes
  fsmon monitor --instance var-log           # Run with instance config
  fsmon install                              # Install systemd template
  fsmon enable web --paths /var/www          # Create + start an instance
  fsmon enable db --paths /var/lib/data      # Another instance
  fsmon disable web                          # Stop + remove instance
  systemctl status fsmon@web                 # Check instance status
  fsmon query --since 1h --cmd nginx         # Query nginx operations in last hour"#
}
