# 安全漏洞详细报告：文件路径解析与符号链接

## 漏洞 F-001: resolve_recursion_check 调用 canonicalize 跟随符号链接

| 属性 | 值 |
|------|-----|
| **文件** | `src/common/filters.rs` |
| **行号** | 25 |
| **类别** | symlink-following |
| **严重性** | HIGH |
| **置信度** | 0.90 |

### 描述

`resolve_recursion_check()` (行 22-25) 对 expanded 路径调用 `canonicalize()`，该调用会解析路径中所有符号链接组件。代码注释明确说明 "follows symlinks"，但没有任何后续验证来确保解析后的路径落在预期范围内。

```rust
// filters.rs:22-25
pub fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = crate::common::config::expand_tilde(path, &home);
    expanded.canonicalize().unwrap_or(expanded)  // 跟随所有符号链接
}
```

此外，函数在行 23 使用 `std::env::var("HOME")` 获取主目录，而非 `config.rs` 中更安全的 `guess_home()` 函数（后者在 root/sudo 场景下通过 `getpwuid` 解析），存在 HOME 环境变量不一致风险。

### 攻击场景

1. 攻击者在用户可写目录创建 symlink: `ln -s /etc /tmp/workspace/configs`
2. 执行 `fsmon add /tmp/workspace/configs --recursive`
3. `resolve_recursion_check` 在行 25 调用 `canonicalize`，将 `/tmp/workspace/configs` 解析为 `/etc`
4. `add_path` 在 live_path.rs 行 112 获取 `canonical=/etc`
5. `mark_recursive` 在行 156 递归标记 `/etc` 下所有子目录
6. `/etc/passwd`、`/etc/shadow` 等敏感文件的读写事件被泄露给监控者

### 修复建议

1. 在 `resolve_recursion_check` 返回后增加路径黑名单校验（`/etc`, `/root`, `/proc`, `/sys`, `/dev`, `/boot` 等）
2. 可选增加白名单模式，限制监控范围为用户主目录和 `/tmp` 等临时目录
3. 统一使用 `config::guess_home()` 代替直接读取 `HOME` 环境变量
4. 或者使用 `dunce::canonicalize` 仅解析中间路径组件，不跟随最后一个组件的符号链接

---

## 漏洞 F-002: mark_recursive_inner 跟随目录符号链接递归标记非预期路径

| 属性 | 值 |
|------|-----|
| **文件** | `src/common/fid_parser.rs` |
| **行号** | 395 |
| **类别** | symlink-following |
| **严重性** | HIGH |
| **置信度** | 0.92 |

### 描述

`mark_recursive_inner()` (行 386-399) 使用 `fs::read_dir()` 遍历目录条目，在行 395 通过 `path.is_dir()` 判断是否递归进入子目录。Rust 的 `Path::is_dir()` 底层调用 `stat()`，会跟随符号链接解析目标。

```rust
// fid_parser.rs:386-399
fn mark_recursive_inner(fan_fd: &OwnedFd, safe_mask: u64, dir: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return discovered,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {  // 跟随符号链接！没有 is_symlink() 检查
            let _ = fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path.as_path());
            discovered.push(path.clone());
            discovered.extend(mark_recursive_inner(fan_fd, safe_mask, &path));
        }
    }
    discovered
}
```

代码中没有 `is_symlink()` 检查来过滤符号链接目录条目。该函数被 `live_path.rs:156`、`live_path.rs:238`、`init.rs:142`、`dir_watcher.rs:245` 多处调用，影响面广。

### 攻击场景

在已被监控的目录 `/data/app` 中，攻击者创建: `ln -s /etc /data/app/system_config`

当 fsmon 执行 `mark_recursive` 时：
1. `read_dir` 发现 `system_config` 条目
2. `is_dir()` 返回 true（因为 stat 跟随到 `/etc` 是目录）
3. 递归标记 `/etc` 及其所有子目录（`/etc/ssh`、`/etc/sudoers.d` 等）
4. 之后 `/etc` 中任何文件的访问、修改、创建、删除都会触发事件回调

### 修复建议

在 `mark_recursive_inner` 的 for 循环中，在 `is_dir()` 判断前增加符号链接过滤：

```rust
for entry in entries.flatten() {
    let path = entry.path();
    // 新增：跳过符号链接
    if entry.file_type().map_or(false, |ft| ft.is_symlink()) {
        continue;
    }
    if path.is_dir() {
        // ... 原有逻辑
    }
}
```

---

## 漏洞 F-003: pending 路径机制存在符号链接注入窗口

| 属性 | 值 |
|------|-----|
| **文件** | `src/common/monitor/dir_watcher.rs` |
| **行号** | 265 |
| **类别** | TOCTOU |
| **严重性** | MEDIUM |
| **置信度** | 0.60 |

### 描述

`check_pending()` (dir_watcher.rs:265-300) 处理 `add_path` 时不存在的路径。当 inotify 检测到目录创建事件后，`check_pending` 调用 `add_path` 重新处理。

```rust
// dir_watcher.rs:265-300
pub(crate) fn check_pending(&mut self) {
    // ...
    let mut i = 0;
    while i < self.inotify_state.pending_paths.len() {
        if self.inotify_state.pending_paths[i].0.exists() {
            let entry = self.inotify_state.pending_paths.remove(i);
            match self.add_path(&entry.1) {  // 重新走完整流程，包括符号链接跟随
                // ...
            }
        } else {
            i += 1;
        }
    }
}
```

在等待期间，攻击者可在该路径位置创建一个指向敏感目录的符号链接。当 inotify 触发后，`add_path → resolve_recursion_check → canonicalize` 会跟随这个新创建的符号链接。

### 攻击场景

1. 用户执行 `fsmon add /tmp/newproject --recursive`，但 `/tmp/newproject` 不存在
2. 路径进入 `pending_paths` 等待
3. 攻击者在 `/tmp/newproject` 位置创建 symlink: `ln -s /etc/ssh /tmp/newproject`
4. inotify 检测到创建事件，`check_pending` 调用 `add_path`
5. `resolve_recursion_check` 解析 `/tmp/newproject → /etc/ssh`
6. `/etc/ssh` 被递归标记监控，SSH 密钥和配置文件的访问事件被泄露

### 修复建议

1. 在 `check_pending` 重新调用 `add_path` 时，检查目标路径是否为符号链接
2. 在 pending 路径被创建时，验证解析后路径是否仍在用户最初指定的范围内
3. 使用 `open(O_PATH|O_NOFOLLOW)` 检查新创建的路径是否为符号链接

---

## 漏洞 F-004: 路径解析后无作用域验证

| 属性 | 值 |
|------|-----|
| **文件** | `src/common/monitor/live_path.rs` |
| **行号** | 112 |
| **类别** | path-traversal |
| **严重性** | MEDIUM |
| **置信度** | 0.75 |

### 描述

`add_path()` 在行 29 通过 `resolve_recursion_check` 解析路径后，在行 112 再次调用 `canonicalize()`。解析后的 canonical 路径直接用于 fanotify mark 操作，没有任何验证确保该路径属于用户预期的监控范围。

```rust
// live_path.rs:112
let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
```

代码中只检查了与 log 目录的冲突（行 73-86），没有检查与系统敏感目录的冲突。

### 攻击场景

非 root 用户创建 symlink: `ln -s /var/log/auth /tmp/monitored_logs`，然后通过 `fsmon add /tmp/monitored_logs --recursive` 获取 `/var/log/auth` 的文件系统事件，可能泄露认证日志中的其他用户登录信息。

### 修复建议

1. 增加敏感目录黑名单，在 `add_path` 中检查 canonical 路径是否落入受限目录
2. 对非特权用户（euid != 0），验证解析后路径的读权限以及是否为符号链接
3. 在 daemon 模式下，对用户提交的路径记录审计日志，标记所有通过符号链接解析的路径

---

## 漏洞 F-005: 递归标记无深度限制

| 属性 | 值 |
|------|-----|
| **文件** | `src/common/fid_parser.rs` |
| **行号** | 390 |
| **类别** | resource-exhaustion |
| **严重性** | MEDIUM |
| **置信度** | 0.72 |

### 描述

`mark_recursive_inner()` 递归遍历子目录时没有深度限制参数。当符号链接目录被当作普通目录递归进入时（结合 F-002），监控范围可从一个小型目录树扩展到系统多个大型目录树的并集。

### 攻击场景

攻击者在被监控的目录中创建多个 symlink 指向大型系统目录：
```bash
ln -s /usr/lib /data/app/lib1
ln -s /usr/share /data/app/lib2
ln -s /var /data/app/var
```

当 fsmon 执行 `mark_recursive` 时，会遍历 `/usr/lib`（数万文件）、`/usr/share`（数万文件）和 `/var` 的全部内容，消耗大量 CPU 时间和内存。

### 修复建议

1. 增加递归深度参数 `max_depth`（建议默认值 32）
2. 结合 F-002 修复，跳过符号链接目录
3. 可选增加已 mark 目录计数器，超过阈值（如 100,000）时停止递归

---

## 代码引用

### filters.rs (行 22-25)
```rust
pub fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = crate::common::config::expand_tilde(path, &home);
    expanded.canonicalize().unwrap_or(expanded)
}
```

### fid_parser.rs (行 386-399)
```rust
fn mark_recursive_inner(fan_fd: &OwnedFd, safe_mask: u64, dir: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return discovered,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = fanotify_mark(fan_fd, FAN_MARK_ADD, safe_mask, AT_FDCWD, path.as_path());
            discovered.push(path.clone());
            discovered.extend(mark_recursive_inner(fan_fd, safe_mask, &path));
        }
    }
    discovered
}
```

### live_path.rs (行 92-112)
```rust
if !path.exists() {
    // ... pending logic ...
    return Ok(());
}

let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
```
