<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">Real-time Linux filesystem change monitoring with process attribution.</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

**fsmon** is a real-time Linux filesystem change monitor powered by fanotify. It watches files and directories, captures every event (create, modify, delete, move, attribute change, etc.), and attributes each change back to the process that caused it — including the PID, command name, user, parent PID, thread group ID, and optional full process ancestry chain.

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## Features

- **Real-time Monitoring**: Captures 14 fanotify event types (default: 8 core events; use `--types all` for all 14)
- **Process Attribution**: Tracks PID, command name, user, PPID, and TGID for every file change — even short-lived processes like `touch`, `rm`, `mv`
- **Process Tree Tracking** (`<CMD>` positional arg): Pinpoint a specific process (e.g., `openclaw`) and fsmon will track it plus all its descendants (fork/exec children), building a complete ancestry chain per event.
- **Process Cache**: Uses `proc-tree` crate for efficient process tree management with TTL-based caching.
- **Recursive Monitoring**: Watch entire directory trees with automatic tracking of newly created subdirectories
- **Complete Deletion Capture**: Captures every file deleted during `rm -rf` via persistent directory handle cache
- **Capture-time Filtering**: Filter by event type and file size — in-process, nanosecond-fast, no fork.
- **Live Updates**: Add/remove paths while daemon runs — no restart needed.

## Quick Start

### Prerequisites

- **OS**: Linux 5.9+ (requires fanotify FID mode)
- **Tested Filesystems**: ext4, XFS, btrfs
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

**Fanotify requires root privileges for the daemon:**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### A Complete Walkthrough

Monitor a web project directory, see what gets logged, then use standard Unix tools to filter and clean.

```bash
# Terminal 1: start the daemon (sudo for fanotify)
sudo fsmon daemon &

# Terminal 1 (or another): add paths to monitor
# Monitor /var/www/myapp recursively, MODIFY + CREATE events only,
# tracking the nginx and vim processes.
fsmon add nginx --path /var/www/myapp -r --types MODIFY --types CREATE
fsmon add vim --path /var/www/myapp -r --types MODIFY --types CREATE

# List what's being monitored
fsmon monitored
# {"cmd":"nginx","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
# {"cmd":"vim","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
```

Now trigger some real file changes:

```bash
# Terminal 2: simulate real usage
echo "<h1>Hello</h1>" > /var/www/myapp/index.html      # nginx writes a file
sleep 2
rm /var/www/myapp/index.html                           # file gets deleted
sleep 2
vim /var/www/myapp/config.json                         # vim edits config
```

Look at what fsmon captured:

```bash
# The raw log — one JSONL line per event
cat ~/.local/state/fsmon/*_log.jsonl
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CREATE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":0,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CLOSE_WRITE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0,"ppid":1234,"tgid":5678}
# → {"time":"2026-05-07T10:00:05+00:00","event_type":"CREATE","path":"/var/www/myapp/.config.json.swp","pid":9012,"cmd":"vim","user":"dev","file_size":4096,"ppid":5678,"tgid":9012,"chain":"9012|vim|dev;5678|sh|deploy;1234|openclaw|root;1|systemd|root"}
```

Every event includes `ppid` (parent PID) and `tgid` (thread group ID). When a `<CMD>` is specified on add, matching events also include `chain` — a compact process ancestry string tracing back to PID 1.

#### Query with pipe

```bash
# What did nginx do in the last hour?
fsmon query _global -t '>1h' | jq 'select(.cmd == "nginx")'

# What files were deleted?
fsmon query _global | jq 'select(.event_type == "DELETE")'

# Who made the biggest changes?
fsmon query _global | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'

# Real-time tail with filter (watch for deployments)
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

No built-in `--pid`, `--cmd`, `--user`, `--sort` flags needed — `jq` does it all.

#### Clean with safety

```bash
# Preview what would be deleted (config default: keep 30 days)
fsmon clean _global --dry-run

# Actually clean with custom retention
fsmon clean _global --time '>7d'

# Or just use Unix tools directly on the files
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# Stop the daemon
kill %1                        # or Ctrl+C (foreground)
# If managed via systemd:
sudo systemctl stop fsmon       # Stop
sudo systemctl status fsmon     # Status
journalctl -u fsmon -f          # Logs
```

### File Locations

| Purpose | Path | Format |
|---|---|---|
| Infrastructure config | `~/.config/fsmon/fsmon.toml` | TOML (created by `fsmon init`, all-commented — defaults apply) |
| Monitored paths database | `~/.local/share/fsmon/monitored.jsonl` | JSONL (grouped by cmd, paths as map keys) |
| Event logs | `~/.local/state/fsmon/*_log.jsonl` | JSONL (one event per line) |
| Unix socket | `/tmp/fsmon-<UID>.sock` | JSON over stream |

Both the store path and log directory are configurable in `~/.config/fsmon/fsmon.toml`
(see `[monitored].path` and `[logging].path`).

The daemon runs as root (via sudo) but resolves your original user's home directory
via `SUDO_UID` + `getpwuid_r`, so it writes to `/home/<you>/...` not `/root/...`.

> **Note for vfat/exfat/NFS users:** The daemon tries to chown log files back to your user.
> Filesystems without standard Unix ownership (vfat, exfat, NFS with no_root_squash off)
> don't support this. Logs remain owned by root. If `fsmon clean` fails as a normal user,
> run `sudo fsmon clean` or use the Unix tools directly on the `.jsonl` files.

### Auto-start on boot (optional — systemd recommended)

Recommended (systemd):

```bash
sudo fsmon init --service        # Create service file + systemctl daemon-reload
sudo systemctl enable --now fsmon # Enable auto-start + start now
sudo systemctl status fsmon       # Check status
journalctl -u fsmon -f            # View logs
```

Fallback (crontab, for non-systemd environments):

```bash
sudo crontab -e
@reboot /usr/local/bin/fsmon daemon &
```

> **Note:** Use `sudo crontab -e` (root's crontab) — the daemon needs root privileges.
> Add the `fsmon` command to sudoers with NOPASSWD if using a user crontab instead.

## Complete Commands

### daemon

Start the fsmon daemon — requires `sudo` for fanotify.

```
sudo fsmon daemon                             # Start daemon in foreground
sudo fsmon daemon &                           # Start daemon in background
sudo fsmon daemon --debug                     # Enable debug output (event matching + cache stats)
sudo fsmon daemon --disk-min-free 10%         # Warn when disk space drops below threshold
sudo fsmon daemon --local-time                # Use local timezone in timestamps
sudo fsmon daemon --buffer-size 65536         # Fanotify read buffer (default: 32768)
sudo fsmon daemon --channel-capacity 1024     # Event channel bound (default: unbounded)
sudo fsmon daemon --subscribe-buf 8192        # Subscribe broadcast buffer (default: 4096)
sudo fsmon daemon --cache-dir-cap 200000      # Dir handle cache capacity (default: 100000)
sudo fsmon daemon --cache-dir-ttl 7200        # Dir handle cache TTL (default: 3600secs)
sudo fsmon daemon --cache-file-size 20000     # File size cache capacity (default: 10000)
sudo fsmon daemon --cache-proc-ttl 1200       # Process cache TTL (default: 600secs)
sudo fsmon daemon --cache-stats-interval 0    # Disable periodic cache stats (default: 60secs)
sudo fsmon daemon --metrics-interval 30       # Print status report to stderr every 30s
sudo fsmon daemon --watchdog-interval 15      # watchdog heartbeat interval (secs), in main loop
sudo fsmon daemon --watchdog-multiplier 3     # WatchdogSec = interval × multiplier
```

**Output modes:**

| Mode | Protocol | Default | Purpose |
|------|----------|---------|---------|
| File | JSONL to `~/.local/state/fsmon/` | ✅ on (config-only) | Persistent storage, query/clean tools |
| Socket | Unix socket — connect and receive JSONL stream | ✅ always available | Real-time, nc / kafkacat / any tool |

Configure file output via `[logging].path` in config (enabled by default).

### add

Add a path (optionally with process tracking) to the monitoring list. No sudo needed.

```
fsmon add nginx --path /var/www/myapp -r       # Track nginx on /myapp recursively
fsmon add nginx --path /var/www/myapp          # Track nginx on /myapp (non-recursive)
fsmon add _global --path /home -r              # Monitor all events on /home (global)
fsmon add _global --path /home --types MODIFY  # Filter by event types
fsmon add _global --path /home --types all     # All 14 event types
fsmon add _global --path /home --size '>=1MB'  # Minimum file size filter
```

**Modes:**

| Mode | Example | Behavior |
|------|---------|----------|
| **CMD + --path** | `fsmon add openclaw --path /home` | Track openclaw (and descendants) on /home. Matching events include `chain`. |
| **Global (_global)** | `fsmon add _global --path /home` | All events on /home captured. Each event has `ppid`/`tgid`. |

- `<CMD>` (positional arg) enables **process tree tracking**: fork/exec children are automatically included. Matching events get a `chain` field (e.g., `"102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"`).
- Multiple entries with different `<CMD>` values can be added (OR logic per entry).
- `--path` is required. Use `_global` as CMD for global monitoring (all processes).

### remove

Remove one or more paths from the monitoring list. No sudo needed.

```
fsmon remove _global               # Remove entire global cmd group
fsmon remove nginx                 # Remove entire nginx cmd group
fsmon remove nginx --path /home    # Remove /home from nginx group
fsmon remove _global --path /home  # Remove /home from global group
```

### monitored

List all monitored paths with their filtering configuration in a human-readable format.

```
fsmon monitored  # Show all monitored path groups
```

Output example:
```
=== Monitored Paths ===

Process: touch
  /home/pilot/.config/what (recursive)

Process: _global (all processes)
  /tmp/fsmon_benchmark (recursive, types: ACCESS, MODIFY, CLOSE_WRITE... (14 total))
```

### changes

Show the most recent event per path — a deduplicated summary. Same filters as `query`,
but only the latest event for each unique path is shown, sorted by time descending.

```
fsmon changes _global -t '>1h'          # What changed in the last hour?
fsmon changes _global -t '>2026-05-01'    # Since a specific date
fsmon changes _global --path /var/www     # Filter by path prefix
```

### health

Query daemon health status from the running daemon via Unix socket.

```
fsmon health
```

### query

Query historical events from log files. Output is JSONL — pipe to `jq` for filtering.

```
fsmon query _global                     # Query global log
fsmon query nginx                       # Query nginx log only
fsmon query _global -t '>1h'            # Events from last hour
fsmon query _global -t '>=2026-05-01'   # From absolute time
fsmon query _global -t '<30m'           # Events until 30 minutes ago
fsmon query _global -t '>1h' -t '<now'  # Time range (since + until)
fsmon query _global --path /tmp         # Filter events by path prefix
```

Examples with `jq`:

```bash
# Search by process (ppid/tgid always present)
fsmon query _global | jq 'select(.ppid == 100)'

# Search by ancestry chain (only when --cmd was used on add)
fsmon query _global | jq 'select(.chain != "") | .chain'

# Traditional cmd/user filtering
fsmon query _global -t '>1h' | jq 'select(.cmd == "nginx")'
fsmon query _global | jq 'select(.event_type == "DELETE")'
fsmon query _global | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'
```

### clean

Clean log files for a specific cmd group. Defaults from `fsmon.toml`: `keep_days=30`, `size==>=1GB`.

```
fsmon clean _global                 # Clean global log (defaults)
fsmon clean nginx --time '>7d'      # Keep last 7 days of nginx events
fsmon clean nginx --size '>=500MB'  # Size limit for nginx log
fsmon clean _global --dry-run       # Preview without deleting
```

Priority: CLI arg > fsmon.toml > code default (keep_days=30, size=>=1GB)

You can also clean the raw log files directly without `fsmon clean`:

```bash
# Keep only last 500 lines per log file
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# Delete logs older than 30 days by mtime
find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
```

> **Note:** Native `fsmon clean` parses JSONL accurately (won't cut mid-line) and handles
> both time and size constraints. Raw Unix tools are simpler but may produce partial lines.

### init

Create the config file at `~/.config/fsmon/fsmon.toml` with all settings commented
(defaults apply). Does NOT create log or monitored directories — those are created
on first use by `fsmon add` (monitored) and `fsmon daemon` / `fsmon cd` (logs).

```
fsmon init
```

### cd

Open a subshell in the monitored store or log directory.

```
fsmon cd -l    # Open subshell in log directory (~/.local/state/fsmon)
fsmon cd -m    # Open subshell in monitored store directory (~/.local/share/fsmon)
```

Type `exit` to return to the original directory.

## Configuration

Config file is optional — `fsmon init` creates a reference config; defaults apply without modifications. The generated config has `[logging]` active (file output on).

```toml
# ~/.config/fsmon/fsmon.toml

[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
#   File output is on by default (remove this section to disable).
path = "~/.local/state/fsmon"
keep_days = 30
size = ">=1GB"
disk_min_free = "10%"           # Warn when free space drops below threshold
local_time = false              # Use local timezone in timestamps

[socket]
path = "/tmp/fsmon-<UID>.sock"

[cache]
dir_capacity = 100000
dir_ttl_secs = 3600
file_size_capacity = 10000
proc_ttl_secs = 600
stats_interval_secs = 60
channel_capacity = 1024         # Event channel bound (omit = unbounded)
subscribe_buf = 4096            # Broadcast buffer for subscribe consumers

# [watchdog]
# interval_secs = 15            # heartbeat interval (secs), runs in main event loop
# multiplier = 2                # WatchdogSec = interval × multiplier (MUST be > 1)
```

### Override priority
```
CLI args > fsmon.toml > code defaults
```

CLI flags override both config file and defaults:
```bash
sudo fsmon daemon --cache-dir-cap 200000 --buffer-size 65536
```

## Event Types

Default captures 8 core events. Use `--types all` for all 14.

**Default (8):** CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF

**All 14 (via `--types all`):** + ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR

`FS_ERROR` only works with filesystem-level marks (requires a filesystem that supports it).

## Log Format

Every event is a single JSON line. All fields are always present.

```json
{
  "time": "2026-05-07T10:00:01+00:00",
  "event_type": "CREATE",
  "path": "/var/www/myapp/index.html",
  "pid": 1234,
  "cmd": "nginx",
  "user": "www-data",
  "file_size": 0,
  "ppid": 1,
  "tgid": 1234
}
```

The `chain` field is always present in the output. When `<CMD>` was specified on add and the event matches, it contains the process ancestry chain. When using `_global` or no process tracking, it's an empty string.

```json
{
  ...
  "ppid": 101,
  "tgid": 102,
  "chain": "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
}
```

The `chain` format: `pid|cmd|user` per entry, `;`-separated from the event process up to PID 1 (root).

## Architecture

```
Linux Kernel (fanotify FID mode)
    → Raw  # FID events pushed to kernel queue
    → tokio reads events asynchronously
    → fid_parser: resolves paths (two-pass + moka dir handle cache)
    → filters: event type, size, recursive/non-recursive scope
    → (if <CMD> was specified) process tree check:
      → not in tracked tree → drop immediately (zero /proc reads)
      → in tracked tree → build ancestry chain → append to event
    → write  # JSONL → per-cmd log file (<cmd>_log.jsonl)

Process tree (proc connector + proc-tree crate):
    Fork/Exec/Exit events from netlink connector socket
    → proc-tree cache: pid → {cmd, ppid, user, tgid, start_time}
    On daemon start: /proc/*/status snapshot seeds existing processes
    is_descendant(pid, "openclaw") → O(depth) proc-tree cache lookups

User pipe:
    tail -f *.jsonl | jq 'select(...)'

Clean:
    fsmon clean → parse  # JSONL, apply time/size filters, truncate
```

## Integrations

fsmon exports file events as standard **JSONL** — one event per line, no custom format.

### JSONL files (persistent)

Events written to `~/.local/state/fsmon/*_log.jsonl`. Use any log shipper:

```bash
# Terminal
jq 'select(.cmd == "nginx")' ~/.local/state/fsmon/*_log.jsonl

# Filebeat → ES/Kafka (filebeat.yml)
filebeat.inputs:
  - type: log
    paths: ["/home/*/.local/state/fsmon/*_log.jsonl"]
    json.keys_under_root: true

# Vector → any destination
```

### Unix socket (real-time, no disk)

Connect to `cmd = "subscribe"` socket — receives the same JSONL events in real time:

```bash
nc -U /tmp/fsmon-$(id -u).sock | jq 'select(.cmd == "nginx")'
nc -U /tmp/fsmon-$(id -u).sock | kafkacat -b broker:9092 -t fsmon-events
```

## License

[MIT License](./LICENSE)
