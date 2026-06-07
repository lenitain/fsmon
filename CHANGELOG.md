# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- **Benchmark suite** (`benchmark/`): complete performance and correctness test framework
  - event correctness tests (create/modify/delete/move/recursive/stress)
  - post-processing performance tests (query/clean)
  - perf collection scripts (`perf/stress.sh`, `perf/query.sh`, `perf/clean.sh`)
  - Shared config `common.sh`: reads paths from `fsmon.toml`, no hardcoded paths
  - Each script manages its own daemon lifecycle

### Changed

- **`fsmon monitored` human-readable output**: Replaced JSONL output with a structured, human-readable format.
  - Shows process groups with clear headers (`Process: <cmd>`).
  - Lists paths with details (recursive/non-recursive, event types, size filters).
  - Displays all event types when fewer than 14 are selected; shows all 14 when all are selected.
  - Example output:
    ```
    === Monitored Paths ===

    Process: nginx
      /var/www/myapp (recursive, types: MODIFY, CREATE)

    Process: _global (all processes)
      /tmp/data (recursive, types: ACCESS, MODIFY, CLOSE_WRITE, CLOSE_NOWRITE, OPEN, OPEN_EXEC, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, FS_ERROR)
    ```
- **Project structure reorganized**: Moved all implementation code from `src/` root to `src/common/` module.
  - Created minimal `src/lib.rs` (3 lines) that only re-exports `pub mod common`.
  - All `use fsmon::xxx` imports changed to `use fsmon::common::xxx`.
  - No `#[path]` hacks — clean module hierarchy.
- **Test files renamed**: Removed meaningless `p1_` prefix from integration tests.
- **CLI hint improved**: `fsmon add` and socket error messages more user-friendly.

## [0.4.9] - 2026-06-06

### Fixed

- **`proc_tree::proc::read_proc_status_fields` removed**: The published `proc-tree` crate (v0.1.1) only exports `parse_proc_entry`, not `read_proc_status_fields`. Updated `utils.rs` to use `parse_proc_entry` directly, simplifying the fallback logic.

### Changed

- **`Monitor` struct refactored**: Extracted 30+ flat fields into three cohesive sub-structs:
  - `FanotifyState`: `groups`, `path_to_group`, `dir_cache`, `shared_dir_cache`
  - `InotifyState`: `inotify`, `watches`, `pending_paths`, `temp_parent_marks`
  - `ProcessState`: `cache`, `tree`
  - Pure structural refactoring, no logic changes.
- **`live_path.rs` split**: 929 lines → three focused modules:
  - `live_path.rs` (411 lines): `add_path`, `remove_path`, `check_disk_space`
  - `dir_watcher.rs` (289 lines): inotify setup, event handling, pending paths
  - `temp_marks.rs` (163 lines): temporary parent mark lifecycle
- **`Monitor::new()` simplified**: Removed `from_config()` wrapper and `#[allow(clippy::too_many_arguments)]`. `new()` now takes `MonitorConfig` directly. Added `MonitorConfig::default_for_test()` for test convenience.
- **`FileLogWriter` file handle caching**: Added `handles: HashMap<PathBuf, BufWriter<File>>` (max 64) to avoid open+close per event. `get_or_open()` reuses existing handles, `sync_dirty_logs()` uses cached handles when available.
- **`handle_proc_events` return type**: Changed from `bool` to `()` — callers never checked the return value.
- **Proc connector drain dedup**: `proc_readable` branch now calls `drain_proc_conn()` instead of inline match loop.

## [0.4.8] - 2026-06-05

### Changed

- **Process cache refactored to use `proc-tree` crate**: Major simplification of process cache internals
  - **`proc-tree` integration**: Added `proc-tree` as dependency, replacing custom moka-based process cache implementation
  - **`proc_cache.rs` simplified**: From ~580 lines to 83 lines, now only handles proc-connector byte parsing and constants
  - **Removed wrapper functions**: Eliminated `snapshot_process_tree()`, `is_descendant()`, `build_chain()`, `CacheParams`, `new_cache_with()`, `new_pid_tree_with()` wrappers
  - **Direct trait usage**: Callers now use `proc_tree::snapshot`, `proc_tree::is_descendant`, `proc_tree::build_chain` directly
  - **Removed duplicate utilities**: Cleaned up `utils.rs` by removing `read_proc_comm`, `read_proc_status_fields`, `uid_to_username` (now in proc-tree)
  - **Type aliases removed**: `PidTree` and `ProcCache` type aliases replaced with direct `DefaultCache`/`DefaultTree` imports
  - **moka retained for dir_cache**: `moka` still used for directory cache (separate concern from process cache)
  - Net result: -543 lines removed, +100 lines added. All 242 tests pass.

## [0.4.7] - 2026-06-05

### Fixed

- **Watchdog liveness detection**: Moved heartbeat from separate tokio task into the main event loop (`tokio::select!`). Previously the watchdog was a standalone task that kept sending `WATCHDOG=1` regardless of whether the main loop was responsive. If a synchronous operation blocked the main loop (e.g., `fs::metadata` on NFS), systemd couldn't detect the hang because heartbeats continued. Now the heartbeat is a tick branch in `select!` — if the main loop blocks, the tick can't be polled, heartbeats stop, and systemd restarts the service.
  - `watchdog.rs`: Removed `start()` task spawn, added `send_heartbeat()` method
  - `monitor/mod.rs`: Added `heartbeat_tick` branch to main `select!` loop

### Changed

- **Docs**: Updated README file trees to reflect current source structure (added `watchdog.rs`, `cli.rs`, `config/` directory, `monitor/init.rs`, `monitor/tests.rs`, `bin/tests/`)

## [0.4.6] - 2026-06-05

### Changed

- **Code quality refactoring**: Comprehensive cleanup based on thermal-nuclear code review
  - **TimeFilter methods**: Extracted `matches()`, `is_lower_bound()`, `is_upper_bound()` methods, eliminating 30+ lines of duplicated match blocks across `query.rs` and `clean.rs`
  - **PID status reading dedup**: Removed duplicate `read_proc_info` in `proc_cache.rs`, now reuses `utils::read_proc_status_fields`
  - **`query.rs` split**: 1078 lines → `query/core.rs` (300 lines) + `query/tests.rs` (750 lines)
  - **`clean.rs` split**: 1014 lines → `clean/core.rs` (230 lines) + `clean/tests.rs` (780 lines)
  - **PathEntry → PathOptions conversion unified**: Added `TryFrom<&PathEntry> for PathOptions` impl, eliminating 4 duplicated conversion blocks
  - **chown logic unified**: `chown_to_original_user` now delegates to `chown_to_user` for single source of truth
  - **Unused code cleanup**: Removed dead imports and unused skeleton modules
- **Structural refactoring**: Major code quality improvements for maintainability and readability
  - **Config module**: Moved from flat `config.rs` to `config/` directory structure for consistency
  - **Monitor event loop**: Extracted `run()` into focused helper methods (`matches_process_tree()`, `handle_canonical_root_deleted()`, etc.)
  - **FsGroup storage**: Replaced `Vec<FsGroup>` with `SlotMap` to eliminate index fixup logic and improve safety
  - **Helper extraction**: Added `path_matches()` and `collect_matching_entries()` helpers to reduce code duplication
  - **Debug logging**: Extracted `debug_log!` macro, replacing 31 debug sites with consistent macro calls
  - **MonitorConfig**: Inlined `MonitorConfig` struct, made `new()` private for better encapsulation
  - **Test organization**: Unified test structure by inlining all unit tests and moving CLI tests to `tests/cli_tests.rs`

### Fixed

- **Singleton lock**: Replaced `flock` with Unix socket for daemon singleton lock to improve reliability
- **Lock file permissions**: Explicitly set `chmod 666` after creating lock file to prevent permission issues
- **Watchdog tests**: Handled `Permission denied` in watchdog validation tests for proper CI execution
- **Daemon restart**: Removed `chown` on daemon lock file to prevent permission denied errors on restart
- **Clippy warnings**: Resolved `module_inception` warning to pass CI checks
- **CLI socket communication (two bugs)**:
  1. **Half-close**: `send_cmd()` did not shut down the write end after sending the command. The server's `read_line` loop blocked waiting for more data, never got EOF, and never sent a response. Added `writer.shutdown(Shutdown::Write)` after flush.
  2. **Response parsing**: Client parsed response as `SocketResponse` but server sends `Result<SocketResponse, SocketError>` (with `Ok`/`Err` wrapper). Client now parses the `Result` type directly.
  - Affected commands: `fsmon health`, `fsmon add` (daemon notification), `fsmon remove` (daemon notification), and all CLI→daemon socket commands.

### Chore

- **Cleanup**: Removed temporary plan/todo files and unused skeleton modules
- **Formatting**: Applied `cargo fmt` across codebase for consistent style
- **Dead code**: Removed unused `debug_log` method and macro

### Test

- All 387 existing tests pass with zero warnings
- No behavioral changes — pure refactoring

## [0.4.5] - 2026-06-04

### Added

- **systemd watchdog integration**: Periodic `WATCHDOG=1` notifications to prevent systemd from restarting the service
  - `--watchdog-interval SECS`: Heartbeat interval in seconds (default: disabled)
  - `--watchdog-multiplier N`: Timeout multiplier (default: 2), `WatchdogSec = interval × multiplier`
  - Config file: `[watchdog]` section with `interval_secs` and `multiplier`
  - Daemon refuses to start if multiplier ≤ 1
  - Tests: 16 unit tests, 10 integration tests
- **Metrics improvements**: All counters now available in `--metrics-interval` output
  - New counters: subscribers, monitored_paths, pending_paths, disk_buffer_events, events by type/cmd
  - Zero overhead when `--metrics-interval` is disabled
  - 37 metrics unit tests added

### Changed

- **Unified systemd integration**: All sd_notify calls now use libsystemd
  - Removed hand-written socket code from `file_writer.rs`
  - Use `libsystemd::daemon::notify` for READY=1 and WATCHDOG=1 signals
  - Added `sd_notify` helper function for consistent usage
- **Default config file**: All parameters documented with strict annotations
  - Watchdog multiplier MUST be > 1 warning
  - Clear section separators and CLI flag references for every option
- **`DaemonOptions` struct**: Grouped daemon command parameters to satisfy clippy

### Fixed

- **Lock file ownership**: Lock file chowned to original user when daemon runs as root
- **Doc-code mismatches**: Corrected README, README.zh-CN, and socket.rs documentation
- **Clippy warnings**: Removed needless raw string hashes, fixed stale doc comments
- **Multiplier validation**: Daemon rejects multiplier ≤ 1 regardless of watchdog enabled state

### Test

- Watchdog configuration tests (`p1_watchdog.rs`): 16 tests for config parsing, CLI args, merge priority
- Watchdog validation tests (`p1_watchdog_validation.rs`): 10 tests for daemon startup rejection
- Metrics unit tests: 37 tests covering all counter operations

## [0.4.4] - 2026-06-04

### Changed

- **Socket protocol upgraded from TOML to JSON**: Type-safe Socket protocol refactoring for improved maintainability and type safety
  - `SocketCmd` changed from string command to enum type, supporting `Add`, `Remove`, `Health`, `Subscribe`, `Metrics` commands
  - `SocketResp` struct changed to `SocketResponse` enum, supporting `Ok`, `Health`, `Error`, `Events` response types
  - `ErrorKind` changed to `SocketError` enum (Permanent/Transient)
  - Protocol format upgraded from TOML to JSON, using `serde_json` instead of `toml`
  - Updated daemon-side `socket_handler.rs` code to support new enum types
  - Updated CLI-side `add.rs`, `remove.rs`, `health.rs` code to use new type-safe API
  - Updated all test cases
- **Extensions examples updated**: Adapted to new JSON protocol format
  - `subscribe.sh`: Send JSON commands
  - `subscribe.py`: Send JSON commands, updated response checks
  - `README.md`: Updated protocol description

### Fixed

- **Improved error handling**: Replaced `unwrap()` with proper error propagation in `AsyncPolyfill::await_()`
- **Code style**: Merged nested or-patterns in `fid_parser.rs` for better readability

### Documentation

- **Added protocol semantic documentation comments**: Improved Socket protocol documentation

## [0.4.3] - 2026-06-04

### Changed

- **Upgraded `nix` from 0.29 to 0.31**: leverages new `OwnedFd`-returning API for
  `dup()` and `open()`, eliminating 2 `unsafe` blocks in `reader.rs`.
- **Wrapped `libc::sysconf` unsafe call** in `clock_ticks_per_sec()` helper
  function (`proc_cache.rs`). Contains the only remaining `unsafe` in a single
  well-documented function with a fallback default of 100 (common Linux value).

## [0.4.2] - 2026-06-01

### Added

- **Independent integration test suite** (`tests/`) with shared `tests/common/` harness
  - `p1_cli.rs` — 22 CLI end-to-end tests (add/monitored/remove/query/changes/clean)
  - `p1_monitor.rs` — 8 event parsing, serialization, and EventType completeness tests
  - `p1_crash_recovery.rs` — 12 crash recovery tests (DaemonLock, atomic writes, config resilience, log truncation)
  - `p1_utils.rs` — 16 utility function tests (parse_size, parse_size_filter, parse_time_filter)
  - `tests/README.md` — test index with run commands and layering guide
- **GitHub Actions CI workflow** (`.github/workflows/ci.yml`)
  - Build + unit tests + integration tests on every push/PR
  - `cargo fmt --check` and `cargo clippy -- -D warnings`
- **GitHub Actions Benchmark workflow** (`.github/workflows/bench.yml`)
  - Release build + binary size measurement + smoke test
- **`--metrics` daemon flag**: periodic one-line status report to stderr
  - Reports uptime, RSS (MB), cache sizes (dir/proc/pid-tree/file-size), and reader task health (total/alive/gave-up)
  - Independent of `--debug`; designed for production monitoring via grep/awk
- **`p1_metrics.rs`** — 3 output format validation tests

### Changed

- **Cache stats moved from `--debug` to `--metrics`**
  - `--debug` now only controls event-level tracing (matching, routing, reader lifecycle)
  - `--cache-stats-interval` and `--debug` no longer required for periodic cache reporting
- `cargo fmt` applied to all source files for consistent formatting

## [0.4.1] - 2026-05-30

### Added

- **Minimal extension examples**: 4 scripts covering the 2 data exit points —
  `read-jsonl.{sh,py}` for persistent JSONL files and `subscribe.{sh,py}` for
  real-time Unix socket streaming. Each exit point has symmetric Shell and Python
  implementations under `examples/`.

### Changed

- **Extensions reorganized**: removed all downstream bridge scripts (Kafka
  producer, Elasticsearch bulk indexer, InfluxDB line protocol, S3 archiver)
  and the TCP /metrics HTTP listener. Extensions consolidated from 4
  subdirectories into a single `examples/` directory with 4 minimal scripts.
  Simplifies the project surface to its core: JSONL file output and Unix socket
  subscribe stream.
- **`extensions/README.md`**: simplified to English-only, documenting the 2
  data exit points with minimal examples.
- **clippy clean**: fixed all clippy warnings across all source files
  (`collapsible_if`, `new_without_default`, `redundant_closure`, `for_kv_map`,
  `unnecessary_sort_by`, `let_and_return`, `useless_vec`). Zero warnings on
  `cargo clippy --all-targets`.

### Fixed

- **subscribe.py buffering bug**: the extension example `subscribe.py` mixed
  `sock.recv(1)` with `sock.makefile("r")`, which caused the buffered reader to
  miss JSONL events that arrived after the TOML handshake. Replaced with unified
  `sock.makefile("rb")` for both response parsing and event streaming.
- **query/changes ignore `local_time` config**: `fsmon query` and `fsmon changes`
  always output UTC timestamps regardless of the `logging.local_time` setting.
  Fixed by threading the `local_time` flag through `Query::new()` and using
  `to_jsonl_string_local()` when enabled. Now query, changes, log files, and
  subscribe streams all consistently respect the configured timezone.
- **monitored.jsonl non-atomic save**: `Monitored::save()` used `File::create`
  which truncates the file before writing, leaving a corrupted store on crash,
  power loss, or `kill -9`. Replaced with temp-file + `sync_all` + `rename` —
  POSIX guarantees atomic rename, so the original file stays intact until the
  new content is fully written and synced.
- **Extension script bugfixes**: corrected subscribe protocol handshake in both
  Shell (`subscribe.sh`) and Python (`subscribe.py`) examples; fixed
  `Elasticsearch()` constructor exception on ES 9.x with try/except wrapper;
  fixed `s3 client` hang with `connect_timeout` when no AWS credentials present;
  fixed `admin.py --socket` parameter being silently ignored; fixed
  `fsmon-log-tail.py` default log path from `/var/log/fsmon` to
  `~/.local/state/fsmon`.
- **Subscribe protocol**: runtime validation fixes for subscribe command payload
  and TOML header parsing.

### Removed

- **TCP /metrics HTTP listener**: `--metrics-listen` CLI flag and `[metrics]`
  config section removed. The socket `cmd="metrics"` command still returns
  Prometheus text format — just not via a separate TCP port. This simplifies
  the daemon's network surface.
- **All downstream bridge extensions**: Kafka, Elasticsearch, InfluxDB, and S3
  bridge scripts removed. These were downstream-specific and better maintained
  separately. The 2 data exits (JSONL files + Unix socket subscribe) are
  universal and tool-agnostic.
- **`regex` dependency**: removed unused crate from `Cargo.toml`.
- **Dead code**: removed `EXIT_CONFIG` constant, `format_prometheus` function,
  `new_cache`/`new_pid_tree` methods, `CounterVec::name`/`help` and
  `IntGauge::name`/`help` fields, `socket::listen`, `PathParams::new`, unused
  field, redundant re-export, and unnecessary thin wrappers.

## [0.4.0] - 2026-05-29

### Added

- **`fsmon cd` flags**: `-m`/`--monitored` and `-l`/`--logging` flags to
  cd directly to the monitored store or logging directory. `-m` auto-creates
  the monitored directory and an empty store on first use.
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
- **Monitored directory deletion recovery**: When a monitored directory is
  deleted (`rm -rf`) and later recreated (`mkdir`), the daemon now detects
  both transitions and re-establishes monitoring on the new inode.
  inotify `DELETE_SELF` is the primary trigger (fanotify `FAN_DELETE_SELF`
  is unreliable in FID mode). After deletion, a temporary fanotify inode
  mark is placed on the nearest existing ancestor directory to capture events
  during the recreate window. `check_pending()` retries path re-monitoring
  when the directory reappears. Steady-state overhead is zero (fast-path
  returns immediately when nothing is pending).
- **Recursive monitoring post-startup gap**: new subdirectories created under
  recursively-monitored paths after daemon startup are now detected via
  inotify and automatically get fanotify inode marks. Previously only
  subdirectories existing at startup were monitored.
- **`rm -rf` recursive delete event preservation**: child file events are no
  longer lost during recursive directory removal. Handle propagation across
  event batches resolves paths from sibling subdirectory deletions.
- **`(deleted)` suffix stripping**: paths resolved via `resolve_file_handle`
  with trailing ` (deleted)` marker now have the suffix stripped from *all*
  path components, fixing display for deeply nested deleted paths.
- **Fanotify edge-triggered epoll draining**: reader tasks drain all queued
  events per edge notification using `retain_ready()`, preventing event loss
  when multiple events arrive between epoll wakeups.
- **`remove_path` PathOptions ordering**: `PathOptions` are now saved *before*
  the `retain` call in `remove_path`, ensuring correct fanotify mark teardown
  even when the same path has entries across multiple cmd groups.
- **`fsmon add/remove` path resolution**: relative paths now resolve to
  absolute before storage in the monitored database.
- **`fsmon cd -m`**: always goes to the store directory root, not the first
  monitored path. Auto-creates directory + empty store on first use.
- **`check_pending` fast path**: when no paths are pending and no temporary
  marks exist, the function returns immediately (2 `is_empty()` checks)
  instead of re-adding all inotify watches (N syscalls).

### Removed

- **Watchdog filesystem mark**: removed the lightweight `FAN_DELETE |
  FAN_MOVED_FROM` filesystem mark that was added alongside every inode mark
  as a fallback for deletion detection. The inotify `DELETE_SELF` mechanism
  is now the sole and reliable primary detector. This simplifies `add_mark`
  from ~80 lines of directory-tree-walking fs mark fallback logic to ~20
  lines of inode mark only, and removes the `FsGroup.is_fs_mark` field and
  all associated branching in 7 call sites.

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
