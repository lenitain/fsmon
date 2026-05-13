<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">Real-time Linux filesystem change monitoring with process attribution.</h3>

🌍 **Select Language | 选择语言**
- [English](./README.md)
- [简体中文](./README.zh-CN.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

**fsmon** is a real-time Linux filesystem change monitor powered by fanotify. It watches files and directories, captures every event (create, modify, delete, move, attribute change, etc.), and attributes each change back to the process that caused it — including the PID, command name, user, parent PID, thread group ID, and optional full process ancestry chain.

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## Features

- **Real-time Monitoring**: Captures 14 fanotify event types (default: 8 core events; use `--types all` for all 14)
- **Process Attribution**: Tracks PID, command name, user, PPID, and TGID for every file change — even short-lived processes like `touch`, `rm`, `mv`
- **Process Tree Tracking** (`<CMD>` positional arg): Pinpoint a specific process (e.g., `openclaw`) and fsmon will track it plus all its descendants (fork/exec children), building a complete ancestry chain per event.
- **Recursive Monitoring**: Watch entire directory trees with automatic tracking of newly created subdirectories
- **Complete Deletion Capture**: Captures every file deleted during `rm -rf` via persistent directory handle cache
- **High Performance**: Rust + Tokio, <5MB memory footprint, zero-copy FID event parsing, binary-search log querying
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
kill %1
```

### File Locations

| Purpose | Path | Format |
|---|---|---|
| Infrastructure config | `~/.config/fsmon/fsmon.toml` | TOML (optional — defaults without it) |
| Monitored paths database | `~/.local/share/fsmon/monitored.jsonl` | JSONL (grouped by cmd, paths as map keys) |
| Event logs | `~/.local/state/fsmon/*_log.jsonl` | JSONL (one event per line) |
| Unix socket | `/tmp/fsmon-<UID>.sock` | TOML over stream |

Both the store path and log directory are configurable in `~/.config/fsmon/fsmon.toml`
(see `[monitored].path` and `[logging].path`).

The daemon runs as root (via sudo) but resolves your original user's home directory
via `SUDO_UID` + `getpwuid_r`, so it writes to `/home/<you>/...` not `/root/...`.

> **Note for vfat/exfat/NFS users:** The daemon tries to chown log files back to your user.
> Filesystems without standard Unix ownership (vfat, exfat, NFS with no_root_squash off)
> don't support this. Logs remain owned by root. If `fsmon clean` fails as a normal user,
> run `sudo fsmon clean` or use the Unix tools directly on the `.jsonl` files.

### Auto-start on Boot (Optional)

fsmon does not install a systemd service. The daemon requires sudo (root) for fanotify.
To start automatically on login:

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
sudo fsmon daemon                     # Start daemon in foreground
sudo fsmon daemon &                   # Start daemon in background
sudo fsmon daemon --debug             # Enable debug output (event matching + cache stats)
sudo fsmon daemon --cache-dir-cap N   # Directory handle cache capacity (default: 100000)
sudo fsmon daemon --cache-dir-ttl N   # Directory handle cache TTL in seconds (default: 3600)
sudo fsmon daemon --cache-file-size N # File size cache capacity (default: 10000)
sudo fsmon daemon --cache-proc-ttl N          # Process cache TTL in seconds (default: 600)
sudo fsmon daemon --cache-stats-interval N    # Cache stats log interval in debug mode (default: 60, 0=off)
sudo fsmon daemon --buffer-size N             # Fanotify read buffer in bytes (default: 32768)
```

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

List all monitored paths with their filtering configuration (JSONL).

```
fsmon monitored  # Show all monitored path groups
```

Each line is a JSON object with `cmd` and `paths` fields. Pipe to `jq` for filtering.

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

Initialize fsmon data directories. Creates log dir and monitored data dir.
Does NOT write a config file — config is optional, defaults apply without it.

```
fsmon init
```

### cd

Open a subshell in the log directory. Type `exit` to return:

```
fsmon cd
ls _global_log.jsonl
```

## Configuration

Config file is optional — defaults apply without it.

```toml
# fsmon configuration file
#
# Infrastructure paths for fsmon. Monitored paths are added via
# 'fsmon add' / 'fsmon remove' and persisted in [monitored].path.
# All paths support ~ expansion. <UID> is replaced with the numeric UID at runtime.

[monitored]
# Path to the auto-monitored monitored paths database.
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
# Path to the event log directory (per-cmd *_log.jsonl files).
path = "~/.local/state/fsmon"
# Defaults for 'fsmon clean' (no auto-clean; use cron/timer).
keep_days = 30
size = ">=1GB"

[socket]
# Unix socket path for daemon-CLI live communication.
path = "/tmp/fsmon-<UID>.sock"

[cache]
# Directory handle cache capacity (default: 100000, ~15-20MB).
# Each entry maps a kernel file handle to a directory path.
# Lower on memory-constrained systems; raise when monitoring
# large directory trees (>100k dirs) to reduce handle re-resolution.
dir_capacity = 100000

# Directory handle cache TTL in seconds (default: 3600 = 1 hour).
# Shorter TTL frees memory faster for volatile directory structures;
# longer TTL reduces handle re-resolution for stable directories.
dir_ttl_secs = 3600

# File size cache capacity (default: 10000, ~1MB).
# Avoids stat() calls for files with known sizes.
# Raise for high-file-volume workloads (git checkout, npm install).
file_size_capacity = 10000

# Process cache TTL in seconds (default: 600 = 10 minutes).
# Applies to both process info cache (PID→cmd/user/ppid/tgid) and
# process tree cache (PID→parent for ancestor chain tracking).
# Shorter TTL cleans up zombie process entries faster;
# longer TTL reduces /proc reads for long-lived processes.
proc_ttl_secs = 600

# Cache stats log interval in seconds in debug mode (default: 60).
# Set to 0 to disable periodic cache stats output.
stats_interval_secs = 60
```

### Override priority
```
CLI arguments (--cache-dir-cap, --cache-dir-ttl, --cache-file-size, --cache-proc-ttl, --cache-stats-interval, --buffer-size)
    > fsmon.toml [cache] section
        > code defaults
```

CLI flags override both config file and defaults:
```bash
# Override dir_cache capacity and fanotify buffer size at startup
sudo fsmon daemon --cache-dir-cap 200000 --buffer-size 65536 &
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

When `<CMD>` was specified on add and the event matches: `chain` is also included.

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
    → fid_parser: resolves paths (two-pass +  # DashMap dir handle cache)
    → filters: event type, size, recursive/non-recursive scope
    → (if <CMD> was specified) process tree check:
      → not in tracked tree → drop immediately (zero /proc reads)
      → in tracked tree → build ancestry chain → append to event
    → write  # JSONL → per-cmd log file (<cmd>_log.jsonl)

Process tree (proc connector):
    Fork/Exec/Exit events from netlink connector socket
    →  # DashMap: pid → {cmd, ppid, user, tgid, start_time}
    On daemon start: /proc/*/stat snapshot seeds existing processes
    is_descendant(pid, "openclaw") → O(depth)  # DashMap lookups

User pipe:
    tail -f *.jsonl | jq 'select(...)'

Clean:
    fsmon clean → parse  # JSONL, apply time/size filters, truncate
```

### Source Tree

```
src/
├── bin/
│   ├── fsmon.rs                 # CLI entry: main(), argument structs, arg tests
│   └── commands/
│       ├── mod.rs               # run() dispatch, parse_path_entries helper
│       ├── daemon.rs            # Daemon: load store,  # Monitor::new(), run()
│       ├── add.rs               # CLI add: path normalization, store + socket
│       ├── remove.rs            # CLI remove: store + socket
│       ├── monitored.rs         # CLI monitored:  # JSONL output
│       ├── query.rs             # CLI query: time filter, execute query
│       ├── clean.rs             # CLI clean: parser delegation
│       └── init_cd.rs           # CLI init, cd
│
├── lib.rs              # FileEvent, EventType,  # DaemonLock (flock singleton)
├── clean.rs            # Log cleanup engine: time/size trim, tail-offset
├── config.rs           # TOML config,  # SUDO_UID home resolution
├── monitored.rs        # Monitored paths database (JSONL store)
├── monitor.rs          # Fanotify loop, socket handler, add/remove/events
├── fid_parser.rs       # FID event parsing, two-pass path recovery
├── filters.rs          # PathOptions, event/size filters, path matching
├── dir_cache.rs        # Directory handle cache (DashMap + HandleKey)
├── proc_cache.rs       # hNetlink proc connector:  # Fork/Exec/Exit, build_chain
├── query.rs            # Binary-search log query on sorted JSONL
├── socket.rs           # Unix socket protocol (TOML req/resp)
├── utils.rs            # Size/time parsing, process info lookup, chown
└── help.rs             # Help text constants
```

## License

[MIT License](./LICENSE)
