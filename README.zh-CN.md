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

- **实时监控**: 默认捕获 8 种核心 fanotify 事件，`--types all` 开启全部 14 种
- **进程追溯**: 追踪每个文件变更的 PID、命令名、用户、PPID、TGID — 包括 `touch`、`rm`、`mv` 等短命进程
- **进程树追踪**（`<CMD>` 位置参数）：指定进程名（如 `openclaw`），自动追踪它及其所有子进程，每条事件附带完整的进程祖先链
- **递归监控**: 监控整个目录树，自动追踪新建子目录
- **完整删除捕获**: 通过持久化目录句柄缓存，完整捕获 `rm -rf` 中的每个文件
- **高性能**: Rust + Tokio，内存占用 <5MB，零拷贝 FID 解析，二分查找日志查询
- **捕获时过滤**: 按事件类型、文件大小过滤 — daemon 进程内完成，无 fork 开销
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

# 添加监控路径
# 递归监控 /var/www/myapp，只捕获 MODIFY 和 CREATE 事件，追踪 nginx 和 vim 进程
fsmon add nginx --path /var/www/myapp -r --types MODIFY --types CREATE
fsmon add vim --path /var/www/myapp -r --types MODIFY --types CREATE

# 查看已监控的路径
fsmon monitored
# {"cmd":"nginx","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
# {"cmd":"vim","paths":{"/var/www/myapp":{"recursive":true,"types":["MODIFY","CREATE"]}}}
```

模拟文件变更：

```bash
echo "<h1>Hello</h1>" > /var/www/myapp/index.html    # nginx 写文件
sleep 2
rm /var/www/myapp/index.html                          # 文件被删除
```

查看日志：

```bash
cat ~/.local/state/fsmon/*_log.jsonl
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CREATE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":0,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"CLOSE_WRITE","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21,"ppid":1,"tgid":1234}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0,"ppid":1234,"tgid":5678}
```

每条事件都包含 PPID 和 TGID。指定 `<CMD>` 时，匹配的事件还有 `chain` 字段。

#### 用管道查询

```bash
# 过去一小时内 nginx 做了什么？
fsmon query _global -t '>1h' | jq 'select(.cmd == "nginx")'

# 哪些文件被删除了？
fsmon query _global | jq 'select(.event_type == "DELETE")'

# 谁改的文件最大？
fsmon query _global | jq -s 'sort_by(.file_size)[] | {cmd, user, file_size, path}'

# 实时追踪
tail -f ~/.local/state/fsmon/*_log.jsonl | jq 'select(.user == "deploy")'
```

#### 安全清理

```bash
# 预览要删除的内容（默认保留 30 天）
fsmon clean _global --dry-run

# 自定义保留时间
fsmon clean _global --time '>7d'

# 直接使用 Unix 工具
for f in ~/.local/state/fsmon/*_log.jsonl; do
  tail -500 "$f" > "${f}.tmp" && mv "${f}.tmp" "$f"
done
```

### 文件位置

| 用途 | 路径 | 格式 |
|------|------|------|
| 基础设施配置 | `~/.config/fsmon/fsmon.toml` | TOML（可选 — 无配置文件时使用默认值） |
| 监控路径数据库 | `~/.local/share/fsmon/monitored.jsonl` | JSONL（按 cmd 分组） |
| 事件日志 | `~/.local/state/fsmon/*_log.jsonl` | JSONL（每行一个事件） |
| Unix 套接字 | `/tmp/fsmon-<UID>.sock` | TOML 协议 |

daemon 以 root 运行（通过 sudo），但通过 `SUDO_UID` + `getpwuid_r` 解析原始用户的 home 目录，日志写到 `/home/<你>/...` 而非 `/root/...`。

> **vfat/exfat/NFS 用户注意：** daemon 会尝试 chown 日志文件到你的用户。
> 不支持标准 Unix 所有权的文件系统（vfat、exfat、NFS no_root_squash 关闭）会在日志目录创建时给出警告。日志归 root 所有。

## 完整命令

### daemon

启动守护进程（需要 sudo）：

```
sudo fsmon daemon          前台运行
sudo fsmon daemon &        后台运行
```

### add

添加监控路径（可选指定进程追踪）。不需要 sudo。

```
fsmon add nginx --path /var/www/myapp -r          追踪 nginx 在 /myapp（递归）
fsmon add nginx --path /var/www/myapp             追踪 nginx 在 /myapp（非递归）
fsmon add _global --path /home -r                 监控 /home 所有事件（全局）
fsmon add _global --path /home --types MODIFY --types CREATE   只监控 MODIFY 和 CREATE
fsmon add _global --path /home --types all                全部 14 种事件
fsmon add _global --path /home --size '>=1MB'             只记录 >=1MB 的文件变更
```

**模式：**

| 模式 | 示例 | 行为 |
|------|------|------|
| CMD + --path | `fsmon add openclaw --path /home` | 只追踪 openclaw（及子进程）在 /home 上的操作。匹配事件含 `chain`。 |
| 全局监控 (_global) | `fsmon add _global --path /home` | 捕获 /home 上所有事件，均有 PPID/TGID。 |

### remove

移除监控路径。不需要 sudo。

```
fsmon remove _global           移除整个全局组
fsmon remove nginx             移除整个 nginx 组
fsmon remove nginx --path /home  从 nginx 组移除 /home
fsmon remove _global --path /home  从全局组移除 /home
```

### monitored

列出所有监控路径及其过滤配置（JSONL 格式）。

```
fsmon monitored
```

### query

查询历史事件。输出 JSONL — 配合 `jq` 解析。

```
fsmon query _global            查询全局日志
fsmon query nginx              只查 nginx 日志
fsmon query _global -t '>1h'   过去 1 小时内
fsmon query _global -t '>=2026-05-01'  从指定日期
fsmon query _global -t '<30m'   30 分钟前至今
fsmon query _global -t '>1h' -t '<now'  时间范围
```

### clean

清理日志文件。默认值来自 `fsmon.toml`：`keep_days=30`, `size=>=1GB`。

```bash
fsmon clean _global              清理全局日志（默认值）
fsmon clean nginx --time '>7d'   保留最近 7 天
fsmon clean nginx --size '>=500MB'  大小限制
fsmon clean _global --dry-run    预览模式
```

### init

初始化 fsmon 数据目录。创建日志目录和监控数据目录。
不写配置文件 — 配置是可选的。

```
fsmon init
```

### cd

在日志目录中打开子 shell：

```
fsmon cd
ls
```

## 配置

配置文件是可选的 — 无文件时使用默认值。

```toml
[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
path = "~/.local/state/fsmon"
keep_days = 30
size = ">=1GB"

[socket]
path = "/tmp/fsmon-<UID>.sock"
```

## 事件类型

默认 8 种核心事件。`--types all` 开启全部 14 种。

**默认（8 种）：** CLOSE_WRITE, ATTRIB, CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF

**全部 14 种：** + ACCESS, MODIFY, OPEN, OPEN_EXEC, CLOSE_NOWRITE, FS_ERROR

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
  "ppid": 101,
  "tgid": 102,
  "chain": "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
}
```

旧日志无 `ppid`/`tgid`/`chain` 字段时完全向后兼容 — 缺失字段默认为 `0` 或 `""`。

`chain` 格式：每项 `pid|cmd|user`，以 `;` 分隔，从事件进程到 PID 1（根进程）。

## 架构

```
Linux 内核 (fanotify FID 模式)
  → FID 事件入队列
  → tokio 异步读取
  → fid_parser 解析路径（两遍 + 目录句柄缓存）
  → 过滤器：事件类型、大小、递归范围
  → 进程树检查（若指定了 <CMD>）：
    → 不在追踪树中 → 丢弃
    → 在追踪树中 → 构建祖先链 → 追加到事件
  → JSONL 写入 per-cmd 日志文件

进程树（proc connector）：
  Fork/Exec/Exit 事件 → DashMap pid → {cmd, ppid, user}

用户使用：
  tail -f *.jsonl | jq 'select(...)'
```

## 许可证

[MIT License](./LICENSE)
