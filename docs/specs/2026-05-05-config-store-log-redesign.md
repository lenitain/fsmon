# fsmon Config / Store / Log Redesign

Date: 2026-05-05

## Motivation

Replace monolithic `~/.config/fsmon/config.toml` (which holds both infrastructure paths and
monitored path entries) with a three-file separation of concerns:

| File | Purpose | Managed by |
|------|---------|------------|
| `~/.config/fsmon/config.toml` | Infrastructure paths (store, logging, socket) | Manual edit + `fsmon generate` |
| `~/.local/share/fsmon/store.toml` | Monitored path entries database | `fsmon add` / `fsmon remove` |
| `~/.local/state/fsmon/<id>.log` | Per-entry event logs | daemon writes, `fsmon clean` manages |

This also replaces the single `history.log` with per-ID log files for better isolation,
querying, and lifecycle management.

## File Layout

```
~/.config/fsmon/config.toml
~/.local/share/fsmon/store.toml
~/.local/state/fsmon/
  ├── 1.log
  ├── 2.log
  └── ...
/tmp/fsmon-<UID>.sock
```

## config.toml Schema

Location: `~/.config/fsmon/config.toml` (via `$XDG_CONFIG_HOME` or `~/.config`).

Manually edited by the user. `fsmon generate` creates a default template.
`fsmon daemon` reads this file on startup — no CLI flags for these paths.

```toml
[store]
file = "~/.local/share/fsmon/store.toml"

[logging]
dir = "~/.local/state/fsmon"

[socket]
path = "/tmp/fsmon-<UID>.sock"
```

- All paths support `~` expansion (via `resolve_home`)
- `socket.path` uses `<UID>` placeholder replaced at runtime

## store.toml Schema

Location: configured by `config.toml` → `[store].file`.
Automatically managed by `fsmon add` and `fsmon remove`.

```toml
next_id = 3

[[entries]]
id = 1
path = "/tmp"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "1KB"
exclude = "*.tmp"
all_events = false

[[entries]]
id = 2
path = "/var/log"
```

- `next_id`: monotonically increasing `u64` counter. Never decremented, never reused.
- `[[entries]]`: required fields are `id` (`u64`) and `path` (string).
- All other fields are optional and default to their CLI equivalent defaults.

## Log File Naming

- `{logging.dir}/{id}.log` — e.g., `~/.local/state/fsmon/1.log`
- Each entry gets its own file, created by daemon on startup if absent.
- Same TOML multi-line block format as current implementation.

## CLI Changes

### `daemon` — no arguments
- Reads `config.toml` only.
- No CLI flags for store/log/socket paths.

### `add <path>` — unchanged interface
```
fsmon add <path> [-r] [-t TYPES] [-m SIZE] [-e PATTERN] [--all-events]
```
- Writes entry to `store.toml` with auto-assigned ID.
- Attempts live update via socket (non-fatal on failure).

### `remove <id>` — unchanged
- Removes entry from `store.toml`. Log file is NOT deleted.

### `managed` — unchanged
- Lists entries from `store.toml` (or live daemon state).

### `query` — `--log-file` replaced by `--id`
```
fsmon query [--id <ids>] [--since <time>] [--until <time>]
            [--pid <pids>] [--cmd <cmd>] [--user <users>]
            [--types <types>] [--min-size <size>]
            [--format <fmt>] [--sort <by>]
```
- `--id <ids>`: comma-separated IDs and/or ranges, e.g., `1,3,5-8`.
- Not specifying `--id` scans all `*.log` files in `logging.dir`.
- Syntax: `--id 1,3,5-8` and `--id 1 --id 3 --id 5` are both accepted.

### `clean` — `--log-file` replaced by `--id`
```
fsmon clean [--id <ids>] [--keep-days <n>] [--max-size <size>] [--dry-run]
```
- Same `--id` syntax as `query`. Default: all logs.

### `generate` — unchanged
- Writes default `config.toml` template.

## Daemon Startup Flow

1. Assert root (fanotify requirement).
2. Resolve original user UID (`SUDO_UID` → `getpwuid_r`).
3. Read `~/.config/fsmon/config.toml` (resolve paths relative to original user's home).
4. Expand `~` in all paths using original user's home directory.
5. Read `store.toml` → get `[[entries]]` list.
6. Ensure `logging.dir` exists.
7. `fanotify_init` → `fanotify_mark` for each entry → bind socket → event loop.
8. On file event: attribute via `proc_cache` → write TOML block to `{logging.dir}/{entry_id}.log`.

## `add` / `remove` Flow (user context)

### add
1. Read `config.toml` → get `store.file` path.
2. Read `store.toml` → `next_id` + `entries`.
3. `entry.id = next_id`, `next_id += 1`, push entry.
4. Write back `store.toml`.
5. Socket notification to daemon (non-fatal).

### remove
1. Read `config.toml` → `store.file`.
2. Read `store.toml`, filter out `entry.id == target`.
3. Write back `store.toml`.
4. Socket notification to daemon (non-fatal).
5. Log file at `{logging.dir}/{id}.log` is preserved.

## `query` / `clean` Flow

### query
1. Read `config.toml` → `logging.dir`.
2. Parse `--id` → set of IDs (comma-sep + ranges; empty = all).
3. For each ID: read `{logging.dir}/{id}.log`, parse all TOML blocks.
4. Merge events, apply filters, sort, output.
5. Sort by ID then by time within ID.

### clean
1. Same `--id` resolution as query (default all).
2. For each matching log file: apply time-based pruning, then size-based truncation.
3. Same logic as current `clean_logs()` but per file.

## Non-goals

- No backward compatibility or migration from old config format.
- No auto-deletion of log files on `remove`.
- No log rotation or compression (future consideration).
- No global `all.log` aggregate (query scans all files when `--id` is omitted).
