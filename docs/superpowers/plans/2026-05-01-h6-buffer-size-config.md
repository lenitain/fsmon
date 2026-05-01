# H6 Fanotify Buffer Size Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make fanotify read buffer size configurable to optimize memory usage and performance for different workloads.

**Architecture:** Add `buffer_size` field to `MonitorConfig` with a sensible default (32KB). Allow users to configure via `fsmon.toml` or CLI argument. The buffer size will be validated and used in the monitoring loop.

**Tech Stack:** Rust, serde, toml, anyhow

---

## File Structure

- `src/config.rs:13-23` - Add `buffer_size` field to `MonitorConfig`
- `src/main.rs:370-425` - Parse and pass buffer_size to Monitor
- `src/monitor.rs:50-93` - Add buffer_size field to Monitor struct and constructor
- `src/monitor.rs:277` - Use buffer_size instead of hardcoded value
- `src/monitor.rs:448-480` - Add tests for buffer size validation
- `src/config.rs:79-197` - Add tests for buffer_size config parsing

---

### Task 1: Add buffer_size to MonitorConfig

**Files:**
- Modify: `src/config.rs:13-23`

- [ ] **Step 1: Add buffer_size field to MonitorConfig**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MonitorConfig {
    pub paths: Option<Vec<PathBuf>>,
    pub min_size: Option<String>,
    pub types: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
    pub output: Option<PathBuf>,
    pub format: Option<String>,
    pub recursive: Option<bool>,
    pub buffer_size: Option<usize>,
}
```

- [ ] **Step 2: Add test for buffer_size config parsing**

Add to `src/config.rs` tests:

```rust
#[test]
fn test_config_load_buffer_size() {
    let dir = std::env::temp_dir().join("fsmon_test_config_buffer_size");
    fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");

    let toml_content = r#"
[monitor]
buffer_size = 65536
"#;

    let mut file = fs::File::create(&config_path).unwrap();
    file.write_all(toml_content.as_bytes()).unwrap();

    let config = Config::load_from_path(&config_path).unwrap();
    let monitor = config.monitor.unwrap();
    assert_eq!(monitor.buffer_size.unwrap(), 65536);

    let _ = fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test test_config_load_buffer_size -- --nocapture`
Expected: FAIL with "no field `buffer_size` on type `MonitorConfig`"

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_config_load_buffer_size -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add buffer_size field to MonitorConfig"
```

---

### Task 2: Add buffer_size to Monitor struct and constructor

**Files:**
- Modify: `src/monitor.rs:50-93`

- [ ] **Step 1: Add buffer_size field to Monitor struct**

```rust
pub struct Monitor {
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<EventType>>,
    exclude_regex: Option<regex::Regex>,
    output: Option<PathBuf>,
    format: OutputFormat,
    recursive: bool,
    all_events: bool,
    proc_cache: Option<ProcCache>,
    file_size_cache: LruCache<PathBuf, u64>,
    buffer_size: usize,
}
```

- [ ] **Step 2: Add buffer_size parameter to Monitor::new**

```rust
pub fn new(
    paths: Vec<PathBuf>,
    min_size: Option<i64>,
    event_types: Option<Vec<EventType>>,
    exclude: Option<String>,
    output: Option<PathBuf>,
    format: OutputFormat,
    recursive: bool,
    all_events: bool,
    buffer_size: Option<usize>,
) -> Self {
    let exclude_regex = exclude.map(|p| {
        let escaped = regex::escape(&p);
        let pattern = escaped.replace("\\*", ".*");
        regex::Regex::new(&pattern).expect("invalid exclude pattern")
    });
    
    let buffer_size = buffer_size.unwrap_or(4096 * 8); // Default 32KB
    
    Self {
        paths,
        min_size,
        event_types,
        exclude_regex,
        output,
        format,
        recursive,
        all_events,
        proc_cache: None,
        file_size_cache: LruCache::new(NonZeroUsize::new(FILE_SIZE_CACHE_CAP).unwrap()),
        buffer_size,
    }
}
```

- [ ] **Step 3: Run clippy to check for warnings**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Commit**

```bash
git add src/monitor.rs
git commit -m "feat(monitor): add buffer_size field to Monitor struct"
```

---

### Task 3: Update Monitor::run to use buffer_size

**Files:**
- Modify: `src/monitor.rs:277`

- [ ] **Step 1: Replace hardcoded buffer size with buffer_size field**

Change line 277 from:
```rust
let mut buf = vec![0u8; 4096 * 8]; // 32KB, reused across loop iterations
```

To:
```rust
let mut buf = vec![0u8; self.buffer_size]; // Reused across loop iterations
```

- [ ] **Step 2: Run clippy to check for warnings**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Commit**

```bash
git add src/monitor.rs
git commit -m "feat(monitor): use configurable buffer size in monitoring loop"
```

---

### Task 4: Update main.rs to pass buffer_size from config

**Files:**
- Modify: `src/main.rs:370-425`

- [ ] **Step 1: Extract buffer_size from config**

Add after line 397 (`let recursive = recursive || config.recursive.unwrap_or(false);`):

```rust
let buffer_size = config.buffer_size;
```

- [ ] **Step 2: Pass buffer_size to Monitor::new**

Change Monitor::new call to include buffer_size:

```rust
let monitor = Monitor::new(
    paths,
    min_size_bytes,
    event_types,
    exclude,
    output,
    format,
    recursive,
    all_events,
    buffer_size,
);
```

- [ ] **Step 3: Run clippy to check for warnings**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): pass buffer_size from config to Monitor"
```

---

### Task 5: Add buffer size validation

**Files:**
- Modify: `src/monitor.rs:64-93`

- [ ] **Step 1: Add validation for buffer_size**

Add validation in Monitor::new after getting buffer_size:

```rust
let buffer_size = buffer_size.unwrap_or(4096 * 8); // Default 32KB

// Validate buffer_size
if buffer_size < 4096 {
    bail!("buffer_size must be at least 4096 bytes (4KB)");
}
if buffer_size > 1024 * 1024 {
    bail!("buffer_size must not exceed 1048576 bytes (1MB)");
}
```

- [ ] **Step 2: Add test for buffer size validation**

Add to `src/monitor.rs` tests:

```rust
#[test]
fn test_monitor_buffer_size_validation() {
    use std::path::PathBuf;
    
    // Test minimum buffer size
    let result = Monitor::new(
        vec![PathBuf::from("/tmp")],
        None,
        None,
        None,
        None,
        OutputFormat::Human,
        false,
        false,
        Some(1024), // Too small
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("buffer_size must be at least 4096 bytes"));
    
    // Test maximum buffer size
    let result = Monitor::new(
        vec![PathBuf::from("/tmp")],
        None,
        None,
        None,
        None,
        OutputFormat::Human,
        false,
        false,
        Some(2 * 1024 * 1024), // Too large
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("buffer_size must not exceed 1048576 bytes"));
    
    // Test valid buffer size
    let result = Monitor::new(
        vec![PathBuf::from("/tmp")],
        None,
        None,
        None,
        None,
        OutputFormat::Human,
        false,
        false,
        Some(65536), // 64KB
    );
    assert!(result.is_ok());
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test test_monitor_buffer_size_validation -- --nocapture`
Expected: FAIL with "buffer_size must be at least 4096 bytes"

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_monitor_buffer_size_validation -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/monitor.rs
git commit -m "feat(monitor): add buffer size validation with min/max bounds"
```

---

### Task 6: Update PROGRESS.md to mark H6 as completed

**Files:**
- Modify: `PROGRESS.md:42-46`

- [ ] **Step 1: Update H6 status**

Change:
```markdown
### H6 [低] fanotify 读取缓冲区大小硬编码
`monitor.rs:259` — `4096 * 8` (32KB)
- 高频场景需要更大缓冲区减少 read 次数
- 低频场景浪费内存
```

To:
```markdown
### H6 [低] fanotify 读取缓冲区大小硬编码 ✅ 可配置缓冲区大小(已完成)
`monitor.rs:277` — `self.buffer_size` (默认 32KB)
- 高频场景可增大缓冲区减少 read 次数
- 低频场景可减小缓冲区节省内存
- 通过 `fsmon.toml` 的 `[monitor]` 段 `buffer_size` 字段配置
- 验证范围：4KB ≤ buffer_size ≤ 1MB
```

- [ ] **Step 2: Update priority table**

Change line 71:
```markdown
| P2 | 硬编码 | H6 fanotify 缓冲区大小 | 小 |
```

To:
```markdown
| P2 | 硬编码 | H6 fanotify 缓冲区大小 ✅ | 小 |
```

- [ ] **Step 3: Update current status**

Add to current status section:
```markdown
- H6 已完成：fanotify 缓冲区大小可配置
```

- [ ] **Step 4: Commit**

```bash
git add PROGRESS.md
git commit -m "docs(progress): mark H6 fanotify buffer size as completed"
```

---

### Task 7: Run all tests and verify

- [ ] **Step 1: Run all tests**

Run: `cargo test --verbose`
Expected: All tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Check formatting**

Run: `cargo fmt -- --check`
Expected: No formatting issues

- [ ] **Step 4: Final commit if needed**

```bash
git add -A
git commit -m "feat: implement H6 fanotify buffer size configuration"
```

---

## Summary

This plan implements configurable fanotify buffer size to optimize memory usage and performance for different workloads. The implementation:

1. Adds `buffer_size` field to `MonitorConfig` for TOML configuration
2. Adds validation with reasonable bounds (4KB-1MB)
3. Maintains backward compatibility with default 32KB buffer
4. Allows users to tune buffer size based on their workload characteristics

**Configuration example:**
```toml
[monitor]
buffer_size = 65536  # 64KB for high-frequency scenarios
```

**Testing:**
- Unit tests for config parsing
- Unit tests for buffer size validation
- Integration with existing test suite