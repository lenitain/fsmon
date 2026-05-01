# PROGRESS.md — fsmon 全面审查与规划

## 当前状态

- 编译通过，53 单元测试全绿，8 集成测试 `#[ignore]`
- Clippy 无警告（R1 已修复）
- P2 已完成：非 CREATE/MODIFY/CLOSE_WRITE 事件跳过 metadata syscall

---

## 1. Bug

### B1 [高] proc_cache `EINTR` 信号中断导致监听线程退出 ✅ 已修复
`proc_cache.rs:100` — `libc::recv` 返回 `-1(errno=EINTR)` 时，循环直接 `break`，proc connector 线程死亡。短生命周期进程缓存失效，直到重启。
**修复**: 对 `EINTR` 做 `continue` 重试；检查 `errno` 区分致命错误。

### B2 [中] DELETE/DELETE_SELF 事件 `size_change` 恒为 0 ✅ 已修复
`monitor.rs:343` — 文件已删除后 `fs::metadata` 失败返回 `unwrap_or(0)`，无法记录删除前大小。
**修复**: 添加 `file_size_cache: HashMap<PathBuf, u64>` 缓存文件大小。CREATE/MODIFY/CLOSE_WRITE 事件时缓存实际大小，DELETE/DELETE_SELF/MOVED_FROM 事件时使用缓存值。

### B3 [中] DELETE/DELETE_SELF 事件路径为空（预存在目录） ✅ 已修复
`monitor.rs:620-634` — 第二遍恢复循环只检查 `dfid_name_handle`，未检查 `self_handle`。预存目录的 DELETE_SELF 事件（kernel 只发 DFID record，无 DFID_NAME）无法从 `dir_cache` 恢复路径。
**修复**: 在恢复循环中添加 `self_handle` → `dir_cache` 查找分支。

### B4 [中] 包含 `keep_days=0` 清理时 `chrono::Duration::days(0)` 会返回 0 天正常
当前代码 `keep_days: u32` 从 CLI 接收，默认 30 天。传 0 表示"保留 0 天"即清空。当前逻辑正确。

---

## 2. 性能优化

### P1 [高] 启动时递归缓存整个目录树句柄 ✅ 已修复
`monitor.rs:275-280` — `cache_recursive` 对每个监控路径递归遍历所有子目录做 `name_to_handle_at`。监控 `/` 时会遍历数百万目录，耗时分钟级，内存爆炸。
**修复**: 懒加载按需缓存，启动时只缓存监控根目录，子目录通过事件驱动增量缓存（CREATE/MOVED_TO 时 cache_recursive）。

### P2 [中] 每次事件调用 `fs::metadata` syscall ✅ 已修复
`monitor.rs:343` — 每个 fanotify 事件都做一次 `stat` 系统调用，高频场景下（>10K events/s）开销显著。
**修复**: 对非 CREATE/MODIFY/CLOSE_WRITE 事件，跳过 metadata 读取，改用缓存值或 0。DELETE/DELETE_SELF/MOVED_FROM 从缓存移除并返回缓存值；其他事件（OPEN, ACCESS, ATTRIB 等）直接读缓存，无缓存返回 0。

### P3 [中] 日志查询不做二进制搜索 ✅ 已修复
`query.rs:89-145` — 每次 `fsmon query` 全量扫描 JSON 日志文件。日志文件数百 MB 时查询慢。
**修复**: 利用日志文件按时间有序写入，对时间范围查询做二分搜索。`seek_and_parse_time` 定位字节偏移处的时间戳；`find_offset_for_time` 和 `find_end_offset_for_time` 分别二分查找起止偏移；`expand_offset_backward` 向前扩展 50 行以覆盖轻微乱序。无时间过滤时回退全量扫描。11 项新测试覆盖边界情况（空前、全前、全后、单条、组合过滤、大文件）。
**后续修复**: (1) `scan_back` 缓冲区从 256 增至 4096 字节，避免长 JSON 行导致错位；(2) `expand_offset_backward` 从 O(offset) 全文件扫描改为有界窗口扫描，大文件性能从 ~10GB 读取降至 ~25KB。新增 3 项测试覆盖长行、少行、大行场景。68 测试全绿。

### P4 [低] `find_tail_offset` + `truncate_from_start` 内存开销大 ✅ 已修复
`main.rs` — `truncate_from_start` 读取整个文件尾部到内存再重写。max_size=100MB 时读取 100MB 到内存。
**修复**: `truncate_from_start` 改为流式读写：8KB 缓冲区分块读取源文件 offset 后的内容，写入同目录临时文件 `.fsmon_trunc_tmp`，然后原子 rename 覆盖原文件。`find_tail_offset` 已有界（max_bytes + 4096），无需修改。

### P5 [低] 监控循环固定 10ms sleep
`monitor.rs:322` — 无事件时也等 10ms。FAN_NONBLOCK 模式下可用 epoll 实现零延迟唤醒。
**修复**: 用 `tokio::io::unix::AsyncFd` 包装 fan_fd，事件驱动。

### P6 [中] `file_size_cache` 无限增长（B2 后续）✅ 已修复
`monitor.rs:36` — `HashMap<PathBuf, u64>` 仅 DELETE/DELETE_SELF/MOVED_FROM 时移除条目。文件被打开、写入、重命名后条目永久累积，长时间监控 `/` 时内存缓慢泄漏。
**修复**: 用 `lru::LruCache`（容量 10000）替换 `HashMap`。`put`/`pop` 替代 `insert`/`remove`，LRU 淘汰最久未访问条目。

---

## 3. 代码质量与设计改进

### R1 [中] clippy 修复 ✅ 已修复
`query.rs:153,156,159` — `sort_by` 改为 `sort_by_key`，共 3 处。

### R2 [中] `monitor.rs` 文件过大 (1253→579行) ✅ 已修复
拆分：`fid_parser.rs`（FID 事件解析）、`dir_cache.rs`（目录句柄缓存）、`output.rs`（事件格式化输出）。
**修复**: 将 FID 事件解析逻辑移至 `fid_parser.rs`（含 `HandleKey`、`FidEvent`、`read_fid_events`、`extract_dfid_name`、`extract_fid`、`resolve_file_handle`、`mask_to_event_types`）；目录句柄缓存移至 `dir_cache.rs`（`path_to_handle_key`、`cache_dir_handle`、`cache_recursive`）；事件格式化输出移至 `output.rs`（`output_event` 自由函数）。`monitor.rs` 仅保留 `Monitor` 结构体及其方法和目录标记辅助函数。53 测试全绿，clippy 无警告。

### R3 [中] 事件类型用字符串比较 ✅ 已修复
`event_types: Option<Vec<String>>` — 字符串比较易手误。应使用 `enum EventType` 枚举 + `FromStr`/`Display`。
**修复**: 添加 `EventType` 枚举（14种事件类型），实现 `FromStr`/`Display`/`Serialize`/`Deserialize`。将 `FileEvent.event_type`、`Monitor.event_types`、`Query.event_types` 从 `String` 改为 `EventType`，所有比较改为类型安全的枚举比较。

### R4 [中] 排除模式 glob → regex 转换不安全 ✅ 已修复
`monitor.rs:102` — `"*.tmp".replace("*", ".*")` 不转义正则元字符。`test.tmp` 中的 `.` 会匹配任意字符。路径含正则元字符时行为异常。
**修复**: 用 `regex::escape` 前处理，再将 `\*` 替换为 `.*`，`\?` 替换为 `.`。

### R5 [低] 无配置文件支持
所有配置通过 CLI 参数传入。systemd 服务切到长时间运行后，调整参数需要重新 `install`。
**提议**: 增加 `fsmon.toml` 配置文件，CLI 参数覆盖配置项。

### R6 [低] 无自动日志轮转
`fsmon clean` 为手动命令。长时间运行的 daemon 日志会无限增长。
**提议**: 集成 `ReadWritePaths=/var/log` 时按大小自动 truncate 当前日志，或支持 log rotate signal。

### R7 [低] `uid_passwd_map` 使用 `OnceLock` 永不过期
`utils.rs:167-186` — 长时间运行的用户增删不体现。
**提议**: 可配置刷新间隔，或监听 `/etc/passwd` inotify 事件。

### R8 [低] `OutputFormat::Csv` 日志文件仍存储 JSON
`monitor.rs:418-433` — 终端显示 CSV，但 `output_file` 始终写 JSON。文档一致（日志统一 JSON），但可提醒。

---

## 规划优先级

| 优先级 | 类别 | 项 | 预估复杂度 |
|--------|------|----|-----------|
| P0 | Bug | B1 EINTR 修复 | 小 |
| P0 | Bug | B2 DELETE size_change | 中 |
| P0 | 质量 | R1 clippy 修复 | 极小 |
| P1 | 性能 | P1 树缓存懒加载 | 大 |
| P1 | 质量 | R4 glob → regex ✅ | 小 |
| P2 | 质量 | R3 EventType 枚举 ✅ | 中 |
| P2 | 质量 | R2 monitor.rs 拆分 | 大 |
| P3 | 性能 | P2 减少 metadata syscall | 中 |
| P3 | 性能 | P3 日志索引查询 | 大 |
| P4 | 性能 | P4 内存优化 ✅ | 中 |
| P4 | 性能 | P6 file_size_cache 无限增长 | 中 |
| P4 | 质量 | R5-R8 增强 | 小-中 |

**下一步建议**: 处理 P5 epoll 事件驱动 或 R5-R8 增强。
