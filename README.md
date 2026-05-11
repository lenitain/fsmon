<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">Real-time Linux filesystem change monitoring with process attribution.</h3>

🌍 **Select Language | 选择语言**
- [English](./README.md)
- [简体中文](./README.zh-CN.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

**fsmon** is a real-time Linux filesystem change monitor powered by fanotify. It watches files and directories, captures every event (create, modify, delete, move, attribute change, etc.), and attributes each change back to the process that caused it — including the PID, command name, and user. Unlike polling-based tools (e.g. `watch`, `find -newer`), fsmon receives push notifications from the kernel with zero busy-wait. Unlike generic audit tools (e.g. `auditd`, `inotifywait`), fsmon is purpose-built for **developer workflows**: live troubleshooting, deployment forensics, security incident reconstruction, and system behavior analysis — with sub-5MB memory footprint, nanosecond-fast in-process filters, and standard JSONL logs you can pipe to `jq`.

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## Features

- **Real-time Monitoring**: Captures 14 fanotify events (default: 8 core events, `--types all` for all 14)
- **Process Attribution**: Tracks PID, command name, and user for every file change — even short-lived processes like `touch`, `rm`, `mv`
- **Recursive Monitoring**: Watch entire directory trees with automatic tracking of newly created subdirectories
- **Complete Deletion Capture**: Captures every file deleted during `rm -rf` via persistent directory handle cache
- **High Performance**: Rust + Tokio, <5MB memory footprint, zero-copy FID event parsing, binary-search log querying
- **Flexible Capture Filtering**: Filter at capture time by event type, size, path pattern, and process name — all in-process, nanosecond-fast, no fork.
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
# Monitor /var/www/myapp recursively, only MODIFY + CREATE events,
# exclude editor temp files, only capture nginx and vim processes
fsmon add /var/www/myapp -r --types MODIFY --types CREATE --exclude '\.swp$' --exclude-cmd '!nginx|vim'

# List what's being monitored
fsmon managed
# → /var/www/myapp | types=MODIFY,CREATE | recursive | size=- | exclude-path=\.swp | exclude-cmd=!nginx|vim
```

Now trigger some real file changes:

```bash
# Terminal 2: simulate real usage
echo "<h1>Hello</h1>" > /var/www/myapp/index.html      # nginx writes a file
sleep 2
rm /var/www/myapp/index.html                              # file gets deleted
sleep 2
vim /var/www/myapp/config.json                            # vim creates swap file
```

Look at what fsmon captured:

```bash
# The raw log — one JSONL line per event
cat ~/.local/state/fsmon/*_log.jsonl
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"MODIFY","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21,"monitored_path":"/var/www/myapp"}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0,"monitored_path":"/var/www/myapp"}
# → {"time":"2026-05-07T10:00:05+00:00","event_type":"CREATE","path":"/var/www/myapp/.config.json.swp","pid":9012,"cmd":"vim","user":"dev","file_size":4096,"monitored_path":"/var/www/myapp"}
```

Notice: vim's `.swp` was captured but won't be logged — the `--exclude '\.swp$'` filter drops it before writing. That means **it never touches disk**.

#### Query with pipe

Now use standard tools, not fsmon options:

```bash
# What did nginx do in the last hour?
fsmon query -t '>1h' | jq 'select(.cmd == "nginx")'

# What files were deleted?
fsmon query | jq 'select(.event_type == "DELETE")'

# Who made the biggest changes?
fsmon query | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'

# Real-time tail with filter (watch for deployments)
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

No built-in `--pid`, `--cmd`, `--user`, `--sort` flags needed — `jq` does it all.

#### Clean with safety

```bash
# Preview what would be deleted (config default: keep 30 days)
fsmon clean --dry-run

# Actually clean with custom retention
fsmon clean --time '>7d'

# Or just use Unix tools directly on the files
# Delete events older than 2026-04-01:
cat ~/.local/state/fsmon/*_log.jsonl | jq 'select(.time < "2026-04-01T00:00:00Z")' > /dev/null

# Trim to last 500 lines per log file
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
| Path database | `~/.local/share/fsmon/managed.jsonl` | JSONL (one entry per line) |
| Event logs | `~/.local/state/fsmon/*_log.jsonl` | JSONL (one event per line) |
| Unix socket | `/tmp/fsmon-<UID>.sock` | TOML over stream |

Both the store path and log directory are configurable in `~/.config/fsmon/fsmon.toml`
(see `[managed].path` and `[logging].path`).

The daemon runs as root (via sudo) but resolves your original user's home directory
via `SUDO_UID` + `getpwuid_r`, so it writes to `/home/<you>/...` not `/root/...`.

> **Note for vfat/exfat/NFS users:** The daemon tries to chown log files back to your user.
> Filesystems without standard Unix ownership (vfat, exfat, NFS with no_root_squash off)
> don't support this. Logs remain owned by root. If `fsmon clean` fails as a normal user,
> run `sudo fsmon clean` or use the Unix tools directly on the `.jsonl` files.

### Auto-start on Boot (Optional)

fsmon does not install a systemd service. The daemon requires sudo (root) for fanotify.
To start automatically on login, add to crontab with passwordless sudo configured:

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
sudo fsmon daemon          Start daemon in foreground
sudo fsmon daemon &        Start daemon in background
```

Config:           `~/.config/fsmon/fsmon.toml` (optional — defaults without it)
Managed paths:    `~/.local/share/fsmon/managed.jsonl`
Log dir:          `~/.local/state/fsmon/`
Socket:           `/tmp/fsmon-<UID>.sock`

### add

Add a path to the monitoring list. No sudo needed.

```
fsmon add <path>                           Monitor a path
fsmon add <path> -r                        Monitor recursively
fsmon add <path> --types MODIFY --types CREATE     Filter by event types
fsmon add <path> --types all               All 14 event types
fsmon add <path> --exclude '\.swp$' --exclude '\.tmp$'   Exclude path patterns
fsmon add <path> --exclude '!.*\.py$'     Only track .py files
fsmon add <path> -s '>=1MB'                Minimum file size change
fsmon add <path> --exclude-cmd 'rsync'     Exclude by process name
fsmon add <path> --exclude-cmd '!nginx'    Only track nginx process
```

All capture filters run inside the daemon process (nanosecond-fast, no fork).
Events that don't match never touch disk.

### remove

Remove one or more paths from the monitoring list. No sudo needed.

```
fsmon remove <path>                        Remove a monitored path
fsmon remove <path1> <path2> <path3>       Remove multiple paths at once
```

### managed

List all monitored paths with their filtering configuration.

```
fsmon managed                              Show all monitored paths
```

### query

Query historical events from log files. Output is JSONL — pipe to `jq` for filtering.

```
fsmon query                                Query all log files
fsmon query --path /tmp                    Query specific path's log
fsmon query --path /tmp --path /var        Query multiple paths
fsmon query -t '>1h'                     Events from last hour
fsmon query -t '>=2026-05-01'             From absolute time
fsmon query -t '<30m'                     Events until 30 minutes ago
fsmon query -t '>1h' -t '<now'            Time range
```

Examples with `jq`:

```bash
fsmon query -t '>1h' | jq 'select(.cmd == "nginx")'
fsmon query | jq 'select(.event_type == "DELETE")'
fsmon query | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'
```

### clean

Clean historical log files. Defaults from `fsmon.toml`: `keep_days=30`, `size=>=1GB`.

```bash
fsmon clean                                Use config defaults
fsmon clean --time '>7d'                 Keep last 7 days
fsmon clean --size '>=500MB'              Size limit per log file
fsmon clean --path /tmp                    Clean specific path's log
fsmon clean --dry-run                      Preview without deleting
```

Priority: CLI arg > fsmon.toml > code default (keep_days=30)

You can also clean the raw log files directly without `fsmon clean`:

```bash
# Keep only last 500 lines per log file
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# Delete logs older than 30 days by mtime
find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
```

> **Performance note:** Native `fsmon clean` parses JSONL accurately (won't cut in the middle of a line) and handles both time+size rules atomically. Raw Unix tools are simpler but may produce partial/corrupt lines.

### init

Initialize fsmon data directories (chezmoi-style). Creates the log directory,
managed data directory. Does NOT write a config file —
config is optional, defaults apply without it.

```
fsmon init                                 Create log & managed directories
```

### cd

Open a subshell in the log directory. Type `exit` to return:

```
fsmon cd                                   Enter log directory in subshell
ls                                         List log files
```

## Configuration

Config file is optional — defaults apply without it.

```toml
# fsmon configuration file
#
# Infrastructure paths for fsmon. Monitored paths are managed separately
# via 'fsmon add' / 'fsmon remove' and persisted in [managed].path.
# All paths support ~ expansion. <UID> is replaced with the numeric UID at runtime.

[managed]
# Path to the auto-managed monitored paths database.
path = "~/.local/share/fsmon/managed.jsonl"

[logging]
# Path to the event log directory (per-path *_log.jsonl files).
path = "~/.local/state/fsmon"
# Defaults for 'fsmon clean' (not auto-cleaned by daemon; use cron/timer).
#   keep_days: delete entries older than N days
#   size:  truncate log file when exceeding this size
# Both can be overridden at runtime:
#   fsmon clean --time '>14d' --size '>=1GB'
keep_days = 30
size = ">=1GB"

[socket]
# Unix socket path for daemon-CLI live communication.
path = "/tmp/fsmon-<UID>.sock"
```

## Event Types

Default captures 8 core events. Use `--types all` for all 14.

**Default (8):** CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF

**All 14 (via --types all):** + ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR

## Architecture

```
Linux Kernel (fanotify)
    → FID events pushed to queue
    → tokio reads events asynchronously
    → fid_parser resolves paths (two-pass + dir cache)
    → filters: event type, size, path pattern, process name
    → JSONL → per-path log files (*_log.jsonl)

User pipe:
    cat/ tail *.jsonl → jq → your custom logic

Clean:
    fsmon clean → clean engine parses JSONL, trims by time/size
```

### Source Tree

```
src/
├── bin/
│   ├── fsmon.rs               CLI entry: main(), CLI structs, arg parsing tests
│   └── commands/
│       ├── mod.rs              Dispatch: run() → per-command handler
│       ├── daemon.rs           cmd_daemon: fanotify init, socket setup, Monitor::run()
│       ├── add.rs              cmd_add: path normalization, store update, live socket
│       ├── remove.rs           cmd_remove: store update, live socket
│       ├── manage.rs           cmd_managed, cmd_list_managed_paths
│       ├── query.rs            cmd_query: time filter, Query::execute()
│       ├── clean.rs            cmd_clean: time/size filter delegation
│       └── init_cd.rs          cmd_init, cmd_cd
├── lib.rs             FileEvent, EventType, DaemonLock (singleton via flock)
├── clean.rs           Log cleanup engine: time/size trim, tail-offset, dry-run
├── config.rs          Infrastructure config, SUDO_UID home resolution
├── managed.rs         Managed paths database (JSONL)
├── monitor.rs         Fanotify loop, socket handler, add/remove/event processing
├── fid_parser.rs      Low-level FID event parsing, two-pass path recovery
├── filters.rs         PathOptions, event/size/path/process filters, path matching
├── dir_cache.rs       Directory handle cache for rm -rf recovery
├── proc_cache.rs      Netlink proc connector (short-lived process attribution)
├── query.rs           Binary-search log query, JSONL output
├── socket.rs          Unix socket protocol (TOML), error classification
├── utils.rs           Size/time parsing, uid lookup, path→log name hash
└── help.rs            Help text for all commands
```

## License

[MIT License](./LICENSE)
