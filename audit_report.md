# Rust API Guidelines 审计报告 — `fsmon v0.4.15`

**范围**：`src/**/*.rs`、`Cargo.toml` | **代码**：~13,650 行 | **日期**：2026-06-29  
**审计分支**：`lenitain/fix-default-impl` | **最新提交**：`53271b8`

---

## 全部 48 项检查结果

| # | 规则 | 状态 | 说明 |
|---|------|:----:|------|
| | **Naming** | | |
| 1 | C-CASE (RFC 430) | ✅ | 类型 PascalCase，函数 snake_case，常量 SCREAMING_SNAKE_CASE |
| 2 | C-CONV (as_/to_/into_) | ✅ | `to_jsonl_string()`, `from_jsonl_str()`, `AsRawFd` 命名正确 |
| 3 | C-GETTER (get_ 前缀) | ✅ | 按值返回无需 `get_`；`is_enabled()`/`is_empty()` 用 `is_` |
| 4 | C-ITER | ✅ | 无违反 |
| 5 | C-ITER-TY | ✅ | 无公开自定义 Iterator |
| 6 | C-FEATURE | ✅ | features 均匹配依赖名 |
| 7 | C-WORD-ORDER | ✅ | `resolve_uid_gid`/`resolve_uid`/`resolve_home` 一致 |
| | **Interoperability** | | |
| 8 | C-COMMON-TRAITS | ✅ | `PathOptions` 和 `MonitorConfig` 现已实现 `Default`（2026-06-29 修复） |
| 9 | C-CONV-TRAITS | ✅ | `FromStr`, `TryFrom`, `From`, `AsRawFd` 均有 |
| 10 | C-COLLECT | ✅ | 无自定义集合 |
| 11 | C-SERDE | ✅ | 公共类型均 `Serialize+Deserialize` |
| 12 | C-SEND-SYNC | ✅ | 基于 `Arc`/tokio，自动满足 |
| 13 | C-GOOD-ERR | ⚠️ | `ParseEventTypeError`/`SocketError` 有 `Display+Error`。公共 API 用 `anyhow::Result`，用户无法 match 错误变体 |
| 14 | C-RW-VALUE | ✅ | 无公开 Read/Write |
| | **Macros** | | |
| 15 | 适用性 | ✅ | 仅内部 `debug_log!`，未导出 |
| | **Documentation** | | |
| 16 | C-CRATE-DOC | ✅ | `lib.rs` 有完整 crate 级文档+示例+模块结构 |
| 17 | C-EXAMPLE | ⚠️ | `FileEvent`/`Config`/`PathOptions` 有示例。8 个公共类型缺少示例 |
| 18 | C-QUESTION-MARK | ⚠️ | 业务代码用 `?`。`to_jsonl_string()` 用 `.expect()`（serde 不失败，可接受） |
| 19 | C-FAILURE | ⚠️ | 5 个公共函数缺少 `# Errors` 文档 |
| 20 | C-LINK | ⚠️ | 有模块链接。缺少外部链接（fanotify/inotify 内核文档） |
| 21 | C-METADATA | ✅ | `rust-version` 和 `documentation` 已添加（2026-06-29 修复） |
| 22 | C-RELNOTES | ✅ | `CHANGELOG.md` 存在（50KB），格式规范 |
| 23 | C-HIDDEN | ✅ | `pub(crate)` 广泛使用，`debug_log!` 未导出 |
| | **Predictability** | | |
| 24 | C-CONV-SPECIFIC | ✅ | `to_jsonl_string()` 特化命名 |
| 25 | C-METHOD | ✅ | 全部正确 |
| 26 | C-NO-OUT | ✅ | 全部通过返回值 |
| 27 | C-OVERLOAD | ✅ | 方法名具体无歧义 |
| 28 | C-DEREF | ✅ | 无自定义 Deref |
| 29 | C-CTOR | ✅ | `new()`/`acquire()` 语义化命名 |
| | **Flexibility** | | |
| 30 | C-INTERMEDIATE | ✅ | `Option<Vec<EventType>>`/全 `Option<T>` 配置 |
| 31 | C-CALLER-CONTROL | ✅ | 过滤器可配置，配置合并链完整 |
| 32 | C-GENERIC | ✅ | `retry()` 通用辅助 |
| 33 | C-OBJECT | ✅ | 公开 API 无 `Box<dyn Trait>` |
| | **Type Safety** | | |
| 34 | C-NEWTYPE | ✅ | `FanFd(RawFd)`/`DaemonLock(UnixListener)`/`DiskFreeThreshold` |
| 35 | C-CUSTOM-TYPE | ✅ | `CdTarget`/`ErrorKind`/`SocketCmd` 等均为枚举 |
| 36 | C-BITFLAG | ✅ | Fanotify 掩码 `u64` 常量组合 |
| 37 | C-BUILDER | ⚠️ | `MonitorConfig` 12 字段但仅内部使用，可接受 |
| | **Dependability** | | |
| 38 | C-VALIDATE | ⚠️ | 多处有验证。`PathEntry` 构造未验证路径；`SocketCmd` 反序列化后未验证 |
| 39 | C-DTOR-FAIL | ✅ | `DaemonLock::drop()` 用 `let _ =` 忽略错误 |
| 40 | C-DTOR-BLOCK | ✅ | 同步删除，非阻塞 |
| | **Debuggability** | | |
| 41 | C-DEBUG | ✅ | 全部有 Debug（`FanFd`/`FsGroup`/`HelpTopic`/`CounterVec`/`IntGauge`/`MetricsRegistry`/`MonitorConfig`/`Monitor`/`Query`/`Watchdog`/`FileWriter`/`ReaderState`/`ChannelReader` 手动实现，隐藏内部字段） |
| 42 | C-DEBUG-NONEMPTY | ✅ | 无 unwrap/panic |
| | **Future Proofing** | | |
| 43 | C-SEALED | ⚠️ | `TimeFilterExt` 是公开 trait，实现类型 `TimeFilter` 来自外部 crate，无法密封 |
| 44 | C-STRUCT-PRIVATE | ⚠️ | `FileEvent`/`Config`/`PathOptions` 等字段 `pub`（需 serde），CLI 工具可接受 |
| 45 | C-NEWTYPE-HIDE | ✅ | 大部分实现细节 `pub(crate)` |
| 46 | C-STRUCT-BOUNDS | ✅ | 无多余 trait bound |
| | **Necessities** | | |
| 47 | C-STABLE | ✅ | `edition = "2024"`，无 nightly |
| 48 | C-PERMISSIVE | ✅ | MIT，依赖许可证兼容 |

---

## 统计

| | ✅ | ⚠️ | ❌ |
|--|:---:|:---:|:---:|
| **修复前** | 38 | 8 | 2 |
| **修复后** | **40** | **8** | **0** |

**合规率**：✅ **83.3%**（40/48）（修复前 79.2%）

---

## 已完成的修复（2026-06-29）

| # | 问题 | 修复方案 | 状态 |
|---|------|----------|:----:|
| 1 | `PathOptions` 缺少 `Default` | 添加 `impl Default for PathOptions`（`recursive: false`, 全 `None`） | ✅ |
| 2 | `MonitorConfig` 缺少 `Default` | 添加 `impl Default for MonitorConfig`（全 `None`/`false`） | ✅ |
| 3 | Cargo.toml 缺少 `rust-version` | 添加 `rust-version = "1.85"` | ✅ |
| 4 | Cargo.toml 缺少 `documentation` | 添加 `documentation = "https://docs.rs/fsmon"` | ✅ |

---

## 剩余改进项（中优先级）

| # | 问题 | 规则 | 修复方案 |
|---|------|------|----------|
| 5 | 8 个公共类型缺示例 | C-EXAMPLE | 为 `DaemonLock`/`Monitor`/`MonitorConfig`/`Monitored`/`SocketCmd`/`Query`/`Watchdog`/`MetricsRegistry` 添加 `# Examples` |
| 6 | 5 个公共函数缺错误文档 | C-FAILURE | 为 `DaemonLock::acquire()`/`Config::load()`/`Monitored::save()`/`Monitor::new()`/`send_cmd()` 添加 `# Errors` |

---

## 剩余改进项（低优先级）

| # | 问题 | 规则 | 修复方案 |
|---|------|------|----------|
| 7 | 公共 API 用 `anyhow::Result` | C-GOOD-ERR | 若作库发布，定义 `thiserror` 枚举错误 |
| 8 | 缺少外部链接 | C-LINK | 添加 [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html) 等 |
| 9 | `to_jsonl_string()` 用 `expect()` | C-QUESTION-MARK | 改为 `?` 或 `unreachable!` |
| 10 | 公共字段为 `pub` | C-STRUCT-PRIVATE | 若长期维护，改 `pub(crate)` + getter |

---

**结论**：`fsmon` 整体 API 设计质量较高（83.3% 合规）。已修复所有 ❌ 项（高优先级问题），文档示例现在可以编译。剩余 ⚠️ 项为文档完善和可选改进，不影响功能正确性。
