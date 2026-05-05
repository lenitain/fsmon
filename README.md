<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">Real-time Linux filesystem change monitoring with process attribution.</h3>

🌍 **Select Language | 选择语言**
- [English](./README.md)
- [简体中文](./README.zh-CN.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## Features

- **Real-time Monitoring**: Captures 14 fanotify events (default: 8 core change events, `--all-events` for all 14)
- **Process Attribution**: Tracks PID, command name, and user for every file change — even short-lived processes like `touch`, `rm`, `mv`
- **Recursive Monitoring**: Watch entire directory trees with automatic tracking of newly created subdirectories
- **Complete Deletion Capture**: Captures every file deleted during `rm -rf` via persistent directory handle cache
- **High Performance**: Rust + Tokio, <5MB memory footprint, zero-copy FID event parsing, binary-search log querying
- **Flexible Filtering**: Filter by time, size, process, user, event type, and exclude patterns (wildcards)
- **Multiple Formats**: Human-readable, JSON, and CSV terminal output (log file always uses JSON for queryability)
- **TOML Configuration**: Persistent config at `~/.config/fsmon/fsmon.toml`
- **Log Management**: Time-based and size-based log rotation with dry-run preview
- **Two Modes**: CLI mode (reads `fsmon.toml`, interactive) and systemd instance mode (reads `/etc/fsmon/fsmon-{name}.toml`, background) — fully independent configs
- **Systemd Service**: Template unit (`fsmon@.service`) for multi-instance monitoring with configurable security hardening — run separate instances for different paths

## Why fsmon

Ever needed to answer "Who modified this file?" on a Linux server? That's exactly what fsmon is for.

Traditional file monitoring tools give you events without context — fsmon bridges that gap by attributing every file change to its responsible process. Whether it's a rogue script, an automated deployment, or a misconfigured service, you'll know exactly what happened, when, and who (or what) caused it.

## Quick Start

### Prerequisites

- **OS**: Linux 5.9+ (requires fanotify FID mode)
- **Tested Filesystems**: ext4, XFS, btrfs (Note: Linux 6.18+ recommended for full recursive operation support of btrfs) 
- **Build**: Rust toolchain (`cargo`)

```bash
# Verify kernel version
uname -r  # requires ≥ 5.9

# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Installation

```bash
# Build from source
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo install --path .

# Or install from crates.io
cargo install fsmon
```

**Important: Fanotify requires root privileges**
```bash
# Method 1: Copy to /usr/local/bin (recommended)
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/

# Method 2: Use full path directly
sudo ~/.cargo/bin/fsmon monitor ... 
```

### CLI Mode — Interactive Monitoring

```bash
# Monitor a directory (output to stdout)
sudo fsmon monitor /etc --types MODIFY

# Monitor with recursive watching
sudo fsmon monitor ~/myproject --recursive

# Write to a log file
sudo fsmon monitor /tmp --recursive -o /tmp/events.log

# Exclude patterns
sudo fsmon monitor /var/log --exclude "*.log"
```

Config read from `fsmon.toml` (search: `~/.fsmon/` → `~/.config/fsmon/` → `/etc/fsmon/`). CLI flags override config values.

### Instance Mode — Systemd Background Monitoring

```bash
# 1. Install systemd template (one-time)
sudo fsmon install

# 2. Create instance config manually
sudo mkdir -p /etc/fsmon
cat > /etc/fsmon/fsmon-web.toml << 'EOF'
paths = ["/var/www"]
types = "MODIFY,CREATE"
output = "/var/log/fsmon/web.log"
EOF

# 3. Start with systemd
sudo systemctl enable fsmon@web --now
sudo systemctl status fsmon@web
sudo journalctl -u fsmon@web

# 4. Stop and disable
sudo systemctl stop fsmon@web && sudo systemctl disable fsmon@web
```

Config read from `/etc/fsmon/fsmon-{name}.toml`. **`fsmon.toml` is ignored in this mode.** Each instance is fully independent.

### Other Commands

```bash
# Query historical events
fsmon query --since 1h --cmd nginx

# Clean old logs (dry-run preview)
fsmon clean --keep-days 7 --dry-run
```

## Examples

### Investigate Configuration Changes

```bash
# Monitor /etc for modifications
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log

# In another terminal, make a change
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# Query the results
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY
```

### Track Large File Creation

```bash
# Watch for files larger than 50MB
sudo fsmon monitor /tmp --types CREATE --min-size 50MB --format json

# Trigger
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100
```

### Audit Deletion Operations

```bash
# Capture complete recursive deletion
sudo fsmon monitor ~/.projects --types DELETE --recursive --output /tmp/deletes.log

# Trigger
rm -rf ~/.projects/fsmon-test/

# Output shows every file deleted (even in subdirectories)
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test/hello.c (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
```

### Filter with Combined Criteria

```bash
# Query nginx operations in last hour, sorted by file size
fsmon query --since 1h --cmd nginx* --sort size

# Monitor only CREATE and DELETE events, exclude temp files
sudo fsmon monitor /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## Command Reference

```bash
fsmon monitor --help    # Real-time monitoring with fanotify
fsmon query --help      # Query history logs with filters and sorting
fsmon clean --help      # Cleanup old logs by time or size
fsmon install           # Install systemd template unit (fsmon@.service)
fsmon uninstall         # Uninstall systemd template
fsmon enable <name>     # Create and start a monitoring instance
fsmon disable <name>    # Stop and remove a monitoring instance
fsmon generate          # Generate default configuration file (~/.config/fsmon/fsmon.toml)
```

## Two Modes: CLI vs Systemd Instance

fsmon has two completely independent operating modes, each with its own config file:

### CLI Mode (`fsmon monitor /path ...`)

| Aspect | Detail |
|--------|--------|
| Config file | `fsmon.toml` (search order below) |
| Config location | `~/.fsmon/` → `~/.config/fsmon/` → `/etc/fsmon/` |
| Log file | stdout only, or `-o` flag |
| Use case | Ad-hoc debugging, interactive investigation |

```bash
# CLI mode: reads fsmon.toml, flags override config values
sudo fsmon monitor /var/www --types MODIFY
sudo fsmon monitor /tmp --recursive -o /tmp/events.log
```

### Instance Mode (`fsmon monitor --instance <name>`)

| Aspect | Detail |
|--------|--------|
| Config file | `fsmon-{name}.toml` (per-instance) |
| Config location | `/etc/fsmon/fsmon-web.toml` only |
| Log file | Defined in instance config (`output` field). If omitted, no file log (events go to journald only) |
| Use case | Long-running systemd background monitoring |

```bash
# 1. Install systemd template (one-time)
sudo fsmon install

# 2. Create instance config manually
#    /etc/fsmon/fsmon-web.toml:
#      paths = ["/var/www"]
#      types = "MODIFY,CREATE"
#      output = "/var/log/fsmon/web.log"

# 3. Start with systemd
sudo systemctl enable fsmon@web --now
sudo systemctl status fsmon@web
sudo journalctl -u fsmon@web
```

**Key rule: the two configs never merge.** CLI mode ignores instance configs; instance mode ignores `fsmon.toml`. Changing one has zero effect on the other.

## Configuration

### CLI Config (`fsmon.toml`)

Search priority (first found wins):

1. `~/.fsmon/fsmon.toml`
2. `~/.config/fsmon/fsmon.toml` (generated by `fsmon generate`)
3. `/etc/fsmon/fsmon.toml`

Default config (`fsmon generate`):

```toml
[monitor]
# Directories to watch for filesystem events
paths = []

# Minimum file size to report (supports KB, MB, GB suffixes, e.g. "100MB", "1GB")
# min_size = "100MB"

# Comma-separated event types to filter (ACCESS, MODIFY, CREATE, DELETE, ...)
# types = "MODIFY,CREATE"

# Glob patterns to exclude from monitoring
# exclude = "*.tmp"

# Report all 14 event types regardless of the 'types' filter
all_events = false

# Path to the event log file
# output = "/var/log/fsmon.log"

# stdout format: "human", "json", or "csv" (log file is always JSON)
format = "human"

# Watch subdirectories recursively
recursive = false

# Fanotify read buffer size in bytes
buffer_size = 32768

[query]
# Event log file to query
# log_file = "/var/log/fsmon.log"

# Start time: relative ("1h", "30m", "7d") or absolute ("2024-05-01 10:00")
# since = "1h"

# End time: same format as since
# until = "2h"

# Filter by process IDs (comma-separated)
# pid = "1234,5678"

# Filter by process name (wildcard support: nginx*, python)
# cmd = "nginx"

# Filter by usernames (comma-separated)
# user = "root,admin"

# Filter by event types (comma-separated)
# types = "MODIFY,CREATE"

# Minimum size change to include
# min_size = "100MB"

# stdout format: "human", "json", or "csv" (log file is always JSON)
format = "human"

# Sort results: "time", "size", or "pid"
sort = "time"

[clean]
# Event log file to clean
# log_file = "/var/log/fsmon.log"

# Number of days to retain log entries
keep_days = 30

# Maximum log file size before tail truncation (e.g. "100MB", "1GB")
# max_size = "500MB"

[install]
# systemd ProtectSystem value ("yes", "no", "strict", "full")
protect_system = "strict"

# systemd ProtectHome value ("yes", "no", "read-only")
protect_home = "read-only"

# Additional read-write paths for systemd service (used when ProtectSystem is strict)
read_write_paths = ["/var/log"]

# systemd PrivateTmp value ("yes" or "no")
private_tmp = "yes"
```

CLI flags override config file values.

### Instance Config (`/etc/fsmon/fsmon-{name}.toml`)

Each systemd instance reads its own config from `/etc/fsmon/fsmon-{name}.toml`:

```toml
# Required: paths to monitor
paths = ["/var/www"]

# Optional fields
output = "/var/log/fsmon/web.log"   # log file path (omit to skip file logging)
types = "MODIFY,CREATE"              # event type filter
min_size = "100MB"                   # minimum size change
exclude = "*.tmp"                    # exclude pattern
all_events = false                   # capture all 14 event types
recursive = true                     # watch subdirectories
```

Available fields (all optional except `paths`):

| Field | CLI equivalent | Description |
|-------|---------------|-------------|
| `paths` | `PATH` arguments | **Required.** Directories/files to monitor |
| `output` | `-o, --output` | Log file path. Not set → no file log (events go to journald only) |
| `types` | `-t, --types` | Event type filter, comma-separated |
| `min_size` | `-s, --min-size` | Minimum size change to report |
| `exclude` | `-e, --exclude` | Exclude pattern with wildcard support |
| `all_events` | `--all-events` | Enable all 14 event types |
| `recursive` | `-r, --recursive` | Recursively monitor subdirectories |

Instance configs have no search priority — they are loaded by exact instance name from `/etc/fsmon/`. CLI flags passed alongside `--instance` override instance config fields.

## Technical Architecture

### Modules

| Module | Description |
|--------|-------------|
| `main.rs` | CLI entry point with clap derive, `FileEvent` struct, log cleaning engine |
| `monitor.rs` | Core fanotify monitoring loop, scope filtering, file size tracking (LRU) |
| `fid_parser.rs` | Low-level FID mode event parsing, two-pass path recovery |
| `dir_cache.rs` | Directory handle caching via `name_to_handle_at` for deleted file path resolution |
| `proc_cache.rs` | Netlink proc connector listener — captures short-lived process info at `exec()` |
| `query.rs` | Log file querying with binary search optimization and combined filters |
| `config.rs` | TOML-based persistent configuration |
| `systemd.rs` | Systemd service install and uninstall |
| `output.rs` | Event output formatting (human, JSON, CSV) |
| `utils.rs` | Size/time parsing, process info helpers, UID lookup |
| `help.rs` | Centralized help text for all commands |

### Data Flow

```
Linux Kernel (fanotify)
    → FID events pushed to queue
    → tokio::select reads events asynchronously
    → fid_parser parses FID records (two-pass: resolve + cache recover)
    → Monitor filters (type, size, exclude, scope)
    → output formats (human/json/csv) → stdout + optional file
```

- **fanotify (FID mode + FAN_REPORT_NAME)**: Kernel pushes file events with directory file handles and filenames. No polling — events delivered immediately via non-blocking read.
- **Proc Connector**: Background thread subscribes to netlink `PROC_EVENT_EXEC` notifications, caching every process's `(pid, cmd, user)` at the instant it execs. This ensures short-lived processes (`touch`, `rm`, `mv`) are attributable even after they exit.
- **FID Parser + Dir Cache**: Two-pass event processing: (1) resolve file handles via `open_by_handle_at`, (2) use persistent directory handle cache to recover paths for events where the parent directory was already deleted. Handles multi-level nested `rm -rf` scenarios.
- **Binary Search Query**: `fsmon query` uses binary search on approximately time-sorted log files, narrowing the scan range to O(log N) seek operations. Combined with `expand_offset_backward` to catch minor out-of-order entries.
- **Rust + Tokio**: Single-threaded async loop (`tokio::select` between fanotify fd and Ctrl+C signal). Background thread for proc connector. No complex concurrency — high efficiency instead.

### Event Mask Strategy

fsmon uses a two-tier marking strategy:
1. **FAN_MARK_FILESYSTEM** (preferred): Marks the entire mount point covering the target path — no race window for newly created files. Falls back if `EXDEV` (btrfs subvolumes).
2. **Inode mark fallback**: Marks individual directories one by one, with recursive traversal for `--recursive` mode. Dynamically marks newly created directories in real-time.

### Event Types

Default captures 8 core events. Use `--all-events` for all 14.

**Default Events (8):**

| Event | Description |
|-------|-------------|
| CLOSE_WRITE | File closed after write (best "modified" signal) |
| ATTRIB | Metadata changed (permissions, timestamps, owner) |
| CREATE | File/directory created |
| DELETE | File/directory deleted |
| DELETE_SELF | The monitored file/directory itself was deleted |
| MOVED_FROM | File moved out of monitored directory |
| MOVED_TO | File moved into monitored directory |
| MOVE_SELF | The monitored file/directory itself was moved |

**Additional Events (6, via --all-events):**

| Event | Description |
|-------|-------------|
| ACCESS | File read |
| MODIFY | File content written (very noisy) |
| OPEN | File/directory opened |
| OPEN_EXEC | File opened for execution |
| CLOSE_NOWRITE | Read-only file closed |
| FS_ERROR | Filesystem error (Linux 5.16+) |

## License

[MIT License](./LICENSE)
