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
- **日常无需 sudo**: 仅 `sudo fsmon daemon` 需要 root（fanotify），其余命令普通用户可执行
- **热更新**: 守护进程运行时添加/移除路径，无需重启
- **无限递归防护**: 自动拒绝包含日志目录的监控路径，防止事件循环
- **无 systemd 架构**: 用户自己管理 daemon。配置按用户隔离

## 为什么选择 fsmon

fsmon 用于记录"谁修改了这个文件？"。

传统的文件监控工具只提供事件本身，缺少上下文。fsmon 将每个文件变更归因到对应的进程。无论是恶意脚本、自动化部署还是配置错误的服务。

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式）
- **已测试的文件系统**: ext4、XFS、btrfs（注：推荐 Linux 6.18+ 内核以获得 btrfs 递归操作的完整支持）
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
cargo build --release

# 或从 crates.io 安装
cargo install fsmon
```

**注意：daemon 需要管理员权限**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 使用

```bash
# 1. 启动守护进程（fanotify 需要 sudo）
sudo fsmon daemon &

# 2. 添加监控路径（不需要 sudo）
fsmon add /etc --types MODIFY
fsmon add /var/www --recursive --types MODIFY,CREATE
fsmon add /tmp --all-events

# 3. 列出已监控路径
fsmon managed

# 4. 查询历史事件
fsmon query --since 1h --cmd nginx

# 5. 清理旧日志（dry-run 预览）
fsmon clean --keep-days 7 --dry-run

# 6. 移除监控路径
fsmon remove /tmp

# 7. 停止守护进程
kill %1
```

### 文件路径

| 用途 | 路径 | 创建者 | 权限 |
|---|---|---|---|
| 基础设施配置 | `~/.config/fsmon/config.toml` | `fsmon generate` / daemon 自动创建 | 用户 |
| 路径数据库 (store) | `~/.local/share/fsmon/store.toml` | `fsmon add` / `fsmon remove` | 用户 |
| 事件日志（按路径分文件） | `~/.local/state/fsmon/_路径名.toml` | daemon (root)¹ | 644 |
| Unix socket | `/tmp/fsmon-<UID>.sock` | daemon (root)¹ | 666 |

¹ daemon 以 root 运行（通过 sudo），但会通过 `SUDO_UID` + `getpwuid_r` 自动解析原始用户的 home 目录，
  所以实际写入的是 `/home/<你>/...` 而不是 `/root/...`

### 开机自启（可选）

fsmon **不安装** systemd 服务。如需登录时自动启动 daemon，请自行添加到 crontab：

```bash
crontab -e
# 添加这一行：
@reboot /usr/local/bin/fsmon daemon &
```

或添加到 shell 配置文件：

```bash
echo 'sudo fsmon daemon &' >> ~/.bashrc
```

## 示例

### 排查配置文件变更

```bash
# 添加 /etc 监控
fsmon add /etc --types MODIFY

# 另一个终端执行修改
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# 查询结果
fsmon query --since 1h --types MODIFY
```

### 追踪大文件创建

```bash
# 监控大文件创建
fsmon add /tmp --types CREATE

# 触发
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# 查询时按最小大小过滤
fsmon query --since 1m --min-size 50MB
```

### 审计删除操作

```bash
# 添加删除事件监控
fsmon add ~/myproject --types DELETE --recursive

# 触发
rm -rf ~/myproject/

# 输出显示每个被删除的文件（包括子目录中的）
[2026-05-04 21:37:47] [DELETE] /home/pilot/myproject/hello.c (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
[2026-05-04 21:37:47] [DELETE] /home/pilot/myproject (PID: 32838, CMD: rm, USER: pilot, SIZE: +0B)
```

### 组合过滤查询

```bash
# 查询最近 1 小时 nginx 的操作，按文件大小排序
fsmon query --since 1h --cmd nginx* --sort size

# 添加监控并排除临时文件
fsmon add /var/www --types CREATE,DELETE --exclude "*.tmp"
```

## 命令参考

```bash
fsmon daemon          # 启动守护进程（需要 sudo）
fsmon add /path -r    # 添加监控路径（实时 + 持久化）
fsmon remove /path    # 移除监控路径
fsmon managed         # 列出所有监控路径及选项
fsmon query --since   # 查询历史事件
fsmon clean --keep    # 清理旧日志
fsmon generate        # 生成默认配置文件
```

使用 `fsmon <COMMAND> --help` 查看每个子命令的详细帮助。

## 日志文件命名

日志文件以监控路径命名，便于查找：

| 路径 | 日志文件名 |
|---|---|
| `/tmp/foo` | `_tmp_foo.toml` |
| `/etc` | `_etc.toml` |
| `/home/my_docs/a_b` | `_home_my!_docs_a!_b.toml` |

方案：`_` 表示路径分隔符 `/`，`!` 作为转义前缀表示字面下划线。双向可逆。

## 架构

fsmon 以用户自己管理的前台守护进程运行。

```
┌──────────────────────────────────────────────────────┐
│  用户运行：  sudo fsmon daemon &                     │
├──────────────────────────────────────────────────────┤
│  守护进程（root）：                                   │
│  1. 通过 SUDO_UID 解析原始用户                        │
│  2. 读取 ~/.config/fsmon/config.toml（基础设施路径）   │
│  3. 读取 ~/.local/share/fsmon/store.toml（监控路径）  │
│  4. 校验路径（拒绝日志目录递归）                       │
│  5. fanotify_init → fanotify_mark(paths)             │
│  6. 绑定 /tmp/fsmon-<UID>.sock（权限 0666）          │
│  7. 主循环：fanotify 事件 + socket 命令               │
├──────────────────────────────────────────────────────┤
│  CLI（用户）：  fsmon add /path                      │
│  1. 校验路径（拒绝日志目录递归）                       │
│  2. 写入 ~/.local/share/fsmon/store.toml             │
│  3. 通过 socket 发送 add 命令（热更新）               │
│  4. 如 daemon 返回永久错误 → 回滚 store              │
└──────────────────────────────────────────────────────┘
```

| 项目 | 说明 |
|------|------|
| 基础设施配置 | `~/.config/fsmon/config.toml` — store 路径、日志目录、socket 路径 |
| 路径数据库 | `~/.local/share/fsmon/store.toml` — 由 `add`/`remove` 自动管理 |
| 路径管理 | `fsmon add` / `fsmon remove /path`（通过 Unix socket 实时生效） |
| 日志输出 | TOML 格式，按路径分文件：`~/.local/state/fsmon/_路径.toml` |
| Socket | `/tmp/fsmon-<UID>.sock`（权限 0666，普通用户可用） |
| 错误分类 | Socket 协议区分 `Permanent`（永久）和 `Transient`（临时）错误 |
| 查询 | `fsmon query --since 1h` — 二分查找优化 |
| 清理 | `fsmon clean --keep-days 7` — 按时间或最大大小轮转 |
| daemon 管理 | 用户自理（`sudo fsmon daemon &`、crontab 等） |

## 配置文件

配置文件位于 `~/.config/fsmon/config.toml`。首次启动 daemon 或执行 `fsmon generate` 时自动生成。

```toml
# fsmon 配置文件
#
# 基础设施路径。监控路径通过 'fsmon add' / 'fsmon remove' 管理，
# 持久化在 [store].file 中。
# 所有路径支持 ~ 扩展。<UID> 在运行时替换为实际 UID。

[store]
# 自动管理的监控路径数据库
file = "~/.local/share/fsmon/store.toml"

[logging]
# 日志目录，每个监控路径一个文件（文件名为 _路径.toml）
dir = "~/.local/state/fsmon"

[socket]
# daemon-CLI 实时通信的 Unix socket 路径
path = "/tmp/fsmon-<UID>.sock"
```

### 查询选项

| 参数 | 说明 |
|------|------|
| `--path` | 按路径查询。默认：全部监控路径。 |
| `--since` | 起始时间（相对如 `1h`、`30m`、`7d`，或绝对时间戳） |
| `--until` | 结束时间 |
| `--pid` | 按 PID 过滤，逗号分隔 |
| `--cmd` | 按进程名过滤（支持通配符，如 `nginx*`） |
| `--user` | 按用户名过滤，逗号分隔 |
| `-t, --types` | 按事件类型过滤，逗号分隔 |
| `-m, --min-size` | 最小变更大小 |
| `-f, --format` | 输出格式：`human`（默认）、`json`（TOML 输出）、`csv` |
| `-r, --sort` | 排序：`time`、`size`、`pid` |

### 清理选项

| 参数 | 说明 |
|------|------|
| `--path` | 按路径清理。默认：全部监控路径。 |
| `--keep-days` | 保留天数（默认：30） |
| `--max-size` | 截断前最大大小（如 `100MB`） |
| `--dry-run` | 预览模式，不实际删除 |

## 技术架构

### 模块

| 模块 | 说明 |
|------|------|
| `lib.rs` | 库根 — 共享类型（`FileEvent`、`EventType`），日志清理引擎 |
| `bin/fsmon.rs` | 主二进制 — `daemon`、`add`、`remove`、`managed`、`query`、`clean`、`generate` |
| `config.rs` | 基础设施配置（`~/.config/fsmon/config.toml`），通过 `SUDO_UID` 解析路径 |
| `store.rs` | 监控路径数据库（`~/.local/share/fsmon/store.toml`） |
| `monitor.rs` | 核心 fanotify 监控循环，按文件系统分 FD 组，LRU 文件大小追踪，无限递归防护 |
| `fid_parser.rs` | 底层 FID 模式事件解析，两阶段路径恢复，内核结构体定义 |
| `dir_cache.rs` | 基于 `name_to_handle_at` 的目录句柄缓存，恢复已删除文件路径 |
| `proc_cache.rs` | Netlink proc connector 监听器 — 在进程 `exec()` 时捕获短命进程信息 |
| `query.rs` | 日志文件查询，二分查找优化，多条件组合过滤 |
| `output.rs` | 事件输出格式化（人类可读、TOML、CSV） |
| `socket.rs` | Unix socket 协议（TOML over stream socket） — daemon 服务器 + 客户端，`ErrorKind` 枚举 |
| `utils.rs` | 大小/时间解析、进程信息获取、UID 查询（`/etc/passwd`）、路径转日志名编码 |
| `help.rs` | 所有命令的集中帮助文本 |
| `systemd.rs` | 已废弃的 systemd 模块 — 引导用户使用 `sudo fsmon daemon &` |

### 数据流

```
Linux Kernel (fanotify)
    → FID 事件推入队列
    → tokio::select 异步读取事件
    → fid_parser 解析 FID 记录（两阶段：解析 + 缓存恢复）
    → Monitor 过滤（类型、大小、排除模式、作用域）
    → output 格式化（TOML）→ 按路径分文件日志
```

- **fanotify (FID 模式 + FAN_REPORT_NAME)**：内核推送文件事件时携带目录文件句柄和文件名。无需轮询，事件通过非阻塞 read 即时送达。
- **按文件系统分 FD 组**：每个文件系统（挂载点）创建独立的 fanotify fd，因为内核禁止单个 fd 跨文件系统标记。每个 fd 独立 tokio 读取任务。
- **Proc Connector**：后台线程订阅 netlink `PROC_EVENT_EXEC` 通知，在每个进程 exec 时缓存 `(pid, cmd, user)`。确保短命进程（`touch`、`rm`、`mv`）即使退出后也能被归因。
- **FID 解析器 + 目录缓存**：两阶段事件处理：(1) 通过 `open_by_handle_at` 解析文件句柄，(2) 使用持久化目录句柄缓存恢复父目录已被删除的事件路径。处理多层嵌套的 `rm -rf` 场景。
- **二分查找查询**：`fsmon query` 在大致按时间排序的日志文件上使用二分查找，将扫描范围缩小到 O(log N) 次 seek。配合 `expand_offset_backward` 处理边界附近的轻微乱序。
- **按路径分文件日志**：日志以监控路径命名（如 `_tmp_foo.toml`），用 `!` 转义字面下划线。无集中日志文件，`ls`/`grep` 友好。
- **错误分类**：Socket 协议区分 `Permanent` 错误（路径冲突、配置无效——CLI 回滚 store）和 `Transient` 错误（运行时问题——重启后生效）。
- **Rust + Tokio**：多 fd 异步读取任务通过 mpsc channel 通信。独立后台线程处理 proc connector。信号处理支持优雅关闭和 SIGHUP 配置重载。

### 事件挂载策略

fsmon 使用两级挂载策略：
1. **FAN_MARK_FILESYSTEM**（首选）：标记目标路径所在的整个挂载点 — 新建文件无竞态窗口。若遇到 `EXDEV`（btrfs 子卷）则降级。
2. **Inode 标记降级**：逐个标记目录，`--recursive` 模式下递归遍历。

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
