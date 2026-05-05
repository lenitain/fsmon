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
- **高性能**: Rust + Tokio 编写，内存占用 <5MB，零拷贝 FID 事件解析，二分查找日志查询
- **灵活过滤**: 支持按时间、大小、进程、用户、事件类型和排除模式（通配符）过滤
- **多种格式**: 人类可读、JSON、CSV 三种终端输出格式（日志文件始终是JSON格式）
- **TOML 配置**: 持久化配置位于 `/etc/fsmon/fsmon.toml`
- **日志管理**: 基于时间和大小的日志轮转，支持 dry-run 预览
- **动态路径管理**: 通过 Unix socket 运行时添加/移除监控路径（无需重启守护进程）
- **Systemd 服务**: 单实例 systemd 服务，支持自动重启和 fanotify 能力集

## 为什么选择 fsmon

fsmon用于记录"谁修改了这个文件？"。

传统的文件监控工具只提供事件本身，缺少上下文。fsmon 将每个文件变更归因到对应的进程。无论是恶意脚本、自动化部署还是配置错误的服务。

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式）
- **已测试的文件系统**: ext4、XFS、btrfs （注：推荐 Linux 6.18+ 内核以获得btrfs递归操作的完整支持）
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
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 守护进程模式 — 后台监控

```bash
# 1. 安装 systemd 服务（一次性）
sudo fsmon install

# 2. 启动守护进程
sudo systemctl enable fsmon --now
sudo systemctl status fsmon

# 3. 添加监控路径（实时生效，无需重启）
sudo fsmon add /etc --types MODIFY
sudo fsmon add /var/www --recursive --types MODIFY,CREATE
sudo fsmon add /tmp --all-events

# 4. 列出已监控路径
sudo fsmon managed

# 5. 查询历史事件
fsmon query --since 1h --cmd nginx

# 6. 清理旧日志（dry-run 预览）
fsmon clean --keep-days 7 --dry-run

# 7. 移除监控路径
sudo fsmon remove /tmp

# 8. 停止守护进程
sudo systemctl stop fsmon
```

配置读取自 `/etc/fsmon/fsmon.toml`。通过 `fsmon add` 添加的路径会持久化到配置中，守护进程重启后自动恢复。



## 示例

### 排查配置文件变更

```bash
# 添加 /etc 监控
sudo fsmon add /etc --types MODIFY

# 另一个终端执行修改
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# 查询结果
sudo fsmon query --since 1h --types MODIFY
```

### 追踪大文件创建

```bash
# 监控大文件创建
sudo fsmon add /tmp --types CREATE

# 触发
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# 查询时按最小大小过滤
sudo fsmon query --since 1m --min-size 50MB --format json
```

### 审计删除操作

```bash
# 添加删除事件监控
sudo fsmon add ~/.projects --types DELETE --recursive

# 触发
rm -rf ~/.projects/fsmon-test/

# 输出显示每个被删除的文件（包括子目录中的）
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test/hello.c (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
[2026-05-04 21:37:47] [DELETE] /home/pilot/.projects/fsmon-test (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
```

### 组合过滤查询

```bash
# 查询最近 1 小时 nginx 的操作，按文件大小排序
sudo fsmon query --since 1h --cmd nginx* --sort size

# 添加监控并排除临时文件
sudo fsmon add /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## 命令参考

```bash
fsmon add /var/www -r           # 添加监控路径（实时 + 持久化）
sudo fsmon remove /var/www       # 移除监控路径
sudo fsmon managed               # 列出所有监控路径及配置
fsmon query --since 1h          # 查询历史事件（支持过滤）
fsmon clean --keep-days 7       # 按时间或大小清理日志
sudo fsmon install               # 安装 systemd 服务和默认配置
sudo fsmon uninstall             # 卸载 systemd 服务
```

使用 `fsmon <COMMAND> --help` 查看每个子命令的详细帮助。

## 架构

fsmon 以 systemd 管理的后台守护进程运行，持久化配置位于 `/etc/fsmon/fsmon.toml`。

```bash
sudo fsmon install              # 安装 systemd 服务
sudo systemctl enable fsmon --now
```

| 项目 | 说明 |
|------|------|
| 配置文件 | `/etc/fsmon/fsmon.toml` |
| 路径管理 | `fsmon add` / `fsmon remove`（通过 Unix socket 实时生效） |
| 日志输出 | JSON 事件写入 `/var/log/fsmon/history.log` |
| 查询 | `fsmon query --since 1h` 读取事件 |
| 清理 | `fsmon clean --keep-days 7` 轮转旧日志 |

## 配置文件

持久化配置位于 `/etc/fsmon/fsmon.toml`（`sudo fsmon install` 自动生成）。

| 字段 | CLI 对应 | 类型 | 说明 |
|------|---------|------|------|
| `log_file` | | `string` | 日志文件路径（默认：`/var/log/fsmon/history.log`） |
| `socket_path` | | `string` | Unix socket 路径，用于实时命令（默认：`/var/run/fsmon/fsmon.sock`） |
| `paths` | `fsmon add` | `PathEntry[]` | 监控的路径列表 |

每个路径条目（`PathEntry`）：

| 字段 | CLI 参数 | 类型 | 说明 |
|------|---------|------|------|
| `path` | `PATH` 参数 | `string` | 监控的目录/文件 |
| `types` | `-t, --types` | `string[]` | 事件过滤，逗号分隔 |
| `min_size` | `-m, --min-size` | `string` | 最小变更大小（如 "100MB"） |
| `exclude` | `-e, --exclude` | `string` | 排除模式（通配符） |
| `all_events` | `--all-events` | `bool` | 开启全部 14 种事件 |
| `recursive` | `-r, --recursive` | `bool` | 递归监控子目录 |

### 查询选项（仅 CLI 参数）

| 参数 | 说明 |
|------|------|
| `--log-file` | 待查询的日志文件 |
| `--since` | 起始时间（相对如 `1h`、`30m`、`7d`，或绝对时间戳） |
| `--until` | 结束时间 |
| `--pid` | 按 PID 过滤，逗号分隔 |
| `--cmd` | 按进程名过滤（支持通配符，如 `nginx*`） |
| `--user` | 按用户名过滤，逗号分隔 |
| `-t, --types` | 按事件类型过滤，逗号分隔 |
| `-m, --min-size` | 最小变更大小 |
| `-f, --format` | 输出格式：`human`（默认）、`json`、`csv` |
| `-r, --sort` | 排序：`time`、`size`、`pid` |

### 清理选项（仅 CLI 参数）

| 参数 | 说明 |
|------|------|
| `--log-file` | 待清理的日志文件 |
| `--keep-days` | 保留天数（默认：30） |
| `--max-size` | 截断前最大大小（如 `100MB`） |
| `--dry-run` | 预览模式，不实际删除 |

### 安装选项（仅 CLI 参数）

| 参数 | 说明 |
|------|------|
| `--force` | 重新安装已存在的服务 |

## 技术架构

### 模块

| 模块 | 说明 |
|------|------|
| `lib.rs` | 库根 — 共享类型（`FileEvent`、`EventType`），日志清理引擎 |
| `bin/fsmon.rs` | 主二进制 — `daemon`、`add`、`remove`、`managed`、`query`、`clean`、`install`、`uninstall` |
| `monitor.rs` | 核心 fanotify 监控循环，作用域过滤，LRU 文件大小追踪 |
| `fid_parser.rs` | 底层 FID 模式事件解析，两阶段路径恢复 |
| `dir_cache.rs` | 基于 `name_to_handle_at` 的目录句柄缓存，恢复已删除文件路径 |
| `proc_cache.rs` | Netlink proc connector 监听器 — 在进程 `exec()` 时捕获短命进程信息 |
| `query.rs` | 日志文件查询，二分查找优化，多条件组合过滤 |
| `config.rs` | TOML 持久化配置管理 |
| `systemd.rs` | Systemd 服务安装和卸载 |
| `socket.rs` | Unix socket 服务器（守护进程端）和客户端（add/remove 命令） |
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
    → output 格式化（JSON）→ 日志文件
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
