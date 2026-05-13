<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">实时监控文件变更，追溯进程操作。</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

**fsmon** 是一款基于 Linux fanotify 的实时文件系统变更监控工具。它监视文件和目录，捕获每一次创建、修改、删除、移动、属性变更等事件，并追溯每个变更的来源进程 — 包括 PID、命令名、用户、父进程 PID、线程组 ID，和可选的完整进程祖先链。

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## 特性

- **实时监控**: 捕获 14 种 fanotify 事件类型（默认 8 种核心事件；`--types all` 开启全部 14 种）
- **进程追溯**: 追踪每个文件变更的 PID、命令名、用户、PPID、TGID — 包括 `touch`、`rm`、`mv` 等短命进程
- **进程树追踪**（`<CMD>` 位置参数）：指定进程名（如 `openclaw`），自动追踪它及其所有子进程（fork/exec），每条事件附带完整的进程祖先链
- **递归监控**: 监控整个目录树，自动追踪新建子目录
- **完整删除捕获**: 通过持久化目录句柄缓存，完整捕获 `rm -rf` 中的每个文件
- **高性能**: Rust + Tokio，内存占用 <5MB，零拷贝 FID 事件解析，二分查找日志查询
- **捕获时过滤**: 按事件类型、文件大小过滤 — daemon 进程内完成，纳秒级，无 fork 开销
- **热更新**: 运行时添加/移除路径，无需重启

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

**fanotify 需要 root 权限运行 daemon:**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 完整操作演示

监控一个 Web 项目目录，查看捕获的日志，然后用标准 Unix 工具过滤和分析。

```bash
# 终端 1：启动守护进程（需 sudo）
sudo fsmon daemon &

# 终端 1（或其他终端）：添加监控路径
# 递归监控 /var/www/myapp，只捕获 MODIFY 和 CREATE 事件，追踪 nginx 和 vim 进程
fsmon add nginx --path /var/www/myapp -r --types MODIFY --types CREATE
fsmon add vim --path /var/www/myapp -r --types MODIFY --types CREATE

# 查看已监控的路径
fsmon monitored
# {"cmd":"nginx","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
# {"cmd":"vim","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
```

现在模拟真实的文件变更：

```bash
# 终端 2：模拟真实使用场景
echo "<h1>Hello</h1>" > /var/www/myapp/index.html      # nginx 写文件
sleep 2
rm /var/www/myapp/index.html                           # 文件被删除
sleep 2
vim /var/www/myapp/config.json                         # vim 编辑配置
```

查看 fsmon 捕获的内容：

```bash
# 原始日志 — 每行一个 JSONL 事件
cat ~/.local/state/fsmon/*_log.jsonl
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CREATE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":0,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CLOSE_WRITE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0,"ppid":1234,"tgid":5678}
# → {"time":"2026-05-07T10:00:05+00:00","event_type":"CREATE","path":"/var/www/myapp/.config.json.swp","pid":9012,"cmd":"vim","user":"dev","file_size":4096,"ppid":5678,"tgid":9012,"chain":"9012|vim|dev;5678|sh|deploy;1234|openclaw|root;1|systemd|root"}
```

每条事件都包含 `ppid`（父进程 PID）和 `tgid`（线程组 ID）。当指定 `<CMD>` 时，匹配的事件还包含 `chain` — 一个紧凑的进程祖先字符串，追溯回 PID 1。

#### 用管道查询

```bash
# 过去一小时内 nginx 做了什么？
fsmon query _global -t '>1h' | jq 'select(.cmd == "nginx")'

# 哪些文件被删除了？
fsmon query _global | jq 'select(.event_type == "DELETE")'

# 谁改的文件最大？
fsmon query _global | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'

# 实时追踪，按用户过滤（监控部署活动）
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

无需内置 `--pid`、`--cmd`、`--user`、`--sort` 等标志 — `jq` 一应俱全。

#### 安全清理

```bash
# 预览要删除的内容（默认保留 30 天）
fsmon clean _global --dry-run

# 实际清理，自定义保留时间
fsmon clean _global --time '>7d'

# 或者直接用 Unix 工具操作文件
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# 停止守护进程
kill %1
```

### 文件位置

| 用途 | 路径 | 格式 |
|------|------|------|
| 基础设施配置 | `~/.config/fsmon/fsmon.toml` | TOML（可选 — 无配置文件时使用默认值） |
| 监控路径数据库 | `~/.local/share/fsmon/monitored.jsonl` | JSONL（按 cmd 分组，路径为 map 键） |
| 事件日志 | `~/.local/state/fsmon/*_log.jsonl` | JSONL（每行一个事件） |
| Unix 套接字 | `/tmp/fsmon-<UID>.sock` | TOML 协议 |

存储路径和日志目录均可通过 `~/.config/fsmon/fsmon.toml` 配置（参见 `[monitored].path` 和 `[logging].path`）。

daemon 以 root 运行（通过 sudo），但通过 `SUDO_UID` + `getpwuid_r` 解析原始用户的 home 目录，日志写到 `/home/<你>/...` 而非 `/root/...`。

> **vfat/exfat/NFS 用户注意：** daemon 会尝试 chown 日志文件到你的用户。
> 不支持标准 Unix 所有权的文件系统（vfat、exfat、NFS no_root_squash 关闭）不支持此操作。日志仍归 root 所有。如果普通用户执行 `fsmon clean` 失败，请运行 `sudo fsmon clean` 或直接用 Unix 工具操作 `.jsonl` 文件。

### 开机自启（可选）

fsmon 不安装 systemd 服务。daemon 需要 sudo（root）权限运行 fanotify。
要在登录时自动启动：

```bash
sudo crontab -e
@reboot /usr/local/bin/fsmon daemon &
```

> **注意：** 使用 `sudo crontab -e`（root 的 crontab）— daemon 需要 root 权限。
> 如果改用用户 crontab，请将 `fsmon` 命令以 NOPASSWD 加入 sudoers。

## 完整命令

### daemon

启动 fsmon 守护进程 — 需要 `sudo` 以使用 fanotify。

```
sudo fsmon daemon                     # 前台启动守护进程
sudo fsmon daemon &                   # 后台启动守护进程
sudo fsmon daemon --debug             # 启用调试输出（事件匹配 + 缓存指标）
sudo fsmon daemon --cache-dir-cap N   # 目录句柄缓存容量（默认 100000）
sudo fsmon daemon --cache-dir-ttl N   # 目录句柄缓存 TTL（秒，默认 3600）
sudo fsmon daemon --cache-file-size N # 文件大小缓存容量（默认 10000）
sudo fsmon daemon --cache-proc-ttl N          # 进程缓存 TTL（秒，默认 600）
sudo fsmon daemon --cache-stats-interval N    # 调试模式缓存统计间隔（秒，默认 60，0=关闭）
sudo fsmon daemon --buffer-size N             # Fanotify 读取缓冲区（字节，默认 32768）
```

### add

添加监控路径（可选指定进程追踪）。不需要 sudo。

```
fsmon add nginx --path /var/www/myapp -r          # 追踪 nginx 在 /myapp（递归）
fsmon add nginx --path /var/www/myapp             # 追踪 nginx 在 /myapp（非递归）
fsmon add _global --path /home -r                 # 监控 /home 所有事件（全局）
fsmon add _global --path /home --types MODIFY     # 只监控 MODIFY 和 CREATE
fsmon add _global --path /home --types all        # 全部 14 种事件
fsmon add _global --path /home --size '>=1MB'     # 只记录 >=1MB 的文件变更
```

**模式：**

| 模式 | 示例 | 行为 |
|------|------|------|
| **CMD + --path** | `fsmon add openclaw --path /home` | 追踪 openclaw（及子进程）在 /home 上的操作。匹配事件含 `chain`。 |
| **全局监控 (_global)** | `fsmon add _global --path /home` | 捕获 /home 上所有事件。每条事件均有 `ppid`/`tgid`。 |

- `<CMD>`（位置参数）启用**进程树追踪**：自动包含 fork/exec 子进程。匹配事件附带 `chain` 字段（例如 `"102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"`）。
- 可添加多个不同 `<CMD>` 的条目（各条目间为 OR 逻辑）。
- `--path` 是必填参数。全局监控请使用 `_global` 作为 CMD。

### remove

移除一个或多个监控路径。不需要 sudo。

```
fsmon remove _global               # 移除整个全局 cmd 组
fsmon remove nginx                 # 移除整个 nginx cmd 组
fsmon remove nginx --path /home    # 从 nginx 组移除 /home
fsmon remove _global --path /home  # 从全局组移除 /home
```

### monitored

列出所有监控路径及其过滤配置（JSONL 格式）。

```
fsmon monitored                显示所有监控路径组
```

每行是一个包含 `cmd` 和 `paths` 字段的 JSON 对象。可管道给 `jq` 过滤。

### query

查询历史事件日志。输出 JSONL — 管道给 `jq` 过滤。

```
fsmon query _global                    # 查询全局日志
fsmon query nginx                      # 只查 nginx 日志
fsmon query _global -t '>1h'           # 过去 1 小时内的事件
fsmon query _global -t '>=2026-05-01'  # 从指定绝对时间
fsmon query _global -t '<30m'          # 30 分钟前至今的事件
fsmon query _global -t '>1h' -t '<now' # 时间范围（起止时间）
fsmon query _global --path /tmp        # 按路径前缀过滤事件
```

配合 `jq` 的示例：

```bash
# 按进程搜索（ppid/tgid 始终存在）
fsmon query _global | jq 'select(.ppid == 100)'

# 按祖先链搜索（仅当 add 时指定了 --cmd）
fsmon query _global | jq 'select(.chain != "") | .chain'

# 传统的 cmd/user 过滤
fsmon query _global -t '>1h' | jq 'select(.cmd == "nginx")'
fsmon query _global | jq 'select(.event_type == "DELETE")'
fsmon query _global | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'
```

### clean

清理指定 cmd 组的日志文件。默认值来自 `fsmon.toml`：`keep_days=30`，`size=>=1GB`。

```
fsmon clean _global                  # 清理全局日志（默认值）
fsmon clean nginx --time '>7d'       # 保留最近 7 天的 nginx 事件
fsmon clean nginx --size '>=500MB'   # nginx 日志大小限制
fsmon clean _global --dry-run        # 预览模式，不实际删除
```

优先级：CLI 参数 > fsmon.toml > 代码默认值（keep_days=30, size=>=1GB）

也可以直接操作原始日志文件，无需 `fsmon clean`：

```bash
# 每个日志文件只保留最后 500 行
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done

# 按 mtime 删除 30 天前的日志
find ~/.local/state/fsmon/ -name '*.jsonl' -mtime +30 -delete
```

> **注意：** 原生 `fsmon clean` 精确解析 JSONL（不会截断到行中间），同时处理时间和大小约束。直接使用 Unix 工具更简单，但可能产生不完整的行。

### init

初始化 fsmon 数据目录。创建日志目录和监控数据目录。
不写配置文件 — 配置是可选的，无配置时使用默认值。

```
fsmon init
```

### cd

在日志目录中打开子 shell。输入 `exit` 返回：

```
fsmon cd
ls _global_log.jsonl
```

## 配置

配置文件是可选的 — 无文件时使用默认值。

```toml
# fsmon 配置文件
#
# fsmon 的基础设施路径。监控路径通过
# 'fsmon add' / 'fsmon remove' 添加并持久化在 [monitored].path 中。
# 所有路径支持 ~ 展开。<UID> 在运行时替换为数字 UID。

[monitored]
# 自动持久化的监控路径数据库路径。
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
# 事件日志目录的路径（每个 cmd 的 *_log.jsonl 文件）。
path = "~/.local/state/fsmon"
# 'fsmon clean' 的默认值（无自动清理；请使用 cron/timer）。
keep_days = 30
size = ">=1GB"

[socket]
# daemon 与 CLI 实时通信的 Unix 套接字路径。
path = "/tmp/fsmon-<UID>.sock"

[cache]
# 目录句柄缓存容量（默认 100,000，约 15-20MB）。
# 每条记录将内核文件句柄映射到目录路径。
# 内存紧张时降低此值；监控大目录树（>10万目录）时提高此值以减少句柄重解析。
dir_capacity = 100000

# 目录句柄缓存 TTL（秒，默认 3600 = 1 小时）。
# 较短 TTL 在目录结构频繁变动时更快释放内存；
# 较长 TTL 在稳定目录上减少句柄重解析。
dir_ttl_secs = 3600

# 文件大小缓存容量（默认 10,000，约 1MB）。
# 避免对已知大小的文件反复 stat()。
# 高文件量工作负载（git checkout、npm install 等）时提高此值。
file_size_capacity = 10000

# 进程缓存 TTL（秒，默认 600 = 10 分钟）。
# 同时影响进程信息缓存（PID→命令/用户/PPID/TGID）和
# 进程树缓存（PID→父进程，用于祖先链追踪）。
# 较短 TTL 更快清理已退出进程条目；
# 较长 TTL 减少常驻进程的 /proc 读取。
proc_ttl_secs = 600

# 调试模式下缓存统计日志间隔（秒，默认 60）。
# 设为 0 可禁用周期性缓存统计输出。
stats_interval_secs = 60
```

### 覆盖优先级
```
CLI 参数（--cache-dir-cap、--cache-dir-ttl、--cache-file-size、--cache-proc-ttl、--cache-stats-interval、--buffer-size）
    > fsmon.toml [cache] 配置段
        > 代码默认值
```

CLI 参数优先级最高：
```bash
# 启动时覆盖 dir_cache 容量和 fanotify 缓冲区大小
sudo fsmon daemon --cache-dir-cap 200000 --buffer-size 65536 &
```

## 事件类型

默认 8 种核心事件。`--types all` 开启全部 14 种。

**默认（8 种）：** CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF

**全部 14 种（通过 `--types all`）：** + ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR

`FS_ERROR` 仅适用于文件系统级别的标记（需要文件系统支持）。

## 日志格式

每条事件是一行 JSON。所有字段始终存在。

```json
{
  "time": "2026-05-07T10:00:01+00:00",
  "event_type": "CREATE",
  "path": "/var/www/myapp/index.html",
  "pid": 1234,
  "cmd": "nginx",
  "user": "www-data",
  "file_size": 0,
  "ppid": 1,
  "tgid": 1234
}
```

指定 `<CMD>` 且事件匹配时，额外包含 `chain`：

```json
{
  ...
  "ppid": 101,
  "tgid": 102,
  "chain": "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
}
```

`chain` 格式：每项 `pid|cmd|user`，以 `;` 分隔，从事件进程到 PID 1（根进程）。

## 架构

```
Linux 内核 (fanotify FID 模式)
    → 原始  # FID 事件推入内核队列
    → tokio 异步读取事件
    → fid_parser: 解析路径（两遍 +  # DashMap 目录句柄缓存）
    → 过滤器: 事件类型、大小、递归/非递归范围
    → （如果指定了 <CMD>）进程树检查：
      → 不在追踪树中 → 立即丢弃（零 /proc 读取）
      → 在追踪树中 → 构建祖先链 → 追加到事件
    → 写入  # JSONL → per-cmd 日志文件（<cmd>_log.jsonl）

进程树（proc connector）:
    Fork/Exec/Exit 事件来自 netlink connector 套接字
    →  # DashMap: pid → {cmd, ppid, user, tgid, start_time}
    守护进程启动时：/proc/*/stat 快照种子填充已有进程
    is_descendant(pid, "openclaw") → O(depth) DashMap 查找

用户管道:
    tail -f *.jsonl | jq 'select(...)'

清理:
    fsmon clean → 解析  # JSONL，应用时间/大小过滤器，截断
```

### 源码结构

```
src/
├── bin/
│   ├── fsmon.rs                 # CLI 入口：main()、参数结构体、参数测试
│   └── commands/
│       ├── mod.rs               # run() 分发、parse_path_entries 辅助
│       ├── daemon.rs            # 守护进程：加载存储、Monitor::new()、run()
│       ├── add.rs               # CLI add：路径规范化、存储 + 套接字
│       ├── remove.rs            # CLI remove：存储 + 套接字
│       ├── monitored.rs         # CLI monitored：JSONL 输出
│       ├── query.rs             # CLI query：时间过滤、执行查询
│       ├── clean.rs             # CLI clean：解析器委托
│       └── init_cd.rs           # CLI init、cd
│
├── lib.rs              # FileEvent、EventType、DaemonLock（flock 单例）
├── clean.rs            # 日志清理引擎：时间/大小修剪、尾偏移
├── config.rs           # TOML 配置、SUDO_UID home 解析
├── monitored.rs        # 监控路径数据库（JSONL 存储）
├── monitor.rs          # Fanotify 循环、套接字处理器、add/remove/events
├── fid_parser.rs       # FID 事件解析、两遍路径恢复
├── filters.rs          # PathOptions、事件/大小过滤、路径匹配
├── dir_cache.rs        # 目录句柄缓存（DashMap + HandleKey）
├── proc_cache.rs       # Netlink proc connector：Fork/Exec/Exit、build_chain
├── query.rs            # 对已排序  # JSONL 进行二分查找日志查询
├── socket.rs           # Unix 套接字协议（TOML 请求/响应）
├── utils.rs            # 大小/时间解析、进程信息查找、chown
└── help.rs             # 帮助文本常量
```

## 许可证

[MIT License](./LICENSE)
