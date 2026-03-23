# fsmon - File System Monitor

**轻量级高性能文件系统变更追踪工具**

fsmon (file system monitor) 是一个实时文件变更监控工具，能够追踪文件系统的变化并记录是哪个进程执行了这些操作。当你需要回答"服务器上谁修改了这个文件？"这个问题时，fsmon 就是你的答案。

## 特性

- **实时监控**: 捕获 CREATE、DELETE、MODIFY、RENAME 事件
- **进程追踪**: 记录执行操作的 PID、命令名和用户名
- **高性能**: Rust 编写，内存占用 <5MB
- **跨平台**: 支持 Linux/macOS
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
  -t, --types TYPES       事件类型，逗号分隔：CREATE,DELETE,MODIFY,RENAME
  -e, --exclude PATTERN   排除路径（正则模式）
  -o, --output FILE       输出到文件
  -f, --format FORMAT     输出格式：human, json, csv
  -d, --daemon           以守护进程运行
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

```bash
# 监控配置文件目录
fsmon monitor /etc --types MODIFY --output /var/log/etc-changes.log

# 发现问题后查询详情
fsmon query --log-file /var/log/etc-changes.log --since 1h
```

### 场景 2: 追踪大文件创建

```bash
# 监控大于 1GB 的文件创建
fsmon monitor /home --types CREATE --min-size 1GB
```

### 场景 3: 审计删除操作

```bash
# 记录所有删除事件
fsmon monitor /data --types DELETE --daemon --output /var/log/deletes.log

# 查询昨天被删除的文件
fsmon query --log-file /var/log/deletes.log \
  --since "2024-05-01 00:00" --until "2024-05-01 23:59" \
  --types DELETE
```

### 场景 4: 监控特定应用

```bash
# 监控数据库目录
fsmon monitor /var/lib/mysql --daemon --output /var/log/mysql-changes.log

# 只查看 mysqld 进程的操作
fsmon query --log-file /var/log/mysql-changes.log --cmd mysqld
```

### 场景 5: 导出报表

```bash
# 导出 CSV 用于分析
fsmon query --since 24h --format csv > changes.csv

# 导出 JSON 用于集成
fsmon query --since 1h --format json | jq '.[] | select(.size_change > 1000000)'
```

## 输出格式示例

### 人类可读格式

```
[2024-05-01 14:30:25] MODIFY /var/log/syslog
  PID: 1234  CMD: rsyslogd  USER: syslog
  Size: +2.5KB
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

## 技术对比

| 工具 | 进程追踪 | 性能 | 配置复杂度 | 日志量 |
|------|---------|------|-----------|--------|
| **fsmon** | ✅ | 高 | 低 | 精简 |
| inotifywait | ❌ | 中 | 中 | 中等 |
| auditd | ✅ | 低 | 高 | 庞大 |

## 注意事项

1. **权限要求**: 监控某些系统目录可能需要 `sudo`
2. **性能影响**: 监控整个文件系统会产生大量事件，建议使用过滤条件
3. **日志轮转**: 定期使用 `clean` 命令管理日志大小
4. **排除路径**: 建议排除 `/proc`, `/sys`, `/dev` 等虚拟文件系统

## 开发计划

- [ ] TUI 交互界面
- [ ] 持久化数据库存储
- [ ] 告警通知系统
- [ ] 容器支持
- [ ] 网络远程监控

## 技术栈

- **语言**: Rust
- **核心库**: notify (文件监控), tokio (异步运行时)
- **CLI**: clap
- **序列化**: serde, serde_json, csv

## 许可证

MIT License
