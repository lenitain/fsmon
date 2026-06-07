# fix: BufWriter 未 flush 导致 daemon 退出时日志丢失

## 问题分析

两条数据流：
1. **磁盘流**: Reader → mpsc → Main loop → broadcast → `FileLogWriter` → `BufWriter<File>` → OS buffer → Disk
2. **Socket流**: Reader → mpsc → Main loop → broadcast → subscriber_task → socket

**根因**: `FileLogWriter` 使用 `BufWriter<File>`，但从未调用 `BufWriter::flush()`。

具体问题点：
- `sync_dirty_logs()` 调用 `fdatasync()` 但不先 `flush()` BufWriter → 数据还在用户态 buffer，fdatasync 无意义
- daemon 收到 SIGTERM/SIGINT 后，`drain_remaining_events` 处理完事件发到 broadcast，然后 drop sender，但 `FileLogWriter` 的 `BufWriter` 从未 flush
- `run()` 结尾的 `sync_dirty_logs()` 只做 fdatasync，不做 flush
- 结果：daemon kill 后日志文件为空

## 数据库项目参考

| 项目 | 机制 | 适用点 |
|------|------|--------|
| SQLite | WAL + `fsync` on commit | 写入后立即确保持久化 |
| PostgreSQL | WAL + `fdatasync` + checkpoint | 批量 sync + 优雅关闭时 flush |
| RocksDB | WAL + `sync_wal()` | 控制 sync 粒度 |

## 解决方案

### 1. `sync_dirty_logs()` 先 flush 再 fdatasync
- 在 `writer.get_ref().sync_data()` 前加 `writer.flush()`
- 确保 BufWriter 的数据写入 OS buffer 后再 sync 到磁盘

### 2. `run()` 结束时显式 flush 所有 handles
- 在 `self.sync_dirty_logs()` 后，遍历所有 `self.handles` 调用 `flush()`
- 确保 daemon 正常退出时数据持久化

### 3. 收到 SIGTERM/SIGINT 后等待 FileLogWriter 完成
- 在 `Monitor::run()` 的 shutdown 路径，drop `event_stream_tx` 前等待 FileLogWriter 处理完
- 方案：用 `tokio::sync::oneshot` 或 `JoinHandle` 追踪 writer task

## 修改文件

1. `src/common/monitor/file_writer.rs`
   - `sync_dirty_logs()`: 加 `writer.flush()` 
   - `run()` 结尾: flush 所有 handles
   - 新增 `flush_all()` 方法

2. `src/common/monitor/init.rs`
   - `spawn_tasks()`: 返回 FileLogWriter 的 JoinHandle
   - 用 `tokio::spawn` 返回 handle

3. `src/common/monitor/mod.rs`
   - 新增 `file_writer_handle` 字段
   - shutdown 路径: 等待 handle 完成

## 验证

```bash
# 测试 1: 正常退出
sudo fsmon daemon &
fsmon add test --path /tmp/test_write -r
echo "hello" > /tmp/test_write/test.txt
sleep 2
kill $(pgrep -f "fsmon daemon")
cat ~/.local/share/fsmon/logs/test_log.jsonl  # 应有数据

# 测试 2: 强制 kill
sudo fsmon daemon &
fsmon add test --path /tmp/test_write2 -r
echo "hello" > /tmp/test_write2/test.txt
sleep 1
kill -9 $(pgrep -f "fsmon daemon")
cat ~/.local/share/fsmon/logs/test_log.jsonl  # 可能丢少量数据，但不应全空

# 测试 3: sync_interval 启用时
# 配置 sync_interval=1，写入后等 2s 再 kill -9
# 应有数据（sync_interval 触发时已 flush+sync）
```

## 风险

- **低**: flush 所有 handles 会增加 I/O，但只在 shutdown 时执行
- **低**: 等待 writer handle 可能短暂阻塞 shutdown，但设合理 timeout
