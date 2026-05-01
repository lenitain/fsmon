<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">实时 Linux 文件系统变更监控，精准追溯进程操作。</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## 特性

- **实时监控**: 默认捕获 8 种核心 fanotify 事件，`--all-events` 开启全部 14 种
- **进程追溯**: 追踪每个文件变更的 PID、命令名和用户 — 即使是 `touch`、`rm`、`mv` 等短命进程
- **递归监控**: 监控整个目录树，自动追踪新建的子目录
- **完整删除捕获**: 通过持久化目录句柄缓存，完整捕获 `rm -rf` 递归删除中的每个文件
- **高性能**: Rust + Tokio 编写，内存占用 <5MB，零拷贝 FID 事件解析，二分查找日志查询
- **灵活过滤**: 支持按时间、大小、进程、用户、事件类型和排除模式（通配符）过滤
- **多种格式**: 人类可读、JSON、CSV 三种输出格式
- **TOML 配置**: 持久化配置文件，支持 `~/.fsmon/config.toml` 或 `/etc/fsmon/config.toml`
- **日志管理**: 基于时间和大小的日志轮转，支持预览模式
- **Systemd 服务**: 安装为 systemd 服务，安全加固可配置

## 为什么选择 fsmon

是否曾想知道"谁修改了这个文件？"这正是 fsmon 要解决的问题。

传统的文件监控工具只给你事件本身，而没有上下文 — fsmon 桥接了这段空白，将每个文件变更归因到对应的进程。无论是恶意脚本、自动化部署还是配置错误的服务，你都能准确知道发生了什么、何时发生的、以及是谁（或什么）导致的。

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式）
- **已测试的文件系统**: ext4、XFS、tmpfs、btrfs （注：推荐 Linux 6.8+ 内核以获得btrfs递归操作的完整支持）
- **构建工具**: Rust 工具链（`cargo`）

```bash
# 验证内核版本
uname -r  # 需要 ≥ 5.9

# 如未安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 安装

```bash
# 从源码构建
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo install --path .

# 或从 crates.io 安装
cargo install fsmon
```

**注意：Fanotify需要管理员权限**
```bash
# 方法1：复制到 /usr/local/bin（推荐）
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/

# 方法2：直接使用完整路径
sudo ~/.cargo/bin/fsmon monitor ...
```

### 基础用法

```bash
# 监控目录
sudo fsmon monitor /etc --types MODIFY

# 递归监控
sudo fsmon monitor ~/myproject --recursive

# 排除模式
sudo fsmon monitor /var/log --exclude "*.log"

# 安装为 systemd 服务，长期审计
sudo fsmon install /var/log /etc -o /var/log/fsmon-audit.log

# 查询历史事件
fsmon query --since 1h --cmd nginx

# 预览清理旧日志
fsmon clean --keep-days 7 --dry-run

# 查看服务状态
fsmon status
```

## 示例

### 排查配置文件变更

```bash
# 监控 /etc 的修改
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log

# 另一个终端执行修改
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# 查询结果
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY
```

### 追踪大文件创建

```bash
# 监控大于 50MB 的文件创建
sudo fsmon monitor /tmp --types CREATE --min-size 50MB --format json

# 触发
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100
```

### 审计删除操作

```bash
# 捕获完整的递归删除
sudo fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log

# 触发
rm -rf ~/test-project/build/

# 输出显示每个被删除的文件（包括子目录中的）
[2026-01-15 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm)
[2026-01-15 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm)
```

### 组合过滤查询

```bash
# 查询最近 1 小时 nginx 的操作，按文件大小排序
fsmon query --since 1h --cmd nginx* --sort size

# 仅监控 CREATE 和 DELETE 事件，排除临时文件
sudo fsmon monitor /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## 命令参考

```bash
fsmon monitor --help    # 实时监控（fanotify）
fsmon query --help      # 查询历史日志（支持过滤和排序）
fsmon clean --help      # 按时间或大小清理旧日志
fsmon status            # 查看 systemd 服务状态
fsmon stop              # 停止 systemd 服务
fsmon start             # 启动 systemd 服务
fsmon install --help    # 安装 systemd 服务（自动检测二进制路径）
fsmon uninstall         # 卸载 systemd 服务
```

## 配置文件

fsmon 支持 TOML 配置文件，路径为 `~/.fsmon/config.toml` 或 `/etc/fsmon/config.toml`：

```toml
[monitor]
paths = ["/var/log", "/tmp"]
min_size = "100MB"
types = "MODIFY,CREATE"
exclude = "*.tmp"
all_events = true
output = "/var/log/fsmon.log"
format = "json"
recursive = true
buffer_size = 65536

[query]
log_file = "/var/log/fsmon.log"
since = "1h"
format = "json"
sort = "size"

[clean]
keep_days = 7
max_size = "500MB"

[install]
protect_system = "false"
protect_home = "false"
read_write_paths = ["/var/log", "/tmp"]
private_tmp = "no"
```

CLI 参数优先级高于配置文件。

## 技术架构

### 模块

| 模块 | 说明 |
|------|------|
| `main.rs` | CLI 入口，clap 命令定义，`FileEvent` 结构体，日志清理引擎 |
| `monitor.rs` | 核心 fanotify 监控循环，作用域过滤，LRU 文件大小追踪 |
| `fid_parser.rs` | 底层 FID 模式事件解析，两阶段路径恢复 |
| `dir_cache.rs` | 基于 `name_to_handle_at` 的目录句柄缓存，恢复已删除文件路径 |
| `proc_cache.rs` | Netlink proc connector 监听器 — 在进程 `exec()` 时捕获短命进程信息 |
| `query.rs` | 日志文件查询，二分查找优化，多条件组合过滤 |
| `config.rs` | TOML 持久化配置管理 |
| `systemd.rs` | Systemd 服务生命周期管理（安装、卸载、状态、启停） |
| `output.rs` | 事件输出格式化（人类可读、JSON、CSV） |
| `utils.rs` | 大小/时间解析、进程信息获取、UID 查询 |
| `help.rs` | 所有命令的集中帮助文本 |

### 数据流

```
Linux Kernel (fanotify)
    → FID 事件推入队列
    → tokio::select 异步读取事件
    → fid_parser 解析 FID 记录（两阶段：解析 + 缓存恢复）
    → Monitor 过滤（类型、大小、排除模式、作用域）
    → output 格式化（human/json/csv）→ stdout + 可选文件
```

- **fanotify (FID 模式 + FAN_REPORT_NAME)**：内核推送文件事件时携带目录文件句柄和文件名。无需轮询，事件通过非阻塞 read 即时送达。
- **Proc Connector**：后台线程订阅 netlink `PROC_EVENT_EXEC` 通知，在每个进程 exec 时缓存 `(pid, cmd, user)`。确保短命进程（`touch`、`rm`、`mv`）即使退出后也能被归因。
- **FID 解析器 + 目录缓存**：两阶段事件处理：(1) 通过 `open_by_handle_at` 解析文件句柄，(2) 使用持久化目录句柄缓存恢复父目录已被删除的事件路径。处理多层嵌套的 `rm -rf` 场景。
- **二分查找查询**：`fsmon query` 在大致按时间排序的日志文件上使用二分查找，将扫描范围缩小到 O(log N) 次 seek。配合 `expand_offset_backward` 处理边界附近的轻微乱序。
- **Rust + Tokio**：单线程异步循环（`tokio::select` 在 fanotify fd 和 Ctrl+C 信号之间）。proc connector 使用独立后台线程。无需复杂并发 — 高效优先。

### 事件挂载策略

fsmon 使用两级挂载策略：
1. **FAN_MARK_FILESYSTEM**（首选）：标记目标路径所在的整个挂载点 — 新建文件无竞态窗口。若遇到 `EXDEV`（btrfs 子卷）则降级。
2. **Inode 标记降级**：逐个标记目录，`--recursive` 模式下递归遍历。实时动态标记新建的目录。

### 事件类型

默认捕获 8 种核心事件。使用 `--all-events` 可开启全部 14 种。

**默认事件（8 种）：**

| 事件 | 说明 |
|------|------|
| CLOSE_WRITE | 文件写完后关闭（最佳"已修改"信号） |
| ATTRIB | 元数据变更（权限、时间戳、所有者） |
| CREATE | 文件/目录已创建 |
| DELETE | 文件/目录已删除 |
| DELETE_SELF | 被监控对象自身被删除 |
| MOVED_FROM | 文件从监控目录移出 |
| MOVED_TO | 文件移入监控目录 |
| MOVE_SELF | 被监控对象自身被移动 |

**额外事件（6 种，使用 --all-events）：**

| 事件 | 说明 |
|------|------|
| ACCESS | 文件被读取 |
| MODIFY | 文件内容被写入（非常频繁） |
| OPEN | 文件/目录被打开 |
| OPEN_EXEC | 文件被打开用于执行 |
| CLOSE_NOWRITE | 只读文件被关闭 |
| FS_ERROR | 文件系统错误（Linux 5.16+） |

## 许可证

[MIT License](./LICENSE)
