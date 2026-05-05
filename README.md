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

Config read from `~/.config/fsmon/fsmon.toml`. CLI flags override config values.

### Instance Mode — Systemd Background Monitoring

```bash
# 1. Install systemd template (one-time)
sudo fsmon install

# 2. Generate instance config template (or create manually)
sudo fsmon generate --instance web

# Edit the template to set paths and options
sudo vim /etc/fsmon/fsmon-web.toml

# Or create manually:
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
fsmon generate                      # Generate CLI config (~/.config/fsmon/fsmon.toml)
fsmon generate --instance web       # Generate instance config template (/etc/fsmon/fsmon-web.toml)
```

## Two Modes: CLI vs Systemd Instance

fsmon has two completely independent operating modes, each with its own config file:

### CLI Mode (`fsmon monitor /path ...`)

| Aspect | Detail |
|--------|--------|
| Config file | `~/.config/fsmon/fsmon.toml` |
| Config location | `~/.config/fsmon/` |
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

Reads from `~/.config/fsmon/fsmon.toml` (generated by `fsmon generate`).

Generate a template: `fsmon generate`

| Section | Field | CLI flag | Type | Description |
|---------|-------|----------|------|-------------|
| `[monitor]` | `paths` | `PATH` args | `string[]` | Directories/files to watch |
| | `types` | `-t, --types` | `string` | Event filter, comma-separated |
| | `min_size` | `-s, --min-size` | `string` | Min size (e.g. "100MB") |
| | `exclude` | `-e, --exclude` | `string` | Glob exclude pattern |
| | `all_events` | `--all-events` | `bool` | Capture all 14 event types |
| | `output` | `-o, --output` | `string` | Log file path |
| | `format` | `-f, --format` | `string` | Output: human/json/csv |
| | `recursive` | `-r, --recursive` | `bool` | Watch subdirectories |
| | `buffer_size` | | `int` | Fanotify buffer (bytes) |
| `[query]` | `log_file` | `--log-file` | `string` | Log to query |
| | `since` | `--since` | `string` | Start time (rel/abs) |
| | `until` | `--until` | `string` | End time |
| | `pid` | `--pid` | `string` | Filter by PID(s) |
| | `cmd` | `--cmd` | `string` | Filter by process name |
| | `user` | `--user` | `string` | Filter by username(s) |
| | `types` | `-t, --types` | `string` | Filter by event type(s) |
| | `min_size` | `-s, --min-size` | `string` | Min size change |
| | `format` | `-f, --format` | `string` | Output format |
| | `sort` | `--sort` | `string` | Sort: time/size/pid |
| `[clean]` | `log_file` | `--log-file` | `string` | Log to clean |
| | `keep_days` | `--keep-days` | `int` | Retention in days |
| | `max_size` | `--max-size` | `string` | Max size before truncation |
| `[install]` | `protect_system` | | `string` | systemd ProtectSystem |
| | `protect_home` | | `string` | systemd ProtectHome |
| | `read_write_paths` | | `string[]` | Extra r/w paths for systemd |
| | `private_tmp` | | `string` | systemd PrivateTmp |

CLI flags override config file values.

### Instance Config (`/etc/fsmon/fsmon-{name}.toml`)

Each systemd instance reads its config from `/etc/fsmon/fsmon-{name}.toml`. Only `paths` is required. Generate a template: `sudo fsmon generate --instance <name>`

```toml
paths = ["/var/www"]
# output = "/var/log/fsmon/web.log"
# types = "MODIFY,CREATE"
# min_size = "100MB"
# exclude = "*.tmp"
# all_events = false
# recursive = true
```

| Field | CLI flag | Description |
|-------|----------|-------------|
| `paths` | `PATH` args | **Required.** Directories/files to monitor |
| `output` | `-o, --output` | Log path. Not set → journald only |
| `types` | `-t, --types` | Event filter, comma-separated |
| `min_size` | `-s, --min-size` | Min size change |
| `exclude` | `-e, --exclude` | Glob exclude pattern |
| `all_events` | `--all-events` | All 14 event types |
| `recursive` | `-r, --recursive` | Watch subdirectories |

Instance configs have no search priority — loaded by exact name from `/etc/fsmon/`. CLI flags alongside `--instance` override instance config fields.

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
