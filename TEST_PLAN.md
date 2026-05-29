# fsmon 详细测试方案

> **测试环境**: daemon 已在后台运行: `sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log`
> **监控目标**: `_global` 组: `/home/pilot/.config/what` + `/tmp/fsmon_ext_test`（均递归）
> **socket**: `/tmp/fsmon-1000.sock`

---

## 测试执行说明

每个测试用例包含：
- **测试内容**: 测试什么能力
- **测试方法**: 具体操作步骤和命令
- **预期结果**: 从系统输出、日志(`/tmp/fsmon.log`)和日志文件(`~/.local/state/fsmon/_global_log.jsonl`)三方面验证

**每次测试后需检查三点**：
1. 命令输出是否正确
2. `/tmp/fsmon.log` (daemon debug 输出) 有无异常
3. 生成的 JSONL 日志事件内容是否正确

---

## 一、CLI 命令测试（不需要 daemon 状态变更）

### 1.1 `fsmon monitored` — 列出已监控路径

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 `monitored` 命令能正确列出当前所有监控路径及配置 |
| **测试方法** | `fsmon monitored` |
| **预期结果** | 输出一行 JSON，包含 `cmd: "_global"` 和 `paths` 字段，其中 paths 包含 `/home/pilot/.config/what`(recursive:true) 和 `/tmp/fsmon_ext_test`(recursive:true) |

### 1.2 `fsmon health` — daemon 健康状态

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 `health` 命令通过 socket 查询 daemon 运行状态 |
| **测试方法** | `fsmon health` |
| **预期结果** | 输出 TOML 格式：`ok = true`, `uptime_secs` > 0, `monitored_paths` = 2, `reader_groups` >= 1, `readers[0].alive = true` |

### 1.3 `fsmon query _global` — 查询历史事件

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 query 命令能读取 JSONL 日志并输出所有事件 |
| **测试方法** | `fsmon query _global` |
| **预期结果** | 输出所有历史事件的 JSONL（每行一个 JSON），至少包含已有的旧事件。事件字段完整：time, event_type, path, pid, cmd, user, file_size, ppid, tgid, chain |

### 1.4 `fsmon query _global -t '>1h'` — 时间过滤查询

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证时间过滤器只返回最近1小时的事件 |
| **测试方法** | `fsmon query _global -t '>1h'` |
| **预期结果** | 仅输出最近1小时内的事件（如果刚触发过文件变化则能看到，否则输出空） |

### 1.5 `fsmon query _global -p /tmp/fsmon_ext_test` — 路径过滤查询

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证路径前缀过滤只返回匹配路径的事件 |
| **测试方法** | `fsmon query _global -p /tmp/fsmon_ext_test` |
| **预期结果** | 仅输出 path 以 `/tmp/fsmon_ext_test` 开头的事件，不包含 `/home/pilot/.config/what` 下的事件 |

### 1.6 `fsmon changes _global` — 去重变化列表

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 `changes` 命令按路径去重，只保留每个路径的最新事件，按时间倒序输出 |
| **测试方法** | `fsmon changes _global` |
| **预期结果** | 每个唯一路径只出现一条事件（最新那条），按时间从新到旧排列 |

### 1.7 `fsmon clean _global --dry-run` — 干运行清理

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 dry-run 模式不修改文件，仅列出将被删除的条目 |
| **测试方法** | `fsmon clean _global --dry-run` |
| **预期结果** | 输出 `[to-delete]` 条目列表（如果有超过30天的事件），底部显示 `Dry run: N entries would be deleted`。检查 `~/.local/state/fsmon/_global_log.jsonl` 内容未被修改 |

### 1.8 `fsmon cd -l` — 进入日志目录

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 `cd` 命令能打开子 shell 进入日志目录 |
| **测试方法** | `echo "pwd && exit" | fsmon cd -l` 或直接在交互 shell 中 `fsmon cd -l` 然后 `ls` 再 `exit` |
| **预期结果** | 进入 `~/.local/state/fsmon` 目录，能看到 `_global_log.jsonl` 文件 |

### 1.9 `fsmon cd -m` — 进入 monitored 存储目录

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 cd -m 进入 monitored.jsonl 所在目录 |
| **测试方法** | `fsmon cd -m` 然后 `ls` 再 `exit` |
| **预期结果** | 进入 `~/.local/share/fsmon` 目录，能看到 `monitored.jsonl` 文件 |

---

## 二、事件捕获核心测试（触发真实文件变化）

### 2.1 文件创建事件 (CREATE + CLOSE_WRITE)

| 项目 | 内容 |
|------|------|
| **测试内容** | 创建新文件时能捕获 CREATE 和 CLOSE_WRITE 事件，且包含正确的进程信息 |
| **测试方法** | `touch /tmp/fsmon_ext_test/create_test` |
| **预期结果** | `<br/> **daemon日志**: 看到 `[DEBUG]` 输出有关 event building<br/> **JSONL日志**: `tail -5 ~/.local/state/fsmon/_global_log.jsonl` 能看到 `CREATE` 事件 (path=/tmp/fsmon_ext_test/create_test) 和 `CLOSE_WRITE` 事件<br/> **关键字段验证**: `cmd="touch"`, `user="pilot"`, `pid` 和 `ppid` 为正确进程 ID, `file_size=0` |

### 2.2 文件修改事件 (MODIFY + CLOSE_WRITE)

| 项目 | 内容 |
|------|------|
| **测试内容** | 向已有文件写入内容时能捕获 MODIFY 和 CLOSE_WRITE 事件 |
| **测试方法** | `echo "hello fsmon" > /tmp/fsmon_ext_test/modify_test` |
| **预期结果** | JSONL 中出现 `MODIFY` 事件（如果写入) 和 `CLOSE_WRITE` 事件<br/> `file_size` 应该是 12（"hello fsmon\n"的长度）<br/> `cmd` 应为 `bash` 或 shell 进程名 |

### 2.3 文件删除事件 (DELETE)

| 项目 | 内容 |
|------|------|
| **测试内容** | 删除文件时能捕获 DELETE 事件 |
| **测试方法** | `rm /tmp/fsmon_ext_test/modify_test` |
| **预期结果** | JSONL 中出现 `DELETE` 事件, path=`/tmp/fsmon_ext_test/modify_test`, cmd=`rm` |

### 2.4 `rm -rf` 完整删除捕获

| 项目 | 内容 |
|------|------|
| **测试内容** | 递归删除目录时能捕获所有被删除文件的 DELETE 事件（依赖目录句柄缓存） |
| **测试方法** | `mkdir -p /tmp/fsmon_ext_test/rmrf_test/sub && touch /tmp/fsmon_ext_test/rmrf_test/a /tmp/fsmon_ext_test/rmrf_test/sub/b && rm -rf /tmp/fsmon_ext_test/rmrf_test` |
| **预期结果** | JSONL 中能看到所有被删除文件的 DELETE 事件（a, b）。每个文件都有一条 DELETE 记录 |

### 2.5 递归子目录事件

| 项目 | 内容 |
|------|------|
| **测试内容** | 在监控路径的深层子目录中创建文件，验证递归监控有效 |
| **测试方法** | `mkdir -p /tmp/fsmon_ext_test/deep/nested/dir && touch /tmp/fsmon_ext_test/deep/nested/dir/deep_file` |
| **预期结果** | JSONL 中出现深度路径的 CREATE 事件：`/tmp/fsmon_ext_test/deep/nested/dir/deep_file` |

### 2.6 目录创建事件

| 项目 | 内容 |
|------|------|
| **测试内容** | 在监控路径下创建新子目录，验证目录创建被捕获，且新目录自动被纳入监控 |
| **测试方法** | `mkdir /tmp/fsmon_ext_test/new_dir` |
| **预期结果** | JSONL 中出现 path=`/tmp/fsmon_ext_test/new_dir` 的 CREATE 事件<br/> **daemon日志** 可能显示 `[DEBUG]` 有关新目录被标记为递归监控 |

### 2.7 属性变更事件 (ATTRIB)

| 项目 | 内容 |
|------|------|
| **测试内容** | 修改文件属性时能捕获 ATTRIB 事件 |
| **测试方法** | `chmod 644 /tmp/fsmon_ext_test/test.txt` |
| **预期结果** | JSONL 中出现 `ATTRIB` 事件, path=`/tmp/fsmon_ext_test/test.txt`, cmd=`chmod` |

### 2.8 移动/重命名事件 (MOVED_FROM / MOVED_TO)

| 项目 | 内容 |
|------|------|
| **测试内容** | 在同一文件系统内移动/重命名文件时捕获 MOVED_FROM 和 MOVED_TO |
| **测试方法** | `mv /tmp/fsmon_ext_test/create_test /tmp/fsmon_ext_test/renamed_test` |
| **预期结果** | JSONL 中出现 `MOVED_FROM` (path=旧名) 和 `MOVED_TO` (path=新名) 事件 |

---

## 三、进程追踪测试

### 3.1 基本进程归属

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证每个事件正确记录了触发进程的 pid, cmd, user, ppid, tgid |
| **测试方法** | `touch /tmp/fsmon_ext_test/proc_test` 然后在 JSONL 中检查 |
| **预期结果** | 事件中 `pid` = touch 进程的 PID, `cmd="touch"`, `user="pilot"`, `ppid` 为父进程 PID(shell), `tgid` 与 pid 一致 |

### 3.2 daemon 自身事件过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | fsmon daemon 自身产生的文件事件不会被记录 |
| **测试方法** | daemon 运行时写日志到 `~/.local/state/fsmon/_global_log.jsonl`，检查这些写操作是否产生了事件 |
| **预期结果** | 日志目录下的写入事件**不应该**出现在监控日志中（daemon PID 被过滤）。验证：`fsmon query _global | jq 'select(.path | startswith("/home/pilot/.local/state/fsmon"))'` 应为空 |

---

## 四、动态路径管理（live add/remove，不重启 daemon）

### 4.1 `fsmon add` — 实时添加路径

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 运行时通过 socket 实时添加新的监控路径 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_live_add -r` |
| **预期结果** | 输出 `Entry added: /tmp/fsmon_live_add`<br/> **daemon日志**: 显示 `[DEBUG] socket command: add` 和 fanotify mark 相关信息<br/> **验证**: `mkdir -p /tmp/fsmon_live_add && touch /tmp/fsmon_live_add/live_file && sleep 1 && fsmon query _global | jq 'select(.path | startswith("/tmp/fsmon_live_add"))'` 能看到事件 |

### 4.2 `fsmon add` — 添加不存在的路径（pending）

| 项目 | 内容 |
|------|------|
| **测试内容** | 添加还不存在的路径 → daemon 等待目录创建后自动开始监控 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_pending_test -r`（路径不存在）<br/> 然后 `mkdir /tmp/fsmon_pending_test && touch /tmp/fsmon_pending_test/after_create` |
| **预期结果** | add 时输出 `[Note] path does not exist yet...`<br/> **daemon日志**: 看到 inotify watch 被添加到 `/tmp`<br/> 创建目录后，touch 的事件能被捕获 |

### 4.3 `fsmon add` — 带类型过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | 添加路径时指定事件类型过滤，只捕获指定类型的事件 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_type_test -r --types CREATE --types DELETE`<br/> `mkdir -p /tmp/fsmon_type_test && touch /tmp/fsmon_type_test/f1 && echo x > /tmp/fsmon_type_test/f1 && rm /tmp/fsmon_type_test/f1` |
| **预期结果** | 只能看到 CREATE 和 DELETE 事件，看不到 MODIFY 或 CLOSE_WRITE<br/> **验证**: `fsmon query _global -p /tmp/fsmon_type_test | jq '.event_type'` 只输出 CREATE 和 DELETE |

### 4.4 `fsmon add` — 带大小过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | 只输出文件大小满足过滤条件的事件 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_size_test -r -s '>0'`<br/> `mkdir -p /tmp/fsmon_size_test && touch /tmp/fsmon_size_test/empty && echo "data" > /tmp/fsmon_size_test/data` |
| **预期结果** | empty 文件的 CREATE/CLOSE_WRITE 事件（file_size=0）不会被输出；data 文件（file_size>0）的事件被输出 |

### 4.5 `fsmon remove` — 实时移除路径

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 运行时通过 socket 实时移除监控路径 |
| **测试方法** | `fsmon remove _global --path /tmp/fsmon_live_add` |
| **预期结果** | 输出 `Entry removed: /tmp/fsmon_live_add`<br/> **验证**: `touch /tmp/fsmon_live_add/after_remove`，检查是否不再产生事件（`fsmon query _global -p /tmp/fsmon_live_add` 没有新事件） |

### 4.6 `fsmon remove` — 移除整个 cmd 组

| 项目 | 内容 |
|------|------|
| **测试内容** | 不带 --path 时移除整个 cmd 组的所有路径 |
| **测试方法** | `fsmon add testapp --path /tmp/fsmon_group_test -r` 然后 `fsmon remove testapp` |
| **预期结果** | 输出 `Entry removed: [testapp]`<br/> `fsmon monitored` 不再包含 testapp 组 |

### 4.7 `fsmon add` — 重复添加覆盖

| 项目 | 内容 |
|------|------|
| **测试内容** | 添加已存在的 (path, cmd) 对时，新参数替换旧参数 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_size_test -r -s '=0'`（覆盖之前的 `>0` 过滤）<br/> `touch /tmp/fsmon_size_test/another_empty` |
| **预期结果** | 添加时输出 `[Note] ... is already monitored — new parameters will replace...`<br/> file_size=0 的事件现在能被看到（因为过滤改为 =0） |

---

## 五、Subscription 实时流测试

### 5.1 基本 subscribe

| 项目 | 内容 |
|------|------|
| **测试内容** | 通过 Unix socket subscribe 实时接收事件流 |
| **测试方法** | 终端 A: `echo 'cmd = "subscribe"' | socat - UNIX-CONNECT:/tmp/fsmon-1000.sock`<br/> 终端 B: `touch /tmp/fsmon_ext_test/sub_test` |
| **预期结果** | socat 首先收到 TOML 响应 `ok = true`，随后流式输出 JSONL 事件（包括 touch 产生的事件） |

### 5.2 subscribe 带 cmd 过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | subscribe 时过滤只接收特定 cmd 组的事件 |
| **测试方法** | 使用 subscribe.py 或手动构造 TOML：<br/> `echo -e 'cmd = "subscribe"\ntrack_cmd = "touch"\n\n' | socat - UNIX-CONNECT:/tmp/fsmon-1000.sock` |
| **预期结果** | 只收到 cmd="touch" 的事件 |

### 5.3 subscribe 带类型过滤

| 项目 | 内容 |
|------|------|
| **测试内容** | subscribe 时过滤只接收特定事件类型 |
| **测试方法** | 使用 subscribe.py 并传入 types 过滤，或手动构造 TOML<br/> `echo -e 'cmd = "subscribe"\ntypes = ["CREATE", "DELETE"]\n\n' | socat - UNIX-CONNECT:/tmp/fsmon-1000.sock` |
| **预期结果** | 只收到 CREATE 和 DELETE 事件，不会收到 MODIFY/ATTRIB 等 |

### 5.4 subscribe 连接断开行为

| 项目 | 内容 |
|------|------|
| **测试内容** | 客户端断开后 daemon 正常运行，其他操作不受影响 |
| **测试方法** | 建立 subscribe 连接后 Ctrl+C 断开，然后 `fsmon health` 检查 |
| **预期结果** | daemon 正常响应 health 命令，没有 crash |

---

## 六、Extensions 扩展测试

### 6.1 `read-jsonl.sh` — JSONL 日志读取

| 项目 | 内容 |
|------|------|
| **测试内容** | extensions 中的 bash 示例脚本能正确读取日志文件 |
| **测试方法** | `bash /home/pilot/.projects/fsmon/extensions/examples/read-jsonl.sh` |
| **预期结果** | 输出 `=== recent 5 events ===` 以及最近的5条事件摘要，显示 time / event_type / path |

### 6.2 `read-jsonl.py` — JSONL 日志读取 (Python)

| 项目 | 内容 |
|------|------|
| **测试内容** | extensions 中的 Python 示例脚本能正确读取日志文件 |
| **测试方法** | `python3 /home/pilot/.projects/fsmon/extensions/examples/read-jsonl.py` |
| **预期结果** | 输出 `=== last 5 events ===` 以及最近的5条事件摘要，显示 time / event_type / cmd / path |

### 6.3 `subscribe.sh` — Socket 订阅 (bash)

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 extensions 中的 subscribe.sh 脚本能通过 socket 订阅事件流 |
| **测试方法** | 终端 A: `bash /home/pilot/.projects/fsmon/extensions/examples/subscribe.sh`<br/> 终端 B: `touch /tmp/fsmon_ext_test/sub_ext_test` |
| **预期结果** | socat 模式：看到 `[subscribed]` 和 TOML ok 响应，然后 JSONL 事件流<br/> Python fallback 模式：同样行为 |

### 6.4 `subscribe.py` — Socket 订阅 (Python)

| 项目 | 内容 |
|------|------|
| **测试内容** | 验证 extensions 中的 subscribe.py 脚本能独立订阅事件流 |
| **测试方法** | 终端 A: `python3 /home/pilot/.projects/fsmon/extensions/examples/subscribe.py | jq '.'`<br/> 终端 B: `touch /tmp/fsmon_ext_test/sub_py_test` |
| **预期结果** | 输出 `[subscribed] ok = true`，然后 touch 产生的事件以 JSON 形式输出 |

---

## 七、daemon 日志验证

### 7.1 启动日志完整性

| 项目 | 内容 |
|------|------|
| **测试内容** | daemon 启动时的 debug 日志是否完整 |
| **测试方法** | `cat /tmp/fsmon.log` |
| **预期结果** | 包含：Config loaded（3 个路径）、cache configuration（所有默认值）、Monitor initialized（2 个 path entries）、combined fanotify mask（0x48000fcc）、Active paths 列表、cache stats（初始状态） |

### 7.2 事件处理日志

| 项目 | 内容 |
|------|------|
| **测试内容** | 触发事件后 debug 日志是否有相关输出 |
| **测试方法** | `touch /tmp/fsmon_ext_test/debug_log_test && sleep 1 && tail -30 /tmp/fsmon.log` |
| **预期结果** | 看到 event building 或 event routing 相关 debug 信息（取决于具体实现），不应有 ERROR/WARNING |

### 7.3 cache stats 周期性输出

| 项目 | 内容 |
|------|------|
| **测试内容** | 每 60 秒输出一次 cache stats（debug 模式下） |
| **测试方法** | 等待至少 60 秒，然后检查 `/tmp/fsmon.log` |
| **预期结果** | 每隔约 60 秒出现 `[DEBUG] --- cache stats ---` 段，显示 dir_cache, proc_cache, pid_tree, file_size_cache 的条目数 |

### 7.4 inotify pending 路径就绪日志

| 项目 | 内容 |
|------|------|
| **测试内容** | pending 路径的目录被创建时，daemon 日志应有记录 |
| **测试方法** | （如果 `/tmp/fsmon_ext_test` 之前是 pending 状态，创建后应有日志）<br/> 查看 `/tmp/fsmon.log` 中是否有 inotify 相关的日志 |
| **预期结果** | 看到 `[DEBUG] inotify fd became readable` 和路径被转移到 Active paths 的相关日志 |

---

## 八、边界场景测试

### 8.1 监控不存在的路径（pending → active 转换）

| 项目 | 内容 |
|------|------|
| **测试内容** | 添加不存在路径后，创建该目录，验证自动开始监控 |
| **测试方法** | `fsmon add _global --path /tmp/fsmon_nonexist_test -r`（add 时目录不存在）<br/> `mkdir /tmp/fsmon_nonexist_test && touch /tmp/fsmon_nonexist_test/hello` |
| **预期结果** | 创建目录后，hello 文件的 CREATE 事件被捕获 |

### 8.2 超大 JSONL 文件查询

| 项目 | 内容 |
|------|------|
| **测试内容** | 日志文件较大时（175KB），query 能正常工作 |
| **测试方法** | `wc -c ~/.local/state/fsmon/_global_log.jsonl && fsmon query _global | wc -l` |
| **预期结果** | query 返回的行数与 `grep -c . ~/.local/state/fsmon/_global_log.jsonl`（不包含空行）一致 |

### 8.3 重复添加同一路径不同 cmd

| 项目 | 内容 |
|------|------|
| **测试内容** | 同一路径可被多个 cmd 组监控，各自独立配置 |
| **测试方法** | `fsmon add myapp --path /tmp/fsmon_ext_test -r --types CREATE`<br/> `touch /tmp/fsmon_ext_test/multi_cmd_test` |
| **预期结果** | 全局组（无过滤）能看到所有事件；myapp 组的事件中 chain 字段应包含 myapp 进程信息（如果由 myapp 进程触发） |

### 8.4 并发文件操作

| 项目 | 内容 |
|------|------|
| **测试内容** | 同时多个进程操作文件时，事件不丢失、不错乱 |
| **测试方法** | `for i in $(seq 1 20); do touch "/tmp/fsmon_ext_test/concurrent_$i" & done; wait` |
| **预期结果** | JSONL 中能数出 20 个 `CREATE` 事件（每个 concurrent_* 文件一个），PID 各不相同 |

### 8.5 删除监控路径本身

| 项目 | 内容 |
|------|------|
| **测试内容** | 删除正在被监控的目录，验证 DELETE_SELF 事件的捕获和 daemon 稳定性 |
| **测试方法** | `mkdir /tmp/fsmon_self_test && fsmon add _global --path /tmp/fsmon_self_test -r`<br/> `rmdir /tmp/fsmon_self_test` |
| **预期结果** | 捕获 DELETE_SELF 事件，daemon 不崩溃 |

### 8.6 SIGHUP 重载配置

| 项目 | 内容 |
|------|------|
| **测试内容** | 发送 SIGHUP 信号后 daemon 重新加载 monitored.jsonl 配置 |
| **测试方法** | `sudo kill -HUP $(pgrep -f "fsmon daemon")` |
| **预期结果** | daemon 日志显示 `[DEBUG] reload_config`，不崩溃。监控路径与最新的 monitored.jsonl 一致 |

### 8.7 daemon 单例锁

| 项目 | 内容 |
|------|------|
| **测试内容** | 尝试启动第二个 daemon 实例时提示已运行 |
| **测试方法** | `sudo fsmon daemon --debug 2>&1 | head -5` |
| **预期结果** | 输出错误信息 `Another fsmon daemon is already running`，退出 |

---

## 九、测试清理

所有测试结束后执行以下清理：

```bash
# 移除测试添加的路径
fsmon remove _global --path /tmp/fsmon_type_test
fsmon remove _global --path /tmp/fsmon_size_test
fsmon remove _global --path /tmp/fsmon_live_add
fsmon remove _global --path /tmp/fsmon_pending_test
fsmon remove _global --path /tmp/fsmon_nonexist_test
fsmon remove _global --path /tmp/fsmon_self_test
fsmon remove myapp
fsmon remove testapp

# 删除测试目录
rm -rf /tmp/fsmon_ext_test
rm -rf /tmp/fsmon_live_add /tmp/fsmon_pending_test /tmp/fsmon_nonexist_test
rm -rf /tmp/fsmon_type_test /tmp/fsmon_size_test /tmp/fsmon_group_test
rm -rf /tmp/fsmon_self_test

# 确认监控路径恢复初始状态
fsmon monitored
```

---

## 执行顺序建议

```
一（CLI 基础） → 二（事件捕获） → 三（进程追踪）
→ 四（动态路径管理） → 八（边界） → 五（订阅流）
→ 六（extensions） → 七（日志验证） → 九（清理）
```

**注意**: 测试四（动态路径管理）和测试八（边界场景）会修改 daemon 的监控配置，建议在基础功能验证完成后执行。
