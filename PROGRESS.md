# fsmon PROGRESS - 全面代码审查报告 (2026-05-06)

## 审查范围

- 10 个 `.rs` 源文件 + 1 个 bin
- clippy `-D warnings` 通过 ✅
- 81 tests pass, 7 ignored (sudo) ✅
- 审查维度: 正确性、边界条件、并发安全、失效模式、代码质量

---

## 🔴 A. 确认 Bug

### B1 (高) - `monitor.rs::run()` dir_cache 预填充全部丢失 ✅ 误报 (2026-05-07)

**文件**: `src/monitor.rs`

**状态**: 审查**误报**。当前代码执行顺序正确:
1. 预缓存循环先执行 (lines 426-436) → `self.dir_cache` 已填充
2. 再 `mem::take` (line 474) → 取出有数据的 HashMap
3. 最后 spawn reader tasks (lines 485-490) → clone 的 Arc 指向有数据的 cache

**结论**: 无需修复。

---

### B2 (高) - `cmd_remove` 未规范化路径 ✅ 已修复 (2026-05-06)

**文件**: `src/bin/fsmon.rs`

**问题**: `cmd_add` 中路径被 `expand_tilde` + `canonicalize` 后存入 store。但
`cmd_remove` 直接用原始 CLI 参数做 `store.remove_entry(&path)`,无 tilde 展开、无 canonicalize。

`fsmon add /tmp/foo/../bar` → store 存 `/tmp/bar`
`fsmon remove /tmp/foo/../bar` → 找不到

**修复**: 在 `remove_entry` 前对 `path` 执行同样规范化: `expand_tilde` → `canonicalize`。

---

### B3 (中) - `remove_path` 索引假设 `mount_fds[pos]` 与 `paths[pos]` 一一对应 ✅ 误报 (2026-05-07)

**文件**: `src/monitor.rs`

**状态**: 审查**误报**。三数组始终保持同序对齐:
- 初始启动: `paths`→`canonical_paths`→`mount_fds` 同顺序填充 (lines 262-273, 409-415)
- `add_path`: 三者都 append 尾部 (lines 887/892/893)
- `remove_path`: 三者用同一 `pos` 删除 (lines 986/987/993)

`pos < mount_fds.len()` 仅防御性检查,无实际风险。无需修复。

---

### B4 (中) - `write_event` unmatched 路径警告无去重 ✅ 误报 (2026-05-07)

**文件**: `src/monitor.rs`

**状态**: 审查**误报**。代码审查时引用的代码模式在当前代码库中不存在:
- `write_event` (`src/monitor.rs:1331-1371`) 无任何 unmatched 路径警告
- 超出监控范围的事件在主循环直接 `continue` 静默跳过 (line 622)
- 描述的 `eprintln!("[WARNING] Event not matched...")` 从未出现

**结论**: 无需修复。

---

### B5 (中) - `truncate_from_start` 并发临时文件冲突 ✅ 已修复 (2026-05-07)

**文件**: `src/lib.rs`

**问题**: 所有日志文件清理共享同一个临时文件名:
```rust
let tmp_path = dir.join(".fsmon_trunc_tmp");
```
同时运行 `fsmon clean --path /a` 和 `fsmon clean --path /b`,
两者使用同一个 `.fsmon_trunc_tmp`,导致数据错乱和损坏。

**修复**: 使用唯一临时文件名(如 `format!(".fsmon_trunc_{}", std::process::id())` 或使用 `tempfile` crate)。

**状态**: ✅ 在 JSONL 迁移中附带修复（`.fsmon_trunc_tmp` → 带 PID 的唯一临时文件名）。

---

### B6 (中) - `persist_config` 静默忽略保存失败 ✅ 误报 (2026-05-07)

**文件**: `src/monitor.rs`

**状态**: 审查**误报**。
- 名为 `persist_config` 的函数在当前代码库中**不存在**
- `store.save()` 只在 `src/bin/fsmon.rs` 中调用,全部使用 `?` 运算符传播错误
- `handle_socket_cmd` 不参与 store 持久化(由 CLI 保存后再发 socket)
- 描述的代码模式(`let _ = store.save(store_path)`)未在任何位置出现

**结论**: 无需修复。

---

### B7 (低) - `count_lines` 无边界校验 ✅ 已修复 (2026-05-07)

**文件**: `src/lib.rs`

**问题**:
```rust
fn count_lines(path: &Path, upto: usize) -> Result<usize> {
    let mut buf = vec![0u8; upto];
    f.read_exact(&mut buf)?;
```
若 `upto > file_size`,`read_exact` 返回 `UnexpectedEof`。
目前调用方保证 `upto ≤ file_size`(来自 `find_tail_offset`),但无防御性校验。

**修复**: 使用 `take(upto).read_to_end(&mut buf)` 替代 `read_exact`，避免 TOCTOU 和多余 syscall。

**状态**: ✅ 已修复。`read_exact` → `take(upto).read_to_end`，自动处理文件小于 `upto` 的情况。

---

### B8 (低) - `path_to_log_name` 编码膨胀导致超长文件名 ✅ 已修复 (2026-05-06)

**文件**: `src/utils.rs`

**问题**: 转义规则 `!`→`!!`、`_`→`!_`、`/`→`_` 使每个特殊字符变 2 字节。
含大量 `_`/`!` 的路径编码后可能超过 255 字节(Linux 文件名限制),
导致 `write_event` 创建日志文件失败。

**修复**: 使用 FNV-1a 64-bit 确定性哈希替代全路径编码,文件名固定为 `{:016x}.toml`(21 字节)。
原始路径作为 `FileEvent.monitored_path` 字段保留在每个事件中(TOML 可解析),
用户可用自己的过滤策略按此字段检索,不影响查询体验。

---

### B9 (极低) - `read_fid_events` 在 async context 中持 std::sync::Mutex ✅ 已修复 (2026-05-07)

**文件**: `src/monitor.rs` + `src/fid_parser.rs` + `src/dir_cache.rs`

**问题**:
```rust
let events = fid_parser::read_fid_events(fd, &mfds, &mut dc.lock().unwrap(), &mut buf);
```
`std::sync::Mutex` 在 tokio 任务中被持有整个解析周期(包含 `open_by_handle_at`
磁盘 I/O),阻塞当前 worker 线程。多 fd 时可能影响并发性能。

**修复**: 使用 `dashmap::DashMap` 替代 `Mutex<HashMap>`，彻底消除锁竞争。
- `dir_cache.rs`: `&mut HashMap` → `&DashMap`
- `fid_parser.rs`: `&mut HashMap` → `&DashMap`
- `monitor.rs`: 移除 `Mutex` 包装，`dc.lock().unwrap()` → `dc.as_ref()`

---

## 🟡 B. 设计问题 / 语义不一致

### D1 - `FileEvent.file_size`  ✅ 已修复 (2026-05-06)

**文件**: `src/lib.rs`

**问题**: 字段名 `size_change` 暗示"增量"(delta),但实际值为**绝对文件大小**:

| EventType | `file_size` 实际含义 |
|-----------|-------------------|
| CREATE/MODIFY/CLOSE_WRITE | 当前文件大小(非增量) |
| DELETE/DELETE_SELF/MOVED_FROM | 删除前缓存的大小 |
| 其他 | 缓存大小或 0 |

TOML 字段名为 `file_size`,`size_change` 已全部替换。

---

### D2 - `file_size: u64`  ✅ 已修复 (2026-05-06)

**文件**: `src/lib.rs`

**问题**: `size_change: i64` 暗示可能存在负值,但实际文件大小永不为负。
`i64` 是历史遗留(原名 `size_change` 时可能存增量)。

**修复**:
- 类型改为 `file_size: u64`,与绝对大小语义一致
- 删除所有 `abs()` 调用和负数测试用例
- TOML 字段名同步为 `file_size`

---

### D3 - 死代码 ✅ 已清理 (2026-05-06)

| 文件 | 函数 | 处理 |
|------|------|------|
| `src/output.rs` | `output_event()` | 删除(从未调用,依赖 Human/Csv 输出) |
| `src/lib.rs` | `FileEvent::to_human_string()` / `FileEvent::to_csv_string()` / `FileEvent::from_csv_str()` | 删除 |
| `src/lib.rs` | `OutputFormat::Human` / `OutputFormat::Csv` | 移除 enum 变体(仅保留 `Toml`) |
| `Cargo.toml` | `csv` 依赖 | 移除 |

---

### D4 - `ALL_EVENT_MASK` 包含 `FAN_FS_ERROR` ⏸️ 搁置，等 fanotify-rs 支持

**文件**: `src/monitor.rs` + `src/fid_parser.rs`

`FAN_FS_ERROR` (0x0000_8000) 需要 Linux 5.16+。`fanotify-rs 0.3.1` 不导出此常量,
在 `fid_parser.rs` 手动定义。在旧内核上 `fanotify_mark` 会返回 EINVAL
(mask 含未知 bit),导致 `all_events: true` 的路径静默监控失败。

**处理**:
- 删除 `fid_parser.rs` 中手动定义的 `FAN_FS_ERROR` 常量
- 从 `EVENT_BITS` 和 `ALL_EVENT_MASK` 中移除该 bit
- 保留 `EventType::FsError` 枚举变体以保持向前兼容
- 等 `fanotify-rs` 后续版本原生导出该常量后再加回来

---

### D5 — `cmd_add` 中重复规范化 ✅ 已修复 (2026-05-06)

**文件**: `src/bin/fsmon.rs`

**问题**: `cmd_add` 中先做一次 `resolve_recursion_check`（递归检查），再做一次
`expand_tilde + canonicalize`（存 store）。内部 `add_path` 再做第三次。

**修复**: `cmd_add` 中规范化一次，结果 `path` 同时用于递归检查、存 store、发 socket。
`add_path` 的规范化保留（socket handler 和 `reload_config` 也需要）。

---

## 🟠 C. 边界条件

### C1 - 进程已退出时 TOCTOU ✅ 已修复 (2026-05-07)

**文件**: `src/monitor.rs` + `src/utils.rs`

`build_file_event` 调用 `get_process_info_by_pid`。短命进程退出后
`/proc/{pid}/status` 读取失败,user 回退到文件 owner。
对 MODIFY/CREATE 事件,user 字段可能不准确(文件 owner ≠ 写入者)。

**修复**:
1. 新增 `pid_cache: LruCache<u32, ProcInfo>` — 成功解析的 PID 缓存 4096 条,
   同 PID 后续事件零延迟命中
2. `get_process_info_by_pid` 中 `/proc/{pid}` 读取失败时重试 2 次,
   每次 500µs sleep — 进程刚退出时可能还在 zombie 态,/proc 短暂可读
3. 提取通用 `retry()` 辅助函数

---

### C2 - 二进制搜索的边界扫描失效

`query.rs::seek_and_parse_time` 从 offset 向后扫描最多 4096 字节寻空白行
(块边界)。若 TOML 块 > 4KB(极端情况),`expand_offset_backward`
可能回退不充分,导致时间边界的少数事件被漏掉。

---

### C3 - `resolve_file_handle` 多 mount_fd 迭代

`resolve_file_handle` 遍历所有 mount_fds 调用 `open_by_handle_at`,
理论上有极低概率因 inode 碰撞用错误 mount_fd 解析出错误路径。
实际 file_handle 是文件系统特化的,不同 fs 不会碰撞。

---

### C4 - fanotify fd 终止时的 post-close 使用

`run()` 末尾 `for &mfd in &self.mount_fds { libc::close(mfd); }` 关闭后
spawn 的 reader task 可能仍在执行(channel 未立即断开),
会尝试用已关闭 fd 调 `open_by_handle_at` → EBADF → 路径为空。
不会崩溃,但最后一批事件可能丢失路径。

---

### C5 - `chown_to_user` 在无所有权的 FS 上失败 ✅ 已修复 (2026-05-07)

若 log_dir 在 vfat/exfat/NFS (no_root_squash off) 等不支持标准 UNIX 所有权的
文件系统上,`chown` 静默失败(`let _ = chown_to_user(dir)`),
导致日志文件归 root 所有,普通用户无法 `fsmon clean`。

**修复**: `chown_to_user` 现在区分三类结果:
- `Ok(true)`: chown 成功
- `Ok(false)`: FS 不支持所有权变更(EPERM/EOPNOTSUPP/ENOSYS)
- `Err(err)`: 真实错误(IO 失败等)

daemon 启动时对 log_dir 的 chown 失败会发一次性 `[WARNING]`,
提示用户此 FS 不支持所有权变更,`clean` 可能需要 `sudo`。

---

---

## 🔴 测试问题

### T1 (低) - config 测试全局 Mutex 中毒导致并行失败

**文件**: `src/config.rs`

**问题**:
```rust
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_isolated_home(f: impl FnOnce(&Path)) {
    let _lock = ENV_LOCK.lock().unwrap();  // panic → 毒化 Mutex
    ...
}
```

3 个 config 测试共享一个全局 `Mutex<()>` 保护 `std::env::set_var`。
若有测试 panic（如 `/tmp` 清理竞争），Mutex 被毒化，后续测试 `lock().unwrap()` 直接 panic。

**影响**: 并行运行时 (`cargo test` 默认) 偶发失败 3 个:
- `config::tests::test_load_returns_default_when_no_file`
- `config::tests::test_resolve_paths_expands_tilde_and_uid`
- `config::tests::test_resolve_uid_no_sudo`

**修复**: 把 `.unwrap()` 改为 `.lock().unwrap_or_else(|e| e.into_inner())` 或使用 `parking_lot::Mutex`（不支持中毒）。

---

## ✅ 已实现: JSONL 格式迁移 (2026-05-07)

### 改动总览

| 文件 | 改动 |
|------|------|
| `Cargo.toml` | 加 `serde_json` 依赖 |
| `src/utils.rs` | `path_to_log_name` 扩展名 `.toml` → `.jsonl` |
| `src/config.rs` | 默认 store 路径 `store.toml` → `store.jsonl` |
| `src/store.rs` | save/load 从 TOML 序列化改为 JSONL 逐行读写 |
| `src/lib.rs` | 加 `FileEvent::to_jsonl_string/from_jsonl_str`, `OutputFormat::Jsonl`, `parse_log_line_jsonl`; `clean_single_log` 改为逐行处理; 删 `read_toml_block`/`TOML_SEPARATOR` |
| `src/monitor.rs` | `write_event` 写一行 JSONL, 无 blank line separator |
| `src/query.rs` | 读/写 JSONL, `expand_offset_backward` 简化为按行扫描 |
| `src/bin/fsmon.rs` | `query` 加 `-F/--format jsonl` 选项 |

### 格式分界

| 目录 | 格式 | 原因 |
|------|------|------|
| `~/.config/fsmon/config.toml` | 多行 TOML | 用户编辑,保持不变 |
| `~/.local/share/fsmon/store.jsonl` | JSONL | 程序读写,一行一个路径 |
| `~/.local/state/fsmon/*.jsonl` | JSONL | 一行一个事件,pipe 友好 |

### 附带修复

- B5: `.fsmon_trunc_tmp` 改为带 PID 的唯一临时文件名 (修复并发 clean 冲突)

---

## 🟣 D. 测试覆盖缺失

| 覆盖缺口 | 风险 |
|----------|------|
| `cmd_remove` 路径规范化(tilde/canonical) | B2 同类 bug 回归 |
| `cmd_clean` 并发操作 | B5 同类 bug 回归 |
| `path_to_log_name` 超长路径(>255 字节) | ✅ 已修复 + 测试覆盖 |
| `persist_config` 失败后恢复 | B6 同类 bug 回归 |
| `build_file_event` MODIFY 事件 file_size 语义 | ✅ 已修复(字段改名 `file_size`) |
| `resolve_file_handle` 多 mount_fd 返回错误 fd | C3 回归 |
| `dir_cache` 预填充在 spawn 之后生效 | B1 回归 |

---

## 修复优先级

| 优先级 | Bug | 影响 |
|--------|-----|------|
| 🔴 P0 | B1 - dir_cache 预填充丢失 | 实时监控首事件解析不全 |
| 🔴 P0 | B2 - cmd_remove 路径规范化 | 用户无法移除已添加路径 |
| 🟡 P1 | B4 - 未去重 unmatched 警告 | 生产环境 stderr 刷屏 |
| 🟡 P1 | B6 - persist_config 吞错误 | 数据不一致可能丢失 |
| 🟡 P2 | B3 - mount_fds 索引误删 | 跨 FS 时路径事件丢失 |
| 🟡 P2 | B5 - 并发 truncate 冲突 | 并发 clean 数据损坏 |
| 🟢 P3 | B7-B9, D1-D5, C1-C5 | 边界 / 设计 / 优化 |
