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
- **TOML Configuration**: Persistent config at `/etc/fsmon/fsmon.toml`
- **Log Management**: Time-based and size-based log rotation with dry-run preview
- **Dynamic Path Management**: Add/remove monitored paths at runtime via Unix socket (no daemon restart needed)
- **Systemd Service**: Systemd service with auto-restart, fanotify capabilities, and runtime directory for Unix socket

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
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### Daemon Mode — Interactive Monitoring

```bash
# 1. Install systemd service (one-time)
sudo fsmon install

# 2. Start the daemon
sudo systemctl enable fsmon --now
sudo systemctl status fsmon

# 3. Add paths to monitor (live, no restart needed)
sudo fsmon add /etc --types MODIFY
sudo fsmon add /var/www --recursive --types MODIFY,CREATE
sudo fsmon add /tmp --all-events

# 4. List monitored paths
sudo fsmon managed

# 5. Query historical events
fsmon query --since 1h --cmd nginx

# 6. Clean old logs (dry-run preview)
fsmon clean --keep-days 7 --dry-run

# 7. Remove a path
sudo fsmon remove /tmp

# 8. Stop the daemon
sudo systemctl stop fsmon
```

Config read from `/etc/fsmon/fsmon.toml`. Paths added via `fsmon add` are persisted in config for automatic monitoring on daemon restart.



## Examples

### Investigate Configuration Changes

```bash
# Add /etc for monitoring
sudo fsmon add /etc --types MODIFY

# In another terminal, make a change
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# Query the results
sudo fsmon query --since 1h --types MODIFY
```

### Track Large File Creation

```bash
# Watch for files larger than 50MB
sudo fsmon add /tmp --types CREATE

# Trigger
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# Query with min-size filter
sudo fsmon query --since 1m --min-size 50MB --format json
```

### Audit Deletion Operations

```bash
# Monitor for deletions
sudo fsmon add ~/.projects --types DELETE --recursive

# Trigger
rm -rf ~/.projects/fsmon-test/

# Output shows every file deleted (even in subdirectories)
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test/hello.c (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
```

### Filter with Combined Criteria

```bash
# Query nginx operations in last hour, sorted by file size
sudo fsmon query --since 1h --cmd nginx* --sort size

# Add monitoring with exclude patterns
sudo fsmon add /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## Command Reference

```bash
fsmon add /var/www -r           # Add a path to monitoring (live + persist)
sudo fsmon remove /var/www       # Remove a path from monitoring
sudo fsmon managed               # List all monitored paths with configuration
fsmon query --since 1h          # Query historical events with filters
fsmon clean --keep-days 7       # Clean old logs by time or size
sudo fsmon install               # Install systemd service and config
sudo fsmon uninstall             # Uninstall systemd service
```

Use `fsmon <COMMAND> --help` for detailed help on each subcommand.

## Architecture

fsmon runs as a systemd-managed background daemon, with persistent config at `/etc/fsmon/fsmon.toml`.

```bash
sudo fsmon install              # Install systemd service
sudo systemctl enable fsmon --now
```

| Aspect | Detail |
|--------|--------|
| Config file | `/etc/fsmon/fsmon.toml` |
| Path management | `fsmon add` / `fsmon remove` (live via Unix socket) |
| Log output | JSON events written to `/var/log/fsmon/history.log` |
| Query | `fsmon query --since 1h` to read events |
| Clean | `fsmon clean --keep-days 7` to rotate old logs |

## Configuration

Persistent config at `/etc/fsmon/fsmon.toml` (auto-generated by `sudo fsmon install`).

| Field | CLI flag | Type | Description |
|-------|----------|------|-------------|
| `log_file` | | `string` | Log file path (default: `/var/log/fsmon/history.log`) |
| `socket_path` | | `string` | Unix socket for live commands (default: `/var/run/fsmon/fsmon.sock`) |
| `paths` | `fsmon add` | `PathEntry[]` | Monitored paths |

Each path entry:

| Field | CLI flag | Type | Description |
|-------|----------|------|-------------|
| `path` | `PATH` arg | `string` | Directory/file to watch |
| `types` | `-t, --types` | `string[]` | Event type filter (comma-separated) |
| `min_size` | `-m, --min-size` | `string` | Minimum size change (e.g. "100MB") |
| `exclude` | `-e, --exclude` | `string` | Glob exclude pattern |
| `all_events` | `--all-events` | `bool` | Capture all 14 event types |
| `recursive` | `-r, --recursive` | `bool` | Watch subdirectories |

### Query Options (CLI flags only)

| Flag | Description |
|------|-------------|
| `--log-file` | Log to query |
| `--since` | Start time (relative like `1h`, `30m`, `7d` or absolute timestamp) |
| `--until` | End time |
| `--pid` | Filter by PID(s), comma-separated |
| `--cmd` | Filter by process name (wildcards like `nginx*`) |
| `--user` | Filter by username(s), comma-separated |
| `-t, --types` | Filter by event type(s), comma-separated |
| `-m, --min-size` | Minimum size change |
| `-f, --format` | Output format: `human` (default), `json`, `csv` |
| `-r, --sort` | Sort by: `time`, `size`, `pid` |

### Clean Options (CLI flags only)

| Flag | Description |
|------|-------------|
| `--log-file` | Log to clean |
| `--keep-days` | Retention in days (default: 30) |
| `--max-size` | Max size before truncation (e.g. `100MB`) |
| `--dry-run` | Preview without making changes |

### Install Options (CLI flags only)

| Flag | Description |
|------|-------------|
| `--force` | Reinstall existing service |

## Technical Architecture

### Modules

| Module | Description |
|--------|-------------|
| `lib.rs` | Library crate root — shared types (`FileEvent`, `EventType`), log cleaning engine |
| `bin/fsmon.rs` | Main binary — `daemon`, `add`, `remove`, `managed`, `query`, `clean`, `install`, `uninstall` |
| `monitor.rs` | Core fanotify monitoring loop, scope filtering, file size tracking (LRU) |
| `fid_parser.rs` | Low-level FID mode event parsing, two-pass path recovery |
| `dir_cache.rs` | Directory handle caching via `name_to_handle_at` for deleted file path resolution |
| `proc_cache.rs` | Netlink proc connector listener — captures short-lived process info at `exec()` |
| `query.rs` | Log file querying with binary search optimization and combined filters |
| `config.rs` | TOML-based persistent configuration |
| `systemd.rs` | Systemd service install and uninstall |
| `output.rs` | Event output formatting (human, JSON, CSV) |
| `socket.rs` | Unix socket server (daemon-side) and client (add/remove commands) |
| `utils.rs` | Size/time parsing, process info helpers, UID lookup |
| `help.rs` | Centralized help text for all commands |

### Data Flow

```
Linux Kernel (fanotify)
    → FID events pushed to queue
    → tokio::select reads events asynchronously
    → fid_parser parses FID records (two-pass: resolve + cache recover)
    → Monitor filters (type, size, exclude, scope)
    → output formats (JSON) → log file
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
