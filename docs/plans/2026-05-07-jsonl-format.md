# JSONL Format Migration Implementation Plan

> **For agentic workers:** Use `/skill:subagent-driven-development` (recommended) or `/skill:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Change event logs and store from multi-line TOML to single-line JSONL. Keep `~/.config/fsmon/config.toml` as multi-line TOML.

**Architecture:** Three-tier format strategy: config stays TOML (human-editable), store and event logs use JSONL (program-friendly, pipeable). `FileEvent` uses existing `serde` derives for JSON serialization. Query output supports both TOML (default, human-readable) and JSONL (`--format jsonl`, pipeable).

**Tech Stack:** serde_json (new dependency), serde (existing), chrono (existing serde feature enabled)

---

### Task 1: Add serde_json dependency + change utils path_to_log_name

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/utils.rs`
- Modify: `src/config.rs`

- [ ] **Step 1: Add serde_json to Cargo.toml**

```toml
# In [dependencies] section, after "serde":
serde_json = "1.0"
```

- [ ] **Step 2: Change path_to_log_name to .jsonl extension**

In `src/utils.rs`, change:
```rust
format!("{:016x}.toml", hash)
```
to:
```rust
format!("{:016x}.jsonl", hash)
```

Update doc comment to reflect `.jsonl`.

Fix tests: change `.toml` assertions to `.jsonl`.

```
src/utils.rs: path_to_log_name returns {:016x}.jsonl
src/utils.rs: test_path_to_log_name asserts .jsonl suffix and len 16+5
src/utils.rs: test_path_to_log_name_deep_path same
src/utils.rs: test_path_to_log_name_special_chars same
```

- [ ] **Step 3: Change default store path in config.rs**

In `src/config.rs` (Default impl), change:
```rust
file: PathBuf::from("~/.local/share/fsmon/store.toml"),
```
to:
```rust
file: PathBuf::from("~/.local/share/fsmon/store.jsonl"),
```

- [ ] **Step 4: Build + test**

Run: `cargo build 2>&1`
Run: `cargo test test_path_to_log_name 2>&1`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/utils.rs src/config.rs
git commit -m "feat: add serde_json, change log/store extension to .jsonl"
```

---

### Task 2: Change store.rs save/load to JSONL

**Files:**
- Modify: `src/store.rs`

Current store format (TOML with `[[entries]]` array):
```toml
[[entries]]
path = "/tmp"
recursive = true
types = ["MODIFY", "CREATE"]
```

New format (JSONL, one entry per line):
```jsonl
{"path":"/tmp","recursive":true,"types":["MODIFY","CREATE"],"min_size":null,"exclude":null,"all_events":null}
{"path":"/etc","recursive":false}
```

- [ ] **Step 1: Change Store::save to write JSONL**

Replace `toml::to_string_pretty(self)` with line-by-line `serde_json::to_string(&entry)` for each entry.

```rust
pub fn save(&self, path: &Path) -> Result<()> {
    let parent = path.parent().context("Store path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    let mut file = fs::File::create(path)
        .with_context(|| format!("Failed to create store {}", path.display()))?;
    for entry in &self.entries {
        let line = serde_json::to_string(entry)
            .context("Failed to serialize store entry")?;
        writeln!(file, "{}", line)
            .with_context(|| format!("Failed to write store entry"))?;
    }
    Ok(())
}
```

- [ ] **Step 2: Change Store::load to read JSONL**

Replace `toml::from_str` with line-by-line `serde_json::from_str`.

```rust
pub fn load(path: &Path) -> Result<Self> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read store {}", path.display()))?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: PathEntry = serde_json::from_str(line)
            .with_context(|| format!("Invalid JSON in store: {}", line))?;
        entries.push(entry);
    }
    let mut store = Store { entries };
    store.validate();
    Ok(store)
}
```

- [ ] **Step 3: Fix tests**

The tests use `dir.join("store.toml")` — change to `dir.join("store.jsonl")`.
Also fix test_save_and_reload test that asserts TOML format → now asserts JSONL.

In `test_old_format_backward_compat` test: the old TOML format test should be removed or updated since we don't support backward compat with TOML store. Change it to test JSONL format with empty/null fields.

- [ ] **Step 4: Build + test**

Run: `cargo test --lib store 2>&1`
Expected: all store tests pass

- [ ] **Step 5: Commit**

```bash
git add src/store.rs
git commit -m "feat: store uses JSONL format (one entry per line)"
```

---

### Task 3: Add JSONL serialization to FileEvent + OutputFormat::Jsonl

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Add import for serde_json**

```rust
// Top of lib.rs
use serde_json;
```

No — `serde_json` is external crate, just use it directly.

Actually, for `FileEvent::to_jsonl_string()` and `from_jsonl_str()`, we can use serde's Serialize/Deserialize derives. Since `FileEvent` already derives `Serialize, Deserialize`, we just call `serde_json::to_string(&event)`.

But wait — `DateTime<Utc>` with `chrono/serde` feature serializes as `{"time":"2024-01-01T10:00:00+00:00"}`. Let me verify this is RFC3339 format (same as current TOML's `to_rfc3339()`). Yes, chrono's serde uses RFC3339.

Similarly `EventType` with `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` serializes as `"MODIFY"` etc.

- [ ] **Step 2: Add `to_jsonl_string` and `from_jsonl_str` methods**

```rust
impl FileEvent {
    /// Serialize to a single JSON line (for log storage / pipe output)
    pub fn to_jsonl_string(&self) -> String {
        serde_json::to_string(self).expect("FileEvent serialization should not fail")
    }

    /// Deserialize from a single JSON line
    pub fn from_jsonl_str(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}
```

- [ ] **Step 3: Add OutputFormat::Jsonl variant**

```rust
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Toml,
    Jsonl,
}
```

- [ ] **Step 4: Add parse_log_line_jsonl for JSONL log file reading**

```rust
/// Parse a JSONL line into a FileEvent.
pub fn parse_log_line_jsonl(line: &str) -> Option<FileEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    FileEvent::from_jsonl_str(trimmed)
}
```

- [ ] **Step 5: Update clean_single_log for JSONL**

Change `clean_single_log` to read line by line instead of multi-line TOML blocks:

```rust
async fn clean_single_log(
    log_file: &Path,
    keep_days: u32,
    max_size: Option<i64>,
    dry_run: bool,
) -> Result<()> {
    // ... same preamble ...
    let temp_file = log_file.with_extension("tmp");
    let mut time_deleted = 0;
    let mut kept_bytes: usize = 0;

    {
        let file = fs::File::open(log_file)?;
        let reader = BufReader::new(file);
        let writer = fs::File::create(&temp_file)?;
        let mut writer = BufWriter::new(writer);
        let cutoff_time = ...;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let should_keep = if let Some(event) = parse_log_line_jsonl(&line) {
                event.time >= cutoff_time
            } else {
                true
            };

            if should_keep {
                writeln!(writer, "{}", line)?;
                kept_bytes += line.len() + 1; // +1 for newline
            } else {
                time_deleted += 1;
            }
        }
    }
    // ... same postamble (truncate from start, rename) ...
}
```

Remove `read_toml_block` function (no longer used after this change).
Remove `TOML_SEPARATOR` constant or keep it if still used? Check — it might be used elsewhere. Let's keep it for query TOML output.

Wait — `read_toml_block` is also used in clean. If clean moves to line-by-line, we remove it. But let me check if it's used elsewhere.

- [ ] **Step 6: Build + test**

Run: `cargo build 2>&1`
Run: `cargo test --lib 2>&1`
Expected: compiles, tests pass

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs
git commit -m "feat: add JSONL serialization for FileEvent, OutputFormat::Jsonl, line-based clean"
```

---

### Task 4: Change monitor write_event to JSONL

**Files:**
- Modify: `src/monitor.rs`

- [ ] **Step 1: Change write_event to write one JSON line**

In `src/monitor.rs`, find `write_event` method (~line 1331).

Change the writeln call from:
```rust
writeln!(file, "{}", event.to_toml_string())?;
writeln!(file)?; // blank line separator
```
to:
```rust
writeln!(file, "{}", event.to_jsonl_string())?;
```

Remove the blank line separator comment/writeln.

- [ ] **Step 2: Build + test**

Run: `cargo build 2>&1`
Run: `cargo test --lib monitor 2>&1`
Expected: compiles, tests pass

- [ ] **Step 3: Commit**

```bash
git add src/monitor.rs
git commit -m "feat: write events as JSONL lines in log files"
```

---

### Task 5: Change query to read JSONL + output JSONL

**Files:**
- Modify: `src/query.rs`

- [ ] **Step 1: Replace block-based reading with line-based reading**

Change `read_next_block` → `read_next_line`:
```rust
/// Read the next non-empty line from the reader.
fn read_next_line(reader: &mut BufReader<File>) -> Result<Option<String>> {
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
        // Skip empty lines
    }
}
```

Update `read_events_from`:
- Change block reading loop to line-by-line
- Parse with `parse_log_line_jsonl` instead of `parse_log_line`
- Update `seek_and_parse_time` to work with JSONL (read one line at a time instead of multi-line blocks)

- [ ] **Step 2: Update binary search for JSONL**

`seek_and_parse_time`: currently reads blank-line-separated TOML blocks. Change to read single JSONL lines.

```rust
fn seek_and_parse_time(
    &self,
    reader: &mut BufReader<File>,
    offset: u64,
) -> Option<(DateTime<Utc>, u64)> {
    let scan_back = SCAN_BACK_BYTES;
    let read_start = offset.saturating_sub(scan_back);

    reader.seek(SeekFrom::Start(read_start)).ok()?;

    // Skip to the start of a line (after a newline)
    if read_start > 0 {
        // Scan forward to next newline
        let mut byte = [0u8; 1];
        loop {
            if reader.read_exact(&mut byte).is_err() {
                return None;
            }
            if byte[0] == b'\n' {
                break;
            }
        }
    }

    // Read one complete line
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let event: FileEvent = serde_json::from_str(trimmed).ok()?;
    Some((event.time, offset))
}
```

- [ ] **Step 3: Add JSONL output support**

Update `output_events` to handle `OutputFormat::Jsonl`:

```rust
fn output_events(&self, events: &[FileEvent]) -> Result<()> {
    if events.is_empty() {
        println!("No matching events found");
        return Ok(());
    }

    match self.format {
        OutputFormat::Toml => {
            for event in events {
                print!("{}", event.to_toml_string());
                println!(); // blank line separator
            }
        }
        OutputFormat::Jsonl => {
            for event in events {
                println!("{}", event.to_jsonl_string());
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Build + test**

Run: `cargo build 2>&1`
Run: `cargo test --lib query 2>&1`
Expected: compiles, tests pass

- [ ] **Step 5: Commit**

```bash
git add src/query.rs
git commit -m "feat: query reads JSONL logs, supports --format jsonl output"
```

---

### Task 6: Add --format jsonl to query CLI

**Files:**
- Modify: `src/bin/fsmon.rs`

- [ ] **Step 1: Add format arg to QueryArgs**

```rust
#[derive(Parser)]
struct QueryArgs {
    // ... existing fields ...
    #[arg(short = 'F', long, value_enum)]
    format: Option<OutputFormat>,
}
```

- [ ] **Step 2: Update cmd_query to pass format**

Find `cmd_query` and change:
```rust
let sort = args.sort.unwrap_or(SortBy::Time);

let query = Query::new(
    cfg.logging.dir,
    paths,
    args.since,
    args.until,
    pids,
    args.cmd,
    users,
    event_types,
    min_size_bytes,
    OutputFormat::Toml,   // <-- change this
    sort,
);
```

To use `args.format.unwrap_or(OutputFormat::Toml)`.

- [ ] **Step 3: Build + test**

Run: `cargo build 2>&1`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add src/bin/fsmon.rs
git commit -m "feat: add -F/--format jsonl option to query command"
```

---

### Task 7: Update PROGRESS.md + cleanup

**Files:**
- Modify: `PROGRESS.md`

- [ ] **Step 1: Update PROGRESS.md with implementation status**

Add an entry about the JSONL migration being complete.

- [ ] **Step 2: Remove unused code**

Check if `read_toml_block`, `TOML_SEPARATOR`, `parse_log_line` (the TOML version) are still used anywhere. If only used for query TOML output (not log reading), keep them.

- [ ] **Step 3: Final build + full test suite**

Run: `cargo build 2>&1`
Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1`
Run: `cargo test --verbose 2>&1`

- [ ] **Step 4: Commit**

```bash
git add PROGRESS.md
git commit -m "docs: update PROGRESS.md with JSONL migration status"
```
