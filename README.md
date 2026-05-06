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
- **No Sudo Required for Daily Use**: Only `sudo fsmon daemon` needs root (fanotify). All other commands run as normal user.
- **Live Updates**: Add/remove paths while daemon runs — no restart needed.
- **Infinite Recursion Protection**: Automatically rejects paths containing the log directory to prevent event loops.
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
cargo build --release

# Or install from crates.io
cargo install fsmon
```

**Important: Fanotify requires root privileges for the daemon**
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
| Infrastructure config | `~/.config/fsmon/config.toml` | `fsmon generate` / daemon auto-create | user-owned |
| Path database (store) | `~/.local/share/fsmon/store.toml` | `fsmon add` / `fsmon remove` | user-owned |
| Event logs (per-path) | `~/.local/state/fsmon/_path_name.toml` | daemon (root)¹ | 644 |
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
echo 'sudo fsmon daemon &' >> ~/.bashrc
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
fsmon query --since 1m --min-size 50MB
```

### Audit Deletion Operations

```bash
# Monitor for deletions
fsmon add ~/myproject --types DELETE --recursive

# Trigger
rm -rf ~/myproject/

# Output shows every file deleted (even in subdirectories)
[2026-05-04 21:37:47] [DELETE] /home/pilot/myproject/hello.c (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
[2026-05-04 21:37:47] [DELETE] /home/pilot/myproject (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
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
fsmon remove /path    # Remove a monitored path
fsmon managed         # List all monitored paths with options
fsmon query --since   # Query historical events
fsmon clean --keep    # Clean old logs
fsmon generate        # Generate default config file
```

Use `fsmon <COMMAND> --help` for detailed help on each subcommand.

## Log File Naming

Log files are named after the monitored path for easy discovery:

| Path | Log filename |
|---|---|
| `/tmp/foo` | `_tmp_foo.toml` |
| `/etc` | `_etc.toml` |
| `/home/my_docs/a_b` | `_home_my!_docs_a!_b.toml` |

The scheme uses `_` as path separator and `!` as escape prefix for literal underscores in path names. This is fully reversible — see `fsmon::utils::log_name_to_path`.

## Architecture

fsmon runs as a foreground daemon managed directly by the user.

```
┌──────────────────────────────────────────────────────┐
│  User runs:  sudo fsmon daemon &                     │
├──────────────────────────────────────────────────────┤
│  Daemon (root):                                      │
│  1. Resolve original user via SUDO_UID               │
│  2. Read ~/.config/fsmon/config.toml (infra paths)   │
│  3. Read ~/.local/share/fsmon/store.toml (paths)     │
│  4. Validate paths (reject log-dir recursion)        │
│  5. fanotify_init → fanotify_mark(paths)             │
│  6. Bind /tmp/fsmon-<UID>.sock (0666)                │
│  7. Loop: fanotify events + socket commands          │
├──────────────────────────────────────────────────────┤
│  CLI (user):  fsmon add /path                        │
│  1. Validate path (reject log-dir recursion)         │
│  2. Write ~/.local/share/fsmon/store.toml            │
│  3. Send add command via socket (live update)        │
│  4. If daemon returns permanent error → rollback     │
└──────────────────────────────────────────────────────┘
```

| Aspect | Detail |
|--------|--------|
| Infrastructure config | `~/.config/fsmon/config.toml` — store path, log dir, socket path |
| Path database | `~/.local/share/fsmon/store.toml` — auto-managed by `add`/`remove` |
| Path management | `fsmon add` / `fsmon remove /path` (live via Unix socket) |
| Log output | TOML events written to per-path files: `~/.local/state/fsmon/_path.toml` |
| Socket | `/tmp/fsmon-<UID>.sock` (mode 0666 for non-root CLI) |
| Error classification | Socket protocol distinguishes `Permanent` vs `Transient` errors |
| Query | `fsmon query --since 1h` — binary-search optimized |
| Clean | `fsmon clean --keep-days 7` — rotate by age or max size |
| Daemon management | User-managed (`sudo fsmon daemon &`, crontab, etc.) |

## Configuration

Config file at `~/.config/fsmon/config.toml`. Auto-generated on first daemon start or via `fsmon generate`.

```toml
# fsmon configuration file
#
# Infrastructure paths for fsmon. Monitored paths are managed separately
# via 'fsmon add' / 'fsmon remove' and persisted in [store].file.
# All paths support ~ expansion. <UID> is replaced with the numeric UID at runtime.

[store]
# Path to the auto-managed monitored paths database.
file = "~/.local/share/fsmon/store.toml"

[logging]
# Directory containing per-path log files (named after monitored path).
dir = "~/.local/state/fsmon"

[socket]
# Unix socket path for daemon-CLI live communication.
path = "/tmp/fsmon-<UID>.sock"
```

### Query Options

| Flag | Description |
|------|-------------|
| `--path` | Path(s) to query. Default: all monitored paths. |
| `--since` | Start time (relative like `1h`, `30m`, `7d` or absolute timestamp) |
| `--until` | End time |
| `--pid` | Filter by PID(s), comma-separated |
| `--cmd` | Filter by process name (wildcards like `nginx*`) |
| `--user` | Filter by username(s), comma-separated |
| `-t, --types` | Filter by event type(s), comma-separated |
| `-m, --min-size` | Minimum size change |
| `-f, --format` | Output format: `human` (default), `json` (TOML output), `csv` |
| `-r, --sort` | Sort by: `time`, `size`, `pid` |

### Clean Options

| Flag | Description |
|------|-------------|
| `--path` | Path(s) to clean. Default: all monitored paths. |
| `--keep-days` | Retention in days (default: 30) |
| `--max-size` | Max size before truncation (e.g. `100MB`) |
| `--dry-run` | Preview without making changes |

## Technical Architecture

### Modules

| Module | Description |
|--------|-------------|
| `lib.rs` | Library crate root — shared types (`FileEvent`, `EventType`), log cleaning engine |
| `bin/fsmon.rs` | Main binary — `daemon`, `add`, `remove`, `managed`, `query`, `clean`, `generate` |
| `config.rs` | Infrastructure config (`~/.config/fsmon/config.toml`), path resolution via `SUDO_UID` |
| `store.rs` | Monitored path database (`~/.local/share/fsmon/store.toml`) |
| `monitor.rs` | Core fanotify monitoring loop, per-filesystem FD groups, scope filtering, file size tracking (LRU), recursion prevention |
| `fid_parser.rs` | Low-level FID mode event parsing, two-pass path recovery, kernel struct definitions |
| `dir_cache.rs` | Directory handle caching via `name_to_handle_at` for deleted file path resolution |
| `proc_cache.rs` | Netlink proc connector listener — captures short-lived process info at `exec()` |
| `query.rs` | Log file querying with binary search optimization and combined filters |
| `output.rs` | Event output formatting (human, TOML, CSV) |
| `socket.rs` | Unix socket protocol (TOML over stream socket) — daemon server + client helpers, `ErrorKind` enum |
| `utils.rs` | Size/time parsing, process info helpers, UID lookup via `/etc/passwd`, path-to-log-name encoding |
| `help.rs` | Centralized help text for all commands |
| `systemd.rs` | Deprecated systemd module — guides users to `sudo fsmon daemon &` |

### Data Flow

```
Linux Kernel (fanotify)
    → FID events pushed to queue
    → tokio::select reads events asynchronously
    → fid_parser parses FID records (two-pass: resolve + cache recover)
    → Monitor filters (type, size, exclude, scope)
    → output formats (TOML) → per-path log files
```

- **fanotify (FID mode + FAN_REPORT_NAME)**: Kernel pushes file events with directory file handles and filenames. No polling — events delivered immediately via non-blocking read.
- **Per-Filesystem FD Groups**: A separate fanotify fd is created per filesystem (mount point) since the kernel forbids marks across different filesystems on a single fd. Each fd gets its own async reader task.
- **Proc Connector**: Background thread subscribes to netlink `PROC_EVENT_EXEC` notifications, caching every process's `(pid, cmd, user)` at the instant it execs. This ensures short-lived processes (`touch`, `rm`, `mv`) are attributable even after they exit.
- **FID Parser + Dir Cache**: Two-pass event processing: (1) resolve file handles via `open_by_handle_at`, (2) use persistent directory handle cache to recover paths for events where the parent directory was already deleted. Handles multi-level nested `rm -rf` scenarios.
- **Binary Search Query**: `fsmon query` uses binary search on approximately time-sorted log files, narrowing the scan range to O(log N) seek operations. Combined with `expand_offset_backward` to catch minor out-of-order entries.
- **Per-Path Log Files**: Logs are named after the monitored path (e.g., `_tmp_foo.toml`), using `!`-escape for literal underscores. No central log file — easy to find, `ls` and `grep` friendly.
- **Error Classification**: Socket protocol distinguishes `Permanent` errors (path conflicts, invalid config — CLI rolls back the store change) from `Transient` errors (runtime issues — change applies on restart).
- **Rust + Tokio**: Per-fd async reader tasks communicate via mpsc channels. Background thread for proc connector. Signal handlers for graceful shutdown and SIGHUP config reload.

### Event Mask Strategy

fsmon uses a two-tier marking strategy:
1. **FAN_MARK_FILESYSTEM** (preferred): Marks the entire mount point covering the target path — no race window for newly created files. Falls back if `EXDEV` (btrfs subvolumes).
2. **Inode mark fallback**: Marks individual directories one by one, with recursive traversal for `--recursive` mode.

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
