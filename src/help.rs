pub enum HelpTopic {
    Root,
    Monitor,
    Query,
    Install,
    Uninstall,
    Clean,
    Generate,
    GenerateInstance,
}

pub const fn about(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => "Lightweight high-performance file change tracking tool",
        HelpTopic::Monitor => "Real-time file change monitoring",
        HelpTopic::Query => "Query historical monitoring logs",
        HelpTopic::Install => "Install systemd service template",
        HelpTopic::Uninstall => "Uninstall systemd service template",
        HelpTopic::Clean => "Clean historical logs",
        HelpTopic::Generate => "Generate CLI configuration file at ~/.config/fsmon/fsmon.toml",
        HelpTopic::GenerateInstance => {
            "Generate instance configuration file at /etc/fsmon/fsmon-{name}.toml"
        }
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
  Use 'systemctl enable fsmon@<name> --now' to create and start an instance
  Use 'systemctl start/stop/status fsmon@<name>' to manage instances

[Instance Mode]
  --instance NAME  Load config from /etc/fsmon/fsmon-{NAME}.toml (systemd template mode)

[Examples]
  fsmon-cli monitor /etc --types MODIFY              # Investigate config file changes
  fsmon-cli monitor --instance var-log               # Run with instance config
  fsmon-cli monitor / --all-events                   # Enable all 14 event types
  fsmon-cli monitor ~/project --recursive            # Recursively monitor project directory
  fsmon-cli monitor /tmp --min-size 100MB            # Track large file creation
  fsmon-cli monitor /var/log --format json           # JSON format output (log file is always JSON)"#
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
  fsmon-cli query                              # Query default log (~/.config/fsmon/history.log)
  fsmon-cli query --since 1h                   # Last 1 hour
  fsmon-cli query --cmd nginx                  # Only nginx operations
  fsmon-cli query --since 1h --cmd java --types MODIFY --min-size 100MB  # Combined filters
  fsmon-cli query --format json --sort size    # JSON terminal output, sorted by size"#
        }
        HelpTopic::Install => {
            r#"Install fsmon systemd service template (fsmon@.service).

Uses systemd template units — one service file manages multiple instances.
Each instance reads its own config from /etc/fsmon/fsmon-{NAME}.toml.

[Service Configuration]
  - Creates /etc/systemd/system/fsmon@.service
  - Auto-restart on failure
  - Logs to systemd journal
  - Security hardening via ProtectSystem, ProtectHome, PrivateTmp

[Workflow]
  fsmon install                                   # One-time: install template
  fsmon generate --instance web                   # Generate instance config
  systemctl enable fsmon@web --now                # Create instance with systemd
  systemctl enable fsmon@db --now                 # Another instance
  systemctl status fsmon@web                      # Check instance status
  journalctl -u fsmon@web                         # View instance logs
  systemctl stop fsmon@web && systemctl disable fsmon@web  # Stop + disable

[Instance Config]
  Each instance reads /etc/fsmon/fsmon-{NAME}.toml.
  Create it with 'fsmon generate --instance <name>' before running systemctl enable.

  Example /etc/fsmon/fsmon-web.toml:
    paths = ["/var/www"]
    output = "/var/log/fsmon/web.log"
    types = "MODIFY,CREATE"

[Examples]
  fsmon install                                          # Install template
  fsmon install --force --protect-system no             # Reinstall with relaxed security"#
        }
        HelpTopic::Uninstall => {
            r#"Uninstall fsmon systemd service template (fsmon@.service).

[Actions]
  - Removes service template file
  - Reloads systemd daemon
  - Does NOT stop running instances (do that first with 'systemctl stop fsmon@<name>')

[Examples]
  fsmon uninstall"#
        }
        HelpTopic::Clean => {
            r#"Clean historical log files, retain by time or size.

[Cleanup Strategy]
  --keep-days   Keep logs from last N days (default: 30 days)
  --max-size    Limit maximum log file size (e.g., 100MB, 1GB)
  --dry-run     Preview mode, don't actually delete

[Examples]
  fsmon-cli clean --keep-days 7           # Keep 7 days of logs
  fsmon-cli clean --max-size 100MB        # Limit logs to 100MB
  fsmon-cli clean --keep-days 7 --dry-run # Preview without deleting"#
        }
        HelpTopic::Generate => {
            "Generate CLI configuration file.\n\
\n\
[Output Path]\n\
  ~/.config/fsmon/fsmon.toml\n\
\n\
[Examples]\n\
  fsmon-cli generate                  # Generate CLI config (fails if exists)\n\
  fsmon-cli generate --force          # Overwrite existing CLI config"
        }
        HelpTopic::GenerateInstance => {
            "Generate instance configuration file for systemd service.\n\
\n\
[Output Path]\n\
  /etc/fsmon/fsmon-{name}.toml\n\
\n\
Edit the generated file to set monitored paths, then:\n\
  systemctl enable fsmon@{name} --now\n\
\n\
[Examples]\n\
  fsmon generate --instance web   # Generate /etc/fsmon/fsmon-web.toml template\n\
  fsmon generate --instance web --force  # Overwrite existing"
        }
    }
}

pub const fn daemon_after_help() -> &'static str {
    r#"Use 'fsmon <COMMAND> --help' for detailed command info

Examples:
  fsmon install                              # Install systemd template
  fsmon install --force                      # Reinstall with --force
  fsmon uninstall                            # Uninstall systemd template
  fsmon generate --instance web              # Generate instance config for 'web'
  fsmon generate --instance web --force      # Overwrite existing instance config

Instance management (via systemctl):
  systemctl enable fsmon@web --now           # Create + start an instance
  systemctl status fsmon@web                 # Check instance status
  journalctl -u fsmon@web                    # View instance logs"#
}

pub const fn cli_after_help() -> &'static str {
    r#"Use 'fsmon-cli <COMMAND> --help' for detailed command info

Examples:
  fsmon-cli monitor /var/log                 # Basic monitoring
  fsmon-cli monitor /etc --types MODIFY     # Investigate config changes
  fsmon-cli query --since 1h                # Last 1 hour of events
  fsmon-cli query --cmd nginx               # Filter by process name
  fsmon-cli clean --keep-days 7             # Keep 7 days of logs
  fsmon-cli clean --max-size 100MB          # Limit log file size
  fsmon-cli generate                        # Generate CLI config at ~/.config/fsmon/fsmon.toml"#
}
