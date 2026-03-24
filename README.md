# fsmon - File System Monitor

**轻量级高性能文件系统变更追踪工具**

fsmon (file system monitor) 是一个实时文件变更监控工具，能够追踪文件系统的变化并记录是哪个进程执行了这些操作。当你需要回答"服务器上谁修改了这个文件？"这个问题时，fsmon 就是你的答案。

## 特性

- **实时监控**: 捕获 CREATE、DELETE、MODIFY、MOVE/RENAME 事件
- **完整进程追踪**: 通过 Proc Connector 捕获短命进程（touch/rm/mv）的 PID、命令名和用户名
- **递归监控**: `-r/--recursive` 参数支持递归监控所有子目录，动态跟踪新建子目录
- **删除路径恢复**: 智能缓存目录句柄，即使目录已删除也能恢复完整路径
- **高性能**: Rust 编写，内存占用 <5MB，零拷贝事件解析
- **灵活过滤**: 按时间、大小、进程、事件类型筛选
- **多种输出**: 人类可读、JSON、CSV 格式
- **守护进程模式**: 可后台运行，持久化日志

## 快速开始

### 安装

```bash
# 编译
cargo build --release

# 二进制文件位于
./target/release/fsmon
```

### 基础用法

#### 1. 实时监控文件

```bash
# 监控单个目录
fsmon monitor /var/log

# 监控多个路径
fsmon monitor /tmp /var/log /home

# 递归监控所有子目录
fsmon monitor /home --recursive

# 监控整个系统（需要 root）
sudo fsmon monitor /
```

#### 2. 添加过滤条件

```bash
# 只追踪大于 100MB 的文件变更
fsmon monitor /tmp --min-size 100MB

# 只关注创建和修改事件
fsmon monitor /var/log --types CREATE,MODIFY

# 排除特定路径（支持通配符）
fsmon monitor / --exclude "*.log"

# 组合过滤
fsmon monitor /tmp --types CREATE,MODIFY --min-size 100MB --exclude "*.tmp"
```

#### 3. 选择输出格式

```bash
# JSON 格式（便于机器处理）
fsmon monitor /var/log --format json

# CSV 格式（便于导入表格）
fsmon monitor /var/log --format csv

# 保存日志到文件
fsmon monitor /var/log --output /var/log/fsmon.log
```

#### 4. 守护进程模式

```bash
# 后台运行，持续监控
fsmon monitor / --daemon --output /var/log/fsmon.log

# 查看守护进程状态
fsmon status

# 停止守护进程
fsmon stop

# 强制停止
fsmon stop --force
```

### 查询历史日志

#### 1. 基础查询

```bash
# 查询默认日志文件 (~/.fsmon/history.log)
fsmon query

# 查询指定日志文件
fsmon query --log-file /var/log/fsmon.log
```

#### 2. 时间范围过滤

```bash
# 最近 1 小时
fsmon query --since 1h

# 最近 30 分钟
fsmon query --since 30m

# 指定时间范围
fsmon query --since "2024-05-01 10:00" --until "2024-05-01 12:00"
```

#### 3. 进程过滤

```bash
# 按 PID 查询
fsmon query --pid 1234

# 按多个 PID 查询
fsmon query --pid 1234,5678

# 按命令名查询（支持通配符）
fsmon query --cmd "nginx*"
fsmon query --cmd python

# 按用户名查询
fsmon query --user root
fsmon query --user root,admin
```

#### 4. 事件类型和大小过滤

```bash
# 只查看删除事件
fsmon query --types DELETE

# 大文件变更（>= 1GB）
fsmon query --min-size 1GB

# 组合条件：过去 1 小时内 Java 进程的修改操作，且变更 >= 100MB
fsmon query --since 1h --cmd java --types MODIFY --min-size 100MB
```

#### 5. 排序和输出

```bash
# 按时间排序（默认）
fsmon query --sort time

# 按文件大小排序
fsmon query --sort size

# 按 PID 排序
fsmon query --sort pid

# JSON 输出 + 按大小排序
fsmon query --since 1h --format json --sort size
```

### 清理历史日志

```bash
# 保留最近 7 天的日志
fsmon clean --keep-days 7

# 限制日志文件大小为 100MB
fsmon clean --max-size 100MB

# 预览将删除的内容（不实际删除）
fsmon clean --keep-days 7 --dry-run

# 清理指定日志文件
fsmon clean --log-file /var/log/fsmon.log --keep-days 30
```

## 完整命令参考

### monitor - 实时监控

```
fsmon monitor [PATHS] [OPTIONS]

参数:
  PATHS              要监控的路径（至少一个）

选项:
  -s, --min-size SIZE     最小变更大小 (如：1GB, 100MB)
  -t, --types TYPES       事件类型，逗号分隔：CREATE,DELETE,MODIFY,MOVE
  -e, --exclude PATTERN   排除路径（正则模式）
  -o, --output FILE       输出到文件
  -f, --format FORMAT     输出格式：human, json, csv
  -d, --daemon           以守护进程运行
  -r, --recursive        递归监控所有子目录
```

### query - 查询历史

```
fsmon query [OPTIONS]

选项:
  -l, --log-file FILE     日志文件路径
  -S, --since TIME        起始时间（相对：1h, 30m 或 绝对："2024-05-01 10:00"）
  -U, --until TIME        结束时间
  -p, --pid PIDS          PID 过滤，逗号分隔
  -c, --cmd PATTERN       命令名过滤（支持 * 通配符）
  -u, --user USERS        用户名过滤，逗号分隔
  -t, --types TYPES       事件类型过滤
  -s, --min-size SIZE     最小变更大小
  -f, --format FORMAT     输出格式：human, json, csv
  -r, --sort SORT_BY      排序字段：time, size, pid
```

### status - 查看状态

```
fsmon status [OPTIONS]

选项:
  -f, --format FORMAT     输出格式：human, json, csv
```

### stop - 停止守护进程

```
fsmon stop [OPTIONS]

选项:
  --force                强制停止（发送 SIGKILL）
```

### clean - 清理日志

```
fsmon clean [OPTIONS]

选项:
  -l, --log-file FILE     日志文件路径
  -k, --keep-days DAYS    保留天数（默认：30）
  -m, --max-size SIZE     最大文件大小
  -n, --dry-run          预览模式
```

## 使用场景示例

### 场景 1: 排查配置文件被谁修改

**背景**: 系统管理员发现 `/etc/hosts` 被意外修改，需要找出是谁、什么时候、通过什么进程修改的。

**准备环境**:
```bash
# 查看当前文件状态
ls -la /etc/hosts
cat /etc/hosts
```

**监控命令**:
```bash
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log --format human
```

**参数说明**:
- `--types MODIFY`: 只关注修改事件，忽略创建/删除等噪音
- `--output /tmp/etc-monitor.log`: 保存日志供后续查询
- `--format human`: 人类可读格式，便于实时观察

**触发操作** (另一个终端):
```bash
# 模拟管理员编辑
sudo cp /etc/hosts /etc/hosts.bak
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts
```

**预期输出**:
```
Starting file trace monitor...
Monitoring paths: /etc
Press Ctrl+C to stop

[2024-05-01 14:30:25] [MODIFY] /etc/hosts (PID: 12345, CMD: tee, USER: root, SIZE_CHANGE: +23B)
```

**事后查询**:
```bash
# 查询最近 1 小时的修改记录
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY

# 按时间排序查看
fsmon query --log-file /tmp/etc-monitor.log --sort time
```

---

### 场景 2: 追踪大文件创建

**背景**: 磁盘空间告警，需要找出哪些进程在创建大文件。

**准备环境**:
```bash
# 查看当前目录结构
ls -lah /tmp
df -h /tmp
```

**监控命令**:
```bash
fsmon monitor /tmp --types CREATE --min-size 50MB --format json
```

**参数说明**:
- `--types CREATE`: 只关注文件创建
- `--min-size 50MB`: 忽略小于 50MB 的文件，减少噪音
- `--format json`: JSON 格式便于脚本处理

**触发操作**:
```bash
# 创建一个大文件
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100

# 压缩产生大文件
tar czf /tmp/backup.tar.gz /home/user/documents/
```

**预期输出**:
```json
{"time":"2024-05-01T15:00:00Z","event_type":"CREATE","path":"/tmp/large_test.bin","pid":23456,"cmd":"dd","user":"pilot","size_change":104857600}
{"time":"2024-05-01T15:01:30Z","event_type":"CREATE","path":"/tmp/backup.tar.gz","pid":23460,"cmd":"tar","user":"pilot","size_change":52428800}
```

**分析**:
```bash
# 导出 CSV 用 Excel 分析
fsmon query --since 1h --types CREATE --min-size 50MB --format csv > large_files.csv
```

---

### 场景 3: 审计删除操作（含递归删除路径恢复）

**背景**: 重要项目目录被误删，需要还原删除历史和责任人。

**准备环境**:
```bash
# 创建测试目录结构
mkdir -p ~/test-project/{src,docs,build}
touch ~/test-project/src/main.rs ~/test-project/src/utils.rs
touch ~/test-project/docs/readme.md
echo "build artifacts" > ~/test-project/build/output.o
ls -R ~/test-project
```

**监控命令**:
```bash
fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log
```

**参数说明**:
- `--types DELETE`: 只关注删除事件
- `--recursive`: 递归监控，自动跟踪新建子目录
- `--output`: 保存日志供事后审计

**触发操作**:
```bash
# 误删整个构建目录
rm -rf ~/test-project/build/

# 删除单个文件
rm ~/test-project/src/utils.rs
```

**预期输出** (关键：路径完整恢复):
```
Starting file trace monitor...
Monitoring paths: /home/pilot/test-project
Press Ctrl+C to stop

[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 16:00:05] [DELETE] /home/pilot/test-project/src/utils.rs (PID: 34568, CMD: rm, USER: pilot, SIZE_CHANGE: +0B)
```

**技术亮点**: 即使 `build/` 目录已被删除，其子文件 `output.o` 的路径仍能正确显示，这得益于 fsmon 的目录句柄缓存机制。

**恢复分析**:
```bash
# 查询所有删除记录
fsmon query --log-file /tmp/deletes.log --types DELETE

# 按用户分组查看
fsmon query --log-file /tmp/deletes.log --types DELETE --user pilot
```

---

### 场景 4: 监控特定应用（短命进程捕获）

**背景**: CI/CD 流水线中某些步骤失败，需要追踪 `make`、`gcc` 等短命进程的文件操作。

**准备环境**:
```bash
# 查看项目结构
ls -la ~/myapp/
cat ~/myapp/Makefile
```

**监控命令**:
```bash
fsmon monitor ~/myapp --recursive --format human
```

**参数说明**:
- `--recursive`: 递归监控所有子目录（源码编译会产生多级目录）
- 无过滤条件：捕获全部事件类型

**触发操作**:
```bash
cd ~/myapp

# 短命进程操作
touch new_file.txt          # 瞬间完成
rm old_config.h             # 瞬间完成
mv temp.c source/temp.c     # 瞬间完成

# 编译过程
make clean && make
```

**预期输出** (关键：短命进程 CMD 正确显示):
```
Starting file trace monitor...
Monitoring paths: /home/pilot/myapp
Press Ctrl+C to stop

[2024-05-01 17:00:00] [CREATE] /home/pilot/myapp/new_file.txt (PID: 45678, CMD: touch, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 17:00:01] [DELETE] /home/pilot/myapp/old_config.h (PID: 45679, CMD: rm, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 17:00:02] [MOVE] /home/pilot/myapp/temp.c -> /home/pilot/myapp/source/temp.c (PID: 45680, CMD: mv, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 17:00:10] [CREATE] /home/pilot/myapp/build/main.o (PID: 45681, CMD: gcc, USER: pilot, SIZE_CHANGE: +8192B)
[2024-05-01 17:00:11] [MODIFY] /home/pilot/myapp/Makefile (PID: 45682, CMD: make, USER: pilot, SIZE_CHANGE: +128B)
```

**技术亮点**: 
- `touch`/`rm`/`mv` 等进程执行后立即退出，传统 fanotify 无法捕获 CMD
- fsmon 通过 Proc Connector 在进程 `exec()` 瞬间缓存信息，保证准确显示

**精准查询**:
```bash
# 只查看 gcc 的操作
fsmon query --since 1h --cmd gcc

# 查看编译相关的所有操作
fsmon query --since 1h --cmd "make*",gcc,ld

# 按 PID 追溯完整操作链
fsmon query --since 1h --pid 45681,45682
```

---

### 场景 5: 文件重命名与移动审计

**背景**: 文档管理系统中文件频繁重命名/移动，需要区分 RENAME（同目录）和 MOVE（跨目录）。

**准备环境**:
```bash
mkdir -p ~/docs/{drafts,published,archive}
touch ~/docs/drafts/report.txt
ls -R ~/docs
```

**监控命令**:
```bash
fsmon monitor ~/docs --recursive --types MOVE
```

**参数说明**:
- `--types MOVE`: 只关注移动/重命名事件
- `--recursive`: 监控所有子目录间的移动

**触发操作**:
```bash
# 同目录重命名
mv ~/docs/drafts/report.txt ~/docs/drafts/report_v2.txt

# 跨目录移动
mv ~/docs/drafts/report_v2.txt ~/docs/published/

# 批量归档
mv ~/docs/published/*.txt ~/docs/archive/
```

**预期输出**:
```
Starting file trace monitor...
Monitoring paths: /home/pilot/docs
Press Ctrl+C to stop

[2024-05-01 18:00:00] [RENAME] /home/pilot/docs/drafts/report.txt -> /home/pilot/docs/drafts/report_v2.txt (PID: 56789, CMD: mv, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 18:00:05] [MOVE] /home/pilot/docs/drafts/report_v2.txt -> /home/pilot/docs/published/report_v2.txt (PID: 56790, CMD: mv, USER: pilot, SIZE_CHANGE: +0B)
[2024-05-01 18:00:10] [MOVE] /home/pilot/docs/published/file1.txt -> /home/pilot/docs/archive/file1.txt (PID: 56791, CMD: mv, USER: pilot, SIZE_CHANGE: +0B)
```

**规则**: 
- 源和目标在同一目录 → `RENAME`
- 源和目标在不同目录 → `MOVE`

**查询分析**:
```bash
# 查看所有重命名
fsmon query --since 1h --types MOVE | grep RENAME

# 查看某个目录的进出移动
fsmon query --since 1h --types MOVE | grep "archive"
```

---

### 场景 6: 守护进程长期监控 + 状态管理

**背景**: 生产服务器需要 7x24 小时监控关键目录，后台运行并定期审计。

**准备环境**:
```bash
# 检查是否有运行中的实例
ps aux | grep fsmon
cat /tmp/fsmon.pid 2>/dev/null
```

**启动守护进程**:
```bash
sudo fsmon monitor /var/log /etc --recursive --daemon --output /var/log/fsmon-audit.log
```

**参数说明**:
- `--daemon`: 后台运行，脱离终端
- `--output`: 必须指定，日志写入文件

**查看状态**:
```bash
# 人类可读格式
fsmon status

# JSON 格式（便于集成监控系统）
fsmon status --format json

# CSV 格式（便于导入报表）
fsmon status --format csv
```

**预期输出** (human):
```
fsmon daemon status: Running (PID: 67890)
Monitored paths: /var/log, /etc
Log file: /var/log/fsmon-audit.log
Start time: 2024-05-01 09:00:00
Event count: 15234
Memory usage: 3.2MB
```

**预期输出** (json):
```json
{
  "status": "running",
  "pid": 67890,
  "paths": ["/var/log", "/etc"],
  "log_file": "/var/log/fsmon-audit.log",
  "start_time": "2024-05-01T09:00:00Z",
  "event_count": 15234,
  "memory_usage": 3355443
}
```

**触发操作** (模拟一天后的查询):
```bash
# 正常操作会产生各种事件
sudo systemctl restart nginx
echo "custom config" | sudo tee -a /etc/nginx/nginx.conf
sudo journalctl --rotate
```

**查询分析**:
```bash
# 查询今天的所有事件
fsmon query --since "2024-05-01 00:00" --until "2024-05-01 23:59"

# 只查看 nginx 相关
fsmon query --since 24h --cmd "nginx*",systemctl

# 按大小排序找出最大变更
fsmon query --since 24h --sort size

# 导出昨天和今天的对比
fsmon query --since "2024-04-30 00:00" --until "2024-04-30 23:59" --format csv > day1.csv
fsmon query --since "2024-05-01 00:00" --until "2024-05-01 23:59" --format csv > day2.csv
```

**停止守护进程**:
```bash
# 优雅停止（推荐）
fsmon stop

# 强制停止（无响应时）
fsmon stop --force
```

---

### 场景 7: 多条件组合查询 + 排序 + 导出

**背景**: 安全审计需要生成综合报表，分析特定时间段内的高风险操作。

**准备环境** (假设已有日志积累):
```bash
# 查看日志文件概况
wc -l ~/.fsmon/history.log
head -5 ~/.fsmon/history.log
```

**组合查询命令**:
```bash
# 过去 7 天内，root 或 admin 用户的删除或移动操作，按时间倒序
fsmon query \
  --since 7d \
  --user root,admin \
  --types DELETE,MOVE \
  --sort time

# 过去 1 小时内，大于 10MB 的创建或修改，按大小降序
fsmon query \
  --since 1h \
  --min-size 10MB \
  --types CREATE,MODIFY \
  --sort size

# 指定精确时间范围，特定 PID 的操作
fsmon query \
  --since "2024-05-01 10:00" \
  --until "2024-05-01 12:00" \
  --pid 12345,67890 \
  --format json
```

**参数组合说明**:
- `--since 7d`: 相对时间（7 天前到现在）
- `--user root,admin`: 多用户逗号分隔
- `--types DELETE,MOVE`: 多事件类型
- `--sort size`: 按文件大小排序（其他选项：time, pid）
- `--format json`: 输出格式切换

**通配符命令匹配**:
```bash
# 所有 Python 相关进程
fsmon query --since 24h --cmd "python*"

# 所有 Java 应用
fsmon query --since 24h --cmd "java*"

# vim/nvim 编辑器操作
fsmon query --since 24h --cmd "vim*","nvim*"
```

**导出报表**:
```bash
# CSV 导出（Excel 可直接打开）
fsmon query --since 7d --format csv > weekly_audit.csv

# JSON 导出（配合 jq 分析）
fsmon query --since 7d --format json | jq '.[] | select(.size_change > 1000000)'

# 只提取特定字段
fsmon query --since 24h --format json | jq '.[] | {time: .time, path: .path, cmd: .cmd}'
```

---

### 场景 8: 日志清理与空间管理

**背景**: 守护进程运行一个月后日志文件达到 2GB，需要清理旧数据释放空间。

**准备环境**:
```bash
# 查看当前日志大小
ls -lh ~/.fsmon/history.log
du -sh ~/.fsmon/
```

**预览清理效果**:
```bash
# 保留最近 7 天，预览不实际删除
fsmon clean --keep-days 7 --dry-run

# 限制日志最大 500MB，预览
fsmon clean --max-size 500MB --dry-run
```

**参数说明**:
- `--keep-days 7`: 保留 7 天内的日志
- `--max-size 500MB`: 限制文件总大小
- `--dry-run`: 预览模式，显示将删除多少行但不实际修改

**预期输出** (dry-run):
```
Dry run: Would delete 152345 lines
No changes made (--dry-run enabled)
```

**执行清理**:
```bash
# 保留 30 天日志
fsmon clean --keep-days 30

# 同时限制大小（先按时间，再按大小）
fsmon clean --keep-days 30 --max-size 100MB

# 清理指定文件
fsmon clean --log-file /var/log/fsmon-audit.log --keep-days 7 --max-size 50MB
```

**清理后验证**:
```bash
ls -lh ~/.fsmon/history.log
# 应该看到文件大小明显减小
```

**自动化建议** (crontab):
```bash
# 每周日凌晨 3 点清理 30 天前的日志
0 3 * * 0 /usr/local/bin/fsmon clean --keep-days 30 --max-size 100MB
```

---

## 输出格式示例

### 人类可读格式

```
[2024-05-01 14:30:25] [MODIFY] /var/log/syslog (PID: 1234, CMD: rsyslogd, USER: syslog, SIZE_CHANGE: +2.5KB)
```

### MOVE/RENAME 事件

```
[2024-05-01 14:35:10] [RENAME] /home/user/old.txt -> /home/user/new.txt (PID: 5678, CMD: mv, USER: user, SIZE_CHANGE: +0B)

[2024-05-01 14:40:22] [MOVE] /tmp/source/file.txt -> /var/data/file.txt (PID: 9012, CMD: mv, USER: root, SIZE_CHANGE: +0B)
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

| 事件类型 | 触发条件 | 说明 |
|---------|---------|------|
| CREATE | 文件创建 | 新文件或目录被创建 |
| DELETE | 文件删除 | 文件或目录被删除（包括递归删除） |
| MODIFY | 文件修改 | 文件内容被修改并关闭（FAN_CLOSE_WRITE） |
| RENAME | 同目录移动 | 文件在同一目录内重命名 |
| MOVE | 跨目录移动 | 文件从一个目录移动到另一个目录 |

### 短命进程捕获原理

传统 fanotify 方案无法检测 `touch`/`rm`/`mv` 等短命进程，因为：
1. 进程执行 → 触发文件操作 → 进程退出
2. fanotify 事件异步通知，到达时进程已退出
3. `/proc/{pid}` 已不存在，无法读取 cmd/user

**fsmon 的解决方案：**
1. 启动 Proc Connector 监听线程，订阅 `PROC_EVENT_EXEC`
2. 进程调用 `exec()` 时立即读取 `/proc/{pid}/comm` 和 UID
3. 缓存到 `DashMap<u32, ProcInfo>`（线程安全）
4. fanotify 事件到达时查缓存，保证短命进程信息不丢失

### 删除目录路径恢复原理

`rm -rf fold/` 时，内核先删除子文件再删除父目录，导致：
1. 子文件的 DFID_NAME 包含 fold/ 的句柄，但 fold/ 已删除，`open_by_handle_at()` 失败
2. 子文件路径显示为空

**fsmon 的解决方案：**
1. 启动时用 `name_to_handle_at()` 预缓存所有目录的 handle → path 映射
2. 收到事件后两遍处理：
   - 第一遍：尝试解析所有事件，更新成功解析的目录到缓存
   - 第二遍：用缓存恢复失败事件的父目录路径
3. 迭代直到所有路径都恢复（支持多级嵌套删除）

## 技术对比

| 工具 | 进程追踪 | 短命进程 | 递归监控 | 删除路径恢复 | 性能 | 配置复杂度 |
|------|---------|---------|---------|-------------|------|-----------|
| **fsmon** | ✅ | ✅ | ✅ | ✅ | 高 | 低 |
| inotifywait | ❌ | ❌ | ❌ | ❌ | 中 | 中 |
| auditd | ✅ | ⚠️ | ⚠️ | ⚠️ | 低 | 高 |

## 注意事项

1. **权限要求**: 监控某些系统目录可能需要 `sudo`（Proc Connector 也需要 root）
2. **性能影响**: 监控整个文件系统会产生大量事件，建议使用过滤条件
3. **日志轮转**: 定期使用 `clean` 命令管理日志大小
4. **排除路径**: 建议排除 `/proc`, `/sys`, `/dev` 等虚拟文件系统
5. **内核版本**: 需要 Linux 5.9+（支持 FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME）
6. **文件系统兼容性**:
   - **ext4/XFS/tmpfs**: 完全支持，使用 `FAN_MARK_FILESYSTEM` 模式，无竞态窗口
   - **btrfs**: 自动回退到 inode mark 模式，递归创建子目录时可能存在竞态窗口（子文件创建事件可能丢失），递归删除正常工作
   - **OverlayFS**: 部分内核版本可能不兼容 `FAN_MARK_FILESYSTEM`，如遇问题请反馈
7. **btrfs 用户注意**: 由于 btrfs 子卷的 fsid 与根 superblock 不同，`FAN_MARK_FILESYSTEM` 会返回 EXDEV 错误。fsmon 会自动回退到 inode mark + 动态标记模式，但在快速连续创建目录和文件的场景下（如 `mkdir -p fold && touch fold/file.txt`），子文件的创建事件可能因竞态窗口而丢失。这是 btrfs 内核限制的固有问题，建议在 ext4/XFS 上运行以获得最佳体验。

## 开发计划

- [ ] TUI 交互界面
- [ ] 持久化数据库存储
- [ ] 告警通知系统（Webhook/邮件）
- [ ] 容器支持（Docker/Kubernetes）
- [ ] 网络远程监控（gRPC API）
- [ ] Windows/macOS 支持（通过其他后端）

## 技术栈

- **语言**: Rust
- **核心库**: 
  - fanotify (Linux 内核接口，通过 libc 调用)
  - tokio (异步运行时)
  - dashmap (并发 HashMap)
  - netlink connector (Proc Connector)
- **CLI**: clap
- **序列化**: serde, serde_json, csv
- **时间**: chrono

## 许可证

MIT License
