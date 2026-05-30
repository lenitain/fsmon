# fsmon 详细测试方案

> **当前状态**: daemon 已在后台运行 `sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log`
> **监控目标**: `_global` 组: `/home/pilot/.config/what` + `/tmp/fsmon_ext_test`（均递归）
> **socket**: `/tmp/fsmon-1000.sock`, **日志路径**: `~/.local/state/fsmon`
> **配置文件**: `~/.config/fsmon/fsmon.toml`（全默认，全部注释状态）

## 阶段一实测结果（2026-05-30）

| 用例 | 结果 |
|------|------|
| 1.1.1-1.1.9 CLI命令 | ✅ 全部通过 |
| 1.2.1-1.2.8 事件捕获 | ✅ 全部通过 |
| 1.3.1-1.3.2 进程追踪 | ✅ 全部通过 |
| 1.4.1-1.4.5 动态管理 | ✅ 全部通过 |
| 1.4.6 live remove | ⚠️ 需 SIGHUP 同步（daemon 重启后生效）|
| 1.4.7-1.4.8 多 cmd 组 | ✅ 通过 |
| 1.5.1-1.5.4 日志验证 | ✅ 全部通过 |
| 4.1.1-4.1.2 扩展读取 | ✅ 通过 |
| 4.2.1 subscribe.sh | ✅ 通过 |
| 4.2.2 subscribe.py | 🔧 修复了 buffering bug（见下方）|
| 5.1-5.7 边界场景 | ✅ 通过（5.3/5.4需sudo，未执行）|

**发现的 bug**:
1. `subscribe.py` 使用 `sock.recv(1)` 逐字节读 TOML 响应后，再调用 `sock.makefile()` 会导致丢失后续 JSONL 数据（已修复为统一使用 makefile）
2. `monitored.jsonl` 在测试过程中发生了一次损坏（第二行 JSON 被截断），根因未确定，可能是 `save()` 使用 `File::create` 而非原子写入

---

## 测试执行说明

每个测试用例包含三要素：**测试内容 / 测试方法 / 预期结果**。
每个用例需从三方面验证：①命令输出 ②`/tmp/fsmon.log`(daemon debug) ③JSONL 日志文件。

测试按 **配置阶段** 组织，不同阶段需要修改 fsmon.toml 并重启 daemon。

---

---

# 阶段一：默认配置

> **配置**: fsmon.toml 保持当前全注释默认状态，daemon 不重启
> **日志路径**: `~/.local/state/fsmon/`, **同步**: 关闭, **时区**: UTC

## 准备：确保测试目录就绪

```bash
mkdir -p /tmp/fsmon_ext_test
# 等待 daemon 通过 inotify pick up 该目录（tail /tmp/fsmon.log 确认）
```

## 1.1 CLI 命令基础测试

### 1.1.1 `fsmon monitored` — 列出监控路径

| 项目 | 内容 |
|------|------|
| **测试内容** | 列出所有监控路径及配置 |
| **测试方法** | `fsmon monitored` |
| **预期结果** | JSON 输出含 `cmd:"_global"`, paths 含两个路径均为 recursive:true |

### 1.1.2 `fsmon health` — daemon 健康状态

| 项目 | 内容 |
|------|------|
| **测试内容** | 通过 socket 查询 daemon 运行状态 |
| **测试方法** | `fsmon health` |
| **预期结果** | TOML: `ok=true`, `uptime_secs>0`, `monitored_paths>=1`, `reader_groups>=1`, `readers[0].alive=true` |

### 1.1.3 `fsmon query _global` — 查询全部历史事件

| 项目 | 内容 |
|------|------|
| **测试内容** | 读取 JSONL 日志输出所有事件 |
| **测试方法** | `fsmon query _global` |
| **预期结果** | 每行一个 JSON，字段完整：time, event_type, path, pid, cmd, user, file_size, ppid, tgid, chain。time 以 Z 结尾(UTC) |

### 1.1.4 `fsmon query _global -t '>1h'` — 时间过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | 只返回最近1小时事件 |
| **测试方法** | `fsmon query _global -t '>1h'` |
| **预期结果** | 仅输出1小时内的事件。与 `fsmon query _global | wc -l` 对比行数应 ≤ 全量 |

### 1.1.5 `fsmon query _global -p /tmp/fsmon_ext_test` — 路径过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | 路径前缀过滤 |
| **测试方法** | `fsmon query _global -p /tmp/fsmon_ext_test` |
| **预期结果** | 所有输出 path 以 `/tmp/fsmon_ext_test` 开头，不包含 `/home/pilot/.config/what` 的事件 |

### 1.1.6 `fsmon changes _global` — 去重变化列表

| 项目 | 内容 |
|------|------|
| **测试内容** | 每路径只保留最新事件，时间倒序 |
| **测试方法** | `fsmon changes _global | head -5` |
| **预期结果** | 输出中同一 path 不重复出现。时间从新到旧排列 |

### 1.1.7 `fsmon clean _global --dry-run` — 干运行清理

| 项目 | 内容 |
|------|------|
| **测试内容** | dry-run 不修改文件，仅预览删除条目 |
| **测试方法** | `fsmon clean _global --dry-run` |
| **预期结果** | 若有超过30天的事件则显示 `[to-delete]`；底部显示 `Dry run: N entries would be deleted`。验证 `~/.local/state/fsmon/_global_log.jsonl` 内容未变 |

### 1.1.8 `fsmon cd -l` / `fsmon cd -m` — 进入目录

| 项目 | 内容 |
|------|------|
| **测试内容** | cd -l 进日志目录，cd -m 进 monitored 存储目录 |
| **测试方法** | `echo "ls _global_log.jsonl && exit" \| fsmon cd -l`<br/>`echo "ls monitored.jsonl && exit" \| fsmon cd -m` |
| **预期结果** | -l 下能看到 `_global_log.jsonl`；-m 下能看到 `monitored.jsonl` |

### 1.1.9 `fsmon cd` 无参数报错

| 项目 | 内容 |
|------|------|
| **测试内容** | cd 不带参数时应有错误提示 |
| **测试方法** | `fsmon cd 2>&1` |
| **预期结果** | 非零退出码，提示需要 `-l` 或 `-m` |

## 1.2 事件捕获核心测试

### 1.2.1 文件创建 (CREATE + CLOSE_WRITE + ATTRIB)

| 项目 | 内容 |
|------|------|
| **测试内容** | `touch` 创建文件捕获完整事件链 |
| **测试方法** | `touch /tmp/fsmon_ext_test/cap_create && sleep 0.5`<br/>`tail -5 ~/.local/state/fsmon/_global_log.jsonl` |
| **预期结果** | 出现 CREATE → CLOSE_WRITE → ATTRIB 三条。`cmd="touch"`, `user="pilot"`, `file_size=0` |

### 1.2.2 文件修改 (MODIFY + CLOSE_WRITE)

| 项目 | 内容 |
|------|------|
| **测试内容** | 写入已有文件捕获 MODIFY 事件 |
| **测试方法** | `echo "hello fsmon" > /tmp/fsmon_ext_test/cap_modify` |
| **预期结果** | 出现 MODIFY + CLOSE_WRITE。`file_size=12`（"hello fsmon\n"） |

### 1.2.3 文件删除 (DELETE)

| 项目 | 内容 |
|------|------|
| **测试内容** | 删除文件捕获 DELETE 事件 |
| **测试方法** | `rm /tmp/fsmon_ext_test/cap_modify` |
| **预期结果** | DELETE 事件，`path=/tmp/fsmon_ext_test/cap_modify`, `cmd="rm"` |

### 1.2.4 `rm -rf` 目录完整删除捕获

| 项目 | 内容 |
|------|------|
| **测试内容** | 递归删除目录时每个文件都被记录 DELETE |
| **测试方法** | `mkdir -p /tmp/fsmon_ext_test/rmrf/sub && touch /tmp/fsmon_ext_test/rmrf/a /tmp/fsmon_ext_test/rmrf/sub/b && rm -rf /tmp/fsmon_ext_test/rmrf` |
| **预期结果** | 能看到 a 和 b 的 DELETE 事件。验证 `fsmon query _global \| jq 'select(.path \| startswith("/tmp/fsmon_ext_test/rmrf"))'` 至少有 2 条 DELETE |

### 1.2.5 递归子目录深层事件

| 项目 | 内容 |
|------|------|
| **测试内容** | 深层子目录中的文件被递归监控 |
| **测试方法** | `mkdir -p /tmp/fsmon_ext_test/deep/nested && touch /tmp/fsmon_ext_test/deep/nested/deep_file` |
| **预期结果** | 出现 path=`/tmp/fsmon_ext_test/deep/nested/deep_file` 的 CREATE 事件 |

### 1.2.6 目录创建

| 项目 | 内容 |
|------|------|
| **测试内容** | 新建子目录产生 CREATE 事件，且自动纳入递归监控 |
| **测试方法** | `mkdir /tmp/fsmon_ext_test/new_subdir && touch /tmp/fsmon_ext_test/new_subdir/inner` |
| **预期结果** | new_subdir 有 CREATE 事件，new_subdir/inner 也有 CREATE 事件（说明新目录已被递归监视） |

### 1.2.7 属性变更 (ATTRIB)

| 项目 | 内容 |
|------|------|
| **测试内容** | chmod 触发 ATTRIB 事件 |
| **测试方法** | `chmod 644 /tmp/fsmon_ext_test/cap_create` |
| **预期结果** | ATTRIB 事件，`cmd="chmod"` |

### 1.2.8 重命名 (MOVED_FROM + MOVED_TO)

| 项目 | 内容 |
|------|------|
| **测试内容** | mv 在同一文件系统内重命名 |
| **测试方法** | `mv /tmp/fsmon_ext_test/cap_create /tmp/fsmon_ext_test/renamed` |
| **预期结果** | MOVED_FROM(旧名) + MOVED_TO(新名) 成对出现 |

## 1.3 进程归属验证

### 1.3.1 基本进程字段

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 pid, cmd, user, ppid, tgid 字段正确 |
| **测试方法** | `touch /tmp/fsmon_ext_test/proc_fields && sleep 0.5`<br/>`tail -5 ~/.local/state/fsmon/_global_log.jsonl \| jq '{pid, cmd, user, ppid, tgid}'` |
| **预期结果** | pid=实际 touch PID, ppid=shell PID, tgid=pid, user="pilot" |

### 1.3.2 daemon 自身事件被过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 写日志文件的操作不会产生监控事件 |
| **测试方法** | `fsmon query _global \| jq 'select(.path \| startswith("/home/pilot/.local/state/fsmon"))'` |
| **预期结果** | 无输出（daemon 的日志写入被 PID 过滤排除） |

## 1.4 动态路径管理（live 操作，不重启 daemon）

### 1.4.1 `fsmon add _global` — 实时添加路径

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 运行时通过 socket 实时添加监控路径 |
| **测试方法** | `mkdir -p /tmp/fsmon_dyn_add`<br/>`fsmon add _global --path /tmp/fsmon_dyn_add -r`<br/>`touch /tmp/fsmon_dyn_add/live_test && sleep 0.5`<br/>`fsmon query _global -p /tmp/fsmon_dyn_add` |
| **预期结果** | add 输出 `Entry added`；随后 live_test 的 CREATE 事件被捕获 |

### 1.4.2 `fsmon add` — 不存在的路径（pending 模式）

| 项目 | 内容 |
|------|------|
| **测试内容** | 添加不存在路径 → daemon 等目录创建后自动开始监控 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_pending -r`（add 时不创建目录）<br/>`mkdir /tmp/fsmon_pending && touch /tmp/fsmon_pending/hello && sleep 1`<br/>`fsmon query _global -p /tmp/fsmon_pending` |
| **预期结果** | add 输出 `[Note] path does not exist yet`；daemon 日志出现 inotify watch；目录创建后 hello 事件被捕获 |

### 1.4.3 `fsmon add` — 带事件类型过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | 只捕获指定事件类型 |
| **测试方法** | `mkdir -p /tmp/fsmon_type_filter`<br/>`fsmon add _global --path /tmp/fsmon_type_filter -r --types CREATE --types DELETE`<br/>`touch /tmp/fsmon_type_filter/f1 && echo x > /tmp/fsmon_type_filter/f1 && rm /tmp/fsmon_type_filter/f1 && sleep 0.5`<br/>`fsmon query _global -p /tmp/fsmon_type_filter \| jq '.event_type'` |
| **预期结果** | 只有 CREATE 和 DELETE，没有 MODIFY/CLOSE_WRITE |

### 1.4.4 `fsmon add` — 带大小过滤 (>0)

| 项目 | 内容 |
|------|------|
| **测试内容** | 只输出文件大小满足条件的 |
| **测试方法** | `mkdir -p /tmp/fsmon_size_filter`<br/>`fsmon add _global --path /tmp/fsmon_size_filter -r -s '>0'`<br/>`touch /tmp/fsmon_size_filter/empty && echo data > /tmp/fsmon_size_filter/data && sleep 0.5`<br/>`fsmon query _global -p /tmp/fsmon_size_filter` |
| **预期结果** | empty(file_size=0) 无事件；data(file_size=5) 有事件 |

### 1.4.5 `fsmon add` — 重复添加覆盖

| 项目 | 内容 |
|------|------|
| **测试内容** | 已存在的(path,cmd)对重复添加时参数被替换 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_size_filter -r -s '=0'`（覆盖之前 >0）<br/>`touch /tmp/fsmon_size_filter/another_empty && sleep 0.5`<br/>`fsmon query _global -p /tmp/fsmon_size_filter \| jq 'select(.path \| endswith("another_empty"))'` |
| **预期结果** | add 输出 `[Note] ... already monitored — new parameters will replace`；file_size=0 的事件现在能看到了 |

### 1.4.6 `fsmon remove _global --path` — 实时移除路径

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 运行时移除监控路径 |
| **测试方法** | `fsmon remove _global --path /tmp/fsmon_dyn_add`<br/>`touch /tmp/fsmon_dyn_add/after_remove && sleep 1`<br/>`fsmon query _global -p /tmp/fsmon_dyn_add \| jq 'select(.path \| endswith("after_remove"))'` |
| **预期结果** | remove 输出 `Entry removed`；after_remove 无新事件 |

### 1.4.7 `fsmon remove <cmd>` — 移除整个 cmd 组

| 项目 | 内容 |
|------|------|
| **测试内容** | 不带 --path 时移除整个 cmd 组 |
| **测试方法** | `fsmon add testapp --path /tmp/fsmon_group_test -r`<br/>`fsmon remove testapp`<br/>`fsmon monitored \| jq 'select(.cmd=="testapp")'` |
| **预期结果** | 输出 `Entry removed: [testapp]`；monitored 不再包含 testapp 组 |

### 1.4.8 同一路径多 cmd 组独立配置

| 项目 | 内容 |
|------|------|
| **测试内容** | 同一路径可被多个 cmd 组监控，各自独立 |
| **测试方法** | `mkdir -p /tmp/fsmon_multi`<br/>`fsmon add myapp --path /tmp/fsmon_multi -r --types CREATE`<br/>`touch /tmp/fsmon_multi/shared_file && sleep 0.5`<br/>`fsmon query _global -p /tmp/fsmon_multi \| jq '.event_type'`（全局组无类型过滤，应看到所有事件） |

## 1.5 日志验证（阶段一）

### 1.5.1 启动日志完整性

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动时的 debug 输出完整 |
| **测试方法** | `head -40 /tmp/fsmon.log` |
| **预期结果** | 包含: Config loaded(3路径), cache configuration(所有默认值), Monitor initialized(2路径), combined fanotify mask, Active paths |

### 1.5.2 事件处理日志

| 项目 | 内容 |
|------|------|
| **测试内容** | 事件产生时 debug 日志有相关输出 |
| **测试方法** | `touch /tmp/fsmon_ext_test/debug_check && sleep 1 && tail -10 /tmp/fsmon.log` |
| **预期结果** | 不应有 ERROR/WARNING（正常流程） |

### 1.5.3 cache stats 周期输出 (60s)

| 项目 | 内容 |
|------|------|
| **测试内容** | debug 模式下每60秒输出 cache 统计 |
| **测试方法** | `grep "cache stats" /tmp/fsmon.log` |
| **预期结果** | 每隔约60秒出现一次 `[DEBUG] --- cache stats ---` 及各缓存条目数 |

### 1.5.4 默认日志路径 UTC 时戳

| 项目 | 内容 |
|------|------|
| **测试内容** | 默认配置下时间戳为 UTC |
| **测试方法** | `fsmon query _global -t '>1s' \| jq -r '.time' \| head -1` |
| **预期结果** | 时间戳以 `Z` 结尾（如 `2026-05-29T23:55:02.426Z`） |

---

# 阶段二：自定义日志路径 + local_time + sync_interval + disk_min_free

> **需要修改 fsmon.toml 并重启 daemon**

### 修改配置

编辑 `~/.config/fsmon/fsmon.toml`，取消注释并修改以下行：

```toml
[logging]
path = "/tmp/fsmon_custom_logs"
sync_interval_secs = 5
local_time = true
disk_min_free = "10%"
```

完整内容（只改 logging 段，其余保持注释）：
```toml
# ================================================================
# fsmon configuration file
# ================================================================
[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

[logging]
path = "/tmp/fsmon_custom_logs"
keep_days = 30
size = ">=1GB"
disk_min_free = "10%"
sync_interval_secs = 5
local_time = true

[socket]
path = "/tmp/fsmon-<UID>.sock"
```

### 重启 daemon

```bash
# 停止当前 daemon
sudo pkill -f "fsmon daemon"
# 创建自定义日志目录
mkdir -p /tmp/fsmon_custom_logs
# 重新启动
sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log &
```

## 2.1 自定义日志路径验证

### 2.1.1 日志写入自定义路径

| 项目 | 内容 |
|------|------|
| **测试内容** | 事件日志写入 `/tmp/fsmon_custom_logs` 而非默认路径 |
| **测试方法** | `touch /tmp/fsmon_ext_test/custom_log_test && sleep 0.5`<br/>`ls -la /tmp/fsmon_custom_logs/` |
| **预期结果** | `/tmp/fsmon_custom_logs/_global_log.jsonl` 存在且包含新事件<br/>**反面验证**: `~/.local/state/fsmon/_global_log.jsonl` 不应有该新事件 |

### 2.1.2 daemon 日志显示自定义路径

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动日志显示自定义日志路径 |
| **测试方法** | `head -20 /tmp/fsmon.log` |
| **预期结果** | `Event logs: /tmp/fsmon_custom_logs` |

## 2.2 local_time 时区验证

### 2.2.1 时间戳使用本地时区

| 项目 | 内容 |
|------|------|
| **测试内容** | 时间戳使用本地时区（+08:00）而非 UTC（Z） |
| **测试方法** | `touch /tmp/fsmon_ext_test/local_time_test && sleep 0.5`<br/>`tail -3 /tmp/fsmon_custom_logs/_global_log.jsonl \| jq -r '.time'` |
| **预期结果** | 时间戳含 `+08:00` 偏移，如 `2026-05-30T07:55:02.426+08:00`<br/>**不应**以 `Z` 结尾 |

### 2.2.2 query/changes 输出也使用本地时间

| 项目 | 内容 |
|------|------|
| **测试内容** | query 输出时间戳也用 local time |
| **测试方法** | `fsmon query _global -p /tmp/fsmon_ext_test -t '>1s' \| jq -r '.time' \| head -1` |
| **预期结果** | 时间戳含 `+08:00` 偏移 |

## 2.3 sync_interval 验证

### 2.3.1 daemon 日志显示 sync_interval

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动显示 sync_interval=5s |
| **测试方法** | `grep sync_interval /tmp/fsmon.log` |
| **预期结果** | `[DEBUG] sync_interval: 5s` |

### 2.3.2 强制 kill 后事件不丢失

| 项目 | 内容 |
|------|------|
| **测试内容** | sync_interval 保证 kill -9 前最多丢失 5s 内事件 |
| **测试方法** | `touch /tmp/fsmon_ext_test/sync_test_1 /tmp/fsmon_ext_test/sync_test_2 /tmp/fsmon_ext_test/sync_test_3`<br/>立即 `sudo kill -9 $(pgrep -f "fsmon daemon")`<br/>然后 `cat /tmp/fsmon_custom_logs/_global_log.jsonl \| jq 'select(.path \| startswith("/tmp/fsmon_ext_test/sync_test"))'` |
| **预期结果** | 三个 sync_test 文件的事件都在日志中（最多丢失最近 5s 内的最后事件，如果刚好在 sync 间隔内）<br/>**注意**: 此测试会杀死 daemon，需要随后重启 |

## 2.4 disk_min_free 验证

### 2.4.1 daemon 启动时检查磁盘空间

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动日志显示磁盘空间检查 |
| **测试方法** | `grep -i "disk\|free" /tmp/fsmon.log \| head -5` |
| **预期结果** | 若 `/tmp` 所在分区空间充足则无警告；空间不足时有 warning 输出 |

---

# 阶段三：禁用文件日志 + 自定义 cache + 自定义 socket 路径

> **需要修改 fsmon.toml 并重启 daemon**

### 修改配置

编辑 `~/.config/fsmon/fsmon.toml`：

```toml
[monitored]
path = "~/.local/share/fsmon/monitored.jsonl"

# [logging]   ← 整个 logging 段注释掉或删除
# path = ...

[socket]
# 自定义 socket 路径测试
path = "/tmp/fsmon_test_custom.sock"

[cache]
dir_capacity = 50000
dir_ttl_secs = 1800
file_size_capacity = 5000
proc_ttl_secs = 300
stats_interval_secs = 30
```

注意：logging 段**必须注释掉或删除**才能禁用文件日志。

### 重启 daemon

```bash
sudo pkill -f "fsmon daemon"
# 如果之前阶段二 kill -9 过，确保没有僵尸进程
sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log &
```

## 3.1 禁用日志验证

### 3.1.1 daemon 日志显示 logging disabled

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动日志显示日志功能已禁用 |
| **测试方法** | `grep "Event logs" /tmp/fsmon.log` |
| **预期结果** | `Event logs: disabled (path not configured)` |

### 3.1.2 不产生 JSONL 日志文件

| 项目 | 内容 |
|------|------|
| **测试内容** | 触发事件后不写日志文件 |
| **测试方法** | `touch /tmp/fsmon_ext_test/no_log_test && sleep 0.5`<br/>`ls ~/.local/state/fsmon/_global_log.jsonl 2>/dev/null \|\| echo "NO FILE"`<br/>`ls /tmp/fsmon_custom_logs/_global_log.jsonl 2>/dev/null \|\| echo "NO FILE"` |
| **预期结果** | 两个路径都不应有新的日志文件或内容增长 |

### 3.1.3 subscribe 在无日志模式下仍正常工作

| 项目 | 内容 |
|------|------|
| **测试内容** | 禁用文件日志不影响实时订阅流 |
| **测试方法** | `echo 'cmd = "subscribe"' \| socat - UNIX-CONNECT:/tmp/fsmon_test_custom.sock 2>&1 \| head -5 &`<br/>`sleep 0.5 && touch /tmp/fsmon_ext_test/no_log_sub_test && sleep 0.5` |
| **预期结果** | socat 首先收到 `ok = true` 的 TOML 响应，随后收到 JSONL 事件流 |

## 3.2 自定义 socket 路径

### 3.2.1 新 socket 路径生效

| 项目 | 内容 |
|------|------|
| **测试内容** | CLI 自动连接新的 socket 路径 |
| **测试方法** | `fsmon health` |
| **预期结果** | 正常返回健康信息（说明 daemon 自动读取了新 socket 路径）<br/>**daemon日志** `Command socket: /tmp/fsmon_test_custom.sock` |

### 3.2.2 旧 socket 路径被清理

| 项目 | 内容 |
|------|------|
| **测试内容** | 旧 socket 文件不再存在 |
| **测试方法** | `ls /tmp/fsmon-1000.sock 2>/dev/null \|\| echo "CLEANED"` |
| **预期结果** | 旧 socket 不存在（daemon 启动时删除旧文件并绑定新路径） |

## 3.3 自定义 cache 配置

### 3.3.1 daemon 日志显示自定义 cache 值

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动日志显示自定义的 cache 配置 |
| **测试方法** | `grep "cache configuration" -A 8 /tmp/fsmon.log` |
| **预期结果** | dir_capacity=50000, dir_ttl_secs=1800, file_size_capacity=5000, proc_ttl_secs=300, stats_interval_secs=30 |

### 3.3.2 cache stats 以30秒间隔输出

| 项目 | 内容 |
|------|------|
| **测试内容** | stats_interval_secs=30 → 每30秒一次 stats |
| **测试方法** | `grep "cache stats" /tmp/fsmon.log` 看时间间隔 |
| **预期结果** | 两次 `--- cache stats ---` 之间约 30 秒 |

---

# 阶段四：Extensios 扩展测试

> **不依赖具体配置**，只要有 daemon 在运行即可。在阶段一或阶段三（文件日志禁用时略有不同）。

## 4.1 日志读取扩展

### 4.1.1 `read-jsonl.sh`

| 项目 | 内容 |
|------|------|
| **测试内容** | bash 脚本能正确读取和格式化 JSONL 日志 |
| **测试方法** | `bash /home/pilot/.projects/fsmon/extensions/examples/read-jsonl.sh` |
| **预期结果** | 输出 `=== recent 5 events ===` 及最近事件摘要（如果日志路径下有文件）；若无日志文件则显示错误提示 |

### 4.1.2 `read-jsonl.py`

| 项目 | 内容 |
|------|------|
| **测试内容** | Python 脚本能正确读取 JSONL 日志 |
| **测试方法** | `python3 /home/pilot/.projects/fsmon/extensions/examples/read-jsonl.py` |
| **预期结果** | 输出 `=== last 5 events ===` 及事件详情（含 time/event_type/cmd/path） |

## 4.2 实时订阅扩展

### 4.2.1 `subscribe.sh` (socat 模式)

| 项目 | 内容 |
|------|------|
| **测试内容** | bash subscribe 脚本通过 socket 接收事件流 |
| **测试方法** | 终端A: `bash /home/pilot/.projects/fsmon/extensions/examples/subscribe.sh`<br/>终端B: `touch /tmp/fsmon_ext_test/sub_sh_test` |
| **预期结果** | 终端A 先显示 `[subscribed] ok = true`，然后收到 touch 事件的 JSONL |

### 4.2.2 `subscribe.py`

| 项目 | 内容 |
|------|------|
| **测试内容** | Python subscribe 脚本独立订阅 |
| **测试方法** | 终端A: `python3 /home/pilot/.projects/fsmon/extensions/examples/subscribe.py \| jq '.'`<br/>终端B: `echo "py test" > /tmp/fsmon_ext_test/sub_py_test` |
| **预期结果** | 终端A 输出 `[subscribed] ok = true`，然后收到事件的 JSON |

### 4.2.3 subscribe 带 cmd 过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | subscribe 时按 track_cmd 过滤事件 |
| **测试方法** | `python3 -c "`<br/>`import socket, os;`<br/>`s = socket.socket(socket.AF_UNIX);`<br/>`s.connect('/tmp/fsmon-$(id -u).sock');`<br/>`s.sendall(b'cmd = \"subscribe\"\ntrack_cmd = \"touch\"\n\n');`<br/>`print(s.makefile().readline())" &`<br/>`PID=$!; sleep 0.5; touch /tmp/fsmon_ext_test/sub_cmd_test; sleep 0.5; kill $PID 2>/dev/null` |
| **预期结果** | 收到的事件 cmd 均为 "touch"（或其他触发进程） |

---

# 阶段五：边界 & 异常场景

> **可在任意阶段执行**

## 5.1 并发文件操作

| 项目 | 内容 |
|------|------|
| **测试内容** | 大量并发 touch 不丢事件 |
| **测试方法** | `for i in $(seq 1 20); do touch "/tmp/fsmon_ext_test/conc_$i" & done; wait; sleep 1`<br/>`fsmon query _global -p /tmp/fsmon_ext_test \| jq -r '.path' \| grep "conc_" \| sort -u \| wc -l` |
| **预期结果** | 输出 20（20 个唯一路径都有事件） |

## 5.2 监视路径本身被删除 (DELETE_SELF)

| 项目 | 内容 |
|------|------|
| **测试内容** | 监控目录被删除 → daemon 不崩溃 |
| **测试方法** | `mkdir /tmp/fsmon_self && fsmon add _global --path /tmp/fsmon_self -r`<br/>`rmdir /tmp/fsmon_self`<br/>`fsmon health` |
| **预期结果** | 捕获 DELETE_SELF 事件；health 正常返回 |

## 5.3 SIGHUP 重载

| 项目 | 内容 |
|------|------|
| **测试内容** | SIGHUP 信号触发重新加载 monitored.jsonl |
| **测试方法** | `sudo kill -HUP $(pgrep -f "fsmon daemon") && sleep 1`<br/>`grep reload_config /tmp/fsmon.log \| tail -3` |
| **预期结果** | daemon 日志出现 `[DEBUG] reload_config`，不崩溃 |

## 5.4 daemon 单例锁

| 项目 | 内容 |
|------|------|
| **测试内容** | 第二个 daemon 实例被拒绝 |
| **测试方法** | `sudo fsmon daemon --debug 2>&1 \| head -5` |
| **预期结果** | 输出 `Another fsmon daemon is already running`，立即退出 |

## 5.5 日志目录与监控路径冲突检测

| 项目 | 内容 |
|------|------|
| **测试内容** | 不能监控日志目录自身 |
| **测试方法** | 阶段二中: `fsmon add _global --path /tmp/fsmon_custom_logs -r` |
| **预期结果** | 报错 `Cannot monitor ... log directory` |

## 5.6 CLI 空参数错误处理

| 项目 | 内容 |
|------|------|
| **测试内容** | 必填参数缺失时给出明确错误 |
| **测试方法** | `fsmon query`（不带 CMD）<br/>`fsmon clean`（不带 CMD）<br/>`fsmon add --path /tmp/x`（不带 CMD） |
| **预期结果** | 均有错误提示说明 CMD is required 及如何使用 `_global` |

## 5.7 大日志文件查询

| 项目 | 内容 |
|------|------|
| **测试内容** | 大日志文件 query 行数正确 |
| **测试方法** | `wc -l ~/.local/state/fsmon/_global_log.jsonl`<br/>`fsmon query _global \| wc -l` |
| **预期结果** | 两者行数一致（已有日志源文件行数 == query 输出行数） |

---

# 阶段六：恢复默认配置并清理

```bash
# 恢复默认 fsmon.toml
cp ~/.config/fsmon/fsmon.toml ~/.config/fsmon/fsmon.toml.bak
# 重新用 fsmon init 生成(或手动恢复注释版本)

# 清理测试路径
fsmon remove _global --path /tmp/fsmon_dyn_add
fsmon remove _global --path /tmp/fsmon_pending
fsmon remove _global --path /tmp/fsmon_type_filter
fsmon remove _global --path /tmp/fsmon_size_filter
fsmon remove _global --path /tmp/fsmon_multi
fsmon remove _global --path /tmp/fsmon_self
fsmon remove myapp
fsmon remove testapp

# 删除测试目录
rm -rf /tmp/fsmon_{dyn_add,pending,type_filter,size_filter,multi,self,group_test}
rm -rf /tmp/fsmon_custom_logs /tmp/fsmon_test_custom.sock

# 重启 daemon 恢复默认
sudo pkill -f "fsmon daemon"
sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log &
```

---

# 执行顺序总览

```
阶段一（默认配置）─→ 阶段二（自定义日志+local_time+sync+disk）
  → 阶段三（禁用日志+自定义cache+socket）→ 阶段四（extensions）
  → 阶段五（边界场景）→ 阶段六（清理恢复）
```

每个阶段间的「重启 daemon」是必要操作，因为日志路径、cache 配置、socket 路径在 daemon 启动时一次性加载。
