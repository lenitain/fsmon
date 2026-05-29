# 0.4.0 测试方案

> 基于 `e546d1a` (bump 0.3.4) 以来的所有代码变动。
> daemon 已在后台运行: `sudo fsmon daemon --debug 2>&1 | tee /tmp/fsmon.log`
> 每个测试点：测什么 → 怎么测 → 预期结果

---

## 1. 目录删除+重建恢复（核心修复）

### 1.1 基本流程: rm -rf → mkdir → touch

**测什么**: 监控目录被删除后重建，新目录内的事件能否被记录

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_recover
fsmon add _global --path /tmp/fsmon_test_recover --recursive
sleep 1
echo "before_delete" > /tmp/fsmon_test_recover/before.txt
sleep 1
rm -rf /tmp/fsmon_test_recover
sleep 1
mkdir -p /tmp/fsmon_test_recover
sleep 1
echo "after_recreate" > /tmp/fsmon_test_recover/after.txt
sleep 1
rm /tmp/fsmon_test_recover/after.txt
sleep 2
# 检查日志
cat ~/.local/state/fsmon/_global_log.jsonl | grep fsmon_test_recover | tail -10
```

**预期结果**:
- `before.txt` 的 CREATE + CLOSE_WRITE + DELETE 事件全部记录
- `after.txt` 的 CREATE + CLOSE_WRITE + DELETE 事件全部记录
- 即重建后的文件操作也能正常记录

### 1.2 空目录删除+重建

**测什么**: 空监控目录被删除后重建，是否仍能恢复监控

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_empty
fsmon add _global --path /tmp/fsmon_test_empty --recursive
sleep 1
rm -rf /tmp/fsmon_test_empty
sleep 1
mkdir -p /tmp/fsmon_test_empty
sleep 1
touch /tmp/fsmon_test_empty/only_file.txt
sleep 2
cat ~/.local/state/fsmon/_global_log.jsonl | grep fsmon_test_empty | tail -5
```

**预期结果**: `only_file.txt` 的 CREATE 事件被记录

### 1.3 连续多次删除+重建

**测什么**: 同一个目录被反复删除重建，每次都能恢复

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_cycle
fsmon add _global --path /tmp/fsmon_test_cycle --recursive
for i in 1 2 3; do
  mkdir -p /tmp/fsmon_test_cycle
  touch /tmp/fsmon_test_cycle/file_$i
  sleep 0.5
  rm -rf /tmp/fsmon_test_cycle
  sleep 0.5
done
sleep 2
cat ~/.local/state/fsmon/_global_log.jsonl | grep fsmon_test_cycle | tail -10
```

**预期结果**: 三轮的 file_1/file_2/file_3 的 CREATE + DELETE 事件全部记录

---

## 2. 递归监控

### 2.1 启动后新建子目录

**测什么**: daemon 启动后，在递归监控目录下新建子目录，子目录内文件是否被监控

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_subdir
fsmon add _global --path /tmp/fsmon_test_subdir --recursive
sleep 1
mkdir -p /tmp/fsmon_test_subdir/new_subdir
sleep 1
touch /tmp/fsmon_test_subdir/new_subdir/deep_file.txt
sleep 2
cat ~/.local/state/fsmon/_global_log.jsonl | grep deep_file | tail -5
```

**预期结果**: `deep_file.txt` 的 CREATE 事件被记录

### 2.2 多层嵌套子目录

**测什么**: 多层新建子目录下的文件

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_subdir/a/b/c
sleep 1
touch /tmp/fsmon_test_subdir/a/b/c/nested.txt
sleep 2
cat ~/.local/state/fsmon/_global_log.jsonl | grep nested.txt | tail -3
```

**预期结果**: `nested.txt` 的 CREATE 事件被记录

---

## 3. 路径解析修复

### 3.1 相对路径 add

**测什么**: `fsmon add` 使用相对路径时是否正确解析为绝对路径

**怎么测**:
```bash
cd /tmp
fsmon add _global --path fsmon_test_rel
cat ~/.local/share/fsmon/monitored.jsonl | grep fsmon_test_rel
```

**预期结果**: monitored.jsonl 中存储的是 `/tmp/fsmon_test_rel`（绝对路径），不是 `fsmon_test_rel`

### 3.2 相对路径 remove

**测什么**: 用相对路径 remove 时是否能正确匹配

**怎么测**:
```bash
cd /tmp
fsmon remove _global --path fsmon_test_rel
```

**预期结果**: 成功移除，无错误

---

## 4. fsmon cd 命令

### 4.1 cd -m 基本功能

**测什么**: `fsmon cd -m` 是否输出正确的目录路径

**怎么测**:
```bash
fsmon cd -m
```

**预期结果**: 输出监控存储目录路径（如 `/home/pilot/.local/share/fsmon`）

### 4.2 cd -l 基本功能

**测什么**: `fsmon cd -l` 是否输出日志目录路径

**怎么测**:
```bash
fsmon cd -l
```

**预期结果**: 输出日志目录路径（如 `/home/pilot/.local/state/fsmon`）

---

## 5. Broadcast 事件流

### 5.1 subscribe 实时流

**测什么**: subscribe socket 能否收到实时事件

**怎么测**:
```bash
# 需要在 daemon 运行时通过 socket 订阅
echo 'cmd="subscribe"
track_cmd="_global"' | sudo socat - UNIX-CONNECT:/tmp/fsmon-1000.sock &
SUB_PID=$!
sleep 1
touch /tmp/fsmon_test_recover/sub_test.txt
sleep 2
kill $SUB_PID 2>/dev/null
```

**预期结果**: subscribe 输出包含 `sub_test.txt` 的 JSONL 事件

### 5.2 metrics 端点

**测什么**: Prometheus metrics 是否可查询

**怎么测**:
```bash
echo 'cmd="metrics"' | sudo socat - UNIX-CONNECT:/tmp/fsmon-1000.sock
```

**预期结果**: 输出 Prometheus 格式的 metrics，包含 `fsmon_events_total`、`fsmon_monitored_paths` 等

---

## 6. 稳定性

### 6.1 大量事件不丢失

**测什么**: 快速大量文件操作后事件是否完整

**怎么测**:
```bash
mkdir -p /tmp/fsmon_test_stress
fsmon add _global --path /tmp/fsmon_test_stress --recursive
sleep 1
for i in $(seq 1 50); do
  touch /tmp/fsmon_test_stress/stress_$i
done
sleep 1
for i in $(seq 1 50); do
  rm /tmp/fsmon_test_stress/stress_$i
done
sleep 3
EVENTS=$(cat ~/.local/state/fsmon/_global_log.jsonl | grep stress_ | wc -l)
echo "Total stress events: $EVENTS"
```

**预期结果**: 事件数 >= 100（每个文件至少 CREATE + DELETE）

### 6.2 添加已存在的路径

**测什么**: 对已在监控中的路径再次 add 不会出错

**怎么测**:
```bash
fsmon add _global --path /tmp/fsmon_test_recover --recursive
# 应该成功，不报错
```

**预期结果**: 成功，提示路径已在监控中

---

## 7. 清理

```bash
# 测试结束后清理临时监控路径
fsmon remove _global --path /tmp/fsmon_test_recover
fsmon remove _global --path /tmp/fsmon_test_empty
fsmon remove _global --path /tmp/fsmon_test_cycle
fsmon remove _global --path /tmp/fsmon_test_subdir
fsmon remove _global --path /tmp/fsmon_test_stress
rm -rf /tmp/fsmon_test_*
```
