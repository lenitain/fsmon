# fsmon PROGRESS — 全面代码审查报告 (2026-05-06)

## 审查范围

- 10 个 `.rs` 源文件 + 1 个 bin
- clippy `-D warnings` 通过 ✅
- 81 tests pass, 7 ignored (sudo) ✅
- 审查维度: 正确性、边界条件、并发安全、失效模式、代码质量

---

## 🔴 A. 确认 Bug

### B1 (高) — `monitor.rs::run()` dir_cache 预填充全部丢失

**文件**: `src/monitor.rs`

**问题**: `mem::take(&mut self.dir_cache)` 在 spawn reader tasks 之前执行，
此时 `self.dir_cache` 还是空的。`Arc<Mutex<HashMap>>` 被所有 task clone 后，下方才填充
`self.dir_cache`，但 Arc 指向旧空 HashMap。所有预缓存的目录 handle 全部浪费。

**代码位置**:
```rust
// run() 中 —— 先 take 再 spawn，然后才填充 self.dir_cache
let dir_cache = Arc::new(Mutex::new(std::mem::take(&mut self.dir_cache)));
// ... spawn reader tasks (clone 了空 cache) ...
for (i, canonical) in self.canonical_paths.iter().enumerate() {
    dir_cache::cache_recursive(&mut self.dir_cache, canonical); // 填到错误的 HashMap
}
```

**后果**: 所有 reader task 的 handle 缓存为空。`read_fid_events` 的二阶段恢复
（被删目录的子文件路径）无法利用预缓存的句柄。首次事件解析可能失败。

**修复**: 把 `mem::take` + Arc + spawn 移到预缓存循环之后。

---

### B2 (高) — `cmd_remove` 未规范化路径 ✅ 已修复 (2026-05-06)

**文件**: `src/bin/fsmon.rs`

**问题**: `cmd_add` 中路径被 `expand_tilde` + `canonicalize` 后存入 store。但
`cmd_remove` 直接用原始 CLI 参数做 `store.remove_entry(&path)`，无 tilde 展开、无 canonicalize。

`fsmon add /tmp/foo/../bar` → store 存 `/tmp/bar`
`fsmon remove /tmp/foo/../bar` → 找不到

**修复**: 在 `remove_entry` 前对 `path` 执行同样规范化: `expand_tilde` → `canonicalize`。

---

### B3 (中) — `remove_path` 索引假设 `mount_fds[pos]` 与 `paths[pos]` 一一对应

**文件**: `src/monitor.rs`

**问题**:
```rust
// pos 来自 self.paths.iter().position()
if pos < self.mount_fds.len() {
    unsafe { libc::close(self.mount_fds[pos]) };
    self.mount_fds.remove(pos);
}
```
`mount_fds` 与 `paths` 并非严格 1:1 对应（同一文件系统的多路径共享 fd、
`add_path` 引入新 fd 后顺序不对齐）。`pos < len` 仅防止越界崩溃，
但可能关闭**其他路径**的 mount fd，导致文件句柄解析失败。

**修复**: 根据被移除的 canonical_path 搜索对应的 mount_fd，或跟踪 path→fd 映射。

---

### B4 (中) — `write_event` unmatched 路径警告无去重

**文件**: `src/monitor.rs`

**问题**: 注释写着 "Warn once per unique unmatched path to avoid log spam"，
但代码实际无条件输出:
```rust
None => {
    // Warn once per unique unmatched path to avoid log spam
    eprintln!("[WARNING] Event not matched to any monitored path: {}", ...);
    return Ok(());
}
```
高频率事件会导致 stderr 被刷屏。

**修复**: 用 `HashSet<PathBuf>` 记录已警告路径，去重后打印。

---

### B5 (中) — `truncate_from_start` 并发临时文件冲突

**文件**: `src/lib.rs`

**问题**: 所有日志文件清理共享同一个临时文件名:
```rust
let tmp_path = dir.join(".fsmon_trunc_tmp");
```
同时运行 `fsmon clean --path /a` 和 `fsmon clean --path /b`，
两者使用同一个 `.fsmon_trunc_tmp`，导致数据错乱和损坏。

**修复**: 使用唯一临时文件名（如 `format!(".fsmon_trunc_{}", std::process::id())` 或使用 `tempfile` crate）。

---

### B6 (中) — `persist_config` 静默忽略保存失败

**文件**: `src/monitor.rs`

**问题**:
```rust
if let Some(ref store_path) = self.store_path
    && let Ok(mut store) = Store::load(store_path)
{
    store.entries = entries;
    let _ = store.save(store_path);  // 错误被完全吞掉
}
```
磁盘满、权限不足等永久失败被静默忽略，daemon 内存状态与磁盘不一致，
后续 socket 操作无法回退。

**修复**: 至少 `eprintln` 警告，或返回错误给调用方（让 CLI 知道持久化失败）。

---

### B7 (低) — `count_lines` 无边界校验

**文件**: `src/lib.rs`

**问题**:
```rust
fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    let mut buf = vec![0u8; upto];
    f.read_exact(&mut buf)?;
```
若 `upto > file_size`，`read_exact` 返回 `UnexpectedEof`。
目前调用方保证 `upto ≤ file_size`（来自 `find_tail_offset`），但无防御性校验。

**修复**: 用 `read_to_end` 替代，或加 `upto.min(file_len)` 保护。

---

### B8 (低) — `path_to_log_name` 编码膨胀导致超长文件名 ✅ 已修复 (2026-05-06)

**文件**: `src/utils.rs`

**问题**: 转义规则 `!`→`!!`、`_`→`!_`、`/`→`_` 使每个特殊字符变 2 字节。
含大量 `_`/`!` 的路径编码后可能超过 255 字节（Linux 文件名限制），
导致 `write_event` 创建日志文件失败。

**修复**: 使用 FNV-1a 64-bit 确定性哈希替代全路径编码，文件名固定为 `{:016x}.toml`（21 字节）。
原始路径作为 `FileEvent.monitored_path` 字段保留在每个事件中（TOML/CSV 均可解析），
用户可用自己的过滤策略按此字段检索，不影响查询体验。

---

### B9 (极低) — `read_fid_events` 在 async context 中持 std::sync::Mutex

**文件**: `src/monitor.rs` + `src/fid_parser.rs`

**问题**:
```rust
let events = fid_parser::read_fid_events(fd, &mfds, &mut dc.lock().unwrap(), &mut buf);
```
`std::sync::Mutex` 在 tokio 任务中被持有整个解析周期（包含 `open_by_handle_at`
磁盘 I/O），阻塞当前 worker 线程。多 fd 时可能影响并发性能。

**修复**: 使用 `tokio::sync::Mutex`（不持有 across .await 即可）或使用
`dashmap` 替代（因为 dir_cache 已在多处使用）。

---

## 🟡 B. 设计问题 / 语义不一致

### D1 — `FileEvent.size_change` 名不副实

**文件**: `src/lib.rs`

字段名暗示"增量"(delta)，但实际值为**绝对文件大小**:

| EventType | size_change 实际含义 |
|-----------|-------------------|
| CREATE/MODIFY/CLOSE_WRITE | 当前文件大小（非增量） |
| DELETE/DELETE_SELF/MOVED_FROM | 删除前缓存的大小 |
| 其他 | 缓存大小或 0 |

CSV header 输出 `size_change`，external API 会误导调用方。

**建议**: 改名为 `file_size` 或 `current_size`，或为 MODIFY 事件计算真实 delta。

---

### D2 — `size_change: i64` 负数语义不清

Delete 事件 `size_change` 恒为非负（`cached_size as i64`）。
仅在 human 输出中使用符号前缀 `+`/`-`，但 TOML/CSV 中看不出正负含义。
`format_size` 的 `-` prefix 只在前端展示时有效。

---

### D3 — 死代码

| 文件 | 函数 | 原因 |
|------|------|------|
| `src/output.rs` | `output_event()` | 从未调用。实际输出走 `write_event` + `query::output_events` |
| `src/socket.rs` | `listen()` / `read_toml_message()` | daemon 用 `monitor.rs::run()` inline socket 处理 |

---

### D4 — `ALL_EVENT_MASK` 包含 `FAN_FS_ERROR`

**文件**: `src/monitor.rs` + `src/fid_parser.rs`

`FAN_FS_ERROR` (0x0000_8000) 需要 Linux 5.16+。`fanotify-rs 0.3.1` 不导出此常量，
在 `fid_parser.rs` 手动定义。在旧内核上 `fanotify_mark` 会返回 EINVAL
（mask 含未知 bit），导致 `all_events: true` 的路径静默监控失败。

---

### D5 — `cmd_add` 中重复规范化

`cmd_add` 中先做一次 `resolve_recursion_check`（递归检查），再做一次
`expand_tilde + canonicalize`（路径规范化）。内部 `add_path` 再做第三次。
后续重构应合并。

---

## 🟠 C. 边界条件

### C1 — 进程已退出时 TOCTOU

`build_file_event` 调用 `get_process_info_by_pid`。短命进程退出后
`/proc/{pid}/status` 读取失败，user 回退到文件 owner。
对 MODIFY/CREATE 事件，user 字段可能不准确（文件 owner ≠ 写入者）。

---

### C2 — 二进制搜索的边界扫描失效

`query.rs::seek_and_parse_time` 从 offset 向后扫描最多 4096 字节寻空白行
（块边界）。若 TOML 块 > 4KB（极端情况），`expand_offset_backward`
可能回退不充分，导致时间边界的少数事件被漏掉。

---

### C3 — `resolve_file_handle` 多 mount_fd 迭代

`resolve_file_handle` 遍历所有 mount_fds 调用 `open_by_handle_at`，
理论上有极低概率因 inode 碰撞用错误 mount_fd 解析出错误路径。
实际 file_handle 是文件系统特化的，不同 fs 不会碰撞。

---

### C4 — fanotify fd 终止时的 post-close 使用

`run()` 末尾 `for &mfd in &self.mount_fds { libc::close(mfd); }` 关闭后
spawn 的 reader task 可能仍在执行（channel 未立即断开），
会尝试用已关闭 fd 调 `open_by_handle_at` → EBADF → 路径为空。
不会崩溃，但最后一批事件可能丢失路径。

---

### C5 — `chown_to_user` 在无所有权的 FS 上失败

若 log_dir 在 vfat/exfat/NFS (no_root_squash off) 等不支持标准 UNIX 所有权的
文件系统上，`chown` 静默失败（`let _ = chown_to_user(dir)`），
导致日志文件归 root 所有，普通用户无法 `fsmon clean`。

---

## 🟣 D. 测试覆盖缺失

| 覆盖缺口 | 风险 |
|----------|------|
| `cmd_remove` 路径规范化（tilde/canonical） | B2 同类 bug 回归 |
| `cmd_clean` 并发操作 | B5 同类 bug 回归 |
| `path_to_log_name` 超长路径（>255 字节） | ✅ 已修复 + 测试覆盖 |
| `persist_config` 失败后恢复 | B6 同类 bug 回归 |
| `build_file_event` MODIFY 事件 size_change 语义 | D1 回归 |
| `resolve_file_handle` 多 mount_fd 返回错误 fd | C3 回归 |
| `dir_cache` 预填充在 spawn 之后生效 | B1 回归 |

---

## 修复优先级

| 优先级 | Bug | 影响 |
|--------|-----|------|
| 🔴 P0 | B1 — dir_cache 预填充丢失 | 实时监控首事件解析不全 |
| 🔴 P0 | B2 — cmd_remove 路径规范化 | 用户无法移除已添加路径 |
| 🟡 P1 | B4 — 未去重 unmatched 警告 | 生产环境 stderr 刷屏 |
| 🟡 P1 | B6 — persist_config 吞错误 | 数据不一致可能丢失 |
| 🟡 P2 | B3 — mount_fds 索引误删 | 跨 FS 时路径事件丢失 |
| 🟡 P2 | B5 — 并发 truncate 冲突 | 并发 clean 数据损坏 |
| 🟢 P3 | B7-B9, D1-D5, C1-C5 | 边界 / 设计 / 优化 |
