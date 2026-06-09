# fsmon Test Suite

## Test Files

- `cli.rs` — CLI end-to-end tests (add, monitored, remove, query, changes, clean)
- `monitor.rs` — Event parsing, serialization, and EventType completeness
- `crash_recovery.rs` — Crash recovery and fault tolerance tests
- `utils.rs` — Utility function tests (parse_size, parse_size_filter, parse_time_filter)
- `watchdog.rs` — Watchdog configuration tests
- `watchdog_validation.rs` — Watchdog CLI argument validation
- `metrics.rs` — Metrics registry tests

## Running Tests

```bash
# All tests
cargo test

# Integration tests only
cargo test --test '*'

# A single test
cargo test --test cli add_global_with_path
```

## Test Layers

| Layer | Content | Location |
|-------|---------|----------|
| Unit tests | Module internals (monitor, events, filtering, fid_parser) | `src/**/*.rs` (`#[cfg(test)]`) |
| CLI parse tests | AddArgs, QueryArgs, etc. | `src/bin/fsmon/main.rs` (`#[cfg(test)]`) |
| Integration tests | End-to-end CLI, crash recovery, utilities | `tests/*.rs` |
