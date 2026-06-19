# fsmon 需要 sudo 权限的测试计划

本文档详细列出所有需要 root 权限的测试项，包括操作步骤和预期结果。
测试环境：用户手动运行 `sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log` 后，ai agent 逐项测试。

## 测试前准备

1. 确保 fsmon 已编译并安装到系统路径：`cargo install --path .`
2. 准备测试目录：`mkdir -p /tmp/fsmon_test`
3. 清理旧日志：`rm -rf ~/.local/state/fsmon/*`
4. 清理旧监控数据：`rm -f ~/.local/state/fsmon/monitored.jsonl`
5. 检查当前记录的进程和路径：`fsmon monitored`
6. 安装必要工具：`jq`、`socat`

### 套接字路径说明

- **命令套接字**：`/run/user/<UID>/fsmon/daemon.sock`（基于 `$XDG_RUNTIME_DIR`）
- **单例锁套接字**：`/run/user/<UID>/fsmon/lock.sock`
- 获取当前 UID：`UID=$(id -u)`

---

## 一、守护进程基本功能

### 1.1 守护进程启动与停止

**测试步骤：**
```bash
# 1. 启动守护进程（后台运行）
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 检查进程是否存在
ps -p $DAEMON_PID

# 3. 停止守护进程
sudo kill $DAEMON_PID
sleep 1

# 4. 检查进程是否已停止
ps -p $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程存在，状态为 R（运行中）或 S（睡眠中）
- 步骤 4：进程不存在，ps 命令返回非零退出码

### 1.2 守护进程单例锁

**测试步骤：**
```bash
# 1. 启动第一个守护进程
sudo fsmon daemon --debug &
PID1=$!
sleep 2

# 2. 尝试启动第二个守护进程（应失败）
sudo fsmon daemon --debug &
PID2=$!
sleep 2

# 3. 检查第二个进程状态
ps -p $PID2

# 4. 清理
sudo kill $PID1 2>/dev/null
sudo kill $PID2 2>/dev/null
```

**预期结果：**
- 步骤 3：第二个进程应已退出（ps 命令失败）
- 守护进程 stderr 应显示单例锁相关错误信息

### 1.3 守护进程套接字创建

**测试步骤：**
```bash
UID=$(id -u)

# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 检查命令套接字文件是否存在
ls -la /run/user/$UID/fsmon/daemon.sock

# 3. 检查锁套接字文件是否存在
ls -la /run/user/$UID/fsmon/lock.sock

# 4. 检查目录权限
stat /run/user/$UID/fsmon/

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：命令套接字文件存在
- 步骤 3：锁套接字文件存在
- 步骤 4：目录权限为 0700（仅所有者可访问）

### 1.4 守护进程启动输出

**测试步骤：**
```bash
# 1. 启动守护进程，捕获 stderr
sudo fsmon daemon --debug 2>/tmp/fsmon_startup.log &
DAEMON_PID=$!
sleep 2

# 2. 检查启动输出
cat /tmp/fsmon_startup.log | head -20

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 输出应包含 "Config loaded:" 信息
- 应显示 "Monitored path database:" 路径
- 应显示 "Singleton lock:" 路径
- 应显示 "Command socket:" 路径

### 1.5 守护进程版本与帮助

**测试步骤：**
```bash
# 1. 检查版本
fsmon --version

# 2. 检查主帮助
fsmon --help

# 3. 检查子命令帮助
fsmon daemon --help
fsmon add --help
fsmon remove --help
fsmon query --help
fsmon clean --help
fsmon changes --help
fsmon init --help
fsmon health --help
fsmon cd --help
```

**预期结果：**
- 步骤 1：应输出版本号
- 步骤 2-3：应显示帮助信息，包含命令别名（如 `d`、`a`、`r` 等）

---

## 二、路径监控功能

### 2.1 添加监控路径

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 检查监控列表
fsmon monitored

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：输出应包含 `/tmp/fsmon_test` 路径，cmd 为 `test_app`，recursive 为 true

### 2.2 添加全局监控路径（_global）

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加全局监控路径（不指定进程名）
fsmon add _global --path /tmp/fsmon_test -r

# 3. 检查监控列表
fsmon monitored

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：输出应包含 `/tmp/fsmon_test` 路径，cmd 为 `_global`

### 2.3 文件创建事件捕获

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径（使用 _global 组捕获所有进程事件）
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建测试文件
touch /tmp/fsmon_test/test_file.txt
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | tail -5

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含 CREATE 事件，路径为 `/tmp/fsmon_test/test_file.txt`，cmd 为实际进程名（如 `touch`）
- **注意：** 使用 `test_app` 等命名 cmd 组时，只有 cmd 匹配的进程事件才会路由到对应日志文件。测试事件捕获功能建议使用 `_global` 组

### 2.4 文件修改事件捕获

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建并修改测试文件
echo "initial" > /tmp/fsmon_test/test_file.txt
sleep 1
echo "modified" > /tmp/fsmon_test/test_file.txt
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep CLOSE_WRITE

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含 CLOSE_WRITE 事件（fanotify 默认不产生 MODIFY 事件，修改文件通过 CLOSE_WRITE 捕获）

### 2.5 文件删除事件捕获

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建并删除测试文件
echo "content" > /tmp/fsmon_test/test_file.txt
sleep 1
rm /tmp/fsmon_test/test_file.txt
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep DELETE

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含 DELETE 事件

### 2.6 文件移动事件捕获

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建并移动测试文件
echo "content" > /tmp/fsmon_test/source.txt
sleep 1
mv /tmp/fsmon_test/source.txt /tmp/fsmon_test/dest.txt
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep -E "MOVED_FROM|MOVED_TO"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含 MOVED_FROM 和 MOVED_TO 事件

### 2.7 事件类型过滤（--types）

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径，仅监控 CREATE 和 CLOSE_WRITE
fsmon add _global --path /tmp/fsmon_test -r --types CREATE --types CLOSE_WRITE

# 3. 创建文件（应捕获）
touch /tmp/fsmon_test/create_test.txt
sleep 1

# 4. 删除文件（不应捕获）
rm /tmp/fsmon_test/create_test.txt
sleep 1

# 5. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.event_type'

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 5：日志中应包含 CREATE 事件，不应包含 DELETE 事件

### 2.8 事件类型过滤（--types all）

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径，监控所有 14 种事件类型
fsmon add _global --path /tmp/fsmon_test -r --types all

# 3. 执行多种操作
touch /tmp/fsmon_test/all_test.txt
sleep 1
echo "data" > /tmp/fsmon_test/all_test.txt
sleep 1
cat /tmp/fsmon_test/all_test.txt > /dev/null
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.event_type' | sort | uniq

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应包含 CREATE、MODIFY、OPEN、ACCESS 等多种事件类型

### 2.9 文件大小过滤（--size）

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径，仅监控大于 1MB 的文件
fsmon add _global --path /tmp/fsmon_test -r --size ">1MB"

# 3. 创建小文件（不应捕获）
echo "small" > /tmp/fsmon_test/small.txt
sleep 1

# 4. 创建大文件（应捕获）
dd if=/dev/zero of=/tmp/fsmon_test/large.txt bs=1M count=2
sleep 1

# 5. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.path'

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 5：日志中应包含 large.txt，不应包含 small.txt

### 2.10 递归监控新子目录

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加递归监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建子目录和文件
mkdir -p /tmp/fsmon_test/subdir
echo "test" > /tmp/fsmon_test/subdir/file.txt
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep "subdir"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含 subdir 下的文件事件

---

## 三、路径管理命令

### 3.1 fsmon remove - 删除整个 cmd 组

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 确认已添加
fsmon monitored

# 4. 删除整个 cmd 组
fsmon remove test_app

# 5. 确认已删除
fsmon monitored

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：输出包含 test_app 组
- 步骤 5：输出不包含 test_app 组

### 3.2 fsmon remove - 删除指定路径

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加多个监控路径
fsmon add test_app --path /tmp/fsmon_test -r
fsmon add test_app --path /tmp/other_test -r

# 3. 删除其中一个路径
fsmon remove test_app --path /tmp/other_test

# 4. 确认监控列表
fsmon monitored

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：输出包含 /tmp/fsmon_test，不包含 /tmp/other_test

### 3.3 fsmon remove - 删除不存在的组（应报错）

**测试步骤：**
```bash
# 1. 尝试删除不存在的 cmd 组
fsmon remove nonexistent_group
```

**预期结果：**
- 应输出错误信息："Cmd group 'nonexistent_group' not found"

### 3.4 fsmon remove - 删除 _global 组

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加全局监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 删除全局监控
fsmon remove _global

# 4. 确认已删除
fsmon monitored

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：输出不包含 _global 组

---

## 四、进程归属功能

### 4.1 进程归属识别

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径（使用 _global 捕获所有进程事件）
fsmon add _global --path /tmp/fsmon_test -r

# 3. 使用特定进程创建文件
bash -c "echo 'test' > /tmp/fsmon_test/test_file.txt"
sleep 1

# 4. 检查日志中的进程信息
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.cmd, .pid, .user'

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志中应包含正确的 cmd（bash）、pid 和 user 信息
- **注意：** 命名 cmd 组（如 `test_app`）只匹配进程树中 cmd 名称匹配的事件。使用 `bash` 创建文件时，事件不会路由到 `test_app_log.jsonl`，因为进程名不匹配

### 4.2 进程树追踪

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径，指定进程名
fsmon add nginx --path /tmp/fsmon_test -r

# 3. 模拟 nginx 进程创建文件
# 注意：这里需要实际运行 nginx 或使用其他方法模拟
# 简化测试：直接使用 bash 模拟
bash -c "echo 'nginx test' > /tmp/fsmon_test/nginx_file.txt"
sleep 1

# 4. 检查日志
cat ~/.local/state/fsmon/nginx_log.jsonl | jq '.chain'

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：如果进程名不匹配 nginx，日志可能为空（事件被过滤）
- 如果进程名匹配，chain 字段应包含进程祖先链

### 4.3 多 cmd 组并行监控

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加多个 cmd 组监控同一路径
fsmon add nginx --path /tmp/fsmon_test -r
fsmon add vim --path /tmp/fsmon_test -r

# 3. 检查监控列表
fsmon monitored

# 4. 生成事件
echo "test" > /tmp/fsmon_test/multi.txt
sleep 1

# 5. 检查各组日志
cat ~/.local/state/fsmon/nginx_log.jsonl | wc -l
cat ~/.local/state/fsmon/vim_log.jsonl | wc -l

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：应显示两个 cmd 组
- 步骤 5：两个日志文件都应有事件记录

---

## 五、配置文件功能

### 5.1 配置文件加载

**测试步骤：**
```bash
# 1. 创建配置目录
mkdir -p ~/.config/fsmon

# 2. 创建配置文件（完整结构）
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[daemon]
debug = true
metrics_interval = 10

[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"
keep_days = 7
size = "100MB"
disk_free = "10%"
local_time = true

[cache]
dir_capacity = 50000
dir_ttl_secs = 1800
file_size_capacity = 5000
proc_ttl_secs = 300
buffer_size = 65536
channel_capacity = 2048
subscribe_capacity = 8192

[watchdog]
interval_secs = 30
multiplier = 3
EOF

# 3. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 4. 检查配置是否生效
ls -la /tmp/fsmon_test/logs/

# 5. 检查启动输出
cat /tmp/fsmon_startup.log | grep -E "Config|Monitored|Event logs|Singleton|Command socket"

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志目录应按配置创建
- 步骤 5：启动输出应显示配置加载信息

### 5.2 配置文件 - 最小配置

**测试步骤：**
```bash
# 1. 创建最小配置文件
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 3. 检查是否正常运行
ps -p $DAEMON_PID

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：进程应正常运行

### 5.3 配置文件热更新（通过重启）

**测试步骤：**
```bash
# 1. 创建初始配置
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs1"
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 3. 停止守护进程
sudo kill $DAEMON_PID
sleep 1

# 4. 修改配置
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs2"
EOF

# 5. 重新启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 6. 检查新配置是否生效
ls -la /tmp/fsmon_test/logs2/

# 7. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 6：新日志目录应被创建，旧目录应保留

### 5.4 fsmon init - 创建配置目录

**测试步骤：**
```bash
# 1. 备份并删除现有配置
mv ~/.config/fsmon ~/.config/fsmon.bak 2>/dev/null

# 2. 运行 init 命令
fsmon init

# 3. 检查配置目录是否创建
ls -la ~/.config/fsmon/

# 4. 恢复配置
rm -rf ~/.config/fsmon
mv ~/.config/fsmon.bak ~/.config/fsmon 2>/dev/null
```

**预期结果：**
- 步骤 3：应创建 ~/.config/fsmon/ 目录

### 5.5 fsmon init --service - 创建 systemd 服务文件

**测试步骤：**
```bash
# 1. 运行 init --service（需要 sudo）
sudo fsmon init --service

# 2. 检查服务文件是否创建
cat /etc/systemd/system/fsmon.service

# 3. 检查服务文件内容
# 应包含 ExecStart、Restart=always、WatchdogSec（如果配置了 watchdog）

# 4. 清理（可选）
sudo rm /etc/systemd/system/fsmon.service
sudo systemctl daemon-reload
```

**预期结果：**
- 步骤 2：服务文件应存在
- 步骤 3：应包含正确的 ExecStart 路径和配置

---

## 六、日志查询与清理

### 6.1 fsmon query - 基本查询

**测试步骤：**
```bash
# 1. 启动守护进程并生成一些事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "test1" > /tmp/fsmon_test/file1.txt
echo "test2" > /tmp/fsmon_test/file2.txt
sleep 2

# 4. 查询日志
fsmon query test_app

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示所有事件

### 6.2 fsmon query - 时间过滤

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "test" > /tmp/fsmon_test/time_test.txt
sleep 2

# 4. 查询最近 1 小时的事件
fsmon query test_app -t ">1h"

# 5. 查询最近 1 天的事件
fsmon query test_app -t ">1d"

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示最近 1 小时的事件
- 步骤 5：应显示最近 1 天的事件

### 6.3 fsmon query - 路径过滤

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "test1" > /tmp/fsmon_test/path1.txt
mkdir -p /tmp/fsmon_test/subdir
echo "test2" > /tmp/fsmon_test/subdir/path2.txt
sleep 2

# 4. 按路径前缀过滤
fsmon query test_app --path /tmp/fsmon_test/subdir

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应仅显示 subdir 下的事件

### 6.4 fsmon query - _global 查询

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加全局监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成事件
echo "global test" > /tmp/fsmon_test/global.txt
sleep 2

# 4. 查询全局日志
fsmon query _global

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示全局事件

### 6.5 fsmon clean - 按时间清理

**测试步骤：**
```bash
# 1. 启动守护进程并生成一些事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成测试事件
for i in {1..10}; do
    echo "test$i" > /tmp/fsmon_test/file$i.txt
    sleep 0.1
done
sleep 2

# 4. 检查日志文件大小
ls -lh ~/.local/state/fsmon/_global_log.jsonl

# 5. 清理日志（删除 1 天前的条目）
fsmon clean _global -t ">1d"

# 6. 检查清理后的日志
ls -lh ~/.local/state/fsmon/_global_log.jsonl

# 7. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 5：清理命令应成功执行
- 步骤 6：日志文件应变小或为空

### 6.6 fsmon clean - 按大小清理

**测试步骤：**
```bash
# 1. 启动守护进程并生成大量事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成大量测试事件
for i in {1..1000}; do
    echo "test$i" > /tmp/fsmon_test/file_$i.txt
done
sleep 5

# 4. 检查日志文件大小
ls -lh ~/.local/state/fsmon/_global_log.jsonl

# 5. 按大小清理（保留小于 10KB）
fsmon clean _global --size "<10KB"

# 6. 检查清理后的日志
ls -lh ~/.local/state/fsmon/_global_log.jsonl

# 7. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 5：清理命令应成功执行
- 步骤 6：日志文件应变小

### 6.7 fsmon clean - 预览模式（--dry-run）

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "test" > /tmp/fsmon_test/dryrun.txt
sleep 2

# 4. 预览清理（不实际执行）
fsmon clean _global -t ">1d" --dry-run

# 5. 检查日志是否未被修改
ls -lh ~/.local/state/fsmon/_global_log.jsonl

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示将要删除的条目数量
- 步骤 5：日志文件大小应保持不变

---

## 七、changes 命令

### 7.1 fsmon changes - 基本查询

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件（同一文件多次修改）
echo "v1" > /tmp/fsmon_test/changes_test.txt
sleep 1
echo "v2" > /tmp/fsmon_test/changes_test.txt
sleep 1
echo "v3" > /tmp/fsmon_test/changes_test.txt
sleep 2

# 4. 查询 changes
fsmon changes test_app

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示每个路径的最新事件（去重），changes_test.txt 应显示最后一次修改

### 7.2 fsmon changes - 路径过滤

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "a" > /tmp/fsmon_test/a.txt
echo "b" > /tmp/fsmon_test/b.txt
sleep 2

# 4. 按路径过滤
fsmon changes test_app --path /tmp/fsmon_test/a.txt

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应仅显示 a.txt 的最新事件

### 7.3 fsmon changes - 时间过滤

**测试步骤：**
```bash
# 1. 启动守护进程并生成事件
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 生成测试事件
echo "time test" > /tmp/fsmon_test/time_changes.txt
sleep 2

# 4. 按时间过滤
fsmon changes test_app -t ">1h"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示最近 1 小时的 changes

### 7.4 fsmon changes - _global 查询

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加全局监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成事件
echo "global changes" > /tmp/fsmon_test/global_changes.txt
sleep 2

# 4. 查询全局 changes
fsmon changes _global

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应显示全局 changes

---

## 八、health 命令

### 8.1 fsmon health - 查询守护进程状态

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 查询健康状态
fsmon health

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：应返回 JSON 格式的健康状态响应

### 8.2 fsmon health - 守护进程未运行时

**测试步骤：**
```bash
# 1. 确保守护进程未运行
sudo pkill -f "fsmon daemon" 2>/dev/null
sleep 1

# 2. 查询健康状态
fsmon health
```

**预期结果：**
- 步骤 2：应返回连接错误信息

---

## 九、cd 命令

### 9.1 fsmon cd - 日志目录

**测试步骤：**
```bash
# 1. 运行 cd 命令（进入日志目录）
# 注意：cd 命令会启动子 shell，需要交互式测试
fsmon cd --logging

# 2. 在子 shell 中检查目录
pwd
ls -la

# 3. 退出子 shell
exit
```

**预期结果：**
- 步骤 2：pwd 应显示日志目录路径

### 9.2 fsmon cd - 监控数据目录

**测试步骤：**
```bash
# 1. 运行 cd 命令（进入监控数据目录）
fsmon cd --monitored

# 2. 在子 shell 中检查目录
pwd
ls -la

# 3. 退出子 shell
exit
```

**预期结果：**
- 步骤 2：pwd 应显示监控数据目录路径

---

## 十、命令别名

### 10.1 所有命令别名测试

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 测试 daemon 别名
sudo fsmon d --debug &
DAEMON_PID2=$!
sleep 2
# 应失败（单例锁）

# 3. 测试 add 别名
fsmon a test_alias --path /tmp/fsmon_test -r

# 4. 测试 monitored 别名
fsmon m

# 5. 测试 query 别名
fsmon q test_alias

# 6. 测试 clean 别名
fsmon cl test_alias --dry-run

# 7. 测试 changes 别名
fsmon ch test_alias

# 8. 测试 health 别名
fsmon h

# 9. 测试 remove 别名
fsmon r test_alias

# 10. 测试 init 别名
fsmon i --help

# 11. 清理
sudo kill $DAEMON_PID 2>/dev/null
sudo kill $DAEMON_PID2 2>/dev/null
```

**预期结果：**
- 步骤 2：第二个守护进程应失败
- 步骤 3-10：所有别名应正常工作

---

## 十一、看门狗功能

### 11.1 看门狗基本功能

**测试步骤：**
```bash
# 1. 启动带看门狗的守护进程
sudo fsmon daemon --debug --watchdog-interval 5 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 等待看门狗超时（假设 multiplier=2，则 WatchdogSec=10）
sleep 12

# 4. 检查进程是否仍在运行
ps -p $DAEMON_PID

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程存在
- 步骤 4：进程仍应存在（看门狗应发送心跳）

### 11.2 看门狗配置验证

**测试步骤：**
```bash
# 1. 尝试使用无效的 multiplier（应失败）
sudo fsmon daemon --debug --watchdog-interval 5 --watchdog-multiplier 1 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID 2>/dev/null
```

**预期结果：**
- 步骤 2：进程应已退出（multiplier 必须大于 1）

### 11.3 看门狗配置文件

**测试步骤：**
```bash
# 1. 创建带看门狗配置的配置文件
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"

[watchdog]
interval_secs = 10
multiplier = 3
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 3. 检查进程状态
ps -p $DAEMON_PID

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：进程应正常运行

---

## 十二、指标收集功能

### 12.1 指标收集基本功能

**测试步骤：**
```bash
# 1. 启动带指标收集的守护进程
sudo fsmon daemon --debug --metrics-interval 5 &
DAEMON_PID=$!
sleep 2

# 2. 生成一些事件
fsmon add test_app --path /tmp/fsmon_test -r
echo "test" > /tmp/fsmon_test/test.txt
sleep 1

# 3. 等待指标输出（每 5 秒）
sleep 6

# 4. 检查 stderr 输出（需要将 stderr 重定向到文件）
# 注意：这个测试可能需要特殊处理，因为指标输出到 stderr

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：守护进程应定期输出指标到 stderr

### 12.2 指标内容验证

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug --metrics-interval 5 2>/tmp/fsmon_metrics.log &
DAEMON_PID=$!
sleep 2

# 2. 生成事件
fsmon add test_app --path /tmp/fsmon_test -r
for i in {1..10}; do
    echo "test$i" > /tmp/fsmon_test/file$i.txt
    sleep 0.1
done
sleep 2

# 3. 等待指标输出
sleep 6

# 4. 检查指标内容
cat /tmp/fsmon_metrics.log | grep -E "uptime|RSS|events"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：指标输出应包含 uptime、RSS、events 等信息

### 12.3 指标配置文件

**测试步骤：**
```bash
# 1. 创建带指标配置的配置文件
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[daemon]
metrics_interval = 5

[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug 2>/tmp/fsmon_metrics_cfg.log &
DAEMON_PID=$!
sleep 2

# 3. 等待指标输出
sleep 6

# 4. 检查指标输出
cat /tmp/fsmon_metrics_cfg.log | grep -E "uptime|RSS|events"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应有指标输出

---

## 十三、套接字通信功能

### 13.1 套接字命令 - health

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 获取套接字路径
SOCKET_FILE=$(fsmon health 2>&1 | grep -oP '/run/user/\d+/fsmon/daemon.sock' || echo "/run/user/$(id -u)/fsmon/daemon.sock")

# 3. 发送 health 命令
echo '{"Health":null}' | socat - UNIX-CONNECT:$SOCKET_FILE

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：应返回健康状态响应

### 13.2 套接字命令 - subscribe

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 获取套接字路径
SOCKET_FILE="/run/user/$(id -u)/fsmon/daemon.sock"

# 3. 订阅事件流
(echo '{"Subscribe":{}}'; sleep 5) | socat - UNIX-CONNECT:$SOCKET_FILE &

# 4. 生成事件
echo "test" > /tmp/fsmon_test/test.txt
sleep 1

# 5. 检查输出
# 注意：这个测试可能需要更复杂的处理

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 5：应接收到实时事件流

---

## 十四、日志格式验证

### 14.1 JSONL 日志格式

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成事件
echo "format test" > /tmp/fsmon_test/format_test.txt
sleep 2

# 4. 检查日志格式
cat ~/.local/state/fsmon/_global_log.jsonl | jq 'keys'

# 5. 检查必需字段
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.event_type, .path, .cmd, .pid, .user, .time'

# 6. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应返回有效的 JSON 对象键列表
- 步骤 5：所有必需字段应存在且非空

### 14.2 日志时间戳格式

**测试步骤：**
```bash
# 1. 启动守护进程（UTC 时间）
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成事件
echo "utc test" > /tmp/fsmon_test/utc_test.txt
sleep 2

# 4. 检查时间戳格式
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.time'

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：时间戳应为 ISO 8601 格式，以 Z 结尾（UTC）

### 14.3 日志本地时间格式

**测试步骤：**
```bash
# 1. 启动守护进程（本地时间）
sudo fsmon daemon --debug --logging-local-time &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 生成事件
echo "local time test" > /tmp/fsmon_test/local_time_test.txt
sleep 2

# 4. 检查时间戳格式
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.time'

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：时间戳应为 ISO 8601 格式，带本地时区偏移（如 +08:00）

---

## 十五、缓存配置功能

### 15.1 目录句柄缓存配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置目录缓存容量
sudo fsmon daemon --debug --cache-dir-cap 50000 --cache-dir-ttl 1800 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.2 文件大小缓存配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置文件大小缓存容量
sudo fsmon daemon --debug --cache-file-size 5000 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.3 进程缓存配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置进程缓存 TTL
sudo fsmon daemon --debug --cache-proc-ttl 300 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.4 读取缓冲区配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置读取缓冲区大小
sudo fsmon daemon --debug --cache-buffer 65536 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.5 事件通道容量配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置事件通道容量
sudo fsmon daemon --debug --cache-channel 2048 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.6 订阅缓冲区配置

**测试步骤：**
```bash
# 1. 启动守护进程，设置订阅缓冲区容量
sudo fsmon daemon --debug --cache-subscribe 8192 &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 15.7 缓存配置文件

**测试步骤：**
```bash
# 1. 创建带缓存配置的配置文件
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"

[cache]
dir_capacity = 50000
dir_ttl_secs = 1800
file_size_capacity = 5000
proc_ttl_secs = 300
buffer_size = 65536
channel_capacity = 2048
subscribe_capacity = 8192
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 3. 检查进程状态
ps -p $DAEMON_PID

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：进程应正常运行

---

## 十六、磁盘空间监控

### 16.1 磁盘空间阈值告警

**测试步骤：**
```bash
# 1. 启动守护进程，设置磁盘空间阈值
sudo fsmon daemon --debug --logging-disk-free "10%" &
DAEMON_PID=$!
sleep 2

# 2. 检查进程状态
ps -p $DAEMON_PID

# 3. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 2：进程应正常运行

### 16.2 磁盘空间配置文件

**测试步骤：**
```bash
# 1. 创建带磁盘空间配置的配置文件
cat > ~/.config/fsmon/fsmon.toml << 'EOF'
[monitored]
path = "/tmp/fsmon_test/monitored.jsonl"

[logging]
path = "/tmp/fsmon_test/logs"
disk_free = "5GB"
EOF

# 2. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 3. 检查进程状态
ps -p $DAEMON_PID

# 4. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 3：进程应正常运行

---

## 十七、错误处理功能

### 17.1 权限错误处理

**测试步骤：**
```bash
# 1. 创建只读目录
mkdir -p /tmp/fsmon_readonly
chmod 444 /tmp/fsmon_readonly

# 2. 尝试添加监控路径（应失败）
fsmon add test_app --path /tmp/fsmon_readonly -r

# 3. 检查错误信息
# 4. 清理
chmod 755 /tmp/fsmon_readonly
rm -rf /tmp/fsmon_readonly
```

**预期结果：**
- 步骤 2：应返回权限错误

### 17.2 路径不存在处理

**测试步骤：**
```bash
# 1. 尝试添加不存在的路径
fsmon add test_app --path /nonexistent/path -r

# 2. 检查输出信息
```

**预期结果：**
- 步骤 1：应输出提示信息 `[Note] path does not exist yet — will start monitoring when created.` 并成功添加（路径将在创建后自动开始监控）

### 17.3 重复添加路径

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add test_app --path /tmp/fsmon_test -r

# 3. 再次添加相同路径
fsmon add test_app --path /tmp/fsmon_test -r

# 4. 检查监控列表
fsmon monitored

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应只有一个条目（不重复）

### 17.4 无效的事件类型

**测试步骤：**
```bash
# 1. 尝试添加无效的事件类型
fsmon add test_app --path /tmp/fsmon_test -r --types INVALID_TYPE
```

**预期结果：**
- 应返回错误信息 `Error: Unknown event type: INVALID_TYPE`

---

## 十八、边界条件测试

### 18.1 大量文件监控

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建大量文件
for i in {1..1000}; do
    echo "file$i" > /tmp/fsmon_test/file$i.txt
done
sleep 5

# 4. 检查日志
wc -l ~/.local/state/fsmon/_global_log.jsonl

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志应包含约 1000 个 CREATE 事件

### 18.2 快速事件生成

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 快速生成事件
for i in {1..100}; do
    echo "test" > /tmp/fsmon_test/file.txt
    rm /tmp/fsmon_test/file.txt
done
sleep 5

# 4. 检查日志
wc -l ~/.local/state/fsmon/_global_log.jsonl

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：日志应包含所有事件，无丢失

### 18.3 深层目录结构

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建深层目录结构
mkdir -p /tmp/fsmon_test/a/b/c/d/e/f/g/h/i/j
echo "deep" > /tmp/fsmon_test/a/b/c/d/e/f/g/h/i/j/deep.txt
sleep 2

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep "deep.txt"

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应捕获深层目录下的文件事件

### 18.4 特殊字符文件名

**测试步骤：**
```bash
# 1. 启动守护进程
sudo fsmon daemon --debug &
DAEMON_PID=$!
sleep 2

# 2. 添加监控路径
fsmon add _global --path /tmp/fsmon_test -r

# 3. 创建带特殊字符的文件
echo "special" > "/tmp/fsmon_test/file with spaces.txt"
echo "special" > "/tmp/fsmon_test/file'with'quotes.txt"
echo "special" > '/tmp/fsmon_test/file"with"doublequotes.txt'
sleep 2

# 4. 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | jq '.path'

# 5. 清理
sudo kill $DAEMON_PID
```

**预期结果：**
- 步骤 4：应正确记录带特殊字符的文件路径

---

## 测试注意事项

1. **环境隔离**：所有测试应在隔离环境中进行，避免影响系统其他部分
2. **清理工作**：每个测试后应清理测试文件和进程
3. **日志检查**：使用 `jq` 工具解析 JSON 日志，便于检查特定字段
4. **超时处理**：适当设置 sleep 时间，确保事件被正确捕获
5. **权限问题**：确保测试用户有权限读取日志文件
6. **套接字路径**：使用 `/run/user/<UID>/fsmon/daemon.sock`，不是 `/tmp/`
7. **配置文件**：使用 `[daemon]`、`[monitored]`、`[logging]`、`[cache]`、`[watchdog]` section，无 `[socket]` section
8. **清理语法**：使用 `fsmon clean <cmd> -t ">1d"` 而不是 `--keep-days`
9. **cmd 路由机制**：命名 cmd 组（如 `test_app`）只匹配进程树中 cmd 名称匹配的事件。测试事件捕获功能时建议使用 `_global` 组，测试进程过滤功能时使用命名 cmd 组
10. **事件类型**：fanotify 默认不产生 MODIFY 和 ACCESS 事件，修改文件通过 CLOSE_WRITE 捕获。使用 `--types all` 可启用全部 14 种事件类型
11. **不存在路径**：`fsmon add` 接受不存在的路径，输出提示信息后成功添加（路径将在创建后自动开始监控）

## 测试工具建议

```bash
# 必要工具
jq socat

# 检查 fsmon 是否安装
which fsmon

# 检查版本
fsmon --version

# 获取当前 UID
id -u

# 检查套接字文件
ls -la /run/user/$(id -u)/fsmon/

# 检查守护进程状态
ps aux | grep "fsmon daemon"

# 杀死守护进程
sudo pkill -f "fsmon daemon"
```

## 测试结果记录

每个测试项应记录：
1. 测试日期和时间
2. 测试环境（OS、内核版本等）
3. 测试结果（通过/失败）
4. 失败原因（如有）
5. 相关日志片段
