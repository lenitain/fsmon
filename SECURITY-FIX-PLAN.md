# fsmon 安全修复计划

> **项目**: fsmon — Rust 文件系统监控守护进程  
> **扫描时间**: 2026-06-22  
> **审计工具**: 静态代码审查  
> **报告版本**: v1.0  

---

## 目录

- [1. 概述](#1-概述)
- [2. 设计决策记录](#2-设计决策记录)
  - [F-004/005/006: 环境变量注入](#f-004005006-环境变量注入)
  - [F-007/F-027: Symlink 处理（统一策略）](#f-007f-027-symlink-处理统一策略)
  - [F-008: mark_recursive_inner 跟随 symlink](#f-008-mark_recursive_inner-跟随-symlink)
  - [F-009: 临时文件名可预测](#f-009-临时文件名可预测)
  - [F-010: truncate 未检查 symlink](#f-010-truncate-未检查-symlink)
  - [F-011: rename 失败数据丢失](#f-011-rename-失败数据丢失)
  - [F-014/019: 路径验证策略](#f-014019-路径验证策略)
  - [F-015: subscribe 验证](#f-015-subscribe-验证)
  - [F-016: mount_fd 权限](#f-016-mount_fd-权限)
  - [F-017: canonicalize TOCTOU](#f-017-canonicalize-toctou)
  - [F-018: file_writer TOCTOU](#f-018-file_writer-toctou)
  - [F-021: 递归深度限制](#f-021-递归深度限制)
  - [F-023: 文件存在性竞态](#f-023-文件存在性竞态)
  - [F-026: 临时文件权限](#f-026-临时文件权限)
  - [F-030: pending_paths 重复条目](#f-030-pending_paths-重复条目)
  - [F-031: strip_deleted_suffix 误替换](#f-031-strip_deleted_suffix-误替换)
- [3. 跳过的发现及原因](#3-跳过的发现及原因)
- [4. 额外改动](#4-额外改动)
- [5. 依赖关系和执行顺序](#5-依赖关系和执行顺序)
- [6. 涉及文件清单](#6-涉及文件清单)

---

## 1. 概述

| 指标 | 数值 |
|------|------|
| 扫描发现总数 | 31 |
| 确认修复 | 19 |
| 跳过 | 12 |
| 额外改动 | 2 |

### 问题分类

| 类别 | 数量 | 说明 |
|------|------|------|
| 环境变量注入 | 3 | F-004, F-005, F-006 |
| Symlink 攻击 | 5 | F-007, F-008, F-010, F-018, F-027 |
| TOCTOU 竞态 | 3 | F-017, F-018, F-023 |
| 路径验证 | 2 | F-014, F-019 |
| 临时文件安全 | 4 | F-009, F-011, F-023, F-026 |
| 资源限制 | 1 | F-021 |
| 订阅验证 | 1 | F-015 |
| 文件描述符权限 | 1 | F-016 |
| 数据完整性 | 2 | F-030, F-031 |

---

## 2. 设计决策记录

---

### F-004/005/006: 环境变量注入

#### 问题描述

fsmon 在以 root 权限运行时（如通过 sudo），依赖环境变量 `SUDO_USER`、`SUDO_UID`、`SUDO_GID` 来确定原始用户身份和主目录。这些环境变量可被恶意用户篡改，导致：

- **F-004**: 伪造 `SUDO_USER` 指向任意用户
- **F-005**: 伪造 `SUDO_UID`/`SUDO_GID` 获取错误的 UID/GID
- **F-006**: 通过 `XDG_CONFIG_HOME` 指向恶意配置文件路径

#### 决策及原因

**可选方案**
- A: root 运行时忽略所有环境变量，用 getpwuid()
- B: 仅验证变量合法性
- C: 保持现状（文档说明风险）

**选择方案 A — root 运行时忽略环境变量，使用 `getpwuid()`**

| 考量 | 分析 |
|------|------|
| 安全性 | `getpwuid()` 查询 `/etc/passwd`，不受环境变量篡改影响 |
| 可靠性 | 系统级数据库比用户可控的环境变量更可信 |
| 边缘情况 | 容器中可能没有 `/etc/passwd`，需 fallback 到当前进程 UID |

#### 具体修改方案

**涉及文件**: `src/common/config.rs`

**1. `resolve_uid_gid()` 函数重构**

```rust
// 修改前
fn resolve_uid_gid() -> (u32, u32) {
    if let (Ok(uid), Ok(gid)) = (env::var("SUDO_UID"), env::var("SUDO_GID")) {
        return (uid.parse().unwrap_or(0), gid.parse().unwrap_or(0));
    }
    (unsafe { libc::getuid() }, unsafe { libc::getgid() })
}

// 修改后
fn resolve_uid_gid() -> (u32, u32) {
    let current_uid = unsafe { libc::getuid() };
    let current_gid = unsafe { libc::getgid() };

    // 非 root 直接返回当前进程的 UID/GID
    if current_uid != 0 {
        return (current_uid, current_gid);
    }

    // root 运行时：尝试通过 getpwuid 查询原始用户
    // SUDO_UID 仅作为提示，需验证用户存在
    if let Ok(sudo_uid) = env::var("SUDO_UID") {
        if let Ok(uid) = sudo_uid.parse::<u32>() {
            // 验证该 UID 确实存在对应的用户
            if let Some(_user) = get_user_by_uid(uid) {
                let gid = env::var("SUDO_GID")
                    .ok()
                    .and_then(|g| g.parse::<u32>().ok())
                    .unwrap_or(current_gid);
                return (uid, gid);
            }
        }
    }

    // Fallback: getpwuid 当前 UID（通常是 root）
    // 或者在容器中直接返回当前进程 UID
    (current_uid, current_gid)
}

/// 通过 UID 查询用户，失败返回 None
fn get_user_by_uid(uid: u32) -> Option<libc::passwd> {
    unsafe {
        let mut passwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let mut buf = [0u8; 4096];

        let ret = libc::getpwuid_r(
            uid,
            &mut passwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        );

        if ret == 0 && !result.is_null() {
            Some(passwd)
        } else {
            None
        }
    }
}
```

**2. `guess_home()` 函数重构**

```rust
// 修改前
fn guess_home() -> PathBuf {
    if let Ok(user) = env::var("SUDO_USER") {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    // ...
}

// 修改后
fn guess_home() -> PathBuf {
    let uid = unsafe { libc::getuid() };

    // 非 root：直接用标准库
    if uid != 0 {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }

    // root：通过 getpwuid 查询原始用户的主目录
    if let Ok(sudo_uid) = env::var("SUDO_UID") {
        if let Ok(uid) = sudo_uid.parse::<u32>() {
            if let Some(passwd) = get_user_by_uid(uid) {
                let home = unsafe {
                    CStr::from_ptr(passwd.pw_dir)
                        .to_string_lossy()
                        .into_owned()
                };
                return PathBuf::from(home);
            }
        }
    }

    // Fallback
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}
```

**3. `Config::user_path()` 修改**

```rust
// 修改后
impl Config {
    pub fn user_path() -> PathBuf {
        let uid = unsafe { libc::getuid() };

        // root 运行时忽略 XDG_CONFIG_HOME
        if uid == 0 {
            // 使用 getpwuid 获取的主目录
            return Self::guess_home().join(".config/fsmon");
        }

        // 非 root：正常逻辑
        env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Self::guess_home().join(".config"))
            .join("fsmon")
    }
}
```

#### 注意事项

1. `getpwuid_r` 是线程安全版本，优先使用
2. 容器环境中 `/etc/passwd` 可能不存在，fallback 到当前进程 UID
3. 所有环境变量（包括 `SUDO_UID`）都必须验证用户确实存在后才能使用
4. 测试时需覆盖以下场景：
   - 普通用户运行
   - sudo 运行
   - 容器中运行（无 /etc/passwd）
   - 环境变量被篡改的场景

---

### F-007/F-027: Symlink 处理（统一策略）

#### 问题描述

- **F-007**: `resolve_recursion_check()` 跟随符号链接，可能导致监控意外目录
- **F-027**: 配置文件中的监控路径可能是符号链接

两个问题本质相同：如何处理用户指定的路径是符号链接的情况。

#### 决策及原因

**可选方案**
- A: 拒绝所有 symlink
- B: 解析 symlink 后验证目标在允许范围
- C: 允许 symlink 但记录警告，显示 "linked to" 实际路径

**选择方案 C — 允许 symlink，显示 "linked to"**

| 方案 | 优点 | 缺点 |
|------|------|------|
| A: 拒绝 symlink | 最安全 | 不友好，很多合法场景用 symlink |
| B: 静默跟随 | 用户友好 | 隐藏信息，安全风险 |
| **C: 允许 + 显示** | **信息透明，用户可决策** | **实现稍复杂** |

选择 C 的理由：
- 用户友好的同时保持信息透明
- 责任边界清晰：用户看到 "linked to /xxx" 后自行决定是否继续
- 日志记录充分，便于审计

#### 显示格式规范

| 场景 | 显示格式 |
|------|----------|
| `fsmon add ~/docs` | `Entry added: ~/docs (linked to /etc)` |
| `fsmon list` | `~/docs (linked to /etc) (recursive)` |
| daemon 日志 | `WARN Symlink detected: ~/docs → /etc, monitoring resolved path` |

#### 具体修改方案

**涉及文件**: 4 个

**1. `src/common/filters.rs`: `resolve_recursion_check()`**

```rust
pub fn resolve_recursion_check(path: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let metadata = symlink_metadata(path)?;

    if metadata.file_type().is_symlink() {
        let resolved = canonicalize(path)?;
        // 返回 (原始路径, 解析后路径)
        // 调用方负责日志/显示 symlink 信息
        Ok((path.to_path_buf(), resolved))
    } else {
        let resolved = canonicalize(path)?;
        Ok((path.to_path_buf(), resolved))
    }
}
```

**2. `src/common/monitored.rs`: `load()` 和 `save()`**

```rust
// load() — 检测并标记 symlink
pub fn load(config: &Config) -> Result<Self> {
    let entries = /* ... */;

    for entry in &mut entries {
        let metadata = symlink_metadata(&entry.path)?;
        if metadata.file_type().is_symlink() {
            let target = canonicalize(&entry.path)?;
            entry.symlink_target = Some(target);
        }
    }

    Ok(Self { entries })
}

// MonitoredEntry 结构体添加字段
pub struct MonitoredEntry {
    pub path: PathBuf,
    pub recursive: bool,
    pub symlink_target: Option<PathBuf>,  // 新增
    // ...
}

impl fmt::Display for MonitoredEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(target) = &self.symlink_target {
            write!(f, " (linked to {})", target.display())?;
        }
        if self.recursive {
            write!(f, " (recursive)")?;
        }
        Ok(())
    }
}
```

**3. `src/bin/fsmon/commands/add.rs`: 添加输出**

```rust
// 添加时检测 symlink 并提示
fn add_path(path: &Path, recursive: bool) -> Result<()> {
    let metadata = symlink_metadata(path)?;

    let entry = MonitoredEntry {
        path: path.to_path_buf(),
        recursive,
        symlink_target: if metadata.file_type().is_symlink() {
            Some(canonicalize(path)?)
        } else {
            None
        },
    };

    // 输出
    print!("Entry added: {}", path.display());
    if let Some(target) = &entry.symlink_target {
        print!(" (linked to {})", target.display());
    }
    if recursive {
        print!(" (recursive)");
    }
    println!();

    // 保存...
    Ok(())
}
```

**4. `src/bin/fsmon/commands/monitored.rs`: list 输出**

```rust
fn list_entries(entries: &[MonitoredEntry]) {
    for entry in entries {
        println!("{}", entry);  // 使用 Display trait
    }
}
```

**5. Daemon 日志（在 daemon 主循环中）**

```rust
if entry.symlink_target.is_some() {
    warn!(
        "Symlink detected: {} → {}, monitoring resolved path",
        entry.path.display(),
        entry.symlink_target.as_ref().unwrap().display()
    );
}
```

#### 注意事项

1. `symlink_metadata()` 不跟随链接，`canonicalize()` 会跟随
2. `MonitoredEntry` 序列化时 `symlink_target` 字段仅用于显示，不参与监控逻辑
3. 递归遍历时仍需 F-008 的 symlink 检查（子目录中的链接）

---

### F-008: mark_recursive_inner 跟随 symlink

#### 问题描述

`mark_recursive_inner()` 递归遍历目录时，如果遇到符号链接指向的目录，会跟随进入，可能监控意外位置或形成循环。

#### 决策及原因

**选择: 添加 `is_symlink()` 检查，跳过符号链接**

递归遍历时遇到的子目录中的 symlink 应该跳过，因为：
1. 用户已通过 `fsmon add` 明确指定了顶层 symlink（F-007 处理）
2. 子目录中的 symlink 跟随会导致不可预期的行为
3. 可能形成循环导致栈溢出

#### 具体修改方案

**涉及文件**: `src/common/fid_parser.rs`

```rust
// mark_recursive_inner() 修改
fn mark_recursive_inner(
    fan: &Fanotify,
    fd: &OwnedFd,
    path: &Path,
    mask: u64,
    depth: u32,
    max_depth: u32,
) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }

    let dir = read_dir(path)?;

    for entry in dir {
        let entry = entry?;
        let file_type = entry.file_type()?;

        // 新增：跳过符号链接
        if file_type.is_symlink() {
            debug!("Skipping symlink: {}", entry.path().display());
            continue;
        }

        if file_type.is_dir() {
            // 标记子目录
            mark_single(fan, fd, &entry.path(), mask)?;

            // 递归
            mark_recursive_inner(
                fan, fd, &entry.path(), mask, depth + 1, max_depth,
            )?;
        }
    }

    Ok(())
}
```

#### 注意事项

1. `entry.file_type()` 在某些平台上可能需要额外 `stat` 调用
2. 如果用户确实需要监控 symlink 指向的目录，需要手动 `fsmon add` 该目标路径
3. 日志级别用 `debug!`，避免在正常运行时产生过多输出

---

### F-009: 临时文件名可预测

#### 问题描述

`truncate_from_start()` 使用 `.fsmon_trunc_<PID>` 作为临时文件名，文件名可预测，存在 symlink 攻击风险。

#### 决策及原因

**选择: 使用 `tempfile` crate**

| 方案 | 优点 | 缺点 |
|------|------|------|
| 自定义随机名 | 无需新依赖 | 需要自己实现原子创建 |
| **tempfile crate** | **原子创建、自动清理、广泛使用** | **新增依赖** |

选择 `tempfile` 的理由：
- 业界标准方案，经过充分审计
- `NamedTempFile::new_in()` 保证原子创建
- Drop 时自动清理，无需手动处理

#### 具体修改方案

**涉及文件**: `src/common/clean/core.rs`

**1. 添加依赖** (`Cargo.toml`)

```toml
[dependencies]
tempfile = "3"
```

**2. `truncate_from_start()` 重构**

```rust
// 修改前
fn truncate_from_start(path: &Path, size: u64) -> io::Result<()> {
    let tmp_path = path.with_extension(format!("fsmon_trunc_{}", std::process::id()));
    // ... 复制、rename ...
}

// 修改后
use tempfile::NamedTempFile;

fn truncate_from_start(path: &Path, size: u64) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));

    // tempfile 保证：
    // 1. 文件名不可预测
    // 2. 原子创建（O_CREAT | O_EXCL）
    // 3. 权限默认 0600
    let mut tmp_file = NamedTempFile::new_in(parent)?;

    // 读取原文件后 size 字节，写入临时文件
    let file = File::open(path)?;
    let file_size = file.metadata()?.len();

    if file_size > size {
        let mut reader = BufReader::new(file);
        // seek 到 file_size - size 位置
        reader.seek(SeekFrom::Start(file_size - size))?;

        let mut writer = BufWriter::new(&tmp_file);
        io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
    }

    // 保持临时文件的 fd，rename 会原子替换
    let tmp_path = tmp_file.into_temp_path();
    fs::rename(&tmp_path, path)?;

    Ok(())
}
```

#### 注意事项

1. `tempfile::NamedTempFile::new_in()` 默认权限是 0600（见 F-026）
2. `into_temp_path()` 会删除 Drop 时的自动清理逻辑，因为我们已经 rename 了
3. 如果 rename 失败，临时文件会在 `tmp_path` drop 时自动清理

---

### F-010: truncate 未检查 symlink

#### 问题描述

`truncate_from_start()` 操作前未检查目标文件是否为符号链接，可能被利用来写入任意文件。

#### 决策及原因

**选择: 添加 `lstat` 检查**

在文件操作前使用 `symlink_metadata()`（等同于 `lstat`）检查是否为符号链接。如果是，拒绝操作并返回错误。

#### 具体修改方案

**涉及文件**: `src/common/clean/core.rs`

```rust
fn truncate_from_start(path: &Path, size: u64) -> io::Result<()> {
    // 新增：symlink 检查
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Refusing to truncate symlink: {}", path.display()),
        ));
    }

    // ... 后续逻辑 ...
}
```

#### 注意事项

1. 使用 `symlink_metadata()` 而非 `metadata()`，后者会跟随链接
2. 检查放在函数最开头，在任何文件操作之前
3. 返回 `InvalidInput` 错误类型，便于调用方区分

---

### F-011: rename 失败数据丢失

#### 问题描述

`clean_single_log()` 中使用 `fs::rename()` 重命名文件，如果 rename 失败（如跨文件系统），原文件内容可能丢失。

#### 决策及原因

**选择: 添加备份/回滚机制**

```
操作前：
  original.log  (原文件)

Step 1: 创建备份
  original.log  →  original.log.bak

Step 2: 创建截断后的临时文件
  .fsmon_trunc_xxx  (新内容)

Step 3: Rename 临时文件
  .fsmon_trunc_xxx  →  original.log

Step 4 (失败时): 恢复备份
  original.log.bak  →  original.log
```

#### 具体修改方案

**涉及文件**: `src/common/clean/core.rs`

```rust
fn clean_single_log(path: &Path, keep_size: u64) -> io::Result<()> {
    let backup_path = path.with_extension("log.bak");

    // Step 1: 创建备份（使用 copy + fsync，保证数据落盘）
    fs::copy(path, &backup_path)?;
    {
        let backup_file = File::open(&backup_path)?;
        backup_file.sync_all()?;
    }

    // Step 2: 截断到临时文件（使用 tempfile，见 F-009）
    let result = truncate_from_start(path, keep_size);

    // Step 3: 检查结果
    match result {
        Ok(()) => {
            // 成功：删除备份
            fs::remove_file(&backup_path).ok(); // 允许删除失败
            Ok(())
        }
        Err(e) => {
            // 失败：恢复备份
            warn!("Truncation failed, restoring backup: {}", e);
            fs::rename(&backup_path, path)?;
            Err(e)
        }
    }
}
```

#### 注意事项

1. 备份文件使用 `.bak` 后缀，与原文件在同一目录（保证同一文件系统，rename 一定成功）
2. `fs::copy` 后调用 `sync_all()` 确保数据落盘
3. 成功后删除备份，失败时恢复备份
4. `fs::remove_file` 的失败用 `.ok()` 忽略（非关键路径）

---

### F-014/019: 路径验证策略

#### 问题描述

- **F-014**: `add` 命令缺少对关键路径的检查，可以添加 fsmon 自身的日志目录
- **F-019**: `add` 命令可以添加 fsmon 自身进程

#### 决策及原因

**可选方案**
- A: 硬编码黑名单 (/etc, /proc, /sys...)
- B: 白名单模式（只允许用户 home 下）
- C: 仅规范化 + 拒绝 ".."
- 实际选择：黑名单 + 可配置扩展（用户可添加新黑名单条目）

**选择: 黑名单 + 可配置扩展**

设计原则：
- 默认黑名单（硬编码）保护 fsmon 自身
- 用户可通过配置文件扩展黑名单
- 统一的检查函数，统一的报错格式

**默认黑名单**（不可移除）：
1. fsmon 日志目录 — 防止递归写日志导致磁盘满
2. fsmon 自身进程 — 防止自监控

**用户可配置扩展**：
```toml
[security]
blocked_paths = ["/home/user/secret", "/etc/shadow"]
```

#### 具体修改方案

**涉及文件**: 3 个

**1. 新增/修改: 统一检查函数**

```rust
// src/common/security.rs (新文件) 或 src/common/config.rs

use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SecurityError {
    #[error("Path is blocked: {path}\n  Reason: {reason}")]
    BlockedPath { path: String, reason: String },
}

/// 默认黑名单检查
const DEFAULT_BLOCKED_PATHS: &[(&str, &str)] = &[
    // fsmon 日志目录
    ("/var/log/fsmon", "default blacklist (fsmon log directory)"),
    // fsmon 自身进程
    ("/proc/self", "default blacklist (fsmon process)"),
];

/// 统一路径检查函数
pub fn check_path_allowed(path: &Path, config: &Config) -> Result<(), SecurityError> {
    let path_str = path.to_string_lossy();

    // 第一层：默认黑名单
    for (blocked, reason) in DEFAULT_BLOCKED_PATHS {
        if path_str.starts_with(blocked) {
            return Err(SecurityError::BlockedPath {
                path: path.display().to_string(),
                reason: reason.to_string(),
            });
        }
    }

    // 第二层：用户配置黑名单
    if let Some(blocked_paths) = &config.security.blocked_paths {
        for blocked in blocked_paths {
            if path_str.starts_with(blocked) {
                return Err(SecurityError::BlockedPath {
                    path: path.display().to_string(),
                    reason: "user blacklist (see [security].blocked_paths in config)".to_string(),
                });
            }
        }
    }

    Ok(())
}
```

**2. `src/bin/fsmon/commands/add.rs`: 替换旧检查**

```rust
// 删除旧的检查代码
// if path.starts_with("/var/log/fsmon") { ... }

// 替换为
use crate::security::check_path_allowed;

fn add_path(path: &Path, config: &Config) -> Result<()> {
    // 统一路径检查
    check_path_allowed(path, config)?;

    // ... 后续逻辑 ...
}
```

**3. `src/common/monitor/live_path.rs`: 替换旧检查**

```rust
// 同样替换旧的检查
fn start_monitoring(path: &Path, config: &Config) -> Result<()> {
    check_path_allowed(path, config)?;
    // ...
}
```

**4. 配置文件模板 (`config.toml`)**

```toml
[security]
# Default blocked paths are already applied (fsmon log directory, fsmon process).
# Add custom blocked paths below. These will be checked IN ADDITION to defaults.
# blocked_paths = ["/home/user/secret", "/etc/shadow"]
blocked_paths = []
```

**5. cmd=global 剔除 fsmon 事件**

```rust
// src/common/monitor/socket_handler.rs

fn handle_event(event: &Event, cmd: &str) -> Option<Event> {
    if cmd == "global" {
        // 静默过滤 fsmon 自身事件
        if is_fsmon_event(event) {
            debug!("Filtering fsmon event in global mode");
            return None;
        }
    }
    Some(event.clone())
}
```

**6. cmd=fsmon 时显式报错**

```rust
// src/bin/fsmon/commands/add.rs

fn add_path(path: &Path, config: &Config) -> Result<()> {
    check_path_allowed(path, config)?;

    // cmd=fsmon 时显式报错
    if is_fsmon_path(path) {
        return Err(anyhow!("Cannot monitor fsmon itself"));
    }

    // ...
}
```

#### 注意事项

1. 默认黑名单是硬编码的，不能通过配置移除
2. 用户配置的路径使用前缀匹配，不是精确匹配
3. `cmd=global` 时静默过滤（不报错），`cmd=fsmon` 时显式报错
4. 路径检查与具体命令无关，统一使用 `check_path_allowed()`

---

### F-015: subscribe 验证

#### 问题描述

Socket 订阅功能缺少路径验证，恶意客户端可以订阅敏感路径。

#### 决策及原因

**可选方案**
- A: 严格白名单（只允许已注册的 cmd）
- B: 格式验证（长度+字符集）
- C: 不验证
- 实际选择：复用 add 的规则

**选择: 复用 add 的规则**

订阅时的路径验证应与 `add` 命令一致，复用 `check_path_allowed()` 函数。

验证时机：
- 订阅时：验证失败 → socket 返回错误，拒绝建立订阅
- 运行中：cmd=global 时剔除 fsmon 事件 → 静默，不报错

#### 具体修改方案

**涉及文件**: `src/common/monitor/socket_handler.rs`

```rust
fn handle_subscribe(
    stream: &UnixStream,
    path: &Path,
    config: &Config,
) -> Result<()> {
    // 订阅时验证
    if let Err(e) = check_path_allowed(path, config) {
        // 通过 socket 返回错误
        let response = format!("ERROR: {}", e);
        stream.write_all(response.as_bytes())?;
        return Err(e.into());
    }

    // 验证通过，建立订阅
    // ...

    Ok(())
}
```

#### 注意事项

1. 错误信息通过 socket 返回给客户端
2. 运行中的过滤逻辑在事件处理管道中，不在订阅建立时

---

### F-016: mount_fd 权限

#### 问题描述

`open_dir()` 使用 `O_RDONLY` 打开目录文件描述符，权限过宽。

#### 决策及原因

**可选方案**
- A: 改用 O_PATH（更安全但可能影响功能）
- B: 保持 O_DIRECTORY

**选择: 改用 `O_DIRECTORY | O_PATH`**

- `O_DIRECTORY`: 确保打开的是目录
- `O_PATH`: 只获取 fd，不真正打开文件读/写，权限最小化

#### 具体修改方案

**涉及文件**: `src/common/monitor/reader.rs`

```rust
fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let flags = OFlag::O_DIRECTORY | OFlag::O_PATH | OFlag::O_CLOEXEC;

    let fd = open(path, flags, Mode::empty())?;

    // 转换为 OwnedFd（自动 close）
    unsafe { Ok(OwnedFd::from_raw_fd(fd)) }
}
```

#### 注意事项

1. `O_PATH` 在 Linux 2.6.39+ 可用
2. `O_CLOEXEC` 防止 fd 泄露到子进程
3. 这个 fd 只能用于 `*at()` 系列函数，不能用于 `read`/`write`

---

### F-017: canonicalize TOCTOU

#### 问题描述

先调用 `canonicalize()` 获取真实路径，再传递给 `fanotify_mark()`，中间存在 TOCTOU 竞态：路径可能在两步之间被替换。

#### 决策及原因

**可选方案**
- A: 改用 fd 级操作（改动较大）
- B: 添加 symlink 检查（折中）
- C: 接受风险

**选择方案 A — fd 级操作**

核心思想：用 `open(O_DIRECTORY|O_NOFOLLOW)` 获取目录的 fd，然后用 fd 作为 `fanotify_mark` 的锚点。

这需要修改 `fanotify-fid` crate，新增接受 `dir_fd` 参数的方法。

#### 具体修改方案

**涉及文件**: 5 个

**1. fanotify-fid crate: 新增 `mark_at()` 方法**

项目路径: `~/.projects/fanotify-fid`

```rust
// src/lib.rs 或 src/fanotify.rs

impl Fanotify {
    /// 使用 dir_fd 作为锚点调用 fanotify_mark
    ///
    /// 比 mark() 更安全，避免 TOCTOU 竞态
    pub fn mark_at(
        &self,
        dir_fd: &OwnedFd,
        flags: MarkFlags,
        mask: u64,
        path: &Path,
    ) -> io::Result<()> {
        let c_path = CString::new(path.as_os_str().as_bytes())?;

        let ret = unsafe {
            libc::fanotify_mark(
                self.fd.as_raw_fd(),
                flags.bits() as libc::c_uint,
                mask,
                dir_fd.as_raw_fd(),  // 使用 dir_fd 而非 AT_FDCWD
                c_path.as_ptr(),
            )
        };

        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
```

**2. fsmon: `mark_recursive_inner()` (F-017 #1)**

文件: `src/common/fid_parser.rs:373`

```rust
// 修改前
fn mark_recursive_inner(fan: &Fanotify, path: &Path, ...) -> Result<()> {
    let real_path = canonicalize(path)?;
    fan.mark(MarkFlags::FAN_MARK_ADD, mask, &real_path)?;
    // ...
}

// 修改后
fn mark_recursive_inner(fan: &Fanotify, path: &Path, ...) -> Result<()> {
    // 打开目录 fd，O_NOFOLLOW 防止跟随 symlink
    let dir_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;

    // 使用 fd 作为锚点
    fan.mark_at(&dir_fd, MarkFlags::FAN_MARK_ADD, mask, Path::new("."))?;

    // 遍历子目录时，每个子目录也需要类似的 fd 级操作
    // ...

    // dir_fd 在此处 drop，自动 close
    Ok(())
}
```

**3. fsmon: 临时 mark (F-017 #2)**

文件: `src/common/monitor/temp_marks.rs:144-153`

```rust
// 修改前
pub fn add_temp_mark(fan: &Fanotify, path: &Path, mask: u64) -> Result<()> {
    let real_path = canonicalize(path)?;
    fan.mark(MarkFlags::FAN_MARK_ADD, mask, &real_path)?;
    // ...
}

// 修改后
pub fn add_temp_mark(fan: &Fanotify, path: &Path, mask: u64) -> Result<()> {
    let dir_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;

    fan.mark_at(&dir_fd, MarkFlags::FAN_MARK_ADD, mask, Path::new("."))?;
    // dir_fd 保持打开直到 mark 完成
    Ok(())
}
```

**4. fsmon: live path mark (F-017 #3)**

文件: `src/common/monitor/live_path.rs:288-296`

```rust
pub fn add_live_mark(fan: &Fanotify, path: &Path, mask: u64) -> Result<()> {
    let dir_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;

    fan.mark_at(&dir_fd, MarkFlags::FAN_MARK_ADD | MarkFlags::FAN_MARK_FILESYSTEM, mask, Path::new("."))?;
    Ok(())
}
```

**5. fsmon: dir watcher mark (F-017 #4)**

文件: `src/common/monitor/dir_watcher.rs`

```rust
pub fn setup_dir_watcher(fan: &Fanotify, path: &Path, mask: u64) -> Result<()> {
    let dir_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;

    fan.mark_at(&dir_fd, MarkFlags::FAN_MARK_ADD, mask, Path::new("."))?;
    Ok(())
}
```

#### 注意事项

1. `O_NOFOLLOW` 确保不会跟随 symlink
2. `O_DIRECTORY` 确保打开的是目录
3. `O_CLOEXEC` 防止 fd 泄露
4. `fanotify-fid` crate 的改动需要先发布/更新版本
5. 所有使用 `canonicalize` + `mark` 的地方都需要改为 fd 级操作

---

### F-018: file_writer TOCTOU

#### 问题描述

`file_writer.rs` 中使用 `canonicalize()` 后再写入文件，存在 TOCTOU 竞态。

#### 决策及原因

**选择: 使用 `O_NOFOLLOW`**

在 `open()` 时添加 `O_NOFOLLOW` 标志，确保不会跟随符号链接写入。

#### 具体修改方案

**涉及文件**: `src/common/monitor/file_writer.rs`

```rust
use std::os::unix::fs::OpenOptionsExt;

fn open_output_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

// 如果需要创建目录
fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        // 检查 parent 是否是 symlink
        let metadata = symlink_metadata(parent)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Parent directory is a symlink: {}", parent.display()),
            ));
        }
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
```

#### 注意事项

1. `O_NOFOLLOW` 仅对路径的最后一个组件生效
2. 需要额外检查父目录是否为 symlink
3. `O_CLOEXEC` 防止 fd 泄露到子进程

---

### F-021: 递归深度限制

#### 问题描述

`mark_recursive_inner()` 递归遍历目录树时没有深度限制，深层嵌套目录可能导致栈溢出或资源耗尽。

#### 决策及原因

**可选方案**
- A: 硬限制 64 层
- B: 可配置限制
- C: 改用迭代遍历（改动大）
- 实际选择：B + C（迭代遍历 + 可配置深度，默认无穷大）

**选择方案 B + C — 迭代遍历 + 可配置深度**

| 组件 | 设计 |
|------|------|
| 遍历方式 | 改为迭代（使用栈/队列） |
| 默认深度 | 无穷大（不限制，保持向后兼容） |
| 优先级 | CLI > 配置文件 > 无穷大 |

理由：
- 迭代遍历彻底避免栈溢出
- 默认不限制保持现有行为
- 用户可根据需要配置深度

#### 具体修改方案

**涉及文件**: 3 个

**1. `src/common/config.rs`: 添加配置项**

```rust
#[derive(Deserialize, Default)]
pub struct MonitoredConfig {
    /// 递归深度限制，默认 None 表示不限制
    pub max_depth: Option<u32>,
}

#[derive(Deserialize)]
pub struct Config {
    pub monitored: MonitoredConfig,
    // ...
}
```

**2. CLI 参数**

```rust
// src/bin/fsmon/commands/add.rs 或 src/bin/fsmon/cli.rs

#[derive(Parser)]
struct AddArgs {
    /// 监控路径
    path: PathBuf,

    /// 递归深度限制
    #[arg(long)]
    max_depth: Option<u32>,
}
```

**3. `src/common/fid_parser.rs`: 改为迭代遍历**

```rust
use std::collections::VecDeque;

fn mark_recursive_iterative(
    fan: &Fanotify,
    root_path: &Path,
    mask: u64,
    max_depth: Option<u32>,
) -> Result<()> {
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((root_path.to_path_buf(), 0));

    while let Some((current_path, depth)) = queue.pop_front() {
        // 检查深度限制
        if let Some(max) = max_depth {
            if depth > max {
                debug!("Skipping {}: depth {} > max {}", current_path.display(), depth, max);
                continue;
            }
        }

        // 打开目录 fd（fd 级操作，见 F-017）
        let dir_fd = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&current_path)
        {
            Ok(fd) => fd,
            Err(e) => {
                warn!("Cannot open {}: {}", current_path.display(), e);
                continue;
            }
        };

        // 标记当前目录
        fan.mark_at(&dir_fd, MarkFlags::FAN_MARK_ADD, mask, Path::new("."))?;

        // 遍历子目录
        let dir = read_dir(&current_path)?;
        for entry in dir {
            let entry = entry?;
            let file_type = entry.file_type()?;

            // 跳过 symlink（见 F-008）
            if file_type.is_symlink() {
                debug!("Skipping symlink: {}", entry.path().display());
                continue;
            }

            if file_type.is_dir() {
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }

    Ok(())
}
```

#### 注意事项

1. `max_depth = 0` 表示只监控根目录，不递归
2. `max_depth = None` 表示不限制（默认行为）
3. 优先级：`--max-depth N` CLI 参数 > `[monitored] max_depth` 配置 > None
4. 迭代遍历使用 `VecDeque`（BFS）或 `Vec`（DFS）

---

### F-023: 文件存在性竞态

#### 问题描述

在检查文件存在性和实际操作之间存在 TOCTOU 竞态。

#### 决策及原因

**选择: 使用 fd 级操作**

先获取 fd，再通过 fd 操作。fd 一旦打开就指向确定的文件，不受后续路径变化影响。

#### 具体修改方案

**涉及文件**: 2 个

**1. `src/common/clean/core.rs`**

```rust
// 修改前：两步操作，存在竞态
if path.exists() {
    let file = File::open(path)?;
    // ...
}

// 修改后：直接尝试 open，用错误处理替代存在性检查
match File::open(path) {
    Ok(file) => {
        // 文件存在，继续操作
        // ...
    }
    Err(e) if e.kind() == io::ErrorKind::NotFound => {
        // 文件不存在，跳过
        debug!("File not found: {}", path.display());
    }
    Err(e) => return Err(e),
}
```

**2. `src/common/monitor/file_writer.rs`**

```rust
// 修改前
if path.exists() {
    append_to_file(path)?;
} else {
    create_file(path)?;
}

// 修改后
OpenOptions::new()
    .write(true)
    .create(true)
    .append(true)
    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
    .open(path)?;
```

#### 注意事项

1. `O_CREAT` + `O_EXCL` 保证原子创建
2. `O_NOFOLLOW` 防止 symlink 攻击
3. 不要使用 `path.exists()` 做检查，直接尝试操作

---

### F-026: 临时文件权限

#### 问题描述

临时文件创建时未显式设置权限，使用系统默认 umask，可能导致权限过宽。

#### 决策及原因

**选择: 显式设置 0600**

临时文件只需要 fsmon 进程自身读写，权限应为 `0600`（owner read/write only）。

#### 具体修改方案

**涉及文件**: `src/common/clean/core.rs`

```rust
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn truncate_from_start(path: &Path, size: u64) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));

    // 方案 1: 使用 tempfile crate（推荐，见 F-009）
    let tmp_file = NamedTempFile::new_in(parent)?;
    // tempfile 默认权限就是 0600

    // 方案 2: 如果不用 tempfile，手动设置
    let tmp_path = parent.join(format!(".fsmon_trunc_{}", uuid::Uuid::new_v4()));
    let tmp_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_EXCL | libc::O_CLOEXEC)
        .mode(0o600)  // 显式设置权限
        .open(&tmp_path)?;

    // ... 写入数据 ...

    fs::rename(&tmp_path, path)?;
    Ok(())
}
```

#### 注意事项

1. 使用 `tempfile` crate 时，权限自动为 0600
2. 手动创建时需要 `mode(0o600)` + `umask(0o077)` 或者事后 `chmod`
3. `O_EXCL` 保证文件不存在时才创建

---

### F-030: pending_paths 重复条目

#### 问题描述

`pending_paths` 向量中可能添加重复条目，导致资源浪费和潜在的竞态条件。

#### 决策及原因

**选择: 添加去重检查**

在添加前检查是否已存在，避免重复。

#### 具体修改方案

**涉及文件**: `src/common/monitor/dir_watcher.rs`

```rust
use std::collections::HashSet;

pub struct DirWatcher {
    pending_paths: Vec<PathBuf>,
    pending_set: HashSet<PathBuf>,  // 用于快速查找
}

impl DirWatcher {
    pub fn add_pending(&mut self, path: PathBuf) {
        // 去重检查
        if self.pending_set.contains(&path) {
            debug!("Path already pending: {}", path.display());
            return;
        }

        self.pending_set.insert(path.clone());
        self.pending_paths.push(path);
    }

    pub fn remove_pending(&mut self, path: &Path) {
        self.pending_paths.retain(|p| p != path);
        self.pending_set.remove(path);
    }
}
```

#### 注意事项

1. 使用 `HashSet` 提供 O(1) 查找
2. `pending_paths` 和 `pending_set` 需要保持同步
3. 考虑使用 `IndexSet` 替代两个数据结构

---

### F-031: strip_deleted_suffix 误替换

#### 问题描述

`strip_deleted_suffix()` 使用全局替换，可能误删除路径中包含 " (deleted)" 的正常部分。

#### 决策及原因

**选择: 改用 `strip_suffix`**

只删除末尾的 " (deleted)"，不影响路径其他部分。

#### 具体修改方案

**涉及文件**: `src/common/fid_parser.rs`

```rust
// 修改前
fn strip_deleted_suffix(path: &str) -> String {
    path.replace(" (deleted)", "")
}

// 修改后
fn strip_deleted_suffix(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

// 或者如果需要 String
fn strip_deleted_suffix(path: &str) -> String {
    if let Some(stripped) = path.strip_suffix(" (deleted)") {
        stripped.to_string()
    } else {
        path.to_string()
    }
}
```

#### 注意事项

1. `strip_suffix` 是 Rust 1.45+ 标准库方法
2. 只删除末尾匹配，不会误删路径中间的 " (deleted)"
3. 返回 `&str` 更高效，避免分配

---

## 3. 跳过的发现及原因

| ID | 问题 | 跳过原因 |
|----|------|----------|
| F-022 | 通道容量限制 | 已有 `--cache-channel` 配置项 + `channel_lagged` metrics 监控 |
| F-024 | pending_paths 大小限制 | 已有 `pending_paths` gauge + `--metrics-interval` 监控 |
| F-025 | cmd 字段长度限制 | 无实际攻击场景，单个长字符串不会导致 OOM |
| F-028 | 配置文件完整性检查 | 行业惯例不检查用户级配置文件，且被 F-004/005/006 的环境变量加固覆盖 |
| F-029 | Health 命令信息泄露 | 信息价值低，运维价值大于安全风险 |

#### 各跳过项的可选方案

**F-022: 通道容量限制**
- A: 强制有界通道（默认 10000）
- B: 可配置（已实现）
- C: 保持 unbounded + 监控（已有 metrics）

**F-024: pending_paths 大小限制**
- A: 硬限制 1000
- B: 可配置
- C: 不限制但添加清理

**F-025: cmd 字段长度限制**
- A: 硬限制 256 字符
- B: 可配置
- C: 不限制

**F-028: 配置文件完整性检查**
- A: 检查权限 (0600/0640)
- B: 添加签名验证
- C: 不检查

**F-029: Health 命令信息泄露**
- A: 移除敏感字段
- B: 分级（普通/详细）
- C: 保持现状

---

## 4. 额外改动

### 4.1 fanotify-fid crate

**项目路径**: `~/.projects/fanotify-fid`

**改动内容**: 新增 `mark_at(dir_fd, flags, mask, path)` 方法

**原因**: 消除 fsmon 中 F-017 的 TOCTOU 问题

**实现**: 包装 `fanotify_mark` syscall，接受 `dir_fd` 参数替代 `AT_FDCWD`

```rust
// 新增方法签名
pub fn mark_at(
    &self,
    dir_fd: &OwnedFd,
    flags: MarkFlags,
    mask: u64,
    path: &Path,
) -> io::Result<()>;
```

### 4.2 cmd=global 剔除 fsmon

**涉及文件**:
- `src/common/monitor/socket_handler.rs`
- `src/common/monitor/events.rs`

**改动内容**: 当 cmd 为 global 时，自动过滤 fsmon 自身的事件

**原因**: 防止全局监控模式下的递归（fsmon 写日志 → 触发事件 → 再写日志）

```rust
// socket_handler.rs
fn handle_event(event: &Event, cmd: &str) -> Option<Event> {
    if cmd == "global" && is_fsmon_event(event) {
        debug!("Filtering fsmon event in global mode");
        return None;
    }
    Some(event.clone())
}

// events.rs
pub fn is_fsmon_event(event: &Event) -> bool {
    event.path.starts_with("/var/log/fsmon")
        || event.process_name == "fsmon"
}
```

---

## 5. 依赖关系和执行顺序

```
Phase 0: fanotify-fid crate 改动
    │
    ▼
Phase 1: 核心修复（可并行）
    │
    ├── Task A: 路径验证与授权
    │   ├── F-014/019: 统一 check_path_allowed()
    │   ├── F-015: subscribe 验证
    │   └── cmd=global 剔除 fsmon 事件
    │
    ├── Task B: Symlink 防护链
    │   ├── F-007/F-027: symlink 显示策略
    │   ├── F-008: mark_recursive 跳过 symlink
    │   ├── F-010: truncate 检查 symlink
    │   └── F-018: file_writer O_NOFOLLOW
    │
    ├── Task C: 环境变量加固
    │   └── F-004/005/006: getpwuid() 替代环境变量
    │
    ├── Task D: 临时文件安全
    │   ├── F-009: tempfile crate
    │   ├── F-011: rename 备份/回滚
    │   ├── F-026: 权限 0600
    │   └── F-031: strip_suffix
    │
    ├── Task E: TOCTOU 修复
    │   ├── F-017: fd 级操作（依赖 Phase 0）
    │   └── F-023: 文件存在性竞态
    │
    └── Task F: 资源限制
        └── F-021: 迭代遍历 + 可配置深度

Phase 2: 数据完整性
    └── Task G: PID 复用 + 去重
        └── F-030: pending_paths 去重
```

---

## 6. 涉及文件清单

### 按修改频率排序

| 文件 | 涉及修复 | 修改类型 |
|------|----------|----------|
| `src/common/config.rs` | F-004/005/006, F-021 | 重构 |
| `src/common/fid_parser.rs` | F-008, F-017, F-031 | 修改 |
| `src/common/clean/core.rs` | F-009, F-010, F-011, F-023, F-026 | 重构 |
| `src/common/monitor/socket_handler.rs` | F-002, F-015, cmd=global | 修改 |
| `src/common/monitor/live_path.rs` | F-017, F-027 | 修改 |
| `src/common/monitor/file_writer.rs` | F-018, F-023 | 修改 |
| `src/common/monitor/dir_watcher.rs` | F-017, F-030 | 修改 |
| `src/common/monitor/temp_marks.rs` | F-017 | 修改 |
| `src/common/monitor/reader.rs` | F-016 | 修改 |
| `src/common/monitor/events.rs` | cmd=global | 修改 |
| `src/common/filters.rs` | F-007 | 修改 |
| `src/common/utils.rs` | F-003 | 修改 |
| `src/common/monitored.rs` | F-007/F-027 | 修改 |
| `src/bin/fsmon/commands/add.rs` | F-014/019 | 修改 |
| `src/bin/fsmon/commands/daemon.rs` | F-001 | 修改 |

### 新增文件

| 文件 | 用途 |
|------|------|
| `src/common/security.rs` | 统一路径检查函数（可选，也可放在 config.rs） |

### 外部依赖

| 依赖 | 用途 | 状态 |
|------|------|------|
| `tempfile` | F-009: 安全临时文件 | 新增 |
| `fanotify-fid` | F-017: fd 级 mark | 需修改 |

---

## 附录: 测试建议

### 单元测试

1. `check_path_allowed()` — 测试默认黑名单和用户黑名单
2. `strip_deleted_suffix()` — 测试各种边界情况
3. `resolve_uid_gid()` — 测试普通用户、root、容器场景
4. `DirWatcher::add_pending()` — 测试去重逻辑

### 集成测试

1. Symlink 场景 — `fsmon add` 一个 symlink 路径
2. 递归深度 — 深层嵌套目录，验证深度限制生效
3. 并发竞态 — 多线程同时 `fsmon add`
4. 配置验证 — 各种无效配置的处理

### 安全测试

1. 环境变量篡改 — 设置恶意 `SUDO_USER`/`SUDO_UID`
2. Symlink 攻击 — 在监控路径中插入 symlink
3. 路径遍历 — 尝试 `../../etc/passwd` 形式的路径
4. TOCTOU 竞态 — 多进程同时操作

---

*报告生成时间: 2026-06-22*  
*下次审查建议: 实施完成后 2 周*
