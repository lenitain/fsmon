# fsmon 手动测试手册

所有需要 `sudo` 的测试项都标记了 ⚡。测试前请确保已构建最新版本：

```bash
cargo build --release
alias fsmon=sudo\ /home/pilot/.projects/fsmon/target/release/fsmon
```

---

## 1. 配置生成（无需 root）

### 1.1 首次生成

```bash
# 确保没有已有配置
mv ~/.config/fsmon/config.toml ~/.config/fsmon/config.toml.bak 2>/dev/null
fsmon generate
```

预期：输出 `Default config generated at ~/.config/fsmon/config.toml`

### 1.2 再次生成（不传 --force）

```bash
fsmon generate
```

预期：报错 `Config already exists at ... Use --force to overwrite`

### 1.3 强制覆盖

```bash
fsmon generate --force
```

预期：成功覆盖，无错误

**恢复备份**：
```bash
mv ~/.config/fsmon/config.toml.bak ~/.config/fsmon/config.toml 2>/dev/null
```

---

## 2. Daemon 启动与基础行为（⚡ 需 sudo）

### 2.1 无配置路径启动

```bash
fsmon daemon
```

预期：输出 `No paths configured. Waiting for socket commands...`，daemon 在前台运行

按 `Ctrl+C` 退出。

### 2.2 预先配置路径后启动

```bash
# 先通过 CLI 添加路径（不启动 daemon）
fsmon add /tmp -r
# 再启动 daemon
fsmon daemon
```

预期：启动时显示 `Monitoring /tmp (inode mark) on new fd N`，然后进入等待循环

按 `Ctrl+C` 退出。

---

## 3. `fsmon add` 添加路径（⚡ 需 daemon 运行）

### 3.1 启动 daemon

```bash
# 先清空 managed 文件
echo '[]' | sudo tee "$(fsmon managed 2>/dev/null; echo /dev/null)" 2>/dev/null
fsmon daemon &
sleep 1
```

### 3.2 添加存在的路径

```bash
fsmon add /tmp
```

预期：
- Daemon 端输出 `Monitoring /tmp (inode mark) on new fd N`
- CLI 输出 `Path added: /tmp`

### 3.3 递归添加目录

```bash
fsmon add ~/.config -r
```

预期：
- Daemon 端输出 `Monitoring /home/pilot/.config (inode mark) on new fd N`
- 耗时取决于 `~/.config` 子目录数量（1-3 秒正常）
- CLI 输出 `Path added: /home/pilot/.config`

### 3.4 重复添加同一路径

```bash
fsmon add /tmp
```

预期：
- CLI 输出 `Note: '/tmp' is already monitored — new parameters will replace...`
- CLI 输出 `Path added: /tmp`
- Daemon 端输出 `Removed path: /tmp` + `Monitoring /tmp (inode mark) on new fd N`
- 只会有**一个** reader 任务处理 `/tmp`，无重复事件

### 3.5 添加不存在的路径

```bash
fsmon add /tmp/fsmon_test_nonexistent
```

预期：CLI 输出 `Path does not exist yet — will start monitoring when created.`

### 3.6 添加路径后创建该路径

```bash
# 在上一步之后，创建该目录
mkdir -p /tmp/fsmon_test_nonexistent
```

预期：Daemon 在几秒内自动开始监控新创建的目录

### 3.7 嵌套路径冲突检测

```bash
# 在已监控 /tmp 的情况下，添加 /tmp/subdir
fsmon add /tmp/subdir
```

预期：CLI 输出 `Note: '/tmp/subdir' is under recursively monitored path '/tmp'`

### 3.8 添加带排除模式的路径

```bash
fsmon add ~/Documents -r --exclude '\.tmp$'
```

预期：添加成功

### 3.9 添加带类型过滤的路径

```bash
fsmon add /var/log --types CREATE --types MODIFY
```

预期：添加成功

### 3.10 添加路径到另一个文件系统

```bash
# 如果有其他 mount 点，比如 /mnt
fsmon add /mnt
```

预期：输出 `Monitoring /mnt (fs mark) on new fd N`（文件系统级标记）或回退到 inode 标记

### 3.11 添加日志目录自身（应被拒绝）

```bash
fsmon add ~/.local/state/fsmon
```

预期：报错 `Cannot monitor ... log directory ... is inside this path`

---

## 4. 事件产生与日志写入（⚡ 需 daemon 运行）

### 4.1 先清空事件日志

```bash
sudo rm -rf ~/.local/state/fsmon/logs/*
```

### 4.2 启动 daemon + 添加路径

```bash
fsmon daemon &
sleep 1
fsmon add /tmp/fsmon_test -r
mkdir -p /tmp/fsmon_test
```

### 4.3 文件创建（CREATE）

```bash
touch /tmp/fsmon_test/test.txt
```

预期：3 秒内，日志文件出现 `CREATE` 事件

### 4.4 文件修改（MODIFY）

```bash
echo "hello" > /tmp/fsmon_test/test.txt
```

预期：日志出现 `MODIFY` 事件

### 4.5 文件删除（DELETE）

```bash
rm /tmp/fsmon_test/test.txt
```

预期：日志出现 `DELETE` 事件

### 4.6 文件重命名（MOVED_FROM + MOVED_TO）

```bash
touch /tmp/fsmon_test/a.txt
mv /tmp/fsmon_test/a.txt /tmp/fsmon_test/b.txt
```

预期：日志出现 `MOVED_FROM` + `MOVED_TO` 两条事件

### 4.7 子目录递归事件

```bash
mkdir /tmp/fsmon_test/subdir
touch /tmp/fsmon_test/subdir/foo.txt
```

预期：日志出现 `CREATE`（子目录）+ `CREATE`（子目录内文件）

### 4.8 目录删除

```bash
rm -rf /tmp/fsmon_test/subdir
```

预期：日志出现 `DELETE_SELF` 或 `DELETE`

### 4.9 属性变更（ATTRIB）

```bash
touch /tmp/fsmon_test/perm.txt
chmod 644 /tmp/fsmon_test/perm.txt
```

预期：日志出现 `ATTRIB` 事件

### 4.10 大量事件产生（队列压力测试）

```bash
for i in $(seq 1 1000); do touch /tmp/fsmon_test/$i.txt; done
```

预期：所有事件正确记录，无 `FAN_Q_OVERFLOW` 警告

### 4.11 检查日志文件内容

```bash
fsmon query --path /tmp/fsmon_test
```

预期：显示所有记录的事件，格式正确

---

## 5. 路径移除（⚡ 需 daemon 运行）

### 5.1 移除已监控路径

```bash
fsmon remove /tmp/fsmon_test
```

预期：
- CLI 输出 `Path removed: /tmp/fsmon_test`
- Daemon 端输出 `Removed path: /tmp/fsmon_test`
- 之后对 `/tmp/fsmon_test` 的文件操作不再产生日志

### 5.2 移除不存在的路径

```bash
fsmon remove /nonexistent
```

预期：报错 `Path not being monitored`

### 5.3 移除后重新添加

```bash
fsmon remove /tmp
fsmon add /tmp
```

预期：`remove` 和 `add` 都成功，重新添加后事件正常记录

---

## 6. managed 持久化（⚡ 需 sudo）

### 6.1 查看 managed 文件

```bash
fsmon managed
```

预期：列出所有已添加的路径及其选项

### 6.2 停止 daemon，重启，验证持久化

```bash
# 在 daemon 运行时添加路径
fsmon add /tmp -r
# 停止 daemon（Ctrl+C）
# 重新启动
fsmon daemon
```

预期：启动时显示之前添加的路径重新被监控

### 6.3 managed.jsonl 文件格式

```bash
cat ~/.local/share/fsmon/managed.jsonl
```

预期：每行一个 JSON 对象，包含 `path`、`recursive` 等字段

---

## 7. 查询历史事件

### 7.1 查询所有事件

```bash
fsmon query
```

预期：显示所有路径的所有事件

### 7.2 按路径查询

```bash
fsmon query --path /tmp/fsmon_test
```

预期：只显示该路径的事件

### 7.3 按时间查询

```bash
# 查询最近 1 小时的事件
fsmon query --since "1h"

# 查询某个时间点之后的事件
fsmon query --since "2025-01-01 00:00:00"
```

预期：正确过滤

### 7.4 多路径查询

```bash
fsmon query --path /tmp --path /home/pilot/.config
```

预期：显示两个路径的事件

---

## 8. 日志清理

### 8.1 干跑清理

```bash
fsmon clean --dry-run
```

预期：显示会删除哪些日志文件，但不实际删除

### 8.2 按天数清理

```bash
fsmon clean --keep-days 30
```

预期：删除 30 天前的日志

### 8.3 按大小清理

```bash
fsmon clean --max-size 100M
```

预期：删除最旧的日志直到总大小小于 100M

### 8.4 清理特定路径的日志

```bash
fsmon clean --path /tmp
```

预期：只清理该路径的日志

---

## 9. 选项过滤功能（⚡ 需 daemon 运行）

### 9.1 按事件类型过滤

```bash
fsmon add /tmp/fsmon_filter_test --types CREATE
touch /tmp/fsmon_filter_test/created.txt        # → 应记录
echo "hi" > /tmp/fsmon_filter_test/created.txt  # → MODIFY，不应记录
```

预期：日志只包含 `CREATE` 事件

### 9.2 按最小文件大小过滤

```bash
fsmon add /tmp/fsmon_size_test -s '>=1K'
touch /tmp/fsmon_size_test/small.txt            # 0 字节 → 不应记录
echo "0123456789" > /tmp/fsmon_size_test/big.txt # >1K → 应记录
```

预期：只记录 `big.txt` 的事件

### 9.3 路径排除模式

```bash
fsmon add /tmp/fsmon_exclude_test -r --exclude '\.log$'
touch /tmp/fsmon_exclude_test/a.log             # → 不应记录
touch /tmp/fsmon_exclude_test/a.txt             # → 应记录
```

预期：只记录 `a.txt` 的事件

### 9.4 进程名排除

```bash
fsmon add /tmp/fsmon_cmd_test --exclude-cmd 'cat'
touch /tmp/fsmon_cmd_test/test.txt              # 由 shell(bash) 创建 → 应记录
cat /tmp/fsmon_cmd_test/test.txt                # cat 进程 → 不应记录
```

预期：cat 的 ACCESS 事件被过滤

### 9.5 组合过滤

```bash
fsmon add /tmp/fsmon_combo_test --types CREATE --types MODIFY -s '>=100' --exclude '\.tmp$'
```

预期：只有 CREATE/MODIFY 事件、文件 ≥100 字节、不是 .tmp 后缀的才被记录

---

## 10. Socket 通信容错

### 10.1 daemon 未运行时执行命令

```bash
# 确保 daemon 未运行
killall fsmon 2>/dev/null
sleep 1
fsmon add /tmp
```

预期：报错 `Failed to connect to fsmon daemon`，但路径已写入 managed 文件（下次 daemon 启动时会自动加载）

### 10.2 daemon 运行中执行命令（已覆盖）

参考 3.2 等测试项。

### 10.3 并发发送多个命令

```bash
fsmon add /tmp/a &
fsmon add /tmp/b &
fsmon add /tmp/c &
wait
```

预期：三个路径都成功添加，无死锁

### 10.4 无效命令

```bash
echo 'cmd = "invalid"' | nc -U /tmp/fsmon-1000.sock
```

预期：返回 `{"ok": false, "error": "Unknown command: invalid"}`

---

## 11. 安全与权限（⚡ 需 sudo）

### 11.1 socket 权限

```bash
ls -la /tmp/fsmon-*.sock
```

预期：权限为 `srw-rw-rw-`（0666），普通用户可以写入

### 11.2 日志文件权限

```bash
ls -la ~/.local/state/fsmon/logs/
```

预期：日志文件的 owner 为原始用户（非 root）

### 11.3 managed 文件权限

```bash
ls -la ~/.local/share/fsmon/managed.jsonl
```

预期：文件 owner 为原始用户

---

## 12. 稳定性测试（⚡ 需 sudo）

### 12.1 长时间运行

```bash
# 启动 daemon，添加多个路径
fsmon daemon &
fsmon add /tmp
fsmon add ~/.config -r
# 让 daemon 运行 1 小时，同时在 /tmp 和 ~/.config 下做一些文件操作
# 1 小时后检查：
# 1. daemon 未崩溃
# 2. 查询事件正常
# 3. 添加/移除路径仍可操作
```

### 12.2 反复添加移除

```bash
for i in $(seq 1 10); do
    fsmon add /tmp/test_cycle
    fsmon remove /tmp/test_cycle
done
```

预期：10 次循环都成功，daemon 无异常，`fan_fds` 无泄漏式增长导致变慢

### 12.3 daemon 重启

```bash
# daemon 运行中
killall fsmon  # SIGTERM
sleep 2
fsmon daemon
```

预期：daemon 干净退出，重启后正常加载之前添加的路径

---

## 13. 清除测试数据

```bash
sudo rm -rf /tmp/fsmon_test /tmp/fsmon_filter_test /tmp/fsmon_size_test \
            /tmp/fsmon_exclude_test /tmp/fsmon_cmd_test /tmp/fsmon_combo_test \
            /tmp/test_cycle /tmp/a /tmp/b /tmp/c /tmp/fsmon_test_nonexistent
killall fsmon 2>/dev/null
```
