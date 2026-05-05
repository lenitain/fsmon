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
- **No Sudo Required for Daily Use**: Only `sudo fsmon daemon` needs root (fanotify). `fsmon add`, `remove`, `managed`, `query`, `clean` all run as a normal user.
- **Podman-style Architecture**: Run the daemon yourself — no systemd dependency. Config per user.

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

### Usage

```bash
# 1. Start the daemon (requires sudo for fanotify)
sudo fsmon daemon &

# 2. Add paths to monitor (no sudo needed)
fsmon add /etc --types MODIFY
fsmon add /var/www --recursive --types MODIFY,CREATE
fsmon add /tmp --all-events

# 3. List monitored paths
fsmon managed

# 4. Query historical events
fsmon query --since 1h --cmd nginx

# 5. Clean old logs (dry-run preview)
fsmon clean --keep-days 7 --dry-run

# 6. Remove a path
fsmon remove /tmp

# 7. Stop the daemon
kill %1
```

**That's it.** No systemd, no `/etc/` config files — everything is per-user.

### File Locations

| Purpose | Path | Created by | Permissions |
|---|---|---|---|
| Config (monitored paths) | `~/.config/fsmon/config.toml` | `fsmon add` / `fsmon remove` | user-owned |
| Event log | `~/.local/state/fsmon/history.log` | daemon (root)¹ | 644 |
| Unix socket | `/tmp/fsmon-<UID>.sock` | daemon (root)¹ | 666 |

¹ The daemon runs as root (via sudo) but resolves your original user's home directory
  via `SUDO_UID` + `getpwuid_r`, so it writes to `/home/<you>/...` not `/root/...`.

### Auto-start on Boot (Optional)

fsmon **does not** install a systemd service. If you want the daemon to start automatically on login, add to your crontab:

```bash
crontab -e
# Add this line:
@reboot /usr/local/bin/fsmon daemon &
```

Or add to your shell profile:

```bash
echo 'fsmon daemon &' >> ~/.bashrc
```

## Examples

### Investigate Configuration Changes

```bash
# Add /etc for monitoring
fsmon add /etc --types MODIFY

# In another terminal, make a change
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# Query the results
fsmon query --since 1h --types MODIFY
```

### Track Large File Creation

```bash
# Watch for files larger than 50MB
fsmon add /tmp --types CREATE

# Trigger
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# Query with min-size filter
fsmon query --since 1m --min-size 50MB --format json
```

### Audit Deletion Operations

```bash
# Monitor for deletions
fsmon add ~/.projects --types DELETE --recursive

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

# Add monitoring with exclude patterns
fsmon add /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## Command Reference

```bash
fsmon daemon          # Start daemon (requires sudo)
fsmon add /path -r    # Add path to monitoring (live + persist)
fsmon remove /path    # Remove path from monitoring
fsmon managed         # List all monitored paths
fsmon query --since   # Query historical events
fsmon clean --keep    # Clean old logs
```

Use `fsmon <COMMAND> --help` for detailed help on each subcommand.

## Architecture

fsmon runs as a foreground daemon managed directly by the user.

```
┌──────────────────────────────────────────────────┐
│  User runs:  sudo fsmon daemon &                 │
├──────────────────────────────────────────────────┤
│  Daemon (root):                                  │
│  1. Resolve original user via SUDO_UID           │
│  2. Read ~/.config/fsmon/config.toml (paths)     │
│  3. fanotify_init → fanotify_mark(paths)         │
│  4. Bind /tmp/fsmon-<UID>.sock (0666)            │
│  5. Loop: fanotify events + socket commands      │
├──────────────────────────────────────────────────┤
│  CLI (user):  fsmon add /path                    │
│  1. Write ~/.config/fsmon/config.toml            │
│  2. Send add command via socket (live update)     │
└──────────────────────────────────────────────────┘
```

| Aspect | Detail |
|--------|--------|
| Config file | `~/.config/fsmon/config.toml` |
| Path management | `fsmon add` / `fsmon remove` (live via Unix socket) |
| Log output | JSON events written to `~/.local/state/fsmon/history.log` |
| Socket | `/tmp/fsmon-<UID>.sock` (mode 0666 for non-root CLI) |
| Query | `fsmon query --since 1h` to read events |
| Clean | `fsmon clean --keep-days 7` to rotate old logs |
| Daemon management | User-managed (`sudo fsmon daemon &`, crontab, etc.) |

## Configuration

Config file at `~/.config/fsmon/config.toml`. No system config needed.

```toml
[[paths]]
path = "/var/www"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "100MB"
exclude = "*.tmp"
all_events = false
```

Each path entry:

| Field | CLI flag | Type | Description |
|-------|----------|------|-------------|
| `path` | `PATH` arg | `string` | Directory/file to watch |
| `types` | `-t, --types` | `string[]` | Event type filter (comma-separated) |
| `min_size` | `-m, --min-size` | `string` | Minimum size change (e.g. "100MB") |
| `exclude` | `-e, --exclude` | `string` | Glob exclude pattern |
| `all_events` | `--all-events` | `bool` | Capture all 14 event types |
| `recursive` | `-r, --recursive` | `bool` | Watch subdirectories |

### Query Options

| Flag | Description |
|------|-------------|
| `--log-file` | Log to query (default: `~/.local/state/fsmon/history.log`) |
| `--since` | Start time (relative like `1h`, `30m`, `7d` or absolute timestamp) |
| `--until` | End time |
| `--pid` | Filter by PID(s), comma-separated |
| `--cmd` | Filter by process name (wildcards like `nginx*`) |
| `--user` | Filter by username(s), comma-separated |
| `-t, --types` | Filter by event type(s), comma-separated |
| `-m, --min-size` | Minimum size change |
| `-f, --format` | Output format: `human` (default), `json`, `csv` |
| `-r, --sort` | Sort by: `time`, `size`, `pid` |

### Clean Options

| Flag | Description |
|------|-------------|
| `--log-file` | Log to clean (default: `~/.local/state/fsmon/history.log`) |
| `--keep-days` | Retention in days (default: 30) |
| `--max-size` | Max size before truncation (e.g. `100MB`) |
| `--dry-run` | Preview without making changes |

## Technical Architecture

### Modules

| Module | Description |
|--------|-------------|
| `lib.rs` | Library crate root — shared types (`FileEvent`, `EventType`), log cleaning engine |
| `bin/fsmon.rs` | Main binary — `daemon`, `add`, `remove`, `managed`, `query`, `clean` |
| `monitor.rs` | Core fanotify monitoring loop, scope filtering, file size tracking (LRU) |
| `fid_parser.rs` | Low-level FID mode event parsing, two-pass path recovery |
| `dir_cache.rs` | Directory handle caching via `name_to_handle_at` for deleted file path resolution |
| `proc_cache.rs` | Netlink proc connector listener — captures short-lived process info at `exec()` |
| `query.rs` | Log file querying with binary search optimization and combined filters |
| `config.rs` | Per-user TOML configuration (`~/.config/fsmon/config.toml`) |
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
