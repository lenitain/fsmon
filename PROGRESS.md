# fsmon 进度追踪

## unsaf e 代码审计与清理计划 🧹

**日期**: 2026-05-07
**分支**: `lenitain/unsafe-audit`

### 总览

```
总 unsafe 55 处（35 处生产代码 + 20 处测试代码）
├── ✅ 已完成                -25 处
├── ✅ 类型安全（编译器保证无 UB）  ~5 处剩余  → geteuid, flock, fs2
├── ⚠️ 薄包装（safe 签名，传错仍 UB） ~6 处剩余  → close(RawFd), netlink, BorrowedFd
└── ❌ 本质 unsafe（指针解引用、FFI） ~19 处剩余 → FID 结构体, name_to_handle_at, 测试代码

关键认知：safe 函数签名 ≠ 安全操作。nix::unistd::close(fd) 是薄包装，
传无效 fd 仍然 UB。真正推动安全的换的是 fs2::try_lock_exclusive (RAII guard)
和删除手动 close（OwnedFd 所有权）。单纯把 unsafe 从自己代码移到 nix crate
里，并没有提升安全性。

**当前进度**: 55 → 30 处 unsafe（生产代码 35 → **11**）

已消除 unsafe 明细:
| 阶段 | 消除 | 累计 |
|------|------|------|
| P0 geteuid/getegid | -7 | 7 |
| P0 chown | -3 | 10 |
| P0 flock | -1 | 11 |
| P1 getpwuid_r | -4 | 15 |
| P1 open/close | -10 | 25 |
| **剩余** | | **11 生产 + 19 测试** |
```

---

### 详细分类与解决方案

#### 1. ✅ 已完成 — `libc::geteuid()` / `libc::getegid()`
类型安全 ✅：无参纯函数，编译器保证无 UB。

**方案**: 替换为 `nix::unistd::geteuid()` / `getegid()`

---

#### 2. ✅ 已完成 — `libc::chown()`
类型安全 ✅：正确参数类型由 `Uid`/`Gid` 新类型保证。

**方案**: 替换为 `nix::unistd::chown()`

---

#### 3. ✅ 已完成 — `libc::flock()`
类型安全 ✅：`fs2::FileExt::try_lock_exclusive()` 返回 RAII guard，
drop 自动释放锁，不可能忘记解锁或双解锁。**这是最干净的改造**。

**方案**: 替换为 `fs2::FileExt::try_lock_exclusive()`

---

#### 4. ✅ 已完成 — `libc::getpwuid_r()` + 指针解引用
类型安全 ✅：`users::get_user_by_uid()` 返回 safe Rust 类型。

**方案**: 替换为 `users::get_user_by_uid()`

---

#### 5. ✅ 已完成 — `libc::open()` / `libc::close()` 目录 fd
混合评价：
- ⚠️ `nix::fcntl::open()` 返回 `RawFd`（薄包装，传错仍 UB）
- ✅ 删除 mount_fds 的 `libc::close`（转为所有权隐含清除）— **真正安全**
- ⚠️ `nix::unistd::close(fd)`（薄包装）

**最有价值的是 mount_fds 手动 close 的删除（RAII 化）**，
而非把 `unsafe { libc::close }` 换成 `nix::unistd::close`。

---

#### 6. ⚠️ Netlink 全程 — ~9 处（未完成）
薄包装 ⚠️：即使换成 `nix::sys::socket`，传错 netlink fd 仍然 UB。
只有将 fd 用 `OwnedFd` 管理才能真安全。

| 位置 | 行号 | 操作 |
|------|------|------|
| `proc_cache.rs` | 61, 309, 328 | `socket()` |
| `proc_cache.rs` | 79, 337 | `zeroed()` 初始化 `sockaddr_nl` |
| `proc_cache.rs` | 84, 342 | `bind()` |
| `proc_cache.rs` | 107 | `recv()` |
| `proc_cache.rs` | 154 | `send()` |
| `proc_cache.rs` | 217, 351 | `close()` |

**优先级**: 低 — 纯薄包装，不提升安全性，除非用 `OwnedFd` + RAII 重构。

---

#### 7. ⚠️ `BorrowedFd::borrow_raw()` — 1 处
薄包装 ⚠️：AsFd trait 实现的标准模式，绕不开。

| 位置 | 行号 |
|------|------|
| `monitor.rs` | 49 |

---

#### 8. ❌ FID 内核结构体指针解引用 — 3 处
本质 unsafe ❌：从 raw buffer 按内存布局 reinterpret 为 `FanMetadata` / `FanInfoHeader`。
Rust 无法验证这些字节是否真的是一个有效结构体。

| 位置 | 行号 | 说明 |
|------|------|------|
| `fid_parser.rs` | 116 | `&*(buf.as_ptr().add(offset) as *const FanMetadata)` |
| `fid_parser.rs` | 132 | `&*(buf.as_ptr().add(info_off) as *const FanInfoHeader)` |
| `fid_parser.rs` | 169 | `libc::close(meta.fd)` |

**本质**: 从 raw buffer reinterpet 变长 FID 事件的内核数据结构
**能否消除**: ❌ 除非有专门的 `fanotify-fid` crate 提供完整安全的 FID 解析
**微优化**: 可封装 `unsafe` 到 `try_parse_metadata()` / `try_parse_info_header()` 函数，用 `Result` 返回，缩小 unsafe 可见范围

---

#### 10. ❌ `name_to_handle_at()` + `open_by_handle_at()` — 4 处

| 位置 | 行号 | 操作 |
|------|------|------|
| `dir_cache.rs` | 18 | `name_to_handle_at()` |
| `fid_parser.rs` | 339 | `open_by_handle_at()` |
| `fid_parser.rs` | 349 | `close()` 关闭结果 fd |

**本质**: Linux 特有 kernel ABI，没有 safe 包装 crate
**能否消除**: ❌ 这是把 kernel file_handle 解析成路径的唯一方式
**微优化**: 封装为 `fn resolve_handle(mount_fds: &[i32], fh: &[u8]) -> Option<PathBuf>` 并用 RAII guard 管理返回的 fd

---

#### 11. ✅ 测试中 `std::env::set_var()` / `remove_var()` — 9 处

| 位置 | 行号 |
|------|------|
| `config.rs` | 303, 311, 434, 445, 456, 461, 472, 476, 479 |

**方案**: 用 `temp-env` crate 或者 `SerialTest` guard 模式。但测试环境变量修改本身是 Rust 标准库设计为 unsafe 的（非线程安全），保持现状也可接受。

---

#### 12. ❌ `libc::read()` 在测试中 — 1 处

| 位置 | 行号 |
|------|------|
| `monitor.rs` | 1833 |

**方案**: 可改用 `std::io::Read` + `unsafe { OwnedFd::from_raw_fd(fd) }`，底层 unsafe 不变。测试代码可容忍。

---

### 执行计划（按顺序）

| 阶段 | 状态 | 内容 | 影响 unsafe | 涉及文件 |
|------|------|------|-------------|---------|
| **P0** | ✅ 已完成 | `geteuid`/`getegid` → `nix::unistd` | -7 处 | `config.rs`, `monitor.rs` |
| **P0** | ✅ 已完成 | `chown` → `nix::unistd::chown` | -3 处 | `config.rs`, `monitor.rs`, `bin/fsmon.rs` |
| **P0** | ✅ 已完成 | `flock` → `fs2::FileExt` | -1 处 | `lib.rs` |
| **P1** | ✅ 已完成 | `getpwuid_r` → `users::get_user_by_uid` | -4 处 | `config.rs` |
| **P1** | ✅ 已完成 | 目录 `open`/`close` → `nix::fcntl::open` + `nix::unistd::close` + SockGuard | -10 处 | `monitor.rs`, `fid_parser.rs`, `proc_cache.rs` |
| **P1** | ⏳ | 测试 `std::env::set_var` → 清理 | -9 处（测试代码） | `config.rs` |
| **P2** | ⏳ | Netlink → `nix::sys::socket`（薄包装，价值低） | -9 处 | `proc_cache.rs` |
| **P3** | ❌ 不做了 | FID 解析封装 | 0 处（本质 unsafe） | `fid_parser.rs` |
| **P3** | ❌ 不做了 | `name_to_handle_at` 封装 | 0 处（本质 unsafe） | `dir_cache.rs`, `fid_parser.rs` |

---

### 遗留问题（不可消除 / 不值得做）

| 类别 | 位置 | 原因 |
|------|------|------|
| FID 结构体 reinterpret | `fid_parser.rs` 3 处 | 本质 unsafe，raw buffer 转结构体 |
| name_to_handle_at / open_by_handle_at | `dir_cache.rs` + `fid_parser.rs` 4 处 | Linux 专属 kernel ABI，无 safe 包装 |
| BorrowedFd::borrow_raw | `monitor.rs` 1 处 | AsFd trait 实现的标准写法 |
| 测试 `libc::close`/`read` | `monitor.rs` + `proc_cache.rs` 6 处 | 测试代码，直接调 libc 做底端测试 |
| Netlink + `std::env::set_var` | ~15 处 | 纯薄包装，换 nix 不提升安全性 |

核心认知：**safe 函数签名 ≠ 安全操作**。仅靠薄包装把 `unsafe` 挪到 nix crate 里，
不改变传参错误就会 UB 的事实。真正推动安全的是 RAII 化（`OwnedFd`、RAII guard）
和所有权设计。否则 `unsafe { libc::close(fd) }` 和 `nix::unistd::close(fd)`
在安全性上等价。

项目当前状态：11 处生产代码 unsafe 均为本质 unsafe 或显式薄包装，
没有隐藏的安全风险。不值得再花时间做薄包装替换。

---

### 历史条目

### `ALL_EVENT_MASK` 包含 `FAN_FS_ERROR` ⏸️ 搁置，等 fanotify-rs 支持

**文件**: `src/monitor.rs` + `src/fid_parser.rs`

`FAN_FS_ERROR` (0x0000_8000) 需要 Linux 5.16+。`fanotify-rs 0.3.1` 不导出此常量,
在 `fid_parser.rs` 手动定义。在旧内核上 `fanotify_mark` 会返回 EINVAL
(mask 含未知 bit),导致 `all_events: true` 的路径静默监控失败。

**处理**:
- 删除 `fid_parser.rs` 中手动定义的 `FAN_FS_ERROR` 常量
- 从 `EVENT_BITS` 和 `ALL_EVENT_MASK` 中移除该 bit
- 保留 `EventType::FsError` 枚举变体以保持向前兼容
- 等 `fanotify-rs` 后续版本原生导出该常量后再加回来
