# fsmon 进度追踪

## unsaf e 代码审计与清理计划 🧹

**日期**: 2026-05-07
**分支**: `lenitain/unsafe-audit`

### 总览

```
总 unsafe 55 处（35 处生产代码 + 20 处测试代码）
├── ✅ 可 safe 替代     ~31 处（换 nix/fs2/users 等 safe crate）
├── 🔶 可包装但本质 unsafe  ~10 处（底层仍是 unsafe，只是接口变 safe）
└── ❌ 不可替代         ~14 处（FID 结构体 + name_to_handle_at + raw fd）
```

---

### 详细分类与解决方案

#### 1. ✅ `libc::geteuid()` / `libc::getegid()` — 8 处

| 位置 | 行号 | 用途 |
|------|------|------|
| `config.rs` | 52, 68, 84, 143, 476 | 获取当前用户 UID |
| `config.rs` | 59 | 获取当前用户 GID |
| `monitor.rs` | 225 | 检查是否 root |

**方案**: 替换为 `nix::unistd::geteuid()` / `getegid()`
**方法**: 全局搜索替换，nix 已经是可选依赖

---

#### 2. ✅ `libc::chown()` — 3 处

| 位置 | 行号 |
|------|------|
| `config.rs` | 70 |
| `monitor.rs` | 63 |
| `bin/fsmon.rs` | 213 |

**方案**: 替换为 `nix::unistd::chown()`

---

#### 3. ✅ `libc::flock()` — 1 处

| 位置 | 行号 |
|------|------|
| `lib.rs` | 45 |

**方案**: 替换为 `fs2::FileExt::lock_exclusive()`（加 `fs2` 依赖）
**备选**: `fd-lock` crate（更轻量）

---

#### 4. ✅ `libc::getpwuid_r()` + 指针解引用 — 3 处

| 位置 | 行号 |
|------|------|
| `config.rs` | 91 (sysconf) |
| `config.rs` | 97 (getpwuid_r) |
| `config.rs` | 116, 121 (CStr 解引用) |

**方案**:
- `getpwuid_r` + `pw_dir` 解引用 → `users::get_user_by_uid()`（`users` crate）
- `sysconf(_SC_GETPW_R_SIZE_MAX)` → hardcode fallback 4096 或 `nix::unistd::sysconf()`

---

#### 5. ✅ `libc::open()` / `libc::close()` 目录 fd — 6 处

| 位置 | 行号 |
|------|------|
| `monitor.rs` | 457, 1017 |
| `monitor.rs` | 1114 (close) |
| `fid_parser.rs` | 339, 349 |

**方案**: 替换为 `nix::fcntl::open(path, OFlag::O_RDONLY | OFlag::O_DIRECTORY, Mode::empty())`
**注意**: `fid_parser.rs:349` 的 `close(fd)` 可用 RAII guard 包裹

---

#### 6. 🔶 Netlink 全程 — ~9 处

| 位置 | 行号 | 操作 |
|------|------|------|
| `proc_cache.rs` | 61, 309, 328 | `socket()` |
| `proc_cache.rs` | 79, 337 | `zeroed()` 初始化 `sockaddr_nl` |
| `proc_cache.rs` | 84, 342 | `bind()` |
| `proc_cache.rs` | 107 | `recv()` |
| `proc_cache.rs` | 154 | `send()` |
| `proc_cache.rs` | 217, 351 | `close()` |

**方案**: 用 `nix::sys::socket` 的 netlink 支持
- `socket(PF_NETLINK, SOCK_DGRAM, NETLINK_CONNECTOR)` → `nix::sys::socket::socket(AddressFamily::Netlink, SockType::Datagram, SockFlag::SOCK_CLOEXEC, Some(NetlinkProtocol::Connector))`
- `bind()` → `nix::sys::socket::bind()`
- `send()` / `recv()` → `nix::sys::socket::sendto()` / `recvfrom()`
- `zeroed()` → 用 `nix::sys::socket::SockaddrNetlink` safe 构造

**优先级**: 中 — 改动大但彻底消除 proc_cache 所有 unsafe

---

#### 7. 🔶 `BorrowedFd::borrow_raw()` — 1 处

| 位置 | 行号 |
|------|------|
| `monitor.rs` | 49 |

**方案**: 改成 `unsafe { OwnedFd::from_raw_fd(self.0) }` — 本质一样，但生命周期更清晰
**免改**: 这是 AsFd trait 实现，底层必然 unsafe

---

#### 8. 🔶 `std::mem::zeroed()` — 2 处

| 位置 | 行号 |
|------|------|
| `proc_cache.rs` | 79, 337 |

**方案**: 如果能用 `nix::sys::socket::SockaddrNetlink` 则消除；否则保持现状（等价于 `MaybeUninit::zeroed().assume_init()`）

---

#### 9. ❌ FID 内核结构体指针解引用 — 3 处

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

| 阶段 | 内容 | 影响 unsafe | 涉及文件 |
|------|------|-------------|---------|
| **P0** | `geteuid`/`getegid` → `nix::unistd` | -8 处 | `config.rs`, `monitor.rs` |
| **P0** | `chown` → `nix::unistd::chown` | -3 处 | `config.rs`, `monitor.rs`, `bin/fsmon.rs` |
| **P0** | `flock` → `fs2::FileExt` | -1 处 | `lib.rs` |
| **P1** | `getpwuid_r` → `users::get_user_by_uid` | -3 处 | `config.rs` |
| **P1** | 目录 `open`/`close` → `nix::fcntl` + RAII guard | -6 处 | `monitor.rs`, `fid_parser.rs` |
| **P1** | `std::env::set_var` 测试 → `temp-env` / SerialTest | -9 处 | `config.rs` |
| **P2** | Netlink 全程 → `nix::sys::socket` | -9 处 | `proc_cache.rs` |
| **P3** | FID 解析封装到安全函数 | 0 处（缩小 scope） | `fid_parser.rs` |
| **P3** | `name_to_handle_at` + RAII guard | 0 处（缩小 scope） | `dir_cache.rs`, `fid_parser.rs` |

**总计**: P0+P1 可消除 ~30 处 unsafe，生产代码 unsafe 从 35 降到 ~15

---

### 遗留问题（不可消除）

- FID 事件变长结构体 reinterpret（`fid_parser.rs` ~3 处）
- `name_to_handle_at()` / `open_by_handle_at()`（`dir_cache.rs` + `fid_parser.rs` ~4 处）
- `BorrowedFd::borrow_raw()` 实现 AsFd trait（`monitor.rs` 1 处）
- 底层 fd close 的 RAII 包装内部（本质需求）

这些是 Linux 系统编程中 Rust 安全抽象和 kernel C ABI 之间不可消除的桥梁。

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
