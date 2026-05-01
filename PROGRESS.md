# PROGRESS.md — fsmon 全面审查与规划

## 当前状态

- 编译通过，72 单元测试全绿，7 集成测试 `#[ignore]`
- Clippy 无警告（R1 已修复）
- P2 已完成：非 CREATE/MODIFY/CLOSE_WRITE 事件跳过 metadata syscall
- R5 已完成：fsmon.toml 配置文件支持
- H1 已完成：systemd 服务模板二进制路径动态检测

## 硬编码清理

全项目扫描发现约 20 处有修改价值的硬编码，分类如下：

### H1 [高] systemd 服务模板二进制路径 ✅ 动态生成(已完成)
`systemd.rs:7-28` — `ExecStart=/usr/local/bin/fsmon monitor %i`
- 二进制实际位置可能不同（`~/.cargo/bin/fsmon`, `/usr/bin/fsmon` 等）
- 安装时未检测 `current_exe()` 自动填充
- **修改**: 使用 `std::env::current_exe()` 在 `install` 时动态检测二进制路径
- **新增**: `--force` 选项支持覆盖已存在的 service 文件

### H2 [中] 默认日志路径重复定义 ✅ 提取常量(已完成)
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
