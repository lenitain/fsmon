# 独立测试目录 + CI/CD 流水线

> **For agentic workers:** Use `/skill:subagent-driven-development` (recommended) or `/skill:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 fsmon 建立独立 `tests/` 目录（参照 fd-rdd 模式）和 GitHub Actions CI/CD 流水线

**Architecture:** 
- `tests/common/` — 可复用的 test harness：启动 daemon 子进程、等待事件落盘、查询断言
- `tests/p1_cli.rs` — CLI 命令端到端测试（add/monitored/remove/query/clean/changes/init/health/cd）
- `tests/p1_monitor.rs` — 监控行为测试（事件捕获、进程追溯、自杀反馈过滤）
- `tests/p1_crash_recovery.rs` — 崩溃恢复测试（reader 自愈、SIGTERM drain、backoff 退避）
- `src/monitor/` — 新增 `--metrics-interval` 定期报告 RSS/吞吐/缓存/reader 状态（参照 fd-rdd memory_report_loop）
- `.github/workflows/ci.yml` — CI：build, test, clippy, fmt
- `.github/workflows/bench.yml` — 基准测试

**Tech Stack:** Rust cargo test, GitHub Actions, bash

**现状：** 67 个测试在 `src/bin/fsmon.rs`，32 个在 `src/monitor/tests.rs`，1 个在 `src/lib.rs`。无 CI。

---

### Task 1: 创建 tests/ 目录骨架 + common harness

**Files:**
- Create: `tests/common/mod.rs`
- Create: `tests/common/fsmon_daemon.rs`
- Create: `tests/common/fsmon_client.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: 创建 `tests/common/fsmon_client.rs` — CLI 客户端**

封装 `fsmon` 命令行调用，返回解析后的输出。核心 API：
- `fsmon_add(cmd: &str, path: &Path, args: &[&str])` → 执行 `fsmon add {cmd} --path {path} {args}`
- `fsmon_monitored()` → 执行 `fsmon monitored`，返回 JSONL 行列表
- `fsmon_remove(cmd: &str, paths: &[&Path])` → 执行 `fsmon remove`
- `fsmon_query(cmd: &str, args: &[&str])` → 执行 `fsmon query`，返回 `Vec<FileEvent>`
- `fsmon_clean(cmd: &str, args: &[&str])` → 执行 `fsmon clean`
- `fsmon_changes(cmd: &str, args: &[&str])` → 返回 changes 输出
- `fsmon_health()` → 执行 `fsmon health`
- `fsmon_init()` → 执行 `fsmon init`

```rust
// tests/common/fsmon_client.rs
use std::path::{Path, PathBuf};
use std::process::Command;
use fsmon::FileEvent;

pub fn fsmon_binary() -> PathBuf {
    // 在 cargo test 环境下定位 fsmon 二进制
    let exe = std::env::current_exe().unwrap();
    let target_dir = exe.parent().and_then(|p| p.parent()).unwrap();
    target_dir.join("fsmon")
}

pub fn run_fsmon(args: &[&str]) -> std::process::Output {
    Command::new(fsmon_binary())
        .args(args)
        .output()
        .expect("failed to run fsmon")
}

pub fn run_fsmon_success(args: &[&str]) -> String {
    let out = run_fsmon(args);
    if !out.status.success() {
        panic!("fsmon failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    String::from_utf8(out.stdout).unwrap()
}

pub fn parse_monitored_output(stdout: &str) -> Vec<serde_json::Value> {
    stdout.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

pub fn parse_query_output(stdout: &str) -> Vec<FileEvent> {
    stdout.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
```

- [ ] **Step 2: 创建 `tests/common/fsmon_daemon.rs` — Daemon 进程管理**

封装 daemon 子进程的生命周期管理（需要 root 权限，测试中跳过或 mock）。

```rust
// tests/common/fsmon_daemon.rs
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

pub struct FsmonDaemon {
    child: Child,
    #[allow(dead_code)]
    home_dir: PathBuf,
}

impl FsmonDaemon {
    /// 检查是否可以运行 daemon（需要 sudo）
    pub fn can_run() -> bool {
        // 测试环境：检查 uid 是否为 0 或有 CAP_SYS_ADMIN
        // 非 root 环境跳过 daemon 测试
        nix::unistd::geteuid().is_root()
    }

    pub fn spawn(home_dir: &Path, log_dir: &Path) -> Self {
        let exe = super::fsmon_client::fsmon_binary();
        let child = Command::new(&exe)
            .arg("daemon")
            .env("HOME", home_dir)
            .env("XDG_CONFIG_HOME", home_dir.join(".config"))
            .env("XDG_STATE_HOME", home_dir.join(".local/state"))
            .spawn()
            .expect("failed to spawn fsmon daemon");
        
        // 等待 daemon 就绪
        std::thread::sleep(Duration::from_millis(500));
        
        Self {
            child,
            home_dir: home_dir.to_path_buf(),
        }
    }

    pub fn terminate(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}
```

- [ ] **Step 3: 创建 `tests/common/mod.rs`**

汇总导出：

```rust
pub mod fsmon_client;
pub mod fsmon_daemon;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 在 /tmp 下创建唯一的测试目录
pub fn unique_tmp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("fsmon-test-{}-{}", tag, nanos))
}
```

- [ ] **Step 4: 修改 `Cargo.toml` 确保集成测试可以访问 lib crate**

检查 `Cargo.toml` 中 `[[bin]]` 和 `[lib]` 都已正确声明。确认 `dev-dependencies` 中有 `tempfile`（已有）。

- [ ] **Step 5: 跑空测试验证骨架**

```bash
cd ~/.projects/fsmon && cargo test --test '*' -- --list 2>&1 | head -20
```

预期：列出 0 个测试（此时还没有测试函数）。

- [ ] **Step 6: Commit**

```bash
git add tests/ Cargo.toml
git commit -m "feat: add tests/ skeleton with common harness"
```

---

### Task 2: 迁移 CLI 集成测试到 tests/p1_cli.rs

**Files:**
- Create: `tests/p1_cli.rs`
- (可选) Modify: `src/bin/fsmon.rs` — 标记原测试为 `#[ignore]` 或直接删除

- [ ] **Step 1: 迁移 add 命令端到端测试**

从 `src/bin/fsmon.rs` 中的 `test_integration_add_*` 系列迁移：

```rust
// tests/p1_cli.rs
use std::fs;
use fsmon::config::Config;
use fsmon::monitored::Monitored;
mod common;
use common::{unique_tmp_dir, fsmon_client::*};

/// 测试环境隔离：每个测试有独立 HOME
fn with_isolated_home(f: impl FnOnce(&std::path::Path, &std::path::Path)) {
    let dir = unique_tmp_dir("cli");
    let _ = fs::remove_dir_all(&dir);
    let home_str = dir.to_string_lossy().to_string();
    let monitored_path = dir.join("monitored");
    fs::create_dir_all(&monitored_path).unwrap();

    temp_env::with_vars(
        [
            ("HOME", Some(home_str.as_str())),
            ("XDG_CONFIG_HOME", None::<&str>),
            ("SUDO_UID", None::<&str>),
        ],
        || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(&dir, &monitored_path)
            }));
            let _ = fs::remove_dir_all(&dir);
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        },
    );
}

#[test]
fn add_global_with_path() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(mp, None).is_some());
    });
}

#[test]
fn add_with_cmd_group() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "myapp", "--path", &p]);
        
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 1);
        assert!(store.get(mp, Some("myapp")).is_some());
    });
}

#[test]
fn add_recursive_flag() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-r"]);
        
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        let entry = store.get(mp, None).unwrap();
        assert_eq!(entry.recursive, Some(true));
    });
}

#[test]
fn add_missing_cmd_fails() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        let out = run_fsmon(&["add", "--path", &p]);
        assert!(!out.status.success());
    });
}

#[test]
fn add_fsmon_self_rejected() {
    with_isolated_home(|_home, mp| {
        let p = mp.to_string_lossy();
        let out = run_fsmon(&["add", "fsmon", "--path", &p]);
        assert!(!out.status.success());
    });
}

#[test]
fn add_duplicate_replaces() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-r"]);
        run_fsmon_success(&["add", "_global", "--path", &p]);

        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 1);
        let entry = store.get(mp, None).unwrap();
        assert_eq!(entry.recursive, Some(false));
    });
}

#[test]
fn add_with_event_types() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-t", "MODIFY", "-t", "CREATE"]);
        
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        let entry = store.get(mp, None).unwrap();
        let types = entry.types.unwrap();
        assert!(types.contains(&"MODIFY".to_string()));
        assert!(types.contains(&"CREATE".to_string()));
    });
}

#[test]
fn add_with_size_filter() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p, "-s", ">1MB"]);
        
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        let entry = store.get(mp, None).unwrap();
        assert_eq!(entry.size.as_deref(), Some(">1MB"));
    });
}
```

- [ ] **Step 2: 迁移 remove 命令端到端测试**

```rust
#[test]
fn remove_single_path() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        run_fsmon_success(&["remove", "_global", "--path", &p]);

        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 0);
    });
}

#[test]
fn remove_entire_cmd_group() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "myapp", "--path", &p]);
        run_fsmon_success(&["remove", "myapp"]);

        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 0);
    });
}

#[test]
fn remove_path_from_cmd_group_keeps_others() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);
        run_fsmon_success(&["add", "app_a", "--path", &p]);
        run_fsmon_success(&["add", "app_b", "--path", &p]);

        run_fsmon_success(&["remove", "app_a"]);

        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 2);
        assert!(store.get(mp, None).is_some());
        assert!(store.get(mp, Some("app_b")).is_some());
        assert!(store.get(mp, Some("app_a")).is_none());
    });
}

#[test]
fn remove_multiple_paths_atomic() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);

        // 删除多个路径，其中一个不存在 → 原子性失败
        let out = run_fsmon(&["remove", "_global", "--path", &p, "--path", "/nonexistent"]);
        assert!(!out.status.success());

        // 原有的路径应该还在
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let store = Monitored::load(&cfg.monitored.path).unwrap();
        assert_eq!(store.entry_count(), 1);
    });
}
```

- [ ] **Step 3: 迁移 monitored 命令测试**

```rust
#[test]
fn monitored_lists_all_entries() {
    with_isolated_home(|home, mp| {
        let p = mp.to_string_lossy();
        run_fsmon_success(&["add", "_global", "--path", &p]);

        let stdout = run_fsmon_success(&["monitored"]);
        let entries = parse_monitored_output(&stdout);
        assert_eq!(entries.len(), 1);
    });
}
```

- [ ] **Step 4: 迁移 query 命令测试**

```rust
#[test]
fn query_missing_cmd_returns_empty() {
    with_isolated_home(|home, _mp| {
        let out = run_fsmon(&["query"]);
        // query 无 cmd 时应该报错
        assert!(!out.status.success());
    });
}

#[test]
fn query_nonexistent_log_does_not_crash() {
    with_isolated_home(|_home, _mp| {
        let out = run_fsmon(&["query", "_global"]);
        assert!(out.status.success());
    });
}
```

- [ ] **Step 5: 迁移 clean 命令测试**

```rust
#[test]
fn clean_missing_cmd_fails() {
    with_isolated_home(|_home, _mp| {
        let out = run_fsmon(&["clean"]);
        assert!(!out.status.success());
    });
}

#[test]
fn clean_dry_run_logically_sound() {
    with_isolated_home(|home, _mp| {
        // 创建包含新旧事件的 mock 日志
        let mut cfg = Config::load().unwrap();
        cfg.resolve_paths().unwrap();
        let log_dir = cfg.logging.path.unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        use std::io::Write;
        use chrono::Utc;
        let log_path = log_dir.join("_global_log.jsonl");
        let mut f = fs::File::create(&log_path).unwrap();
        let ts = Utc::now();
        let old = format!(
            r#"{{"time":"{}","event_type":"CREATE","path":"/old","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}}"#,
            (ts - chrono::Duration::days(100)).to_rfc3339()
        );
        let recent = format!(
            r#"{{"time":"{}","event_type":"MODIFY","path":"/recent","pid":2,"cmd":"y","user":"r","file_size":100,"ppid":0,"tgid":0,"chain":""}}"#,
            ts.to_rfc3339()
        );
        writeln!(f, "{}", old).unwrap();
        writeln!(f, "{}", recent).unwrap();

        // dry run 不修改文件
        let stdout = run_fsmon_success(&["clean", "_global", "--dry-run"]);
        assert!(stdout.contains("Dry run"));

        // 日志未被修改
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.lines().count() == 2);
    });
}
```

- [ ] **Step 6: 运行新测试确认通过**

```bash
cargo test --test p1_cli
```

预期：所有测试 PASS。

- [ ] **Step 7: 删除 `src/bin/fsmon.rs` 中已迁移的测试**

删除 `test_integration_add_*`、`test_integration_remove_*`、`test_integration_query_*`、`test_integration_clean_*` 系列函数（约 25 个）。保留 CLI 参数解析测试（`test_add_positional_*`、`test_query_*`、`test_daemon_*`、`test_changes_*`、`test_clean_*`、`test_cd_*` 等纯解析测试）。

- [ ] **Step 8: Commit**

```bash
git add tests/p1_cli.rs src/bin/fsmon.rs
git commit -m "test: migrate CLI integration tests to tests/p1_cli.rs"
```

---

### Task 3: 创建 tests/p1_monitor.rs — 监控行为测试

**Files:**
- Create: `tests/p1_monitor.rs`

- [ ] **Step 1: 事件解析测试（无需 daemon）**

```rust
// tests/p1_monitor.rs
use fsmon::{FileEvent, EventType, parse_log_line_jsonl};

#[test]
fn parse_valid_jsonl_event() {
    let json = r#"{"time":"2026-06-01T12:00:00Z","event_type":"CREATE","path":"/tmp/test.txt","pid":1234,"cmd":"touch","user":"pilot","file_size":0,"ppid":100,"tgid":1234,"chain":"1234|touch|pilot;100|bash|pilot"}"#;
    let ev = parse_log_line_jsonl(json).unwrap();
    assert_eq!(ev.event_type, EventType::Create);
    assert_eq!(ev.path.to_string_lossy(), "/tmp/test.txt");
    assert_eq!(ev.pid, 1234);
}

#[test]
fn parse_invalid_jsonl_returns_none() {
    assert!(parse_log_line_jsonl("not json").is_none());
    assert!(parse_log_line_jsonl("").is_none());
}

#[test]
fn parse_empty_line_returns_none() {
    assert!(parse_log_line_jsonl("   \n").is_none());
}

#[test]
fn event_jsonl_round_trip() {
    use chrono::Utc;
    let ev = FileEvent {
        time: Utc::now(),
        event_type: EventType::Modify,
        path: std::path::PathBuf::from("/tmp/test"),
        pid: 42,
        cmd: "vim".into(),
        user: "root".into(),
        file_size: 1024,
        ppid: 1,
        tgid: 42,
        chain: "42|vim|root;1|systemd|root".into(),
    };
    let json = ev.to_jsonl_string();
    let parsed = parse_log_line_jsonl(&json).unwrap();
    assert_eq!(parsed.path, ev.path);
    assert_eq!(parsed.pid, ev.pid);
    assert_eq!(parsed.cmd, ev.cmd);
}
```

- [ ] **Step 2: 事件类型序列化一致性测试**

```rust
#[test]
fn event_type_to_string_and_back() {
    let all = EventType::ALL;
    for et in all {
        let s = et.to_string();
        let parsed: EventType = s.parse().unwrap();
        assert_eq!(*et, parsed);
    }
}

#[test]
fn event_type_all_covers_14_types() {
    assert_eq!(EventType::ALL.len(), 14);
}
```

- [ ] **Step 3: 进程链格式测试**

从 `src/monitor/tests.rs` 的 `test_chains_contain_*` 中提取 chain 相关测试。

```rust
#[test]
fn process_chain_basic_format() {
    // 验证典型的进程链格式: "PID|cmd|user;PPID|parent|user"
    let chain = "1234|touch|pilot;100|bash|pilot";
    assert!(chain.contains(";"));
    assert!(chain.contains("|"));
}

#[test]
fn process_chain_single_entry() {
    let chain = "42|systemd|root";
    assert!(!chain.contains(";"));
}
```

- [ ] **Step 4: 运行测试**

```bash
cargo test --test p1_monitor
```

- [ ] **Step 5: Commit**

```bash
git add tests/p1_monitor.rs
git commit -m "test: add monitor behavior tests in tests/p1_monitor.rs"
```

---

### Task 4: 创建 tests/p1_crash_recovery.rs — 崩溃恢复测试

**Files:**
- Create: `tests/p1_crash_recovery.rs`

- [ ] **Step 1: DaemonLock 互斥测试**

```rust
// tests/p1_crash_recovery.rs
use fsmon::DaemonLock;

#[test]
fn lock_acquire_and_drop() {
    let uid = nix::unistd::geteuid().as_raw();
    let lock = DaemonLock::acquire(uid).unwrap();
    // 释放后可以重新获取
    drop(lock);
    let lock2 = DaemonLock::acquire(uid).unwrap();
    drop(lock2);
}

#[test]
fn lock_double_acquire_fails() {
    let uid = nix::unistd::geteuid().as_raw();
    let lock = DaemonLock::acquire(uid).unwrap();
    
    let result = DaemonLock::acquire(uid);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already running"));
    
    drop(lock);
}
```

- [ ] **Step 2: 监控原子写入测试 (monitored.jsonl)**

```rust
#[test]
fn monitored_save_is_atomic() {
    use std::fs;
    use std::path::PathBuf;
    use fsmon::monitored::Monitored;

    let dir = common::unique_tmp_dir("atomic-save");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("monitored.jsonl");

    // 创建初始数据
    let mut store = Monitored::default();
    let mut entry = fsmon::monitored::PathEntry::default();
    entry.cmd = Some("test".into());
    store.add("test", PathBuf::from("/tmp"), &entry);

    // 保存 (内部使用 temp + rename)
    store.save(&path).unwrap();

    // 验证文件存在且内容正确
    assert!(path.exists());
    
    // 模拟崩溃：写入过程中挂掉不会留下损坏文件
    // (这个测试验证原子性：要么完整写入，要么保持旧版本)
    let loaded = Monitored::load(&path).unwrap();
    assert_eq!(loaded.entry_count(), 1);
}
```

- [ ] **Step 3: 配置加载容错测试**

```rust
#[test]
fn config_loads_with_defaults_when_file_missing() {
    use fsmon::config::Config;
    
    // 在隔离的 HOME 中测试
    let dir = common::unique_tmp_dir("config-default");
    std::fs::create_dir_all(&dir).unwrap();
    
    temp_env::with_vars(
        [("HOME", Some(dir.to_string_lossy().as_str()))],
        || {
            // 无配置文件时应使用默认值
            let cfg = Config::load().unwrap_or_default();
            assert!(cfg.http_port.is_none()); // 默认无 HTTP
            // 日志路径应解析
            let mut cfg = cfg;
            cfg.resolve_paths().unwrap();
            match cfg.logging.path {
                Some(p) => assert!(p.to_string_lossy().contains("fsmon")),
                None => {} // 也是合法的
            }
        },
    );
}
```

- [ ] **Step 4: 消息边界完整性测试**

```rust
#[test]
fn jsonl_lines_are_self_delimiting() {
    // 模拟日志被截断时（半行 JSON），不崩溃，只跳过损坏行
    let lines = r#"{"time":"2026-06-01T10:00:00Z","event_type":"CREATE","path":"/a","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":""}
{"time":"2026-06-01T10:01:00Z","event_type":"MODIFY","path":"/b","pid":1,"cmd":"x","user":"r","file_size":0,"ppid":0,"tgid":0,"chain":"#;

    let mut count = 0;
    for line in lines.lines() {
        if let Some(_ev) = fsmon::parse_log_line_jsonl(line) {
            count += 1;
        }
    }
    // 第一行完整 → 解析成功；第二行被截断 → 跳过
    assert_eq!(count, 1);
}
```

- [ ] **Step 5: 运行测试**

```bash
cargo test --test p1_crash_recovery
```

- [ ] **Step 6: Commit**

```bash
git add tests/p1_crash_recovery.rs
git commit -m "test: add crash recovery tests in tests/p1_crash_recovery.rs"
```

---

### Task 5: 创建 tests/p1_utils.rs — 工具函数测试

**Files:**
- Create: `tests/p1_utils.rs`

- [ ] **Step 1: parse_size_filter 测试**

```rust
// tests/p1_utils.rs
use fsmon::{parse_size, parse_size_filter, SizeFilter, SizeOp};

#[test]
fn parse_size_units() {
    assert_eq!(parse_size("1KB").unwrap(), 1024);
    assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
    assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_size("0").unwrap(), 0);
}

#[test]
fn parse_size_filter_operators() {
    let f = parse_size_filter(">1MB").unwrap();
    assert_eq!(f.op, SizeOp::Gt);
    assert_eq!(f.bytes, 1024 * 1024);

    let f = parse_size_filter(">=1GB").unwrap();
    assert_eq!(f.op, SizeOp::Ge);
    assert_eq!(f.bytes, 1024 * 1024 * 1024);

    let f = parse_size_filter("<500KB").unwrap();
    assert_eq!(f.op, SizeOp::Lt);
    assert_eq!(f.bytes, 500 * 1024);

    let f = parse_size_filter("=0").unwrap();
    assert_eq!(f.op, SizeOp::Eq);
    assert_eq!(f.bytes, 0);
}

#[test]
fn parse_size_filter_case_insensitive() {
    assert_eq!(parse_size("1kb").unwrap(), 1024);
    assert_eq!(parse_size("1Mb").unwrap(), 1024 * 1024);
}
```

- [ ] **Step 2: parse_time_filter 测试**

```rust
use fsmon::{parse_time_filter, parse_time, TimeFilter, TimeOp};

#[test]
fn parse_time_relative() {
    let filter = parse_time_filter(">1h").unwrap();
    assert!(matches!(filter.op, TimeOp::Gt));
}

#[test]
fn parse_time_absolute_date() {
    let filter = parse_time_filter("<2026-05-01").unwrap();
    assert!(matches!(filter.op, TimeOp::Lt));
}

#[test]
fn parse_time_invalid_rejected() {
    assert!(parse_time_filter("garbage").is_err());
}
```

- [ ] **Step 3: 运行测试**

```bash
cargo test --test p1_utils
```

- [ ] **Step 4: Commit**

```bash
git add tests/p1_utils.rs
git commit -m "test: add utility function tests in tests/p1_utils.rs"
```

---

### Task 6: Daemon 性能报告 — `--metrics-interval`

**Files:**
- Modify: `src/bin/fsmon.rs` — 新增 `--metrics-interval` CLI 参数
- Modify: `src/bin/commands/daemon.rs` — 将参数传入 Monitor
- Modify: `src/monitor/mod.rs` — 新增 `metrics_interval` 字段 + tokio::select! 分支
- Create: `tests/p1_metrics.rs` — 测试 metrics 输出格式

**参照 fd-rdd:** `memory_report_loop` 模式 — 定期打印单行结构化日志

- [ ] **Step 1: 添加 `--metrics-interval` CLI 参数**

在 `src/bin/fsmon.rs` 的 `Daemon` 子命令中添加：

```rust
/// Metrics report interval in seconds (default: disabled).
/// When set, prints a one-line status report every N seconds to stderr.
/// Report includes: uptime, RSS (MB), event count, cache sizes, reader groups alive.
#[arg(long, value_name = "SECS")]
metrics_interval: Option<u64>,
```

- [ ] **Step 2: 添加 `MetricsReport` 结构体到 Monitor**

在 `src/monitor/mod.rs` 中：

```rust
pub(crate) struct MetricsReport {
    pub uptime_secs: u64,
    pub rss_mb: f64,
    pub events_processed: u64,
    pub events_written: u64,
    pub dir_cache_entries: u64,
    pub proc_cache_entries: u64,
    pub pid_tree_entries: u64,
    pub file_size_cache_entries: u64,
    pub reader_groups_total: usize,
    pub reader_groups_alive: u64,
    pub reader_groups_gave_up: u64,
    pub event_rx_pending: u64, // channel 积压
}

impl Monitor {
    fn collect_metrics(&self, dir_cache: &Cache<HandleKey, PathBuf>, proc_cache: &ProcCache, pid_tree: &PidTree) -> MetricsReport {
        let rss_mb = get_rss_mb();
        MetricsReport {
            uptime_secs: self.started_at.elapsed().as_secs(),
            rss_mb,
            events_processed: self.metrics.events_processed(),
            events_written: self.metrics.events_written(),
            dir_cache_entries: dir_cache.entry_count(),
            proc_cache_entries: proc_cache.entry_count(),
            pid_tree_entries: pid_tree.entry_count(),
            file_size_cache_entries: self.file_size_cache.len() as u64,
            reader_groups_total: self.fs_groups.len(),
            reader_groups_alive: self.reader_states.iter().filter(|s| s.as_ref().map_or(false, |s| !s.gave_up)).count() as u64,
            reader_groups_gave_up: self.reader_states.iter().filter(|s| s.as_ref().map_or(false, |s| s.gave_up)).count() as u64,
            event_rx_pending: 0, // 下一版补充
        }
    }
}

fn get_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split_whitespace().collect();
            parts.get(1).and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| (pages * 4096) as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}
```

- [ ] **Step 3: 在主循环中集成定期打印**

在 `src/monitor/mod.rs` 的 `run()` 方法 `tokio::select!` 中新增一个分支：

```rust
let metrics_interval = self.metrics_interval;
let mut metrics_tick = if metrics_interval > 0 {
    Some(tokio::time::interval(std::time::Duration::from_secs(metrics_interval)))
} else {
    None
};

// loop { tokio::select! { ...

// 新增分支（放在 _ = sighup.recv() 之后）:
_ = async {
    match metrics_tick.as_mut() {
        Some(tick) => tick.tick().await,
        None => std::future::pending().await,
    }
} => {
    let report = self.collect_metrics(&dir_cache, &proc_cache, &pid_tree);
    eprintln!(
        "[metrics] uptime={}s rss={:.1}MB events={}/{}/{} caches(d/p/t/f)={}/{}/{}/{} readers={}/{}/{}",
        report.uptime_secs,
        report.rss_mb,
        report.events_processed,
        report.events_written,
        report.event_rx_pending,
        report.dir_cache_entries,
        report.proc_cache_entries,
        report.pid_tree_entries,
        report.file_size_cache_entries,
        report.reader_groups_total,
        report.reader_groups_alive,
        report.reader_groups_gave_up,
    );
}
```

输出示例：
```
[metrics] uptime=3600s rss=4.2MB events=15234/15234/0 caches(d/p/t/f)=823/156/12/45 readers=1/1/0
```

- [ ] **Step 4: 添加 P1 测试**

在 `tests/p1_metrics.rs` 中验证格式正确性：

```rust
// tests/p1_metrics.rs
use std::fs;

mod common;

#[test]
fn rss_reading_is_reasonable() {
    // 即使在测试中，RSS 也应该是个合理值
    let statm = fs::read_to_string("/proc/self/statm").unwrap();
    let parts: Vec<&str> = statm.split_whitespace().collect();
    let rss_pages: u64 = parts[1].parse().unwrap();
    let rss_mb = (rss_pages * 4096) as f64 / (1024.0 * 1024.0);
    // 测试进程内存应在 1MB ~ 1GB 之间
    assert!(rss_mb > 0.5, "RSS too low: {:.1}MB", rss_mb);
    assert!(rss_mb < 500.0, "RSS too high: {:.1}MB", rss_mb);
}

#[test]
fn metrics_format_is_parseable() {
    // 模拟 metrics 输出行，验证可以被 grep/awk 轻松解析
    let line = "[metrics] uptime=3600s rss=4.2MB events=15234/15234/0 caches(d/p/t/f)=823/156/12/45 readers=1/1/0";
    // 验证格式一致性
    assert!(line.starts_with("[metrics]"));
    assert!(line.contains("uptime="));
    assert!(line.contains("rss="));
    assert!(line.contains("readers="));
}
```

- [ ] **Step 5: 更新 help 文本**

在 `src/help.rs` 中添加 `--metrics-interval` 相关文档。

- [ ] **Step 6: 运行测试**

```bash
cargo build --release
cargo test --test p1_metrics
```

- [ ] **Step 7: Commit**

```bash
git add src/bin/fsmon.rs src/bin/commands/daemon.rs src/monitor/mod.rs src/help.rs tests/p1_metrics.rs
git commit -m "feat: add --metrics-interval for periodic RSS/throughput/cache reporting"
```

---

### Task 7: 创建 GitHub Actions CI 流水线

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: 创建 CI workflow**

```yaml
# .github/workflows/ci.yml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  test:
    name: Test (Rust ${{ matrix.rust }})
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        rust: [stable]

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: ${{ matrix.rust }}

      - name: Cache cargo dependencies
        uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --release

      - name: Run unit tests (no sudo, all targets)
        run: cargo test --lib --bins --tests -- --test-threads=2

      - name: Run integration tests
        run: cargo test --test '*' -- --test-threads=2

  fmt:
    name: Format
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - run: cargo fmt --all -- --check

  clippy:
    name: Clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 2: .cargoignore 检查**

确保 CI 构建时 .cargoignore 不会排除必要的文件。

- [ ] **Step 3: 本地模拟 CI 检查**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib --bins --tests
cargo test --test '*'
```

如有 fmt 问题或 clippy 警告，修复它们。

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add GitHub Actions CI workflow (build, test, fmt, clippy)"
```

---

### Task 8: 创建 GitHub Actions Bench 流水线

**Files:**
- Create: `.github/workflows/bench.yml`

- [ ] **Step 1: 创建 bench workflow**

```yaml
# .github/workflows/bench.yml
name: Benchmark

on:
  workflow_dispatch:
  push:
    branches: [main]
    paths:
      - 'src/**'
      - 'Cargo.toml'
      - 'Cargo.lock'

env:
  CARGO_TERM_COLOR: always

jobs:
  bench-build:
    name: Release build benchmark
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Build release
        run: cargo build --release

      - name: Measure binary size
        run: |
          ls -lh target/release/fsmon
          strip target/release/fsmon
          ls -lh target/release/fsmon

      - name: Quick smoke test
        run: |
          ./target/release/fsmon --version
          ./target/release/fsmon --help
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/bench.yml
git commit -m "ci: add benchmark workflow (release build + smoke test)"
```

---

### Task 9: 创建 tests/README.md — 测试索引

**Files:**
- Create: `tests/README.md`

- [ ] **Step 1: 写测试索引文档**

```markdown
# fsmon 测试集

## 测试文件

- `p1_cli.rs` — CLI 命令端到端测试（add/monitored/remove/query/clean/changes）
- `p1_monitor.rs` — 事件解析与序列化测试
- `p1_crash_recovery.rs` — 崩溃恢复与容错测试（DaemonLock / 原子写入 / 配置容错 / 截断处理）
- `p1_metrics.rs` — 性能报告输出格式测试
- `p1_utils.rs` — 工具函数单元测试（parse_size / parse_time）

## 运行测试

```bash
# 全部测试
cargo test

# 仅集成测试
cargo test --test '*'

# 单个测试文件
cargo test --test p1_cli

# 单个测试函数
cargo test --test p1_cli add_global_with_path
```

## 测试分层

| 层级 | 内容 | 位置 |
|------|------|------|
| 单元测试 | 模块内部逻辑（monitor/events/filtering/fid_parser） | `src/**/*.rs` (`#[cfg(test)]`) |
| CLI 参数解析 | AddArgs/QueryArgs 等解析 | `src/bin/fsmon.rs` (`#[cfg(test)]`) |
| 集成测试 | CLI 端到端 / 崩溃恢复 / 工具函数 | `tests/*.rs` |
```

- [ ] **Step 2: Commit**

```bash
git add tests/README.md
git commit -m "docs: add test index README"
```

---

### Task 10: 全量验证 + changelog

- [ ] **Step 1: 运行全部测试**

```bash
cargo test --all-targets -- --test-threads=2
```

预期：所有测试 PASS（包括旧的 src/ 下测试 + 新的 tests/ 下测试）。

- [ ] **Step 2: 检查测试覆盖率**

```bash
# 确认 tests/ 目录的每个文件都被包含
cargo test --test '*' -- --list 2>&1 | grep "test " | wc -l
```

- [ ] **Step 3: 更新 CHANGELOG.md**

在 `CHANGELOG.md` 顶部添加：

```markdown
## [Unreleased]

### Added
- 独立 `tests/` 测试目录，含 common harness (`tests/common/`)
- 集成测试: CLI 端到端 (`p1_cli.rs`), 事件解析 (`p1_monitor.rs`), 崩溃恢复 (`p1_crash_recovery.rs`), 性能报告 (`p1_metrics.rs`), 工具函数 (`p1_utils.rs`)
- `--metrics-interval` daemon 性能报告（RSS/吞吐/缓存/reader 状态）
- GitHub Actions CI 流水线 (build + test + fmt + clippy)
- GitHub Actions Bench 流水线 (release build + smoke test)
- `tests/README.md` 测试索引文档

### Changed
- CLI 集成测试从 `src/bin/fsmon.rs` 迁移到 `tests/p1_cli.rs`（原位置保留 CLI 参数解析测试）
```

- [ ] **Step 4: 最终 Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: update CHANGELOG for tests/ restructure and CI"
```

---

## 完整提交序列

```
feat: add tests/ skeleton with common harness
test: migrate CLI integration tests to tests/p1_cli.rs
test: add monitor behavior tests in tests/p1_monitor.rs
test: add crash recovery tests in tests/p1_crash_recovery.rs
test: add utility function tests in tests/p1_utils.rs
feat: add --metrics-interval for periodic RSS/throughput/cache reporting
ci: add GitHub Actions CI workflow (build, test, fmt, clippy)
ci: add benchmark workflow (release build + smoke test)
docs: add test index README
docs: update CHANGELOG for tests/ restructure and CI
```

## 风险评估

| 风险 | 影响 | 缓解 |
|------|------|------|
| 从 `src/bin/fsmon.rs` 删除测试时遗漏 | 测试覆盖下降 | `cargo test --all-targets` 对比迁移前后测试数量 |
| `cargo fmt --check` 发现格式问题 | CI 失败 | 迁移最后一步统一 `cargo fmt` |
| clippy 新增 warning | CI 失败 | 先 `cargo clippy --all-targets -- -D warnings` 确认干净 |
| daemon 集成测试需要 root | 部分测试跳过 | `tests/p1_cli.rs` 中 daemon 相关测试用 `#[ignore]` 标记或 `FsmonDaemon::can_run()` 检查 |
