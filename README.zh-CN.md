<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">实时监控文件变更，追溯进程操作。</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## 特性

- **实时监控**: 默认捕获 8 种核心 fanotify 事件，`--all-events` 开启全部 14 种
- **进程追溯**: 追踪每个文件变更的 PID、命令名和用户 — 包括 `touch`、`rm`、`mv` 等短命进程
- **递归监控**: 监控整个目录树，追踪新建的子目录
- **完整删除捕获**: 通过持久化目录句柄缓存，完整捕获 `rm -rf` 递归删除中的每个文件
- **高性能**: Rust + Tokio，内存占用 <5MB，零拷贝 FID 解析，二分查找日志查询
- **Unix 哲学**: JSONL 日志格式 — `jq` 查询、`grep` 过滤、`sort` 排序。fsmon 只负责捕获和写入，过滤策略由你掌控
- **灵活的捕获过滤**: 按事件类型、大小、路径模式、进程名过滤 — 全部在 daemon 进程内完成，纳秒级，无 fork 开销
- **日常无需 sudo**: 仅 `sudo fsmon daemon` 需要 root（fanotify），其余命令普通用户可执行
- **热更新**: 守护进程运行时添加/移除路径，无需重启
- **磁盘安全网**: 可配置 `keep_days`（默认 30 天）和 `max_size`（默认 1GB），防止磁盘写满
- **无 systemd 架构**: 用户自己管理 daemon，配置按用户隔离

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式）
- **已测试的文件系统**: ext4、XFS、btrfs
- **构建工具**: Rust 工具链（`cargo`）

```bash
# 验证内核版本
uname -r  # 需要 ≥ 5.9

# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 安装

```bash
# 从源码构建
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo build --release

# 或从 crates.io 安装
cargo install fsmon
```

**fanotify 需要 root 权限运行 daemon：**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 使用

```bash
# 1. 启动守护进程（需要 sudo）
sudo fsmon daemon &

# 2. 添加监控路径（无需 sudo）
fsmon add /etc --types MODIFY
fsmon add /var/www --recursive --types MODIFY,CREATE
fsmon add /var/log --exclude-cmd "rsync|apt"     # 忽略构建噪音
fsmon add /tmp --only-cmd nginx                   # 只捕获 nginx

# 3. 列出已监控路径及其过滤配置
fsmon managed

# 4. 查询历史事件 — pipe 到 jq 做过滤
fsmon query --since 1h | jq 'select(.cmd == "nginx")'

# 5. 清理日志（默认 keep_days=30，max_size=1GB）
fsmon clean                       # 使用 config.toml 默认值
fsmon clean --keep-days 7         # CLI 覆盖
fsmon clean --dry-run             # 预览模式，不删除

# 6. 移除路径
fsmon remove /tmp

# 7. 停止 daemon
kill %1
```

**无 systemd，一切按用户隔离。**

### Pipe 示例

```bash
# 按 PID 过滤
fsmon query --since 1h | jq 'select(.pid == 1234)'

# 按事件类型过滤
fsmon query | jq 'select(.event_type == "MODIFY")'

# 按文件大小排序
fsmon query | jq -s 'sort_by(.file_size)[]'

# 组合过滤
fsmon query --since 1h | jq 'select(.cmd == "nginx" and .file_size > 10240)'

# 实时 tail
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.event_type == "CREATE")'
```

### 文件位置

| 用途 | 路径 | 格式 | 权限 |
|---|---|---|---|
| 基础设施配置 | `~/.config/fsmon/config.toml` | TOML（可手动编辑） | 用户所有 |
| Managed 路径数据库 | `~/.local/share/fsmon/managed.jsonl` | JSONL（每行一条目） | 用户所有 |
| 事件日志 | `~/.local/state/fsmon/*_log.jsonl` | JSONL（每行一事件） | 644 |

store 路径和日志目录均在 `~/.config/fsmon/config.toml` 中可配
（见 `[store].file` 和 `[logging].dir`）。
| Unix Socket | `/tmp/fsmon-<UID>.sock` | TOML over stream | 666 |

daemon 通过 sudo 以 root 运行，但通过 `SUDO_UID` + `getpwuid_r` 解析原始用户的 home 目录，
所以日志文件会写入 `/home/<你>/...` 而非 `/root/...`。

### 开机自启动（可选）

fsmon 不安装 systemd 服务。如需登录时自动启动：

```bash
crontab -e
@reboot /usr/local/bin/fsmon daemon &
```

## 捕获过滤

所有捕获过滤都在 daemon 进程内完成（纳秒级，无 fork），不匹配的事件不会写盘。

| 参数 | 类型 | 开销 | 原因 |
|------|------|------|------|
| `--types` | 内核 mask | 零 | fanotify 只传递匹配事件 |
| `--recursive` | 内核范围 | 零 | 监控子目录 |
| `--exclude` | 路径 regex | ~µs | 减少写盘 I/O |
| `--min-size` | u64 比较 | ~ns | 减少写盘 I/O |
| `--exclude-cmd` | 进程名 regex | ~µs | 减少写盘 I/O |
| `--only-cmd` | 进程名 regex | ~µs | 减少写盘 I/O |
| `--all-events` | 内核 mask | 零 | 开启全部 14 种事件 |

## 查询与清理

查询只保留性能攸关的参数，其余过滤通过 pipe 到标准 Unix 工具完成。

```
fsmon query                  →  扫所有日志文件，输出 JSONL
fsmon query --path /tmp      →  只读 /tmp 的日志文件
fsmon query --since 1h       →  二分搜索 + 输出
```

清理使用 config.toml 中的安全网默认值，可通过 CLI 覆盖：

```bash
# 优先级: CLI 参数 > config.toml > 代码默认值 (30 天)
fsmon clean                       # 使用 config 默认
fsmon clean --keep-days 60        # 覆盖默认值
```

## 配置

首次启动 daemon 或执行 `fsmon generate` 自动生成。包含磁盘安全网：

```toml
[logging]
dir = "~/.local/state/fsmon"
keep_days = 30          # 防止磁盘写满
max_size = "1GB"        # 单日志文件上限
```

## 事件类型

默认捕获 8 种核心事件，`--all-events` 开启全部 14 种。

**默认（8 种）：** CLOSE_WRITE、ATTRIB、CREATE、DELETE、DELETE_SELF、MOVED_FROM、MOVED_TO、MOVE_SELF

**额外（6 种，通过 --all-events）：** ACCESS、MODIFY、OPEN、OPEN_EXEC、CLOSE_NOWRITE、FS_ERROR

## 架构

```
Linux Kernel (fanotify)
    → FID 事件推入队列
    → tokio 异步读取
    → fid_parser 解析路径（两阶段 + 目录缓存）
    → Monitor 过滤（类型、大小、路径模式、进程名）
    → JSONL → 按路径分文件日志 (*_log.jsonl)

用户 pipe:
    cat/ tail *.jsonl → jq → 你的自定义逻辑
```

### 源码结构

```
src/
├── bin/fsmon.rs       CLI: daemon, add, remove, managed, query, clean, generate
├── lib.rs             FileEvent、EventType、清理引擎、临时文件安全
├── config.rs          基础设施配置、SUDO_UID 用户解析
├── store.rs           Managed 路径数据库（JSONL 格式）
├── monitor.rs         Fanotify 循环、socket 处理、所有捕获过滤
├── fid_parser.rs      FID 事件底层解析、两阶段路径恢复
├── dir_cache.rs       目录句柄缓存（rm -rf 路径恢复）
├── proc_cache.rs      Netlink proc 连接器（短命进程归因）
├── query.rs           二分查找日志查询、JSONL 输出
├── socket.rs          Unix socket 协议（TOML）、错误分类
├── utils.rs           大小/时间解析、uid 查询、路径→日志名哈希
└── help.rs            所有命令的帮助文本
```

## 许可证

[MIT License](./LICENSE)
