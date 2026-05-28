# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-05-28

### Added

- **Unified broadcast event stream**: all events flow through a single
  `tokio::sync::broadcast` channel inside the daemon. File writing, subscribe,
  and metrics all consume from the same stream — no duplicated work, consistent
  ordering, zero-copy cloning.
- **Subscribe protocol** (`cmd = "subscribe"`): real-time event streaming over the
  Unix socket. Subscribers receive JSONL events as they happen. Supports per-connection
  filters (`track_cmd`, `types`, `local_time`). See `extensions/subscribe-stream/`.
- **Prometheus metrics endpoint**: dual-transport design — always available via Unix
  socket (`cmd = "metrics"`), plus an optional TCP HTTP listener
  (`--metrics-listen 127.0.0.1:9845`). Returns standard Prometheus text format.
  Counters: `fsmon_events_total{event_type,cmd}`. Gauges: `fsmon_subscribers`,
  `fsmon_monitored_paths`, `fsmon_reader_groups`, `fsmon_pending_paths`,
  `fsmon_disk_buffer_events`. Configurable via `--subscribe-buf` (default 4096).
- **Local time support**: `--local-time` CLI flag and `logging.local_time` config
  option. When enabled, all timestamps in JSONL output use local timezone offset
  (e.g. `+08:00`) instead of UTC `Z`. Per-subscriber override via subscribe command.
  Field order is preserved identically to UTC mode.
- **Integration examples** (`extensions/`): 10 Python scripts organized by the
  4 data exit points, all with detailed quick-start / wire-protocol / bridge-to
  documentation:
  - `jsonl-logs/fsmon-log-tail.py` — tail, grep, aggregate on-disk JSONL files
  - `subscribe-stream/fsmon-subscribe-demo.py` — minimal subscribe consumer
  - `subscribe-stream/fsmon-webhook.py` — HTTP webhook bridge (Slack, Discord, etc.)
  - `subscribe-stream/fsmon-kafka.py` — Apache Kafka producer
  - `subscribe-stream/fsmon-to-es.py` — Elasticsearch bulk indexing
  - `subscribe-stream/fsmon-to-influxdb.py` — InfluxDB line protocol
  - `subscribe-stream/fsmon-to-s3.py` — S3 batch archiver
  - `subscribe-stream/fsmon-custom-format.py` — CSV, TSV, syslog, Loki, JSON output
  - `socket-admin/fsmon-admin.py` — programmatic add/remove/list/health
  - `http-metrics/fsmon-metrics.py` — pull metrics via socket
  - `http-metrics/prometheus.yml` — Prometheus scrape config + 4 alerting rules
  - `http-metrics/fsmon-grafana.json` — Grafana dashboard (8 panels)
- **Atomic metrics counters**: `MetricsRegistry` with lock-free `AtomicU64` counters
  and labeled `CounterVec` — zero overhead on the hot path.
- **Subscribe protocol integration tests**: end-to-end wire format test with TOML
  command parsing, JSONL event streaming, and cmd/type filter verification.
- **TCP /metrics integration tests**: HTTP 200, Content-Type, Connection: close,
  Content-Length matching, empty registry, and labeled counter output.
- **TESTING.md**: manual test procedures for all output channels.

### Changed

- **FileLogWriter decoupled**: log writing moved from `process_event_batch` hot path
  to a standalone `FileLogWriter` task. It subscribes to the broadcast stream
  just like any other consumer — file I/O no longer blocks event processing.
- **File logging on by default**: the default `[logging].path` is
  `~/.local/state/fsmon`. Set to `""` (empty) or remove the `[logging]` section to disable.
  Log path is config-only; no CLI flag to toggle.
- **`--log-path` removed**: log path is now config-only (`logging.path` in fsmon.toml).
  Removed from all CLI flags, help text, and documentation.
- **Generated config restructured**: `fsmon init` output has three active sections —
  `[monitored]`, `[logging]`, `[socket]` — with CLI availability annotations.
- **Monitor module split**: `monitor.rs` (3153 lines) decomposed into 9 focused
  submodules: `channel`, `events`, `file_writer`, `filtering`, `live_path`,
  `reader`, `socket_handler`, `tests`, `mod`. Each ~100–400 lines.
- **`src/help/` removed**: inline `changes.md` help text, removing the separate
  help module directory.
- **Internationalization cleanup**: all Chinese text removed from source code,
  comments, and docs (except `README.zh-CN.md`). Codebase is now English-only.
- **Extensions reorganized**: moved from flat `extensions/*.py` to 4 subdirectories
  matching the 4 data exit points. All scripts updated with path references,
  enriched docstrings, and consistent `EXAMPLE ONLY` disclaimer.
- **to_jsonl_string_local**: preserves exact struct field order (same as UTC mode),
  only replacing the timestamp value inline.
- **Deferred event publish**: `process_event_batch` no longer sends events
  directly to the broadcast channel. It returns `Vec<PendingEvent>`, and the
  event loop handles the two-phase drain→build→drain→patch→publish pipeline.
  This closes the race window between fanotify and proc connector events
  without locks, sleeping, or callback indirection.

### Fixed

- **`fsmon cd` fallback**: when `path` is `None`, falls back to
  `~/.local/state/fsmon` instead of panicking.
- **`fsmon add` directory creation**: only `fsmon add` auto-creates the
  monitored store directory; `fsmon init` never creates log directories.
- **Config template**: added missing `local_time` field to the default
  commented configuration template.
- **Stale `--log-path` references**: purged from all code, docs, help text,
  and README.
- **Process attribution regression**: snapshot-produced `start_time_ns` was
  hardcoded to `0`, causing the PID reuse check to reject ALL pre-existing
  process cache entries as "reused" — every event from those processes fell
  through to the `/proc` fallback, and for short-lived processes (`rm`,
  `touch`) that had already exited, the fallback also failed, resulting in
  `cmd="unknown"` / `user="unknown"` across all events. Fixed by reading the
  real `start_time_ns` from `/proc/{pid}/stat` during the snapshot.
- **Race between fanotify and proc connector**: fanotify DELETE events and
  proc connector Exec events arrive through independent kernel subsystems
  with no ordering guarantee. A short-lived process's fanotify event could
  be processed before its Exec event populated the caches. Fixed with a
  **two-phase publish pipeline**: build events → drain proc events a second
  time (typically ~200ns, almost always `WouldBlock`) → resolve any
  remaining `unknown` fields from now-populated caches → publish to broadcast.
  This eliminates the race at the architecture level rather than patching
  events after they're emitted.

### Architecture

The 0.4.0 daemon exposes **4 data exit points**, all consuming from a single
broadcast event stream:

```
fanotify → event_stream_tx (broadcast)
              ├── FileLogWriter  → JSONL disk files       (exit ①)
              ├── subscribe      → Unix socket stream      (exit ②)
              ├── socket admin   → add/remove/list/health  (exit ③)
              └── metrics        → Prometheus text         (exit ④)
```

## [0.3.4] - 2026-05-26

### Added

- `fsmon changes` subcommand: deduplicated per-path event summary. Same filters as `query`,
  but groups by path and outputs only the latest event per path, sorted by time descending.
  (`fsmon changes _global -t '>2026-05-25'`)
- `--sync-interval N` CLI flag and `sync_interval_secs` config option: periodic `fdatasync`
  on dirty log files to prevent event loss on crash. Default: disabled. Recommended: 5s.
- `test_fanotify_mark_null_byte_path_no_root`: verifies `fanotify_mark` rejects interior null
  bytes before syscall (no root needed).

### Changed

- Log writes now track dirty log paths for periodic `fdatasync` (when sync-interval is enabled).
- SIGTERM / Ctrl-C handlers drain pending events then `fdatasync` before exit.
- Switched `fanotify-fid` dependency to local path for development.
- `fanotify_mark`: replaced `path.as_bytes().to_vec()` + manual null-termination
  with `CString::new(path.as_encoded_bytes())`, eliminating one heap allocation.

### Removed

- Removed unused `procfs` dependency from `Cargo.toml` (reduces compilation time).

### Fixed

- **Permissions**: `resolve_uid_gid()` and `resolve_uid()` no longer depend on
  `SUDO_UID` as the only fallback.  When running as root without `SUDO_UID`
  (e.g. systemd), they now derive the original user from the owner of `$HOME`.
  This means log files and directories are correctly owned by the user
  regardless of how fsmon was started — `sudo`, `systemd`, or any other
  root-launched context.

## [0.3.2] - 2026-05-15

### Added

- `--channel-capacity` CLI / `channel_capacity` config: bounded event channel to cap memory
  under extreme event storms (default: unbounded).
- `--disk-min-free` CLI / `disk_min_free` config: disk space pre-check with runtime buffer
  (up to 10,000 events buffered in memory when disk is full, retried every 10s).
- `fsmon health` subcommand: query daemon health via Unix socket (alive/dead tracking).
- `fsmon init --service`: install systemd service file for automatic crash recovery.
- `sd_notify(READY=1)` support for systemd `Type=notify` services.
- Reader task supervision with auto-restart on death.
- Graceful shutdown: drain event channel on SIGTERM/Ctrl-C before exit.
- Debug mode: periodic cache stats output (`--debug`, default every 60s).
- Configurable cache parameters: dir_cache capacity/TTL, file_size_cache capacity,
  proc_cache TTL, buffer size — via CLI > fsmon.toml > code defaults.
- PID reuse detection: stores `start_time_ns` from `/proc/stat` and verifies on cache hit.
- Process tree snapshot on daemon start: seeds `PidTree` + `ProcCache` from `/proc/*/status`.
- Log directory auto-recreation on ENOENT during runtime.

### Changed

- **Cache rewrite**: `DashMap` → `moka::sync::Cache` for dir_cache (100k cap + 1h TTL +
  W-TinyLFU eviction) and proc_cache/pid_tree, removing `dashmap` dependency.
- **Process cache refactor**: removed `pid_cache` (LruCache), unified under `proc_cache` (moka).
- Is_descendant / build_chain: added cycle detection via visited set.
- Event routing: `monitored_entries` stores `PathOptions` per (path, cmd) pair, supporting
  the same path under multiple cmd groups with different filters.
- Socket add/remove now uses `(path, cmd)` pair as the unique entry identifier.

### Fixed

- `rm -rf` recursive delete: child file events no longer lost during directory removal.
- Daemon startup validates `cmd=fsmon` even from manual `monitored.jsonl` edits.
- Cached processes with reused PIDs are now correctly detected and re-fetched.
- All Clippy warnings resolved.

### Performance

- `BufWriter` for log writes (reduces write syscalls).
- Reusable 32KB fanotify read buffer across loop iterations.
- Pre-compiled regex patterns outside hot path.
- `/etc/passwd` UID lookup cached with `OnceLock`.
- `SmallVec` for per-event type lists and handle keys.

## [0.3.1] - 2026-05-13

### Added

- CLI parameter and config file support for cache TTL intervals.
- `proc-connector` dependency for safe netlink process event handling.

### Fixed

- `rm -rf` recursive delete: sub-file events no longer lost during recursive directory removal.
- Daemon startup validates `cmd=fsmon` (prevents manual `monitored.jsonl` edits from bypassing validation).
- All Clippy warnings resolved.

## [0.3.0] - 2026-05-13

### Added

- **Cache system rewrite**: `DashMap` replaced with `moka::sync::Cache` for:
  - `dir_cache`: 100k capacity + 1h TTL + W-TinyLFU eviction
  - `proc_cache`: process info cache (PID → cmd/user/ppid/tgid/start_time)
  - `pid_tree`: process ancestry tree (PID → ppid + cmd)
- **Configurable cache parameters**: CLI args + `fsmon.toml` + code defaults chain.
- **Cache stats**: periodic debug output (`--debug`, default every 60s).
- **PID reuse detection**: verifies `start_time_ns` from `/proc/stat` on cache hit.
- **Process tree snapshot**: seeds tree from `/proc/*/status` on daemon start.
- **Cycle-safe tree walks**: `is_descendant` / `build_chain` with visited set.
- `timefilter` dependency for time-based event filtering.

### Changed

- Eliminated `dashmap` dependency entirely.
- `pid_cache` (LruCache) removed, unified under `proc_cache` (moka).

### Fixed

- `is_descendant` and `build_chain` now handle cycles without infinite loops.

## [0.2.7] - 2026-05-12

### Added

- **Process tree tracking**: `--cmd` flag for process ancestry chain in events.
- **Cmd-based log files**: each cmd group writes to its own `{cmd}_log.jsonl`.
- **`fsmon remove` enhancements**: remove entire cmd group, atomic multi-path removal.
- **Event routing**: (path, cmd) pair as unique entry identifier; same path under multiple
  cmd groups with different filters.
- **Debug mode**: `daemon --debug` with event routing traces.
- **Integration tests**: full add/remove/query/clean coverage for CLI (no sudo).

### Changed

- **Monitored store**: migrated from flat list to cmd-grouped JSONL structure.
- **Log filenames**: `{hash}_log.jsonl` → `{cmd}_log.jsonl`.
- **Query**: `<CMD>` positional argument required, `--path` filters by event path.
- **Clean**: `<CMD>` positional required, aligned with cmd-group model.
- **Add/Remove**: cmd is now the first positional argument.
- **Null group**: renamed to `_global` internally and in log filenames.
- `ppid` / `tgid` fields added to `FileEvent`.
- `sizefilter` extracted to separate crate for size parsing/filtering.

### Removed

- `--cmd` inverted mode (`!` prefix).
- All exclude-path / exclude-cmd functionality (superseded by cmd groups).

## [0.2.6] - 2026-05-11

### Changed

- Replaced threaded proc connector polling with async event-driven model using
  `tokio::signal::unix` + `AsyncFd`.

### Fixed

- Drain proc connector events before processing fanotify batch to minimize cache miss window.
- Handle `Truncated` error from proc connector gracefully instead of exiting.

## [0.2.5] - 2026-05-11

### Added

- `fsmon p2l` (path-to-log) and `fsmon log-path` commands.
- `temp-env` dev-dependency for safe env var manipulation in tests.

### Changed

- Replaced raw netlink FFI with safe `proc-connector` crate.
- Migrated from `libc::read` to `fanotify_fid::read` in tests.
- Replaced unsafe env var manipulation with `temp-env` crate.
- Various unsafe code cleanups: `safe_open_dir`, `safe_dup` helpers, removed unused `AsFd`.

## [0.2.4] - 2026-05-09

### Added

- `--types all` shorthand for all 14 event types.
- `--exclude` / `--exclude-cmd` with `!` invert prefix and `|` alternation.
- `FAN_FS_ERROR` event type (14th type).
- `fsmon remove` supports multiple paths.
- Hidden `list-managed-paths` subcommand for shell completion.
- CREATE event recovery: direct handle resolution when cache misses.
- CLI parsing tests for add, query, clean, remove, types, exclude.
- `fsmon init` / `fsmon cd` subcommands for chezmoi-style directory setup.
- Completely rewritten help system.

### Changed

- **Comprehensive refactor**: `monitor.rs` split into `filters.rs` + `fid_parser.rs`;
  `lib.rs` split out `clean.rs`; binary split into `commands/` module directory.
- `--size` now uses operators (`>=1MB`, `<500KB`), replaces old `--min-size`/`--max-size`.
- `--all-events` removed; `--types` now controls kernel mask directly.
- `--format` flag removed; all output is JSONL.
- `--since`/`--until` unified into repeatable `-t` flag.
- Short flags unified to lowercase.
- `generate` subcommand renamed to `init`.
- Config field `max_size` renamed to `size`.
- Fanotify fd dedup: same-filesystem paths share a single fan_fd + mount_fd.

### Fixed

- fd leak on repeated add/remove (dedup by filesystem).
- CREATE events with unresolved paths now recover correctly.
- `FAN_FS_ERROR` stripped from inode marks to avoid EINVAL.
- Directory auto-creation and chown on log path recreation.

## [0.2.3] - 2026-05-09

### Changed

- Ported to `fanotify-fid` 0.2.0 API (`OwnedFd`, `FanotifyError`, `&[OwnedFd]`).
- Pulled `fanotify-fid` from crates.io.

### Fixed

- Reader task was creating a temporary `OwnedFd` each loop iteration, closing the fanotify fd
  every cycle. Now properly holds the fd.

## [0.2.2] - 2026-05-08

### Changed

- Replaced `fanotify-rs` with `fanotify-fid`.
- Replaced `DashMap` with `moka::sync::Cache` for dir cache (performance).

### Fixed

- `remove_path` removes inode marks from ALL fanotify fds, not just the first.
- `add` on already-managed path no longer removes and re-adds (no unnecessary mark churn).

## [0.2.1] - 2026-05-08

### Changed

- README updates and clarifications.

## [0.2.0] - 2026-05-07

### Added

- Daemon singleton via `flock` (`DaemonLock`).
- Path validation in `fsmon add` (reject overlapping with log dir).

### Changed

- **Major safety refactor**: replaced unsafe `libc` calls with safe Rust alternatives:
  - `libc::open/close` → `nix::fcntl::open` + `nix::unistd::close`
  - `getpwuid_r/sysconf/CStr` → `users::get_user_by_uid`
  - `libc::flock` → `fs2::FileExt::try_lock_exclusive`
  - `libc::chown` → `nix::unistd::chown`
  - `libc::geteuid/getegid` → `nix::unistd` safe versions
- Fanotify fd ownership RAII-ized: spawned tasks auto-close fds on drop.
- `mount_fds` `Vec<OwnedFd>` fully RAII, fixing fd leak bug.

### Fixed

- fd leak: mount_fds were not being closed properly on group teardown.

## [0.1.6] - 2026-05-04

### Added

- **JSONL migration**: log files, monitored store, and query output all use JSONL format.
- **Process connector**: netlink-based process event monitoring via `proc-connector`.
- **Process tree**: `is_descendant` + `build_chain` for ancestry tracking.
- **Multi-fd architecture**: cross-filesystem fanotify monitoring with separate fds + reader tasks.
- **Socket-based CLI-daemon protocol**: live add/remove without daemon restart.
- **Cmd-based log files**: per-process-name log segregation.
- **Config system rewrite**: `Config` struct with `[monitored]`, `[logging]`, `[socket]` sections.
- **Store system**: `Monitored` struct for persistent path database (JSONL).
- **Clean/Query improvements**: per-cmd filtering, binary search optimization, stream-based cleaning.
- **Systemd hardening**: configurable security options via CLI/config.
- **Type-safe EventType enum**: replaces string-based event types.
- **Buffer size validation**: min 4KB, max 1MB.
- `--exclude-cmd` / `--only-cmd` capture filters.
- `keep_days` / `max_size` config with CLI override chain.
- LRU capacity limit on file_size_cache.
- Recursive cache for `-r` flag.
- Generate default config command.

### Changed

- **Single-binary merge**: `fsmon` and `fsmon-cli` merged into one binary with subcommands.
- Configuration moved from `/etc/fsmon/fsmon.toml` to user path (`~/.config/fsmon/fsmon.toml`).
- Podman-style architecture: user manages daemon lifecycle, no systemd dependency.
- Log/db file extension changed to `.jsonl`.
- `--since`/`--until` → `-t` with operator syntax.
- Output format always JSONL; removed `OutputFormat` enum.
- `generate` → `init` subcommand.

### Performance

- Binary search optimization for time-range queries (avoids full scan).
- Stream-based log cleaning (constant memory, no full file load).
- `BufWriter` for log writes.
- `SmallVec` for handle keys and event type lists.
- `/etc/passwd` UID cache with `OnceLock`.
- Pre-compiled exclude regex.
- Reusable 32KB read buffer.

### Fixed

- fd leak: reader task creating temp OwnedFd each iteration.
- Cross-filesystem `fanotify_mark` EXDEV fallback.
- Handle `FAN_Q_OVERFLOW` events.
- `DELETE_SELF` on pre-existing dirs returns empty path (now resolved).
- Regex metacharacter escaping in exclude patterns.
- PID-based temp file names to avoid collision in `clean_logs`.
- `fsid` included in `HandleKey` to prevent cross-filesystem collisions.
- Drop guard for temp file cleanup in `clean_logs` on crash.
- `SIGTERM` graceful shutdown.
- `EPERM` from `fanotify_mark` handled gracefully in live-add.
- Config absence unlimited retry bug.

## [0.1.5] - 2026-05-01

### Added

- `fsmon generate`: default config template generation.
- Expanded config search paths.

## [0.1.4] - 2026-04-30

### Added

- `PROGRESS.md` for tracking implementation status.
- `fsmon.toml` configuration file support.
- Configurable systemd security hardening.
- Binary search optimization for time-range queries.
- LRU capacity limit on `file_size_cache` (prevents unbounded growth).
- RAII guards for fanotify fd and mount fds.
- Recursive cache for `-r` flag.

### Changed

- `tokio::select!` with `AsyncFd` for event loop (removed 10ms sleep).
- Extracted magic numbers as named constants.
- String event types replaced with type-safe `EventType` enum.

### Fixed

- `DELETE/DELETE_SELF` events: `size_change` was always 0, now captured via `fstat`.
- Regex metacharacter escaping in exclude patterns.
- `EINTR` handling in proc connector `recv` loop.
- `fsid` included in `HandleKey` to prevent cross-filesystem collisions.
- PID-based temp file name to avoid collision in `clean_logs`.

## [0.1.3] - 2026-04-11

### Added

- User hints when `sudo` cannot find cargo-installed `fsmon`.
- Alternative install methods in README.

### Fixed

- Version display bug.

## [0.1.2] - 2026-04-07

### Added

- Integration tests requiring sudo.
- Crates.io release preparation.

## [0.1.1] - 2026-03-31

### Added

- `AGENTS.md` for AI agent instructions.
- License link in README.

### Changed

- Replaced self-managed daemon with systemd service management.

## [0.1.0] - 2026-03-22

### Added

- Initial release of `fsmon`.
- Fanotify FID-based file change monitoring.
- Daemon mode: background monitoring with fanotify.
- CLI commands: `add`, `remove`, `query`, `clean`, `daemon`, `status`, `stop`, `start`.
- Event types: ACCESS, MODIFY, CLOSE_WRITE, OPEN, CREATE, DELETE, MOVED_FROM, MOVED_TO,
  OPEN_EXEC, ATTRIB, DELETE_SELF, MOVE_SELF, CLOSE_NOWRITE.
- Log files with time-range query and size-based cleaning.
- Exclude patterns (glob/regex).
- Recursive directory monitoring.
- Systemd service integration.
- English and Chinese READMEs.
