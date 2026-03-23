### 一、工具核心定位（fsmon）
轻量级、高性能的**系统级文件变更溯源工具**，专注记录文件操作行为（创建/删除/修改等），关联进程/PID/用户信息，支持多维度查询，**无文件复原功能**，仅提供溯源信息。

### 二、核心命令/参数设计（CLI 版，无 TUI）
#### 基础约定
- 所有参数支持短/长格式（如 `-p`/`--pid`）；
- 时间格式支持“人类可读”（如 `1h`/`30m`/`2024-05-01 10:00`）；
- 大小单位支持 `B/KB/MB/GB/TB`，大小写兼容；
- 输出格式默认 `human`（易读），可选 `json`/`csv`（机器解析）。

---

## 1. 核心命令：`monitor`（实时监控文件变更）
### 作用
启动实时监控，输出文件变更事件及溯源信息（核心功能）。

### 参数
| 参数                | 短格式 | 说明                                                                 | 必填 | 默认值       |
|---------------------|--------|----------------------------------------------------------------------|------|--------------|
| `PATH`              | -      | 监控的目录/文件路径（支持多个路径，如 `fsmon monitor /var/log /tmp`） | 是   | -            |
| `--min-size`        | `-s`   | 仅记录大小变化≥指定值的事件（如 `1GB`/`100MB`）                     | 否   | 0（无限制）  |
| `--types`           | `-t`   | 仅监控指定操作类型（逗号分隔），可选值：CREATE/DELETE/MODIFY/RENAME/SIZE_CHANGE | 否   | 所有类型     |
| `--exclude`         | `-e`   | 排除监控的路径（支持通配符，如 `*.log`/`/tmp/*`）                   | 否   | 无           |
| `--output`          | `-o`   | 将监控日志写入指定文件（不影响终端输出）                             | 否   | 仅终端输出   |
| `--format`          | `-f`   | 输出格式：`human`/`json`/`csv`                                       | 否   | human        |
| `--daemon`          | `-d`   | 后台守护进程运行（仅 Linux/macOS）                                   | 否   | 前台运行     |

### 示例 & 输出
#### 示例1：基础监控（监控 /var/log，输出易读格式）
```bash
fsmon monitor /var/log
```
#### 输出（human 格式）：
```
[2024-05-01 14:30:05] [MODIFY] /var/log/nginx/access.log (PID: 1234, CMD: nginx, USER: root, SIZE_CHANGE: +1.2KB)
[2024-05-01 14:30:10] [CREATE] /var/log/mysqld.log (PID: 5678, CMD: mysqld, USER: mysql, SIZE_CHANGE: 0B)
[2024-05-01 14:30:15] [DELETE] /var/log/old.log (PID: 9012, CMD: bash, USER: root, SIZE_CHANGE: -500MB)
```

#### 示例2：监控 /tmp，仅记录 CREATE/MODIFY 且大小≥100MB 的事件，输出 JSON
```bash
fsmon monitor /tmp --types CREATE,MODIFY --min-size 100MB --format json
```
#### 输出（json 格式）：
```json
{"time":"2024-05-01T14:35:20+08:00","type":"MODIFY","path":"/tmp/large_file.dat","pid":12345,"cmd":"python","user":"ubuntu","size_change":102400000,"size_change_human":"+100MB"}
{"time":"2024-05-01T14:36:10+08:00","type":"CREATE","path":"/tmp/big_archive.tar","pid":6789,"cmd":"tar","user":"ubuntu","size_change":153600000,"size_change_human":"+150MB"}
```

#### 示例3：后台监控 /，排除 /proc/*，日志写入 /var/log/fsmon.log
```bash
fsmon monitor / --exclude /proc/* --daemon --output /var/log/fsmon.log
```
#### 输出（终端仅提示守护进程启动，日志写入文件）：
```
fsmon daemon started (PID: 7890), log file: /var/log/fsmon.log
```

---

## 2. 核心命令：`query`（查询历史监控日志）
### 作用
查询已记录的监控日志（需先通过 `monitor --output` 保存日志，或指定历史日志文件）。

### 参数
| 参数                | 短格式 | 说明                                                                 | 必填 | 默认值       |
|---------------------|--------|----------------------------------------------------------------------|------|--------------|
| `--log-file`        | `-l`   | 待查询的日志文件路径（若未指定，默认读取 `~/.fsmon/history.log`）   | 否   | ~/.fsmon/history.log |
| `--since`           | `-S`   | 起始时间（如 `1h`/`2024-05-01 10:00`）                              | 否   | 日志起始时间 |
| `--until`           | `-U`   | 结束时间（如 `30m`/`2024-05-01 12:00`）                              | 否   | 当前时间     |
| `--pid`             | `-p`   | 仅查询指定 PID 的事件（支持多个，逗号分隔）                          | 否   | 所有 PID     |
| `--cmd`             | `-c`   | 仅查询指定进程名的事件（支持通配符，如 `nginx*`/`python`）           | 否   | 所有进程     |
| `--user`            | `-u`   | 仅查询指定用户的事件（支持多个，逗号分隔）                           | 否   | 所有用户     |
| `--types`           | `-t`   | 仅查询指定操作类型（同 monitor 命令）                                | 否   | 所有类型     |
| `--min-size`        | `-s`   | 仅查询大小变化≥指定值的事件                                          | 否   | 0            |
| `--format`          | `-f`   | 输出格式：`human`/`json`/`csv`                                       | 否   | human        |
| `--sort`            | `-r`   | 排序方式：`time`（默认）/`size`（按大小变化）/`pid`                  | 否   | time         |

### 示例 & 输出
#### 示例1：查询过去1小时内，进程名包含 nginx 的事件
```bash
fsmon query --since 1h --cmd nginx*
```
#### 输出（human 格式）：
```
[2024-05-01 14:25:05] [MODIFY] /var/log/nginx/access.log (PID: 1234, CMD: nginx, USER: root, SIZE_CHANGE: +800KB)
[2024-05-01 14:30:05] [MODIFY] /var/log/nginx/access.log (PID: 1234, CMD: nginx, USER: root, SIZE_CHANGE: +1.2KB)
[2024-05-01 14:32:10] [MODIFY] /var/log/nginx/error.log (PID: 1234, CMD: nginx, USER: root, SIZE_CHANGE: +500B)
```

#### 示例2：查询 2024-05-01 10:00-12:00 期间，root 用户删除的文件，按大小降序排序，输出 CSV
```bash
fsmon query --since "2024-05-01 10:00" --until "2024-05-01 12:00" --user root --types DELETE --sort size --format csv
```
#### 输出（csv 格式）：
```
time,type,path,pid,cmd,user,size_change,size_change_human
2024-05-01T11:05:20+08:00,DELETE,/var/log/large.log,9012,bash,root,-1073741824,-1GB
2024-05-01T11:10:15+08:00,DELETE,/tmp/old_data.dat,5678,rm,root,-524288000,-500MB
2024-05-01T11:20:30+08:00,DELETE,/home/root/temp.txt,1234,bash,root,-102400,-100KB
```

#### 示例3：查询指定日志文件，PID 为 1234 且大小变化≥1GB 的事件
```bash
fsmon query --log-file /var/log/fsmon.log --pid 1234 --min-size 1GB
```
#### 输出（human 格式）：
```
[2024-05-01 13:00:05] [MODIFY] /data/file1.dat (PID: 1234, CMD: java, USER: app, SIZE_CHANGE: +1.5GB)
[2024-05-01 13:10:20] [MODIFY] /data/file2.dat (PID: 1234, CMD: java, USER: app, SIZE_CHANGE: +2.0GB)
```

---

## 3. 辅助命令：`status`（查看监控状态）
### 作用
查看当前后台监控进程的状态（是否运行、监控路径、日志路径等）。

### 参数
无必填参数，可选 `--format json` 输出机器可读格式。

### 示例 & 输出
#### 示例1：查看状态（human 格式）
```bash
fsmon status
```
#### 输出：
```
fsmon daemon status: Running (PID: 7890)
Monitored paths: / (exclude: /proc/*)
Log file: /var/log/fsmon.log
Start time: 2024-05-01 14:00:00
Event count: 1258 (today)
Memory usage: 3.2MB
```

#### 示例2：查看状态（json 格式）
```bash
fsmon status --format json
```
#### 输出：
```json
{"status":"running","pid":7890,"monitored_paths":["/"],"excluded_paths":["/proc/*"],"log_file":"/var/log/fsmon.log","start_time":"2024-05-01T14:00:00+08:00","event_count_today":1258,"memory_usage":"3.2MB","memory_usage_bytes":3355443}
```

---

## 4. 辅助命令：`stop`（停止后台监控）
### 作用
停止后台运行的 fsmon 守护进程。

### 参数
无（可选 `--force` 强制终止）。

### 示例 & 输出
#### 示例1：停止后台监控
```bash
fsmon stop
```
#### 输出：
```
fsmon daemon (PID: 7890) stopped successfully
```

#### 示例2：强制停止（进程无响应时）
```bash
fsmon stop --force
```
#### 输出：
```
fsmon daemon (PID: 7890) force stopped
```

---

## 5. 辅助命令：`clean`（清理历史日志）
### 作用
清理过期的监控日志（按时间/大小），避免日志文件过大。

### 参数
| 参数                | 短格式 | 说明                                                                 | 必填 | 默认值       |
|---------------------|--------|----------------------------------------------------------------------|------|--------------|
| `--log-file`        | `-l`   | 待清理的日志文件路径                                                 | 否   | ~/.fsmon/history.log |
| `--keep-days`       | `-k`   | 保留最近 N 天的日志（如 `7` 保留7天）                               | 否   | 30           |
| `--max-size`        | `-m`   | 日志文件最大大小（超过则清理旧日志，如 `100MB`）                     | 否   | 无限制       |
| `--dry-run`         | `-n`   | 模拟清理，仅输出要删除的内容（不实际删除）                           | 否   | 实际删除     |

### 示例 & 输出
#### 示例1：清理日志，仅保留最近7天的内容
```bash
fsmon clean --keep-days 7
```
#### 输出：
```
Cleaning /home/user/.fsmon/history.log...
Deleted 12589 lines (logs older than 7 days)
Log file size reduced from 200MB to 45MB
```

#### 示例2：模拟清理，日志最大保留 50MB
```bash
fsmon clean --max-size 50MB --dry-run
```
#### 输出：
```
Dry run: Would delete 8901 lines (logs to keep size: 50MB)
No changes made (--dry-run enabled)
```

---

### 三、总结
1. **核心命令**：`monitor`（实时监控）、`query`（历史查询）—— 覆盖“记录-溯源”核心需求；
2. **辅助命令**：`status`/`stop`（守护进程管理）、`clean`（日志清理）—— 提升工具易用性；
3. **参数设计**：兼顾“易用性（人类可读参数）”和“灵活性（多维度筛选）”，输出支持易读/机器解析格式；
4. **核心边界**：所有功能仅记录“行为+元数据”，不涉及文件内容/复原，与 Git 无冲突，聚焦“溯源”核心。

该设计满足“高性能、小规模、创新性”的核心诉求，且所有命令/参数都围绕“文件变更溯源”展开，无冗余功能。
