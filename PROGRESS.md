# PROGRESS.md — fsmon 全面审查与规划

## 当前状态

- 编译通过，72 单元测试全绿，7 集成测试 `#[ignore]`
- Clippy 无警告（R1 已修复）
- P2 已完成：非 CREATE/MODIFY/CLOSE_WRITE 事件跳过 metadata syscall
- R5 已完成：fsmon.toml 配置文件支持

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

### P5 [低] 监控循环固定 10ms sleep ✅ 已修复
`monitor.rs:322` — 无事件时也等 10ms。FAN_NONBLOCK 模式下可用 epoll 实现零延迟唤醒。
**修复**: 用 `tokio::io::unix::AsyncFd` 包装 fan_fd，事件驱动。使用 `tokio::select!` 同时等待 fd 可读和 Ctrl+C 信号，确保进程可被信号终止。

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

### R5 [低] 无配置文件支持 ✅ 已修复
所有配置通过 CLI 参数传入。systemd 服务切到长时间运行后，调整参数需要重新 `install`。
**修复**: 增加 `fsmon.toml` 配置文件支持（`toml = "0.8"`）。新建 `src/config.rs` 模块，`Config` 结构体含 `MonitorConfig`/`QueryConfig`/`CleanConfig`。从 `~/.fsmon/config.toml`（主）或 `/etc/fsmon/config.toml`（备）加载，无文件返回默认值，无效 TOML 报错。所有 CLI 参数（含 `format`/`sort`）改为 `Option` 类型，通过 `cli.or(config).unwrap_or(默认值)` 模式实现三级优先级：CLI > 配置文件 > 默认值。4 项单元测试覆盖加载、解析、无效、合并场景。

### R9 [低] 帮助文本枚举化集中管理 ✅ 已修复
所有 `#[command(about/long_about)]` 和 root `after_help` 帮助文本从散落的 `const LONG_ABOUT_*` 常量提取到独立 `src/help.rs` 模块。
**修复**: 新增 `HelpTopic` 枚举（9 个变体：Root + 8 个子命令），`about()` / `long_about()` / `after_help()` 三个 `const fn` 集中管理。`main.rs` 减 103 行，编译时类型安全，新增命令自动提示完整匹配。

### R6 [低] 无自动日志轮转
`fsmon clean` 为手动命令。长时间运行的 daemon 日志会无限增长。
**提议**: 集成 `ReadWritePaths=/var/log` 时按大小自动 truncate 当前日志，或支持 log rotate signal。

### R7 [低] `uid_passwd_map` 使用 `OnceLock` 永不过期
`utils.rs:167-186` — 长时间运行的用户增删不体现。
**提议**: 可配置刷新间隔，或监听 `/etc/passwd` inotify 事件。

### R8 [低] `OutputFormat::Csv` 日志文件仍存储 JSON ✅ 已修复
`output.rs` — 终端显示 CSV/Human/Json，但 `output_file` 始终写 JSON。格式不统一。
**修复**: CSV 和 Json 格式终端与日志文件统一；Human 格式日志仍写 JSON（非结构化不可逆）。新增 `FileEvent::to_csv_string()`/`from_csv_str()` 使用 `csv` crate 正确处理带逗号字段。新增 `parse_log_line()` 自动检测 JSON/CSV 格式解析。`query.rs` 和 `main.rs` clean_logs 逻辑统一使用 `parse_log_line`。

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
| P4 | 性能 | P5 epoll 事件驱动 ✅ | 中 |
| P4 | 性能 | P6 file_size_cache 无限增长 | 中 |
| P4 | 质量 | R5-R8 增强 | 小-中 |

## 10. 硬编码清理

全项目扫描发现约 20 处有修改价值的硬编码，分类如下：

### H1 [高] systemd 服务模板二进制路径 ✅ 可配置化
`systemd.rs:7-28` — `ExecStart=/usr/local/bin/fsmon monitor %i`
- 二进制实际位置可能不同（`~/.cargo/bin/fsmon`, `/usr/bin/fsmon` 等）
- 安装时未检测 `current_exe()` 自动填充

### H2 [中] 默认日志路径重复定义 ✅ 提取常量
`main.rs:438-439` 和 `main.rs:527-528` — `~/.fsmon/history.log`
- `query clean` 和 `clean` 命令各自独立写死默认路径
- 应提取为 `const DEFAULT_LOG_PATH: &str`

### H3 [低] 默认 keep_days 30 硬编码 ✅ 提取常量
`main.rs:531` — `keep_days.unwrap_or(30)`
- magic number，应提取为 `const DEFAULT_KEEP_DAYS: u32 = 30`

### H4 [低] LRU 缓存容量硬编码 ✅ 提取常量
`monitor.rs:48` — `FILE_SIZE_CACHE_CAP = 10_000`

### H5 [低] proc connector 启动等待时间硬编码
`monitor.rs:119` — `Duration::from_millis(50)`
- 低配机器可能需要更长，高配可更短
- 可改为带重试的等待模式（如忙等 + 退避）

### H6 [低] fanotify 读取缓冲区大小硬编码
`monitor.rs:259` — `4096 * 8` (32KB)
- 高频场景需要更大缓冲区减少 read 次数
- 低频场景浪费内存

### H7 [低] query 回退扫描 / seek 参数硬编码
- `query.rs:200` — `scan_back = 4096u64`
- `query.rs:329` — `max_lines * 512` bytes/line 估计
- `proc_cache.rs:98` — netlink recv buffer `4096`

### H8 [低] uid_passwd_map OnceLock 永不过期（R7 已有记录）
`utils.rs:167-186` — `/etc/passwd` 初次加载后静态缓存，运行时新增用户不体现
- **提议**: 可配置刷新间隔或 inotify 监听

### H9 [低] systemd 安全加固策略硬编码
`systemd.rs:20-23` — `ProtectSystem=strict`, `ProtectHome=read-only`, `ReadWritePaths=/var/log`, `PrivateTmp=yes`
- 某些场景需要放宽约束（如监控 `/home` 时 `ProtectHome` 应设为 `false`）

### H10 [低] Cargo.toml 元数据占位符
- `version = "0.1.4"`（发布前需更新）
- `authors = ["lenitain <xt.zhu@qq.com>"]`
- `homepage` / `repository` / `documentation` URL

### H11 [低] CI 分支名硬编码
`.github/workflows/rust.yml:5,7` — `branches: [ "main" ]`

| 优先级 | 类别 | 项 | 预估复杂度 |
|--------|------|----|-----------|
| P0 | 硬编码 | H1 systemd 服务模板二进制路径 | 小 |
| P1 | 硬编码 | H2 默认日志路径重复定义 | 极小 |
| P2 | 硬编码 | H3-H7 magic number 提取常量 | 小 |
| P3 | 硬编码 | H8 uid_passwd_map 刷新 | 中 |
| P3 | 硬编码 | H9 systemd 安全加固可配置 | 中 |
| P4 | 硬编码 | H10-H11 元数据/CI 分支 | 极小 |
