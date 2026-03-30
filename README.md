<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">Real-time file system change monitoring with process attribution.</h3>

🌍 **Select Language | 选择语言**
- [English](./README.md)
- [简体中文](./README.zh-CN.md)

[![License](https://img.shields.io/github/license/lenitain/fsmon)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## Features

- **Real-time Monitoring**: Captures 8 core fanotify events (CREATE, DELETE, CLOSE_WRITE, ATTRIB, etc.)
- **Process Attribution**: Tracks PID, command name, and user for every file change — even short-lived processes like `touch`, `rm`, `mv`
- **Recursive Monitoring**: Watch entire directory trees with automatic tracking of newly created subdirectories
- **Complete Deletion Capture**: No more missing events during `rm -rf` — captures every file deleted in recursive operations
- **High Performance**: Written in Rust, <5MB memory footprint, zero-copy event parsing
- **Flexible Filtering**: Filter by time, size, process, user, and event type
- **Multiple Formats**: Human-readable, JSON, and CSV output
- **Daemon Mode**: Run in background with persistent logging for long-term auditing

## Why fsmon

Ever needed to answer "Who modified this file?" on a Linux server? That's exactly what fsmon is for.

Traditional file monitoring tools give you events without context — fsmon bridges that gap by attributing every file change to its responsible process. Whether it's a rogue script, an automated deployment, or a misconfigured service, you'll know exactly what happened, when, and who (or what) caused it.

## Quick Start

### Prerequisites

- **OS**: Linux 5.9+ (requires fanotify FID mode)
- **Filesystem**: ext4, XFS, tmpfs (btrfs partial support)
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

**Important: Copy to system path for sudo usage:**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### Basic Usage

```bash
# Monitor a directory
sudo fsmon monitor /etc --types MODIFY

# Monitor with recursive watching
sudo fsmon monitor ~/myproject --recursive

# Run as daemon for long-term auditing
sudo fsmon monitor /var/log /etc --recursive --daemon --output /var/log/fsmon-audit.log

# Query historical events
fsmon query --since 1h --cmd nginx

# Check daemon status
fsmon status
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
sudo fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log

# Trigger
rm -rf ~/test-project/build/

# Output shows every file deleted (even in subdirectories)
[2026-01-15 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm)
[2026-01-15 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm)
```

## Command Reference

```bash
fsmon monitor --help    # Real-time monitoring
fsmon query --help      # Query history logs
fsmon status --help     # Check daemon status
fsmon stop --help       # Stop daemon
fsmon clean --help      # Cleanup old logs
```

## Technical Architecture

- **fanotify (FID mode)**: Linux kernel-level file monitoring
- **Proc Connector**: Caches process info at `exec()` time for accurate attribution
- **name_to_handle_at**: Directory handle caching for complete deletion tracking
- **Rust + Tokio**: Async runtime with high concurrency

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
| FS_ERROR | Filesystem error (Linux 5.16+)

## License

[MIT License](./LICENSE)
