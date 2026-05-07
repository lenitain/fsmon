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
- **Unix Philisophy**: JSONL logs — query with `jq`, filter with `grep`, sort with `sort(1)`. fsmon captures and writes, you control the rest.
- **Flexible Capture Filtering**: Filter at capture time by event type, size, path pattern, and process name — all in-process, nanosecond-fast, no fork.
- **No Sudo Required for Daily Use**: Only `sudo fsmon daemon` needs root (fanotify). All other commands run as normal user.
- **Live Updates**: Add/remove paths while daemon runs — no restart needed.
- **Disk Safety Nets**: Configurable `keep_days` (default: 30) and `max_size` (default: 1GB) prevent disk overflow. Set once in `config.toml`.
- **Podman-style Architecture**: Run the daemon yourself — no systemd dependency. Config per user.

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
cargo build --release

# Or install from crates.io
cargo install fsmon
```

**Fanotify requires root privileges for the daemon:**
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
fsmon add /var/log --exclude-cmd "rsync|apt"     # ignore build noise
fsmon add /tmp --only-cmd nginx                   # only nginx events

# 3. List monitored paths with their filters
fsmon managed

# 4. Query historical events — pipe to jq for custom filtering
fsmon query --since 1h | jq 'select(.cmd == "nginx")'

# 5. Clean old logs (config defaults: keep_days=30, max_size=1GB)
fsmon clean                       # use config defaults
fsmon clean --keep-days 7         # override retention
fsmon clean --dry-run             # preview without deleting

# 6. Remove a path
fsmon remove /tmp

# 7. Stop the daemon
kill %1
```

**No systemd, no `/etc/` config files — everything is per-user.**

### Pipe Examples

```bash
# Filter by pid
fsmon query --since 1h | jq 'select(.pid == 1234)'

# Filter by event type
fsmon query | jq 'select(.event_type == "MODIFY")'

# Sort by file size
fsmon query | jq -s 'sort_by(.file_size)[]'

# Combined
fsmon query --since 1h | jq 'select(.cmd == "nginx" and .file_size > 10240)'

# Real-time tail
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.event_type == "CREATE")'
```

### File Locations

| Purpose | Path | Format | Permissions |
|---|---|---|---|
| Infrastructure config | `~/.config/fsmon/config.toml` | TOML (human-editable) | user-owned |
| Path database | `~/.local/share/fsmon/store.jsonl` | JSONL (one entry per line) | user-owned |
| Event logs | `~/.local/state/fsmon/*_log.jsonl` | JSONL (one event per line) | 644 |
| Unix socket | `/tmp/fsmon-<UID>.sock` | TOML over stream | 666 |

The daemon runs as root (via sudo) but resolves your original user's home directory
via `SUDO_UID` + `getpwuid_r`, so it writes to `/home/<you>/...` not `/root/...`.

### Auto-start on Boot (Optional)

fsmon does not install a systemd service. To start automatically on login:

```bash
crontab -e
@reboot /usr/local/bin/fsmon daemon &
```

## Capture Filtering

All capture filters run inside the daemon process (nanosecond-fast, no fork).
They reduce write I/O — events that don't match never touch disk.

| Flag | Type | Cost | Reason |
|------|------|------|--------|
| `--types` | kernel mask | zero | fanotify only delivers matching events |
| `--recursive` | kernel scope | zero | watch subdirectories |
| `--exclude` | path regex | ~µs | reduce write I/O |
| `--min-size` | u64 compare | ~ns | reduce write I/O |
| `--exclude-cmd` | cmd regex | ~µs | reduce write I/O (new) |
| `--only-cmd` | cmd regex | ~µs | reduce write I/O (new) |
| `--all-events` | kernel mask | zero | enable all 14 events |

## Query & Clean

Query only keeps performance-critical options. All other filtering is done by piping JSONL to standard Unix tools.

```
fsmon query                  →  scan all log files, output JSONL
fsmon query --path /tmp      →  only read /tmp's log file
fsmon query --since 1h       →  binary search + output
```

Clean uses safety net defaults from config.toml, overridable via CLI:

```bash
# Priority: CLI arg > config.toml > code default (30)
fsmon clean                       # uses config defaults
fsmon clean --keep-days 60        # overrides config
```

## Configuration

Auto-generated on first daemon start or via `fsmon generate`. Safety nets included:

```toml
[logging]
dir = "~/.local/state/fsmon"
keep_days = 30          # prevent disk overflow
max_size = "1GB"        # max per log file
```

## Event Types

Default captures 8 core events. Use `--all-events` for all 14.

**Default (8):** CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF

**Additional (6, via --all-events):** ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR

## Architecture

```
Linux Kernel (fanotify)
    → FID events pushed to queue
    → tokio reads events asynchronously
    → fid_parser resolves paths (two-pass + dir cache)
    → Monitor filters (types, size, path pattern, cmd pattern)
    → JSONL → per-path log files (*_log.jsonl)

User pipe:
    cat/ tail *.jsonl → jq → your custom logic
```

### Source Tree

```
src/
├── bin/fsmon.rs       CLI: daemon, add, remove, managed, query, clean, generate
├── lib.rs             FileEvent, EventType, clean engine, temp file safety
├── config.rs          Infrastructure config, SUDO_UID home resolution
├── store.rs           Path database (JSONL)
├── monitor.rs         Fanotify loop, socket handler, all capture filters
├── fid_parser.rs      Low-level FID event parsing, two-pass path recovery
├── dir_cache.rs       Directory handle cache for rm -rf recovery
├── proc_cache.rs      Netlink proc connector (short-lived process attribution)
├── query.rs           Binary-search log query, JSONL output
├── socket.rs          Unix socket protocol (TOML), error classification
├── utils.rs           Size/time parsing, uid lookup, path→log name hash
└── help.rs            Help text for all commands
```

## License

[MIT License](./LICENSE)
