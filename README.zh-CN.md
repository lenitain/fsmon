# fsmon - File System Monitor

🌍 **选择语言 | Language**
- [简体中文](README.zh-CN.md)
- [English](README.md)

---

**轻量级高性能文件系统变更追踪工具**

fsmon (file system monitor) 是一个实时文件变更监控工具，能够追踪文件系统的变化并记录是哪个进程执行了这些操作。当你需要回答"服务器上谁修改了这个文件？"这个问题时，fsmon 就是你的答案。

## 特性

- **实时监控**: 默认捕获 8 种核心变更事件（CREATE, DELETE, DELETE_SELF, MOVED_FROM, MOVED_TO, MOVE_SELF, CLOSE_WRITE, ATTRIB），`--all-events` 开启全部 14 种 fanotify 事件
- **完整进程追踪**: 通过 Proc Connector 捕获短命进程（touch/rm/mv）的 PID、命令名和用户名
- **递归监控**: `-r/--recursive` 参数支持递归监控所有子目录，动态跟踪新建子目录
- **递归删除捕捉**: 递归删除目录时，完整捕获所有子文件的删除事件（包括已删除目录的子文件路径）
- **高性能**: Rust 编写，内存占用 <5MB，零拷贝事件解析
- **灵活过滤**: 按时间、大小、进程、事件类型筛选
- **多种输出**: 人类可读、JSON、CSV 格式
- **守护进程模式**: 可后台运行，持久化日志

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式支持）
- **文件系统**: ext4 / XFS / tmpfs（btrfs 部分支持，有竞态窗口）
- **构建工具**: Rust 工具链（cargo）

**检查内核版本**：
```bash
uname -r  # 需要 ≥ 5.9
```

**安装 Rust**（如果未安装）：
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version  # 验证安装
```

---

### 安装方式

#### 方式 1: 从源码编译（推荐）

```bash
# 1. 克隆仓库
git clone https://github.com/lenitain/fsmon.git
cd fsmon

# 2. 直接从源码安装
cargo install --path .
```

#### 方式 2: 从 crates.io 安装

```bash
# 直接从 crates.io 安装
cargo install fsmon

# 或从 Git 安装
cargo install --git https://github.com/lenitain/fsmon.git
```

**安装后**（建议 - 加到 PATH）：

```bash
# bash: export PATH="$HOME/.cargo/bin:$PATH"
# fish: fish_add_path $HOME/.cargo/bin
```

**可选 - 复制到系统路径以便 sudo 使用**：

```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

---

### 权限配置

**监控某些目录需要 root 权限**：

```bash
# 临时使用 sudo
sudo fsmon monitor /etc

# 或添加当前用户到特定组（可选）
sudo usermod -aG systemd-journal $USER
# 注销重新登录后生效
```

**Proc Connector 需要 root**（用于获取进程信息）：
```bash
# 建议始终使用 sudo 运行
sudo fsmon monitor /home
```

**注意**：如果你将 fsmon 安装到自定义路径（如 `~/.cargo/bin` 或项目目录），`sudo` 将找不到它，因为 `sudo` 会重置 PATH。解决方案：先将 fsmon 安装到系统路径：

```bash
# 一次性安装到系统路径
sudo cp /path/to/fsmon /usr/local/bin/

# 现在 sudo 可以找到 fsmon
sudo fsmon monitor /home
```

---

### 8 个典型场景

#### 场景 1: 排查配置文件被谁修改

```bash
# 监控 /etc 目录的修改事件
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log

# 另一个终端执行修改
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# 预期输出
[2024-05-01 14:30:25] [MODIFY] /etc/hosts (PID: 12345, CMD: tee, USER: root, SIZE: +23B)

# 事后查询
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY
```

---

#### 场景 2: 追踪大文件创建

```bash
# 监控大于 50MB 的文件创建
fsmon monitor /tmp --types CREATE --min-size 50MB --format json

# 触发操作
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# 预期输出
{"time":"2024-05-01T15:00:00Z","event_type":"CREATE","path":"/tmp/large_test.bin","pid":23456,"cmd":"dd","user":"pilot","size_change":104857600}
```

---

#### 场景 3: 审计删除操作（递归删除完整捕获）

```bash
# 递归监控删除事件
fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log

# 触发操作
rm -rf ~/test-project/build/

# 预期输出（子文件路径不丢失）
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm)
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm)
```

**技术亮点**: 通过目录句柄缓存机制，`rm -rf` 递归删除时，子文件和子目录的删除事件都能被完整捕获。

---

#### 场景 4: 监控特定应用（短命进程捕获）

```bash
# 递归监控项目目录
fsmon monitor ~/myapp --recursive

# 触发操作（touch/rm/mv 等短命进程）
touch new_file.txt
rm old_config.h
mv temp.c source/temp.c
make

# 预期输出（短命进程 CMD 正确显示）
[2024-05-01 17:00:00] [CREATE] /home/pilot/myapp/new_file.txt (PID: 45678, CMD: touch)
[2024-05-01 17:00:01] [DELETE] /home/pilot/myapp/old_config.h (PID: 45679, CMD: rm)
[2024-05-01 17:00:02] [MOVED_FROM] /home/pilot/myapp/temp.c (PID: 45680, CMD: mv)
[2024-05-01 17:00:02] [MOVED_TO] /home/pilot/myapp/source/temp.c (PID: 45680, CMD: mv)
```

**技术亮点**: Proc Connector 在进程 `exec()` 瞬间缓存信息，保证 `touch`/`rm`/`mv` 等短命进程的 CMD 准确显示。

---

#### 场景 5: 文件移动审计

```bash
# 监控移动事件
fsmon monitor ~/docs --recursive --types MOVED_FROM,MOVED_TO

# 触发操作
mv ~/docs/drafts/report.txt ~/docs/drafts/report_v2.txt
mv ~/docs/drafts/report_v2.txt ~/docs/published/

# 预期输出
[2024-05-01 18:00:00] [MOVED_FROM] /home/pilot/docs/drafts/report.txt (PID: 56789, CMD: mv)
[2024-05-01 18:00:00] [MOVED_TO] /home/pilot/docs/drafts/report_v2.txt (PID: 56789, CMD: mv)
```

---

#### 场景 6: 守护进程长期监控

```bash
# 启动守护进程
sudo fsmon monitor /var/log /etc --recursive --daemon --output /var/log/fsmon-audit.log

# 查看状态
fsmon status

# JSON 格式（便于集成监控系统）
fsmon status --format json

# 查询分析
fsmon query --since 24h --cmd nginx
fsmon query --since 24h --sort size

# 停止守护进程
fsmon stop
```

---

#### 场景 7: 多条件组合查询

```bash
# 过去 7 天内，root 或 admin 用户的删除/移动操作
fsmon query --since 7d --user root,admin --types DELETE,MOVED_FROM,MOVED_TO --sort time

# 过去 1 小时内，大于 10MB 的创建/修改操作
fsmon query --since 1h --min-size 10MB --types CREATE,MODIFY --sort size

# 通配符命令匹配
fsmon query --since 24h --cmd "python*"
fsmon query --since 24h --cmd "nginx*",systemctl

# CSV 导出
fsmon query --since 7d --format csv > weekly_audit.csv
```

---

#### 场景 8: 日志清理与空间管理

```bash
# 预览清理效果（保留 7 天）
fsmon clean --keep-days 7 --dry-run

# 执行清理
fsmon clean --keep-days 7

# 同时限制大小
fsmon clean --keep-days 30 --max-size 100MB
```

---

## 命令参考

运行 `fsmon <command> --help` 查看完整参数说明：

```bash
fsmon monitor --help    # 实时监控
fsmon query --help      # 查询历史
fsmon status --help     # 查看状态
fsmon stop --help       # 停止守护进程
fsmon clean --help      # 清理日志
```

---

## 输出格式示例

### 人类可读格式

```
[2024-05-01 14:30:25] [MODIFY] /var/log/syslog (PID: 1234, CMD: rsyslogd, USER: syslog, SIZE: +2.5KB)
```

### MOVED_FROM / MOVED_TO 事件

```
[2024-05-01 14:35:10] [MOVED_FROM] /home/user/old.txt (PID: 5678, CMD: mv, USER: user, SIZE: +0B)
[2024-05-01 14:35:10] [MOVED_TO] /home/user/new.txt (PID: 5678, CMD: mv, USER: user, SIZE: +0B)

[2024-05-01 14:40:22] [MOVED_FROM] /tmp/source/file.txt (PID: 9012, CMD: mv, USER: root, SIZE: +0B)
[2024-05-01 14:40:22] [MOVED_TO] /var/data/file.txt (PID: 9012, CMD: mv, USER: root, SIZE: +0B)
```

### JSON 格式

```json
{
  "time": "2024-05-01T14:30:25Z",
  "event_type": "MODIFY",
  "path": "/var/log/syslog",
  "pid": 1234,
  "cmd": "rsyslogd",
  "user": "syslog",
  "size_change": 2560
}
```

### CSV 格式

```csv
time,event_type,path,pid,cmd,user,size_change
2024-05-01T14:30:25Z,MODIFY,/var/log/syslog,1234,rsyslogd,syslog,2560
```

## 技术架构

### 核心技术

- **fanotify (FID 模式)**: Linux 内核级文件监控，支持 FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME，可获取完整事件信息
- **Proc Connector (Netlink)**: 监听进程 exec() 事件，在进程启动瞬间缓存 PID → (cmd, user) 映射，解决短命进程检测问题
- **name_to_handle_at**: 预缓存目录文件句柄，实现删除目录时的路径恢复
- **Rust + Tokio**: 异步运行时，高并发低延迟

### 事件类型说明

默认捕获 8 种核心变更事件，`--all-events` 开启全部 14 种。

**默认事件（8 种变更事件）：**

| 事件类型 | fanotify 常量 | 触发条件 |
|---------|--------------|---------|
| CLOSE_WRITE | FAN_CLOSE_WRITE | 以写模式打开的文件被关闭（最佳"文件被修改"信号） |
| ATTRIB | FAN_ATTRIB | 文件元数据被修改（权限、所有者、时间戳等） |
| CREATE | FAN_CREATE | 文件/目录被创建 |
| DELETE | FAN_DELETE | 文件/目录被删除 |
| DELETE_SELF | FAN_DELETE_SELF | 被监控的对象自身被删除 |
| MOVED_FROM | FAN_MOVED_FROM | 文件从此目录移出 |
| MOVED_TO | FAN_MOVED_TO | 文件移入此目录 |
| MOVE_SELF | FAN_MOVE_SELF | 被监控的对象自身被移动 |

**--all-events 额外事件（6 种访问/诊断事件）：**

| 事件类型 | fanotify 常量 | 触发条件 |
|---------|--------------|---------|
| ACCESS | FAN_ACCESS | 文件被读取 |
| MODIFY | FAN_MODIFY | 文件内容被写入（每次 write() 触发，极其嘈杂） |
| CLOSE_NOWRITE | FAN_CLOSE_NOWRITE | 以只读模式打开的文件/目录被关闭 |
| OPEN | FAN_OPEN | 文件/目录被打开 |
| OPEN_EXEC | FAN_OPEN_EXEC | 文件被打开用于执行 |
| FS_ERROR | FAN_FS_ERROR | 文件系统错误（Linux 5.16+） |

此外，`FAN_Q_OVERFLOW` 在事件队列溢出时由内核自动投递，fsmon 会输出警告到 stderr。

## 许可证

MIT License
