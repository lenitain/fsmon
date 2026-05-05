# Config/Store/Log Redesign Implementation Plan

> **For agentic workers:** Use `/skill:subagent-driven-development` (recommended) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split current monolithic `config.rs` into `Config` (infrastructure paths) + `Store` (monitored paths database), and change logging from single file to per-entry-ID files.

**Architecture:**
- `~/.config/fsmon/config.toml` holds only `[store]`, `[logging]`, `[socket]` sections
- `~/.local/share/fsmon/store.toml` holds `next_id` + `[[entries]]` (PathEntry database)
- `~/.local/state/fsmon/<id>.log` per-entry log files
- `config.rs` → thin reader for config.toml
- `store.rs` (new) → CRUD for store.toml

**Tech Stack:** Rust, toml, serde, clap, anyhow

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `src/config.rs` | Config struct (store, logging, socket), resolve_uid/resolve_home |
| Create | `src/store.rs` | Store struct, PathEntry, add/remove/load/save |
| Modify | `src/lib.rs` | Split clean_logs into per-file utility; update exports |
| Modify | `src/bin/fsmon.rs` | All commands use new Config + Store |
| Modify | `src/monitor.rs` | Per-ID log file writing, log_dir replaces single output |
| Modify | `src/query.rs` | Accept log_dir + --id filter, read multiple files |
| Modify | `docs/specs/2026-05-05-config-store-log-redesign.md` | (already written) |

---

### Task 1: Rewrite config.rs — pure Config struct

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Read current config.rs to understand all function signatures**

- [ ] **Step 2: Replace entire config.rs with new Config struct**

Remove: PathEntry, add_path, remove_path, default_log_file, default_socket_path, generate_default (old format), save.

Keep: resolve_uid, resolve_home, guess_home.

New Config:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub store: StoreConfig,
    pub logging: LoggingConfig,
    pub socket: SocketConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketConfig {
    pub path: PathBuf,
}

impl Config {
    /// Return the config file path: `$XDG_CONFIG_HOME/fsmon/config.toml`
    pub fn path() -> PathBuf { ... } // same as old UserConfig::path()

    /// Load config from file. Returns default if file doesn't exist.
    pub fn load() -> Result<Self> { ... }

    /// Expand ~ in all paths using original user's home
    pub fn resolve_paths(&mut self) -> Result<()> { ... }

    /// Generate default config template
    pub fn generate_default() -> Result<()> { ... }
}
```

Default config content:
```toml
[store]
file = "~/.local/share/fsmon/store.toml"

[logging]
dir = "~/.local/state/fsmon"

[socket]
path = "/tmp/fsmon-<UID>.sock"
```

`resolve_paths()` replaces `<UID>` placeholder with actual UID and expands `~`.

- [ ] **Step 3: Run tests to verify compilation**

Run: `cargo build` — expected: fails because other files still reference old UserConfig

- [ ] **Step 4: Commit**

```bash
git add src/config.rs
git commit -m "refactor(config): replace UserConfig with Config struct"
```

---

### Task 2: Create store.rs — Store struct for store.toml

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs` (add `pub mod store;`)

- [ ] **Step 1: Create store.rs with Store struct**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    pub next_id: u64,
    pub entries: Vec<PathEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    pub id: u64,
    pub path: PathBuf,
    pub recursive: Option<bool>,
    pub types: Option<Vec<String>>,
    pub min_size: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
}

impl Store {
    /// Load Store from file. Returns empty Store if file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> { ... }

    /// Save Store to file (creates parent dirs).
    pub fn save(&self, path: &Path) -> Result<()> { ... }

    /// Add entry, auto-assign id. Returns the assigned id.
    pub fn add_entry(&mut self, entry: PathEntry) -> u64 { ... }

    /// Remove entry by id. Returns true if found and removed.
    pub fn remove_entry(&mut self, id: u64) -> bool { ... }

    /// Get entry by id.
    pub fn get(&self, id: u64) -> Option<&PathEntry> { ... }
}
```

- [ ] **Step 2: Add `pub mod store;` to lib.rs**

- [ ] **Step 3: Run build to verify**

Run: `cargo build`

- [ ] **Step 4: Commit**

```bash
git add src/store.rs src/lib.rs
git commit -m "feat(store): add Store struct for store.toml CRUD"
```

---

### Task 3: Update bin/fsmon.rs — daemon, add, remove, generate

**Files:**
- Modify: `src/bin/fsmon.rs`

- [ ] **Step 1: Rewrite `cmd_daemon`**

```rust
async fn cmd_daemon() -> Result<()> {
    let config_path = Config::path();
    if !config_path.exists() {
        eprintln!("Config not found at {}, generating default...", config_path.display());
        Config::generate_default()?;
    }

    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let store = Store::load(&cfg.store.file)?;

    if store.entries.is_empty() {
        eprintln!("Warning: No paths configured. Use 'fsmon add <path>'.");
    }

    // Ensure log dir exists
    fs::create_dir_all(&cfg.logging.dir)?;

    // Clean up old socket
    if cfg.socket.path.exists() {
        fs::remove_file(&cfg.socket.path)?;
    }
    if let Some(parent) = cfg.socket.path.parent() {
        fs::create_dir_all(parent)?;
    }

    let socket_listener = tokio::net::UnixListener::bind(&cfg.socket.path)
        .with_context(|| format!("Failed to bind socket at {}", cfg.socket.path.display()))?;

    set_socket_permissions(&cfg.socket.path)?;

    let paths_and_options = parse_path_entries(&store.entries)?;
    let path_ids: HashMap<_, _> = store.entries.iter().map(|e| (e.path.clone(), e.id)).collect();

    let mut monitor = Monitor::new(
        paths_and_options,
        path_ids,
        Some(cfg.logging.dir),  // was: Some(log_file)
        OutputFormat::Toml,
        None,
        None,
        Some(socket_listener),
    )?;

    monitor.run().await?;
    Ok(())
}
```

- [ ] **Step 2: Rewrite `cmd_add`**

```rust
fn cmd_add(args: AddArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let mut store = Store::load(&cfg.store.file)?;

    let id = store.add_entry(PathEntry {
        id: 0,
        path: args.path.clone(),
        recursive: if args.recursive { Some(true) } else { None },
        types: args.types.map(|t| t.split(',').map(|s| s.trim().to_string()).collect()),
        min_size: args.min_size,
        exclude: args.exclude,
        all_events: if args.all_events { Some(true) } else { None },
    });

    store.save(&cfg.store.file)?;
    println!("Path added (ID: {}): {}", id, args.path.display());

    // Try live socket update
    match socket::send_cmd(&cfg.socket.path, &SocketCmd { ... }) {
        Ok(resp) if resp.ok => println!("Daemon updated live"),
        Ok(resp) => { eprintln!("Daemon error: {}", resp.error.unwrap_or_default()); }
        Err(e) => { eprintln!("Daemon not reachable: {}", e); }
    }
    Ok(())
}
```

- [ ] **Step 3: Rewrite `cmd_remove`**

Same pattern: load config → load store → remove → save → socket notify.

- [ ] **Step 4: Rewrite `cmd_managed`**

Read config → read store → print entries. Also try socket live list first, fall back to store.

- [ ] **Step 5: Rewrite `cmd_generate`**

Replace `UserConfig::generate_default()` with `Config::generate_default()`.

- [ ] **Step 6: Remove unused imports**

Remove `use fsmon::config::UserConfig;` and other old references. Replace with `use fsmon::config::Config;` and `use fsmon::store::Store;`.

- [ ] **Step 7: Run build to verify**

Run: `cargo build`

- [ ] **Step 8: Commit**

```bash
git add src/bin/fsmon.rs
git commit -m "refactor(bin): update daemon/add/remove/managed/generate to use Config + Store"
```

---

### Task 4: Update monitor.rs — per-ID log files

**Files:**
- Modify: `src/monitor.rs`

- [ ] **Step 1: Change Monitor struct**

Replace:
```rust
output: Option<PathBuf>,
format: OutputFormat,
```
With:
```rust
log_dir: Option<PathBuf>,
```

Remove `output_file` field entirely (was `let mut output_file = ...` in run()).

Add a helper method:
```rust
fn log_path_for_entry(&self, entry_id: u64) -> Option<PathBuf> {
    self.log_dir.as_ref().map(|dir| dir.join(format!("{}.log", entry_id)))
}
```

- [ ] **Step 2: Update `run()` — replace single output_file with per-ID appends**

In the event loop, instead of `output::output_event(&event, self.format, &mut output_file)?`:

```rust
// Find entry ID for this event's path
if let Some(entry_id) = self.path_ids.iter()
    .find(|(p, _)| event.path.starts_with(*p))
    .map(|(_, id)| *id)
{
    if let Some(log_dir) = &self.log_dir {
        let log_path = log_dir.join(format!("{}.log", entry_id));
        // Open with append, create if not exists
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = output::write_event_to_file(&event, OutputFormat::Toml, &mut file);
        }
    }
}
```

Remove all the header-writing and file-locking logic (was tied to single file).

- [ ] **Step 3: Update Monitor::new() signature**

```rust
pub fn new(
    paths_and_options: Vec<(PathBuf, PathOptions)>,
    path_ids: HashMap<PathBuf, u64>,
    log_dir: Option<PathBuf>,         // was: output: Option<PathBuf>
    buffer_size: Option<usize>,
    instance_name: Option<String>,
    socket_listener: Option<tokio::net::UnixListener>,
) -> Result<Self>
```

Remove `format: OutputFormat` parameter — all log files are TOML.

- [ ] **Step 4: Add a `write_event` helper to output.rs**

In `src/output.rs`, add a function that writes one event to an open file:
```rust
use std::fs::File;
use std::io::Write;

pub fn write_event_to_file(event: &FileEvent, format: OutputFormat, file: &mut File) -> std::io::Result<()> {
    match format {
        OutputFormat::Toml => {
            write!(file, "{}", event.to_toml_string())?;
            writeln!(file)?; // blank line separator
        }
        OutputFormat::Csv => {
            writeln!(file, "{}", event.to_csv_string())?;
        }
        OutputFormat::Human => {
            // Human format not written to log files
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Update `persist_config` and `reload_config`**

These currently use `UserConfig::save`. Change to use `Store::save` with the store path stored in Monitor (add a `store_path: Option<PathBuf>` field or pass it from daemon).

Actually, simplest: keep `persist_config` but have it write to `store_path` field. Add `store_path: Option<PathBuf>` to Monitor. Set it in `cmd_daemon`.

Actually, `persist_config` is called from `handle_socket_cmd` when daemon receives add/remove via socket. The daemon needs to write back to store.toml to persist hot-added paths.

Add field `store_path: Option<PathBuf>` to Monitor, set in `Monitor::new()` or via a setter.

Alternatively, simplify: Monitor doesn't need to persist config. Socket add/remove only updates in-memory state. The CLI `fsmon add` already writes to store.toml before notifying daemon. So the daemon just receives the add/remove command and updates its fanotify marks. No need for daemon to write store.toml.

**Decision: Remove `persist_config` from monitor. Socket commands only update in-memory state.**

- [ ] **Step 6: Update `reload_config`**

Change `UserConfig::load()` → `Config::load()` + `Store::load()`.

- [ ] **Step 7: Remove `format` field from Monitor and its usage in `should_output`**

`should_output` doesn't use `format` — it's fine. Just remove the field.

- [ ] **Step 8: Run build to verify**

Run: `cargo build`

- [ ] **Step 9: Run tests**

Run: `cargo test --lib monitor` — verify unit tests still pass

- [ ] **Step 10: Commit**

```bash
git add src/monitor.rs src/output.rs
git commit -m "refactor(monitor): replace single log file with per-ID log files"
```

---

### Task 5: Update query.rs — --id filter, multi-file support

**Files:**
- Modify: `src/query.rs`

- [ ] **Step 1: Change Query struct fields**

Replace:
```rust
log_file: PathBuf,
```
With:
```rust
log_dir: PathBuf,
ids: Option<Vec<u64>>,  // None = all files
```

- [ ] **Step 2: Add ID parsing helper**

```rust
/// Parse --id argument: comma-separated IDs and/or ranges, e.g. "1,3,5-8"
/// Also supports repeated: --id 1 --id 3 --id 5
pub fn parse_ids(raw: &[String]) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for part in raw {
        if part.contains('-') {
            let range: Vec<&str> = part.splitn(2, '-').collect();
            let start: u64 = range[0].parse()?;
            let end: u64 = range[1].parse()?;
            ids.extend(start..=end);
        } else {
            ids.push(part.parse()?);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}
```

- [ ] **Step 3: Update `execute()` to iterate over matching log files**

```rust
pub async fn execute(&self) -> Result<()> {
    let log_files = self.resolve_log_files()?; // returns Vec<PathBuf>

    let mut all_events = Vec::new();
    for log_file in &log_files {
        let events = self.read_events_from(log_file, ...)?;
        all_events.extend(events);
    }
    // sort, output
}
```

`resolve_log_files()`:
- If `ids` is Some, return `{log_dir}/{id}.log` for each id (skip missing files)
- If `ids` is None, scan `log_dir` for `*.log` files, extract IDs from filenames, sort

- [ ] **Step 4: Add `--id` to CLI arguments**

In `bin/fsmon.rs`, modify QueryArgs:
```rust
#[derive(Parser)]
struct QueryArgs {
    /// Entry ID(s) to query. Comma-separated and/or ranges. Repeatable. Default: all.
    #[arg(short, long, value_name = "IDS")]
    id: Vec<String>,
    // ... rest same
}
```

- [ ] **Step 5: Update cmd_query in bin/fsmon.rs**

```rust
async fn cmd_query(args: QueryArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let ids = if args.id.is_empty() {
        None
    } else {
        Some(parse_ids(&args.id)?)
    };

    // ... rest of filter parsing

    let query = Query::new(
        cfg.logging.dir,
        ids,
        args.since, args.until, ...
    );
    query.execute().await
}
```

Remove `resolve_log_file` function entirely.

- [ ] **Step 6: Run tests**

Run: `cargo test --lib query`

- [ ] **Step 7: Commit**

```bash
git add src/query.rs src/bin/fsmon.rs
git commit -m "feat(query): support --id filter and multi-file query"
```

---

### Task 6: Update clean_logs — per-ID, directory-based

**Files:**
- Modify: `src/lib.rs`
- Modify: `src/bin/fsmon.rs`

- [ ] **Step 1: Update `clean_logs` signature**

```rust
pub async fn clean_logs(
    log_dir: &Path,
    ids: Option<&[u64]>,
    keep_days: u32,
    max_size: Option<i64>,
    dry_run: bool,
) -> Result<()>
```

- [ ] **Step 2: Update implementation**

Same logic as before but iterate over matching log files:
```rust
let log_files = resolve_log_files(log_dir, ids)?;
for log_file in &log_files {
    clean_single_log(log_file, keep_days, max_size, dry_run)?;
}
```

Extract the per-file logic into `clean_single_log` (identical to current `clean_logs` internals).

- [ ] **Step 3: Add `--id` to CleanArgs**

```rust
#[derive(Parser)]
struct CleanArgs {
    #[arg(short, long, value_name = "IDS")]
    id: Vec<String>,
    // ...
}
```

- [ ] **Step 4: Update cmd_clean**

```rust
async fn cmd_clean(args: CleanArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_paths()?;

    let ids = if args.id.is_empty() { None } else { Some(parse_ids(&args.id)?) };

    let keep_days = args.keep_days.unwrap_or(DEFAULT_KEEP_DAYS);
    let max_size = args.max_size.map(|s| parse_size(&s)).transpose()?;

    clean_logs(&cfg.logging.dir, ids, keep_days, max_size, args.dry_run).await
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib` — all lib tests pass

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/bin/fsmon.rs
git commit -m "refactor(clean): support --id filter, per-file log cleaning"
```

---

### Task 7: Final integration — remove dead code, full build, full test

**Files:**
- All modified files

- [ ] **Step 1: Full build**

Run: `cargo build --verbose` — zero warnings

- [ ] **Step 2: Full lint**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

- [ ] **Step 3: Full test**

Run: `cargo test --verbose` — all tests pass

- [ ] **Step 4: Format check**

Run: `cargo fmt -- --check`

- [ ] **Step 5: Verify no remaining references to old UserConfig**

Run: `grep -rn "UserConfig\|default_log_file\|default_socket_path" src/`

- [ ] **Step 6: Update help.rs if needed**

Check if help.rs references old config paths and update.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: final cleanup, remove dead UserConfig references"
```

---

### Task 8: Update PROGRESS.md

- [ ] **Step 1: Rewrite PROGRESS.md**

Document the new architecture and completed tasks.

- [ ] **Step 2: Commit**

```bash
git add PROGRESS.md
git commit -m "docs: update PROGRESS.md for config/store/log redesign"
```
