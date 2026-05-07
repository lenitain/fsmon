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
cargo build --release

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
fsmon add /var/www/myapp -r --types MODIFY,CREATE --exclude "*.swp" --only-cmd nginx,vim

# 查看当前监控配置
fsmon managed
# → /var/www/myapp | types=MODIFY,CREATE | recursive | min_size=- | exclude-path=*.swp | exclude-cmd=- | only-cmd=nginx,vim | events=filtered
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
# → {"time":"2026-05-07T10:00:01+00:00","event_type":"MODIFY","path":"/var/www/myapp/index.html","pid":1234,"cmd":"nginx","user":"www-data","file_size":21,"monitored_path":"/var/www/myapp"}
# → {"time":"2026-05-07T10:00:03+00:00","event_type":"DELETE","path":"/var/www/myapp/index.html","pid":5678,"cmd":"rm","user":"deploy","file_size":0,"monitored_path":"/var/www/myapp"}
# → {"time":"2026-05-07T10:00:05+00:00","event_type":"CREATE","path":"/var/www/myapp/.config.json.swp","pid":9012,"cmd":"vim","user":"dev","file_size":4096,"monitored_path":"/var/www/myapp"}
```

注意：vim 的 `.swp` 虽然被 fanotify 捕获，但 **不会落盘**——`--exclude "*.swp"` 在写磁盘前就拦截了。

#### 用管道过滤查询

```bash
# nginx 在过去一小时做了什么？
fsmon query --since 1h | jq 'select(.cmd == "nginx")'

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
fsmon clean --keep-days 7

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
| 基础设施配置 | `~/.config/fsmon/config.toml` | TOML（可通过fsmon generate生成） |
| Managed 路径数据库 | `~/.local/share/fsmon/managed.jsonl` | JSONL（每行一条目） |
| 事件日志 | `~/.local/state/fsmon/*_log.jsonl` | JSONL（每行一事件） |
| Unix Socket | `/tmp/fsmon-<UID>.sock` | TOML over stream |

managed 路径和日志目录均在 `~/.config/fsmon/config.toml` 中可配
（见 `[managed].file` 和 `[logging].dir`）。

daemon 通过 sudo 以 root 运行，但通过 `SUDO_UID` + `getpwuid_r` 解析原始用户的 home 目录，
所以日志文件会写入 `/home/<你>/...` 而非 `/root/...`。

> **vfat/exfat/NFS 用户注意：** daemon 会尝试把日志文件 chown 回你的用户。
> 不支持标准 Unix 所有权的文件系统（vfat、exfat、NFS no_root_squash off）无法执行 chown，
> 日志文件保持 root 所有。如果普通用户执行 `fsmon clean` 失败，请用 `sudo fsmon clean` 或直接操作 `.jsonl` 文件。

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

清理使用 config.toml 中的安全网默认值（keep_days=30，max_size="1GB"），可通过 CLI 覆盖：

```bash
# 优先级: CLI 参数 > config.toml > 代码默认值
fsmon clean                       # 使用 config 默认
fsmon clean --keep-days 60        # 覆盖默认值
```

## 配置

首次启动 daemon 或执行 `fsmon generate` 自动生成。安全网默认值已包含：

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

用户管道:
    cat/ tail *.jsonl → jq → 你的自定义逻辑
```

### 源码结构

```
src/
├── bin/fsmon.rs       CLI: daemon, add, remove, managed, query, clean, generate
├── lib.rs             FileEvent、EventType、清理引擎、临时文件安全
├── config.rs          基础设施配置、SUDO_UID 用户解析
├── managed.rs         Managed 路径数据库（JSONL 格式）
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
