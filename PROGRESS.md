# PROGRESS — unsafe 代码清理计划

> 目标：消除所有可安全化的 `unsafe`，在无法消除处记录原因并添加 safety 注释。

## 当前 unsafe 分布总览

| 文件 | Unsafe 类型 | 数量 | 状态 |
|------|-----------|------|------|
| `src/monitor.rs` | `libc::dup` + `OwnedFd::from_raw_fd` | 10→2 | ✅ `safe_dup()` 集中到1处 |
| `src/monitor.rs` | `nix::fcntl::open` + `from_raw_fd` | 2→2 | ✅ `safe_open_dir()` 集中到1处 |
| `src/monitor.rs` | `libc::read` (集成测试) | 1 | ✅ 加safety注释 |
| `src/proc_cache.rs` | Netlink conn: `socket/bind/recv/send/zeroed` | 5+5 | ❌ 无safe替代 |
| `src/fid_parser.rs` | `BorrowedFd::borrow_raw` | 1 | ⏳ 待改 |
| `src/config.rs` | `std::env::set_var/remove_var` (测试) | 8 | ⏳ 待改（低优） |

## 改造计划（按优先级）

### ✅ P0 — monitor.rs: 用 `safe_dup()` 替代 `libc::dup` + `from_raw_fd`

- **文件**: `src/monitor.rs`，run() + spawn_fd_reader() 两组
- **方案**: 新增 `Monitor::safe_dup()` 辅助函数，内部用 `nix::unistd::dup` + `OwnedFd::from_raw_fd`（唯一unsafe集中点），外部用 RAII drop 自动清理
- **效益**: 10 个分散 unsafe → 2 个集中 unsafe（在 `safe_dup` 函数内）
- **状态**: ✅ 已完成

### ✅ P1 — monitor.rs: `from_raw_fd` for mount fd（open 结果）

- **文件**: `src/monitor.rs` 2 处 `nix::fcntl::open` + `OwnedFd::from_raw_fd`
- **方案**: `nix::unistd::dup` 不能消除这类 —— 因为 `nix::fcntl::open` 本身已 safe，unsafe 仅来自 `OwnedFd::from_raw_fd`（Rust 语言限制：从裸整数构造 owned fd 必然 unsafe）。改用 `safe_open_dir()` 辅助函数集中管理
- **效益**: 2 个分散 unsafe → 1 处集中（在 `safe_open_dir` 内）
- **状态**: ✅ 已完成

### P2 — fid_parser.rs: `BorrowedFd::borrow_raw`

- **文件**: `src/fid_parser.rs` L34
- **方案**: 将 `FanFd` 内部改为 `OwnedFd`，`as_fd()` 直接调用 `self.0.as_fd()`
- **风险**: `FanFd` 同时用于 `AsyncFd` 包装（`inotify`），需确认兼容性
- **状态**: ⏳

### P3 — config.rs 测试中环境变量 unsafe

- **文件**: `src/config.rs`，8 处 `std::env::set_var`/`remove_var`
- **方案**: 引入 `temp_env` crate，用 `temp_env::with_var` 替代
- **状态**: ⏳

### ❌ 无法消除 — proc_cache.rs

- **原因**: Linux Netlink Proc Connector 没有主流 Rust safe 封装，FFI 调用不可避免
- **措施**: 已有 `SockGuard` RAII 保证 close，补充 safety 注释说明不可消除的理由
- **状态**: ✅ 已完成注释

### ❌ 无法消除 — monitor.rs 集成测试 `libc::read`

- **原因**: `fanotify_fid::read::read_fid_events` 需要完整的 `mount_fds` + `dir_cache` 参数，测试中只是原始读取验证事件计数，改用封装反而复杂
- **措施**: 补充 safety 注释
- **状态**: ✅ 已完成注释

## 实施顺序

1. ✅ proc_cache.rs + monitor.rs 集成测试 — safety 注释完善
2. ✅ P0 — monitor.rs `safe_dup()` 辅助函数
3. ✅ P1 — monitor.rs `safe_open_dir()` 辅助函数
4. ⏳ P2 — fid_parser.rs `BorrowedFd` → `OwnedFd`
5. ⏳ P3 — config.rs 测试环境变量
