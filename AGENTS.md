# AGENTS.md — fsmon

## Project Overview

`fsmon` is a Rust CLI tool for real-time Linux filesystem change monitoring with process attribution, built on fanotify (FID mode). Edition 2024. Single binary, requires `sudo` for monitoring operations.

## Build / Lint / Test Commands

```bash
# Build
cargo build --verbose
cargo build --release          # optimized (LTO + strip)

# Lint
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt -- --check           # check formatting (no rustfmt.toml, use defaults)

# Test
cargo test --verbose           # run all tests
cargo test test_parse_size     # run a single test by name
cargo test -- --nocapture      # show stdout from tests

# Run
sudo ./target/debug/fsmon monitor /tmp
cargo run -- monitor /tmp      # alternative (still needs sudo)
```

## Code Architecture

```
src/
  main.rs       — CLI entry point, clap derive args, FileEvent struct, log cleaning
  monitor.rs    — Core fanotify monitoring loop, FID event parsing, directory handle caching
  daemon.rs     — Daemon lifecycle (PID file, status, stop)
  query.rs      — Log file querying with filters and sorting
  proc_cache.rs — Netlink proc connector listener for short-lived process attribution
  utils.rs      — Size/time parsing, process info helpers, uid lookup
```

## Code Style Guidelines

### Imports
- Group order: `std::*`, external crates, then `crate::*` — each group separated by a blank line.
- Import specific items; avoid glob imports (`use std::io::*`).
- Prefer `use anyhow::{Context, Result}` over `use anyhow::*`.

### Naming
- `snake_case` for functions, methods, variables, modules.
- `PascalCase` for types, structs, enums, traits.
- `SCREAMING_SNAKE_CASE` for constants.
- Use descriptive names: `fan_fd`, `mount_fds`, `dir_cache`, `event_len`.

### Types & Generics
- Use `PathBuf` for owned paths, `&Path` for borrows — never `String` for paths.
- Prefer `Result<T>` (anyhow) over panics; use `bail!` for early returns, `with_context` for error chains.
- Use `Option<T>` for optional config; `.transpose()?` to flatten `Option<Result<T>>`.
- Use `SmallVec<[T; N]>` for small collections that usually fit on the stack.

### Error Handling
- Use `anyhow::Result` for application-level errors. No custom error types.
- Use `bail!("message")` for early error returns with strings.
- Use `.with_context(|| format!(...))` to add context to propagated errors.
- Use `?` operator everywhere; avoid `.unwrap()` except in tests or truly infallible cases.
- For CLI user errors: `eprintln!` + `process::exit(1)`.
- Ignore non-critical errors with `let _ = ...`.

### Formatting
- 4-space indentation. No rustfmt.toml — use `rustfmt` defaults.
- Max line length ~100 chars; break long function signatures across lines with trailing commas.
- Chain method calls on separate lines when complex (see `fanotify_init` in monitor.rs).
- Use `match` for enum dispatch; prefer `if let Some(ref x)` for single-arm Option matching.

### unsafe Code
- `unsafe` is used for libc/fanotify syscalls — this is expected and necessary.
- Always close file descriptors: manual `libc::close(fd)` or RAII guards (`SockGuard` in proc_cache.rs).
- Use `#[repr(C)]` for structs that map to kernel data structures.

### Concurrency
- Use `tokio` for async runtime (monitor loop, signal handling).
- Use `std::thread` for background listeners (proc connector).
- Share state across threads with `Arc<DashMap>` (proc_cache) or `Arc<AtomicBool>` (shutdown signal).

### Documentation
- Use `///` doc comments on public functions and non-trivial internal functions.
- Module-level `//!` doc comments for subsystem overviews (see proc_cache.rs).
- Keep comments focused on *why*, not *what*.

### Dependencies
- `anyhow` — error handling
- `clap` (derive) — CLI argument parsing
- `chrono` — timestamps
- `serde` / `serde_json` — serialization for events and daemon config
- `dashmap` — concurrent hash map for proc cache
- `fanotify-rs` — fanotify bindings
- `libc` — raw Linux syscalls
- `smallvec` — stack-allocated vectors for handle keys
- `regex` — path and cmd filtering
- `tokio` — async runtime

### Testing
- Unit tests live in `#[cfg(test)] mod tests` blocks within each source file (currently only `utils.rs`).
- No integration tests or test framework beyond `#[test]`.
- Run single test: `cargo test <test_name>`.
