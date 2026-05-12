# PROGRESS.md — fsmon full review & fix plan

## Done
- ✅ Full code review (bugs, performance, improvement)
- ✅ Task 5: Include FSID in HandleKey to prevent cross-filesystem collisions (3.1)
- ✅ Task 6: Add RAII wrappers for fanotify fd and mount fds (3.2)
- ✅ Task 7: Fix `clean_logs` leaves `.tmp` file on crash (3.3)

## Todo

### 1. Bug Fixes

#### 1.1 Glob-to-regex `.` not escaped in exclude patterns
- **File**: `main.rs:102`
- **Problem**: `--exclude "*.log"` -> regex `.*.log` (`.` matches any char)
- **Severity**: High (user-facing wrong behavior)
- **Fix**: Write `glob_to_regex()` helper that converts `*` -> `.*` and escapes all other regex special chars (`.` -> `\\.` etc.), or use `regex::escape` + manual wildcard handling

#### 1.2 `size_change` always 0 for DELETE/MOVED_FROM events
- **File**: `monitor.rs:343`
- **Problem**: `fs::metadata` fails after file is deleted, returns 0
- **Severity**: Medium (inaccurate data)
- **Fix**: During `read_fid_events` parsing, when `open_by_handle_at` succeeds, read file size via `fstat(fd)`. Add `optional_size: Option<i64>` to `FidEvent`. In `build_file_event`, prefer this value, fallback to `fs::metadata`.

#### 1.3 Concurrent `clean_logs` temp file collision
- **File**: `main.rs:446`
- **Problem**: All clean ops share the same `history.tmp`
- **Severity**: Medium (concurrent scenario)
- **Fix**: Use `log_file.with_extension(format!("tmp.{}", std::process::id()))` or `tempfile` crate

#### 1.4 Clippy `sort_by_key` warnings
- **File**: `query.rs:153,156,159`
- **Problem**: Can simplify `sort_by` into `sort_by_key`; use `sort_by_key(|b| Reverse(b.size_change.abs()))` for size
- **Severity**: Low (code style)

### 2. Performance Optimization

#### 2.1 `resolve_file_handle` no mount fd caching
- **File**: `monitor.rs:703-729`
- **Problem**: Traverses all mount_fds for every handle resolution
- **Severity**: Medium (multi-mount scenarios)
- **Fix**: Add `HashMap<fsid, i32>` cache mapping filesystem id to the first successful mount_fd; extract fsid from info record

#### 2.2 Query loads all matching events into memory
- **File**: `query.rs:77-148`
- **Problem**: `read_events` returns `Vec<FileEvent>` before output
- **Severity**: Low (large result sets)
- **Fix**: Change `read_events` to accept a callback `&mut dyn FnMut(&FileEvent) -> Result<()>`, stream output instead of collecting

#### 2.3 `find_tail_offset` large heap allocation
- **File**: `main.rs:525-526`
- **Problem**: `vec![0u8; file_len - read_start]` reads entire tail into memory
- **Severity**: Low (log clean is not hot path)
- **Fix**: Use `BufReader` + byte-by-byte/chunk scan for first `\n`

### 3. Architecture / Maintainability

#### 3.1 ~~HandleKey does not include FSID — cross-filesystem collision risk~~ ✅
- **File**: `monitor.rs:656,697,808`
- **Problem**: Key only uses file_handle bytes, omits fsid
- **Severity**: **High** (correctness risk)
- **Fix**: Include fsid (8 bytes) in `HandleKey`. Updated all three `HandleKey::from_slice` calls — `extract_dfid_name` and `extract_fid` now start slice at `fsid_off`; `path_to_handle_key` prepends fsid from `statfs`.

#### 3.2 ~~fanotify fd / mount fds lack RAII wrappers~~ ✅
- **File**: `monitor.rs:150-253, 326-333`
- **Problem**: Manual close; panic or early return leaks fds
- **Severity**: Medium (resource leak)
- **Fix**: Create `FanFdGuard(i32)` and `MountFdsGuard(Vec<i32>)` RAII types (follow `SockGuard` pattern in `proc_cache.rs:199`), auto-close in `Drop`. Remove manual close code.

#### 3.3 `clean_logs` leaves `.tmp` file on crash
- **File**: `main.rs:445-506`
- **Problem**: If process is killed mid-clean, `.tmp` file is left behind
- **Severity**: Medium (graceful degradation)
- **Fix**: Register cleanup at function entry via `DropGuard` or similar to ensure temp file removal. Or append `.{pid}` to path for uniqueness.

#### 3.4 systemd binary path hardcoded
- **File**: `systemd.rs:13`
- **Problem**: `ExecStart=/usr/local/bin/fsmon` hardcoded
- **Severity**: Medium (cargo install users)
- **Fix**: In `install()`, auto-detect binary path via `std::env::current_exe()` and use real path in service template

#### 3.5 proc connector thread cannot exit gracefully
- **File**: `proc_cache.rs:42-51`
- **Problem**: Thread is detached; keeps running after Monitor exits
- **Severity**: Low (harmless but inelegant)
- **Fix**: `start_proc_listener()` accepts `Arc<AtomicBool>` shutdown signal; `run_listener` checks it in loop and `break`s

### 4. Fix Order

Phase 1: Bug fixes (1.1 -> 1.2 -> 1.3 -> 1.4)
Phase 2: Architecture (3.1 -> 3.2 -> 3.3 -> 3.4)
Phase 3: Performance (2.1 -> 2.2 -> 2.3 -> 3.5)

### 5. Verification

After each fix:
```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --verbose
cargo build --release
```
