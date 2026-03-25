# fsmon - File System Monitor

🌍 **Select Language | 选择语言**
- [English](README.md)
- [简体中文](README.zh-CN.md)

---

**Lightweight High-Performance File System Change Tracking Tool**

fsmon (file system monitor) is a real-time file change monitoring tool that tracks filesystem changes and records which process executed them. When you need to answer "Who modified this file on the server?", fsmon is your answer.

## Features

- **Real-time Monitoring**: Captures 8 core change events by default (CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, CLOSE_WRITE, ATTRIB), `--all-events` enables all 14 fanotify events
- **Complete Process Tracking**: Captures PID, command name, and username for short-lived processes (touch/rm/mv) via Proc Connector
- **Recursive Monitoring**: `-r/--recursive` parameter monitors all subdirectories, dynamically tracking newly created directories
- **Recursive Deletion Capture**: Completely captures all file deletion events during recursive directory deletion (including paths of files in deleted directories)
- **High Performance**: Written in Rust, <5MB memory usage, zero-copy event parsing
- **Flexible Filtering**: Filter by time, size, process, event type
- **Multiple Output Formats**: Human-readable, JSON, CSV
- **Daemon Mode**: Run in background with persistent logging

## Quick Start

### Prerequisites

- **OS**: Linux 5.9+ (requires fanotify FID mode support)
- **Filesystem**: ext4 / XFS / tmpfs (btrfs partial support with race window)
- **Build Tools**: Rust toolchain (cargo)

**Check kernel version**:
```bash
uname -r  # requires ≥ 5.9
```

**Install Rust** (if not installed):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version  # verify installation
```

---

### Installation

#### Method 1: Build from Source (Recommended)

```bash
# 1. Clone repository
git clone https://github.com/lenitain/fsmon.git
cd fsmon

# 2. Install directly from source
cargo install --path .
```

#### Method 2: Install from crates.io

```bash
# Install from crates.io 
cargo install fsmon

# Or install from Git
cargo install --git https://github.com/lenitain/fsmon.git
```

**After installation** (recommended - add to PATH):

```bash
# bash: export PATH="$HOME/.cargo/bin:$PATH"
# fish: fish_add_path $HOME/.cargo/bin
```

**Optional - copy to system path for sudo usage**:

```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

---

### Permission Configuration

**Root permissions required for monitoring certain directories**:

```bash
# Use sudo temporarily
sudo fsmon monitor /etc

# Or add current user to specific group (optional)
sudo usermod -aG systemd-journal $USER
# Log out and log back in for changes to take effect
```

**Proc Connector requires root** (for retrieving process information):
```bash
# Recommended to always run with sudo
sudo fsmon monitor /home
```

**Note**: If you installed fsmon to a custom path (e.g., `~/.cargo/bin` or project directory), `sudo` will not find it because `sudo` resets the PATH. Solution: install to a system path first:

```bash
# Install to system path (one-time setup)
sudo cp /path/to/fsmon /usr/local/bin/

# Now sudo can find fsmon
sudo fsmon monitor /home
```

---

### 8 Typical Scenarios

#### Scenario 1: Investigate Who Modified Configuration Files

```bash
# Monitor /etc directory for modifications
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log

# Execute modification in another terminal
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# Expected output
[2024-05-01 14:30:25] [MODIFY] /etc/hosts (PID: 12345, CMD: tee, USER: root, SIZE: +23B)

# Query afterwards
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY
```

---

#### Scenario 2: Track Large File Creation

```bash
# Monitor file creation larger than 50MB
fsmon monitor /tmp --types CREATE --min-size 50MB --format json

# Trigger operation
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# Expected output
{"time":"2024-05-01T15:00:00Z","event_type":"CREATE","path":"/tmp/large_test.bin","pid":23456,"cmd":"dd","user":"pilot","size_change":104857600}
```

---

#### Scenario 3: Audit Deletion Operations (Complete Recursive Deletion Capture)

```bash
# Recursively monitor deletion events
fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log

# Trigger operation
rm -rf ~/test-project/build/

# Expected output (subdirectory file paths preserved)
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm)
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm)
```

**Technical Highlight**: Through directory handle caching mechanism, `rm -rf` recursive deletion completely captures deletion events of all files and subdirectories.

---

#### Scenario 4: Monitor Specific Applications (Short-lived Process Capture)

```bash
# Recursively monitor project directory
fsmon monitor ~/myapp --recursive

# Trigger operations (short-lived processes like touch/rm/mv)
touch new_file.txt
rm old_config.h
mv temp.c source/temp.c
make

# Expected output (short-lived process CMD correctly displayed)
[2024-05-01 17:00:00] [CREATE] /home/pilot/myapp/new_file.txt (PID: 45678, CMD: touch)
[2024-05-01 17:00:01] [DELETE] /home/pilot/myapp/old_config.h (PID: 45679, CMD: rm)
[2024-05-01 17:00:02] [MOVED_FROM] /home/pilot/myapp/temp.c (PID: 45680, CMD: mv)
[2024-05-01 17:00:02] [MOVED_TO] /home/pilot/myapp/source/temp.c (PID: 45680, CMD: mv)
```

**Technical Highlight**: Proc Connector caches information at process `exec()` instant, ensuring accurate CMD display for short-lived processes like `touch`/`rm`/`mv`.

---

#### Scenario 5: File Move/Rename Audit

```bash
# Monitor move events
fsmon monitor ~/docs --recursive --types MOVED_FROM,MOVED_TO

# Trigger operations
mv ~/docs/drafts/report.txt ~/docs/drafts/report_v2.txt
mv ~/docs/drafts/report_v2.txt ~/docs/published/

# Expected output
[2024-05-01 18:00:00] [MOVED_FROM] /home/pilot/docs/drafts/report.txt (PID: 56789, CMD: mv)
[2024-05-01 18:00:00] [MOVED_TO] /home/pilot/docs/drafts/report_v2.txt (PID: 56789, CMD: mv)
```

---

#### Scenario 6: Long-term Daemon Monitoring

```bash
# Start daemon
sudo fsmon monitor /var/log /etc --recursive --daemon --output /var/log/fsmon-audit.log

# Check status
fsmon status

# JSON format (for integration with monitoring systems)
fsmon status --format json

# Query analysis
fsmon query --since 24h --cmd nginx
fsmon query --since 24h --sort size

# Stop daemon
fsmon stop
```

---

#### Scenario 7: Multi-condition Combined Queries

```bash
# Delete/move operations by root or admin users in past 7 days
fsmon query --since 7d --user root,admin --types DELETE,MOVED_FROM,MOVED_TO --sort time

# Create/modify operations larger than 10MB in past 1 hour
fsmon query --since 1h --min-size 10MB --types CREATE,MODIFY --sort size

# Wildcard command matching
fsmon query --since 24h --cmd "python*"
fsmon query --since 24h --cmd "nginx*",systemctl

# CSV export
fsmon query --since 7d --format csv > weekly_audit.csv
```

---

#### Scenario 8: Log Cleanup and Space Management

```bash
# Preview cleanup effect (keep 7 days)
fsmon clean --keep-days 7 --dry-run

# Execute cleanup
fsmon clean --keep-days 7

# Limit size simultaneously
fsmon clean --keep-days 30 --max-size 100MB
```

---

## Command Reference

Run `fsmon <command> --help` for full parameter documentation:

```bash
fsmon monitor --help    # Real-time monitoring
fsmon query --help      # Query history
fsmon status --help     # View status
fsmon stop --help       # Stop daemon
fsmon clean --help      # Cleanup logs
```

---

## Output Format Examples

### Human-readable Format

```
[2024-05-01 14:30:25] [MODIFY] /var/log/syslog (PID: 1234, CMD: rsyslogd, USER: syslog, SIZE: +2.5KB)
```

### MOVED_FROM / MOVED_TO Events

```
[2024-05-01 14:35:10] [MOVED_FROM] /home/user/old.txt (PID: 5678, CMD: mv, USER: user, SIZE: +0B)
[2024-05-01 14:35:10] [MOVED_TO] /home/user/new.txt (PID: 5678, CMD: mv, USER: user, SIZE: +0B)

[2024-05-01 14:40:22] [MOVED_FROM] /tmp/source/file.txt (PID: 9012, CMD: mv, USER: root, SIZE: +0B)
[2024-05-01 14:40:22] [MOVED_TO] /var/data/file.txt (PID: 9012, CMD: mv, USER: root, SIZE: +0B)
```

### JSON Format

```json
{
  "time": "2024-05-01T14:30:25Z",
  "event_type": "MODIFY",
  "path": "/var/log/syslog",
  "pid": 1234,
  "cmd": "rsyslogd",
  "user": "syslog",
  "size_change": 2560
}
```

### CSV Format

```csv
time,event_type,path,pid,cmd,user,size_change
2024-05-01T14:30:25Z,MODIFY,/var/log/syslog,1234,rsyslogd,syslog,2560
```

## Technical Architecture

### Core Technologies

- **fanotify (FID mode)**: Linux kernel-level file monitoring with FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME support for complete event information
- **Proc Connector (Netlink)**: Listens to process exec() events, caches PID → (cmd, user) mapping at process startup instant, solving short-lived process detection
- **name_to_handle_at**: Pre-caches directory file handles for path recovery during directory deletion
- **Rust + Tokio**: Async runtime with high concurrency and low latency

### Event Types

By default captures 8 core change events, `--all-events` enables all 14.

**Default Events (8 Change Events):**

| Event Type | fanotify Constant | Trigger Condition |
|------------|-------------------|-------------------|
| CLOSE_WRITE | FAN_CLOSE_WRITE | Write-mode file closed (best "file modified" signal) |
| ATTRIB | FAN_ATTRIB | File metadata modified (permissions, owner, timestamps, etc.) |
| CREATE | FAN_CREATE | File/directory created |
| DELETE | FAN_DELETE | File/directory deleted |
| DELETE_SELF | FAN_DELETE_SELF | Monitored object itself deleted |
| MOVED_FROM | FAN_MOVED_FROM | File moved out from this directory |
| MOVED_TO | FAN_MOVED_TO | File moved into this directory |
| MOVE_SELF | FAN_MOVE_SELF | Monitored object itself moved |

**--all-events Additional Events (6 Access/Diagnostic Events):**

| Event Type | fanotify Constant | Trigger Condition |
|------------|-------------------|-------------------|
| ACCESS | FAN_ACCESS | File read |
| MODIFY | FAN_MODIFY | File content written (triggers on every write(), very noisy) |
| CLOSE_NOWRITE | FAN_CLOSE_NOWRITE | Read-only file/directory closed |
| OPEN | FAN_OPEN | File/directory opened |
| OPEN_EXEC | FAN_OPEN_EXEC | File opened for execution |
| FS_ERROR | FAN_FS_ERROR | Filesystem error (Linux 5.16+) |

Additionally, `FAN_Q_OVERFLOW` is automatically delivered by kernel when event queue overflows; fsmon outputs warning to stderr.

## License

MIT License
