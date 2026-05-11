<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">实时监控文件变更，追溯进程操作。</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

**fsmon** 是一款基于 Linux fanotify 的实时文件系统变更监控工具。它监视文件和目录，捕获每一次创建、修改、删除、移动、属性变更等事件，并追溯每个变更的来源进程 — 包括 PID、命令名和用户。

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## 特性

- **实时监控**: 默认捕获 8 种核心 fanotify 事件，`--types all` 开启全部 14 种
- **进程追溯**: 追踪每个文件变更的 PID、命令名和用户 — 包括 `touch`、`rm`、`mv` 等短命进程
- **递归监控**: 监控整个目录树，追踪新建的子目录
- **完整删除捕获**: 通过持久化目录句柄缓存，完整捕获 `rm -rf` 递归删除中的每个文件
- **高性能**: Rust + Tokio，内存占用 <5MB，零拷贝 FID 解析，二分查找日志查询
- **灵活的捕获过滤**: 按事件类型、大小、路径模式、进程名过滤 — 全部在 daemon 进程内完成，无 fork 开销
- **热更新**: 守护进程运行时添加/移除路径，无需重启

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
cargo install --path .

# 或从 crates.io 安装
cargo install fsmon
```

**fanotify 需要 root 权限运行 daemon：**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 完整流程

监控一个 Web 项目目录，看看日志里有什么，然后用标准 Unix 工具过滤和清理。

```bash
# 终端 1：启动 daemon（sudo 给 fanotify）
sudo fsmon daemon &

# 添加监控路径：递归监控 /var/www/myapp，只捕获 MODIFY/CREATE，
# 排除编辑器临时文件，只记录 nginx 和 vim 进程的事件
fsmon add /var/www/myapp -r --types MODIFY --types CREATE --exclude '\.swp$' --exclude-cmd '!nginx|vim'

# 查看当前监控配置
fsmon managed
# → /var/www/myapp | types=MODIFY,CREATE | recursive | size=- | exclude-path=\.swp | exclude-cmd=!nginx|vim
```

模拟真实操作：

```bash
# 终端 2
echo "<h1>Hello</h1>" > /var/www/myapp/index.html      # nginx 写文件
sleep 2
rm /var/www/myapp/index.html                              # 文件被删除
sleep 2
vim /var/www/myapp/config.json                            # vim 创建交换文件
```

查看 fsmon 捕获了什么：

```bash
# 原始日志 — 每行一个 JSONL 事件
cat ~/.local/state/fsmon/*_log.jsonl
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"MODIFY","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0}
# → {"time":"2026-05-07T10:00:05+00:00","event_type":"CREATE","path":"/var/www/myapp/.config.json.swp","pid":9012,"cmd":"vim","user":"dev","file_size":4096}
```

注意：vim 的 `.swp` 虽然被 fanotify 捕获，但 **不会落盘**——`--exclude '\.swp$'` 在写磁盘前就拦截了。

#### 用管道过滤查询

```bash
# nginx 在过去一小时做了什么？
fsmon query -t '>1h' | jq 'select(.cmd == "nginx")'

# 哪些文件被删除了？
fsmon query | jq 'select(.event_type == "DELETE")'

# 谁改了最大的文件？
fsmon query | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'

# 实时跟踪 deploy 用户的操作
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

#### 安全清理

```bash
# 预览将要删除的内容（默认保留 30 天）
fsmon clean --dry-run

# 实际清理
fsmon clean --time '>7d'

# 或者直接用 Unix 工具操作文件
# 删除早于 2026-04-01 的事件：
cat ~/.local/state/fsmon/*_log.jsonl | jq 'select(.time < "2026-04-01T00:00:00Z")' > /dev/null

# 每个日志文件只保留最后 500 行
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# 关闭 daemon
kill %1
```

### 文件位置

| 用途 | 路径 | 格式 |
|---|---|---|
| 基础设施配置 | `~/.config/fsmon/fsmon.toml` | TOML（可选 — 无配置文件时使用默认值） |
| Managed 路径数据库 | `~/.local/share/fsmon/managed.jsonl` | JSONL（每行一条目） |
| 事件日志 | `~/.local/state/fsmon/*_log.jsonl` | JSONL（每行一事件） |
| Unix Socket | `/tmp/fsmon-<UID>.sock` | TOML over stream |

managed 路径和日志目录均在 `~/.config/fsmon/fsmon.toml` 中可配
（见 `[managed].path` 和 `[logging].path`）。

daemon 通过 sudo 以 root 运行，但通过 `SUDO_UID` + `getpwuid_r` 解析原始用户的 home 目录，
所以日志文件会写入 `/home/<你>/...` 而非 `/root/...`。

> **vfat/exfat/NFS 用户注意：** daemon 会尝试把日志文件 chown 回你的用户。
> 不支持标准 Unix 所有权的文件系统（vfat、exfat、NFS no_root_squash off）无法执行 chown，
> 日志文件保持 root 所有。如果普通用户执行 `fsmon clean` 失败，请用 `sudo fsmon clean` 或直接操作 `.jsonl` 文件。

### 开机自启动（可选）

fsmon 不安装 systemd 服务。daemon 需要 sudo (root) 权限使用 fanotify。
如需登录时自动启动，请使用 root 的 crontab 并确保 sudo 免密码：

```bash
sudo crontab -e
@reboot /usr/local/bin/fsmon daemon &
```

> **注意：** 使用 `sudo crontab -e`（root 的 crontab）— daemon 需要 root 权限。
> 如果使用用户 crontab，需将 fsmon 命令添加到 sudoers NOPASSWD 列表中。

## 完整命令

### daemon

启动 fsmon 守护进程 — 需要 `sudo` 获取 fanotify 权限。

```
sudo fsmon daemon          启动守护进程（前台）
sudo fsmon daemon &        后台启动守护进程
```

配置：            `~/.config/fsmon/fsmon.toml`（可选 — 无配置文件时使用默认值）
监控路径数据库：  `~/.local/share/fsmon/managed.jsonl`
日志目录：        `~/.local/state/fsmon/`
Socket：          `/tmp/fsmon-<UID>.sock`

### add

添加监控路径。无需 sudo。

```
fsmon add <path>                           监控一个路径
fsmon add <path> -r                        递归监控子目录
fsmon add <path> --types MODIFY --types CREATE     按事件类型过滤
fsmon add <path> --types all               全部 14 种事件
fsmon add <path> --exclude '\.swp$' --exclude '\.tmp$'   排除路径模式
fsmon add <path> --exclude '!.*\.py$'     只跟踪 .py 文件
fsmon add <path> -s '>=1MB'                最小文件变更大小
fsmon add <path> --exclude-cmd 'rsync'     按进程名排除
fsmon add <path> --exclude-cmd '!nginx'    只跟踪 nginx 进程
```

所有捕获过滤在 daemon 进程内完成（纳秒级，无 fork），不匹配的事件不会写盘。

### remove

移除一个或多个监控路径。无需 sudo。

```
fsmon remove <path>                        移除一个监控路径
fsmon remove <path1> <path2> <path3>       一次移除多个路径
```

### managed

列出所有监控路径及其过滤配置。

```
fsmon managed                              显示所有监控路径
```

### query

查询历史事件日志。输出为 JSONL 格式 — 可管道到 `jq` 进行自定义过滤。

```
fsmon query                                查询所有日志文件
fsmon query --path /tmp                    查询指定路径的日志
fsmon query --path /tmp --path /var        查询多个路径
fsmon query -t '>1h'                     查询最近一小时事件
fsmon query -t '>=2026-05-01'             从绝对时间开始
fsmon query -t '<30m'                     查询直到 30 分钟前
fsmon query -t '>1h' -t '<now'            时间范围查询
```

搭配 `jq` 使用示例：

```bash
fsmon query -t '>1h' | jq 'select(.cmd == "nginx")'
fsmon query | jq 'select(.event_type == "DELETE")'
fsmon query | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'
```

也可直接操作原始日志文件（无需 `fsmon query`）：

```bash
cat ~/.local/state/fsmon/*_log.jsonl | jq 'select(.cmd == "nginx")'
grep '"event_type":"MODIFY"' ~/.local/state/fsmon/*_log.jsonl
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

> **性能说明：** 原生 `fsmon query` 使用二分查找，大文件时显著更快。直接 Unix 工具线性读取整个文件。

### clean

清理历史日志文件。默认值来自 `fsmon.toml`：`keep_days=30`，`size=>=1GB`。

```bash
fsmon clean                                使用 config 默认值
fsmon clean --time '>7d'                 保留最近 7 天
fsmon clean --size '>=500MB'              每个日志文件大小上限
fsmon clean --path /tmp                    清理指定路径的日志
fsmon clean --dry-run                      预览模式，不实际删除
```

优先级：CLI 参数 > fsmon.toml > 代码默认值（keep_days=30）

也可以直接操作原始日志文件（无需 `fsmon clean`）：

```bash
# 每个日志文件只保留最后 500 行
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# 按 mtime 删除 30 天前的日志
find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
```

> **性能说明：** 原生 `fsmon clean` 精确解析 JSONL（不会在行中间截断），且原子化处理时间+大小规则。Unix 工具简单但可能产生不完整行。

### init

初始化 fsmon 数据目录（chezmoi 风格）。创建日志目录和 managed 数据目录。
**不会**写入配置文件 — 配置文件是可选的，无配置时使用默认值。

```
fsmon init                                 创建日志 & managed 目录
```

### p2l

路径转日志文件名 — 纯哈希计算，无 I/O。将受监控路径解析为日志文件路径，
方便管道和 tail。

```
fsmon p2l /path                             解析日志文件路径
tail -f "$(fsmon p2l /path)"                实时查看某路径事件
fsmon p2l /path1 /path2 /path3              多路径，每行一个
```

### cd

在日志目录中打开子 shell。输入 `exit` 返回原目录：

```
fsmon cd                                   进入日志目录子 shell
ls                                         查看日志文件
```

## 配置

配置文件是可选的 — 无配置时使用默认值。

```toml
# fsmon 配置文件
#
# 基础设施路径。监控路径通过 'fsmon add' / 'fsmon remove' 管理，存储在 [managed].path 中。
# 所有路径支持 ~ 展开。<UID> 在运行时替换为实际 UID。

[managed]
# 自动管理的监控路径数据库。
path = "~/.local/share/fsmon/managed.jsonl"

[logging]
# 事件日志目录（按路径哈希命名的文件）。
path = "~/.local/state/fsmon"
# 'fsmon clean' 的默认值（daemon 不自动清理；使用 cron/timer）。
#   keep_days: 删除早于 N 天的条目
#   size: 日志文件超过此大小时截断
# 运行时均可覆盖：fsmon clean --time '>14d' --size '>=1GB'
keep_days = 30
size = ">=1GB"

[socket]
# daemon 与 CLI 通信的 Unix socket 路径。
path = "/tmp/fsmon-<UID>.sock"
```

## 事件类型

默认捕获 8 种核心事件，`--types all` 开启全部 14 种。

**默认（8 种）：** CLOSE_WRITE、ATTRIB、CREATE、DELETE、DELETE_SELF、MOVED_FROM、MOVED_TO、MOVE_SELF

**全部 14 种（通过 --types all）：** + ACCESS、MODIFY、OPEN、OPEN_EXEC、CLOSE_NOWRITE、FS_ERROR

## 架构

```
Linux Kernel (fanotify)
    → FID 事件推入队列
    → tokio 异步读取
    → fid_parser 解析路径（两阶段 + 目录缓存）
    → filters 过滤（事件类型、大小、路径模式、进程名）
    → JSONL → 按路径分文件日志 (*_log.jsonl)

用户管道:
    cat/ tail *.jsonl → jq → 你的自定义逻辑

清理:
    fsmon clean → clean 引擎解析 JSONL，按时间/大小截断
```

### 源码结构

```
src/
├── bin/
│   ├── fsmon.rs               CLI 入口: main(), CLI 结构体, 参数解析测试
│   └── commands/
│       ├── mod.rs              分发: run() → 各命令处理函数
│       ├── daemon.rs           cmd_daemon: fanotify 初始化, socket 设置, Monitor::run()
│       ├── add.rs              cmd_add: 路径标准化, store 更新, socket 热更新
│       ├── remove.rs           cmd_remove: store 更新, socket 热更新
│       ├── manage.rs           cmd_managed, cmd_list_managed_paths
│       ├── query.rs            cmd_query: 时间过滤, Query::execute()
│       ├── clean.rs            cmd_clean: 时间/大小过滤委托
│       ├── init_cd.rs          cmd_init, cmd_cd
│       └── p2l.rs              cmd_p2l: 路径→日志文件名的哈希计算
├── lib.rs             FileEvent, EventType, DaemonLock (flock 进程单例)
├── clean.rs           日志清理引擎: 时间/大小截断, 尾部偏移, 预览模式
├── config.rs          基础设施配置, SUDO_UID 用户解析
├── managed.rs         Managed 路径数据库（JSONL 格式）
├── monitor.rs         Fanotify 循环, socket 处理, add/remove/事件处理
├── fid_parser.rs      FID 事件底层解析, 两阶段路径恢复
├── filters.rs         PathOptions, 事件/大小/路径/进程过滤, 路径匹配
├── dir_cache.rs       目录句柄缓存（rm -rf 路径恢复）
├── proc_cache.rs      Netlink proc 连接器（短命进程归因）
├── query.rs           二分查找日志查询, JSONL 输出
├── socket.rs          Unix socket 协议（TOML）, 错误分类
├── utils.rs           大小/时间解析, uid 查询, 路径→日志名哈希
└── help.rs            所有命令的帮助文本
```

## 许可证

[MIT License](./LICENSE)
