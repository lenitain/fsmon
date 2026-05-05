# Single Binary Daemon — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development or executing-plans to implement. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Merge `fsmon` + `fsmon-cli` into single `fsmon` binary with unix socket daemon architecture

**Architecture:** Single binary with two modes: `fsmon daemon` (systemd-managed background process with fanotify + unix socket listener) and CLI commands (`add`, `remove`, `managed`, `query`, `clean`, `install`, `uninstall`). CLI talks to daemon via `/var/run/fsmon/fsmon.sock` using JSON-line protocol. Config at `/etc/fsmon/fsmon.toml`.

**Tech Stack:** Rust, tokio, clap, fanotify-rs, serde, anyhow

---

## File Structure

```
Current → Target:

src/bin/
  fsmon.rs       → replaced: new binary with all commands
  fsmon-cli.rs   → deleted

src/
  config.rs      → rewritten: single config with [[paths]] entries
  socket.rs      → new: unix socket protocol, listener, client helper
  monitor.rs     → refactored: add socket integration, dynamic fanotify_mark add/remove
  lib.rs         → minor changes: export socket types, new config
  systemd.rs     → rewritten: single service (not template)
  help.rs        → rewritten: new command tree
  query.rs       → keep (minor: read config path)
  output.rs      → keep
  proc_cache.rs  → keep
  fid_parser.rs  → keep
  dir_cache.rs   → keep
  utils.rs       → keep

Cargo.toml       → single [[bin]]
```

---

### Task 1: Rewrite config.rs

**Files:**
- Rewrite: `src/config.rs`
- Test: existing tests in `src/config.rs` will be replaced

**New config format:**
```toml
log_file = "/var/log/fsmon/history.log"
socket_path = "/var/run/fsmon/fsmon.sock"

[[paths]]
path = "/var/www"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "100MB"
exclude = "*.tmp"
all_events = false
```

- [ ] **Step 1: Write Config + PathEntry structs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub log_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    pub paths: Vec<PathEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEntry {
    pub path: PathBuf,
    pub recursive: Option<bool>,
    pub types: Option<Vec<String>>,
    pub min_size: Option<String>,
    pub exclude: Option<String>,
    pub all_events: Option<bool>,
}
```

- [ ] **Step 2: Implement Config methods**

- `Config::load() -> Result<Self>` — reads from `/etc/fsmon/fsmon.toml`, returns default Config (empty paths) if not found
- `Config::load_from_path(path) -> Result<Self>` — parse TOML
- `Config::save(&self) -> Result<()>` — serialize to /etc/fsmon/fsmon.toml
- `Config::default_config_path() -> PathBuf` — returns `/etc/fsmon/fsmon.toml`
- `Config::generate_default() -> Result<()>` — create default config file if not exists
- `Config::add_path(entry: PathEntry) -> Result<()>` — load, add, save
- `Config::remove_path(path: &Path) -> Result<()>` — load, remove matching, save

- [ ] **Step 3: Update lib.rs exports**

Remove `DEFAULT_KEEP_DAYS`, `DEFAULT_LOG_PATH` exports or keep them if still used by clean. Remove `InstanceConfig`. Keep `EventType`, `FileEvent`, `clean_logs`, etc.

- [ ] **Step 4: Build + test**

Run: `cargo build`
Expected: compiles (existing binary targets still work for now)

---

### Task 2: Create src/socket.rs

**Files:**
- Create: `src/socket.rs`
- Modify: `src/lib.rs` to export socket module

Socket protocol types and both sides (daemon listener + client connector).

- [ ] **Step 1: Define protocol types**

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum SocketCmd {
    #[serde(rename = "add")]
    Add {
        path: PathBuf,
        recursive: Option<bool>,
        types: Option<Vec<String>>,
        min_size: Option<String>,
        exclude: Option<String>,
        all_events: Option<bool>,
    },
    #[serde(rename = "remove")]
    Remove { path: PathBuf },
    #[serde(rename = "list")]
    List,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SocketResp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<PathEntry>>,
}
```

- [ ] **Step 2: Write client-side helper**

```rust
pub fn send_cmd(cmd: &SocketCmd) -> Result<SocketResp>
```
- Connect to `/var/run/fsmon/fsmon.sock`
- Send JSON line
- Read JSON line response
- Return parsed `SocketResp`
- Handle connection refused → bail with "Daemon not running. Start with: sudo systemctl start fsmon"

- [ ] **Step 3: Write daemon listener skeleton**

```rust
pub async fn listen(socket_path: &Path, handler: impl Fn(SocketCmd) -> Result<SocketResp>) -> Result<()>
```
- Bind UnixListener
- Accept connections in loop
- Read JSON line, deserialize, call handler, write response
- Error handling: malformed JSON → respond with error, don't crash

- [ ] **Step 4: Add `pub mod socket;` to lib.rs**

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: compiles

- [ ] **Step 6: Commit**

```bash
git add src/socket.rs src/lib.rs
git commit -m "feat: Add socket protocol types and listener"
```

---

### Task 3: Refactor monitor.rs for daemon mode

**Files:**
- Modify: `src/monitor.rs`

Core change: Monitor needs to handle dynamic path add/remove and run as a daemon.

- [ ] **Step 1: Read entire monitor.rs to understand current structure**

The current `Monitor` struct has:
- `paths: Vec<PathBuf>` — initial paths
- `output: Option<PathBuf>` — log file
- Per-path options: `min_size`, `event_types`, `exclude_regex`, `recursive`, `all_events`
- `fan_fd: FanFd` — fanotify fd
- Various caches and state

The `run()` method does:
1. `fanotify_init` → get fan_fd
2. `fanotify_mark` for each path (with mount FD tracking)
3. Main event loop reading fan_fd events
4. SIGINT/SIGTERM handling

- [ ] **Step 2: Add dynamic path management methods**

```rust
impl Monitor {
    /// Add a new path to monitoring without restarting fanotify
    pub fn add_path(&mut self, entry: &PathEntry) -> Result<()> { ... }
    
    /// Remove a path from monitoring
    pub fn remove_path(&mut self, path: &Path) -> Result<()> { ... }
    
    /// Reload all paths from config (diff current vs new)
    pub fn reload_paths(&mut self, entries: &[PathEntry]) -> Result<()> { ... }
}
```

- `add_path`: calls `fanotify_mark(FAN_MARK_ADD, ... )` for the new path, stores per-path options
- `remove_path`: calls `fanotify_mark(FAN_MARK_REMOVE, ...)`, cleans up per-path state
- `reload_paths`: diff old vs new, add missing, remove extra, update changed

The current code uses `mount_fds: HashMap<u64, RawFd>` keyed by mount ID. Need to track which mount FD belongs to which path entry for removal. Store a reverse mapping: `path_to_mount_ids: HashMap<PathBuf, Vec<u64>>`.

- [ ] **Step 3: Add socket integration to run()**

In the main event loop (currently reads from `AsyncFd<FanFd>`), add `tokio::select!` to also listen on socket:

```rust
tokio::select! {
    Some(events) = fan_reader.next() => { /* handle fanotify events */ }
    Ok(conn) = socket_listener.accept() => { /* handle cmd */ }
    _ = sigterm.recv() => { /* graceful shutdown */ }
    _ = sighup.recv() => { /* reload config */ }
}
```

When receiving a socket command:
- `Add` → call `self.add_path()`, then persist config
- `Remove` → call `self.remove_path()`, then persist config
- `List` → return current paths from self.paths + per-path options

Socket handler callbacks will be passed a reference to `&mut Monitor`.

- [ ] **Step 4: Config persistence in daemon**

When daemon modifies paths (via add/remove), it writes the config file itself:
```rust
fn persist_config(&self) -> Result<()> {
    let cfg = Config {
        log_file: self.output.clone(),
        socket_path: Some(PathBuf::from("/var/run/fsmon/fsmon.sock")),
        paths: self.build_path_entries(),
    };
    cfg.save()
}
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: compiles

- [ ] **Step 6: Commit**

```bash
git add src/monitor.rs
git commit -m "feat: Add dynamic path management and socket integration to daemon"
```

---

### Task 4: Build new binary entrypoint

**Files:**
- Create: `src/bin/fsmon.rs` (new, replaces old)
- Delete: `src/bin/fsmon-cli.rs`
- Delete: `src/bin/fsmon.rs` (old)
- Modify: `Cargo.toml`

- [ ] **Step 1: Update Cargo.toml**

Remove `[[bin]] name = "fsmon-cli"`. Keep single `[[bin]] name = "fsmon"`.

- [ ] **Step 2: Write new binary entrypoint**

Full command tree:

```rust
#[derive(Parser)]
#[command(name = "fsmon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Daemon,        // runs daemon loop
    Add(AddArgs),  // add path to monitoring
    Remove(RemoveArgs),
    Managed,
    Query(QueryArgs),
    Clean(CleanArgs),
    Install(InstallArgs),
    Uninstall,
}
```

Each subcommand implementation:

**`Daemon`**:
1. Load config from `/etc/fsmon/fsmon.toml`
2. Create Monitor with config paths
3. Create socket listener
4. Pass socket handler to Monitor that delegates to Monitor's add/remove/list
5. Run Monitor (blocks forever)

**`Add`**: 
1. Parse args into `SocketCmd::Add`
2. Call `socket::send_cmd(cmd)`
3. Print result

**`Remove`**:
1. Parse args into `SocketCmd::Remove`
2. Call `socket::send_cmd(cmd)`
3. Print result

**`Managed`**:
1. Call `socket::send_cmd(&SocketCmd::List)`
2. Format response as table

**`Query`**: 
1. Load config to find `log_file`
2. Use existing `Query::new(...).execute()`
3. Override with `--log-file` if specified

**`Clean`**:
1. Load config to find `log_file`
2. Use existing `clean_logs()`
3. Override with `--log-file` if specified

**`Install`**: 
1. Create `/etc/systemd/system/fsmon.service`
2. Create `/etc/fsmon/fsmon.toml` if not exists
3. Ensure socket dir exists

**`Uninstall`**: 
1. Remove service file + daemon-reload

- [ ] **Step 3: Keep all original add flags**

```rust
struct AddArgs {
    path: PathBuf,
    #[arg(short)]
    recursive: bool,
    #[arg(short, long)]
    types: Option<String>,
    #[arg(short, long)]
    min_size: Option<String>,
    #[arg(short, long)]
    exclude: Option<String>,
    #[arg(long)]
    all_events: bool,
}
```

- [ ] **Step 4: Keep all original query/clean flags**

Query: `--since`, `--until`, `--pid`, `--cmd`, `--user`, `--types`, `--min-size`, `--format`, `--sort`, `--log-file` (override)
Clean: `--keep-days`, `--max-size`, `--dry-run`, `--log-file` (override)

- [ ] **Step 5: Build + remove old binaries**

Run: `cargo build`
Expected: single `fsmon` binary, no `fsmon-cli`

Run: `cargo test`
Expected: existing tests pass (may need minor fixes for deleted code)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/bin/fsmon.rs
git rm src/bin/fsmon-cli.rs src/bin/fsmon.rs
git commit -m "feat: Merge fsmon and fsmon-cli into single binary"
```

---

### Task 5: Rewrite systemd.rs

**Files:**
- Rewrite: `src/systemd.rs`

- [ ] **Step 1: Write new service template**

```systemd
[Unit]
Description=fsmon filesystem monitor
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/fsmon daemon
Restart=on-failure
RestartPreventExitStatus=78
RestartSec=5
RuntimeDirectory=fsmon
RuntimeDirectoryMode=0755
StandardOutput=journal
StandardError=journal
CapabilityBoundingSet=CAP_SYS_ADMIN
AmbientCapabilities=CAP_SYS_ADMIN

[Install]
WantedBy=multi-user.target
```

- `RuntimeDirectory=fsmon` → systemd creates `/run/fsmon` (where socket goes)
- CAP_SYS_ADMIN for fanotify
- No `User=` → runs as root (fanotify requirement)

- [ ] **Step 2: Install function**

```rust
pub fn install(force: bool) -> Result<()> {
    // Check root
    // Create service file at /etc/systemd/system/fsmon.service
    // Create /etc/fsmon/ if not exists
    // Generate default config if not exists
    // systemctl daemon-reload
}
```

- [ ] **Step 3: Uninstall function**

```rust
pub fn uninstall() -> Result<()> {
    // Remove service file
    // systemctl daemon-reload
}
```

- [ ] **Step 4: Config initialization during install**

If `/etc/fsmon/fsmon.toml` doesn't exist, create default with:
```toml
log_file = "/var/log/fsmon/history.log"
socket_path = "/run/fsmon/fsmon.sock"
# No paths yet — use 'fsmon add' to add paths
```

- [ ] **Step 5: Build + test**

Run: `cargo build`
Expected: compiles

---

### Task 6: Update help.rs

**Files:**
- Rewrite: `src/help.rs`

- [ ] **Step 1: New help text for all commands**

Remove: `HelpTopic::GenerateInstance`, `HelpTopic::Generate`, `daemon_after_help`, `cli_after_help`
Add: `HelpTopic::Add`, `HelpTopic::Remove`, `HelpTopic::Managed`, `HelpTopic::Daemon`
Update: all existing help text to reflect new command tree

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add src/help.rs src/systemd.rs
git commit -m "feat: Rewrite systemd service and help for single binary"
```

---

### Task 7: Cleanup old InstanceConfig and dead code

**Files:**
- Modify: `src/config.rs` — remove `InstanceConfig`, `generate_instance_config`
- Modify: `src/lib.rs` — remove unused exports

- [ ] **Step 1: Remove InstanceConfig**

Delete `InstanceConfig` struct and `generate_instance_config()` function.
Remove `INSTANCE_CONFIG_DIR`, `INSTANCE_CONFIG_TEMPLATE` constants.

- [ ] **Step 2: Clean lib.rs exports**

Check what `lib.rs` currently exports. Remove anything only used by deleted code.
Keep: `FileEvent`, `EventType`, `OutputFormat`, `SortBy`, `clean_logs`, `parse_log_line`, `parse_output_format`, `parse_sort_by`, `DEFAULT_KEEP_DAYS`

- [ ] **Step 3: Build + test**

Run: `cargo build && cargo test`
Expected: all passing

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/lib.rs
git commit -m "ref: Remove InstanceConfig and dead code"
```

---

### Task 8: Final integration and testing

**Files:**
- All modified files

- [ ] **Step 1: Run full build**

```bash
cargo build --release
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Run tests**

```bash
cargo test --verbose
```

- [ ] **Step 4: Manual smoke test**

```bash
# 1. Install service
sudo ./target/debug/fsmon install

# 2. Add a path
sudo ./target/debug/fsmon add /tmp -r --types MODIFY,CREATE

# 3. Start daemon
sudo systemctl start fsmon

# 4. List managed paths
sudo ./target/debug/fsmon managed

# 5. Create a file and query
touch /tmp/test-file
sleep 1
./target/debug/fsmon query --since 10s

# 6. Remove path
sudo ./target/debug/fsmon remove /tmp

# 7. Stop and uninstall
sudo systemctl stop fsmon
sudo ./target/debug/fsmon uninstall
```

- [ ] **Step 5: Fix any issues**

- [ ] **Step 6: Commit final fixes**
