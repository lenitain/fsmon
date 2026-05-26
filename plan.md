# fsmon 自愈体系完善计划

## 已完成

- [x] **Reader task 崩溃恢复** — 死亡通知 + 自动重启 + 退避（3次/60s）
- [x] **PID 回收去重** — `start_time_ns` 校验，已在 `get_process_info_by_pid` 中实现
- [x] **日志标签规范** — `[debug]` → `[DEBUG]`，与 `[ERROR]`/`[WARNING]`/`[INFO]` 统一
- [x] **SIGTERM 排空 event channel** — 关闭时 `try_recv` 耗尽 channel 中已排队事件后再退出

---

## P1 — 鲁棒性基础

### 2. Bounded channel + 事件降级

当前 `unbounded_channel`，事件洪峰时内存无限增长 → OOM。

**实现**：
```
bounded(100_000)
事件类型优先级：CREATE/DELETE/MODIFY > MOVE > ATTRIB/CLOSE > ACCESS/OPEN
满时：丢弃最低优先级事件，累加 dropped 计数器
```

成本 ~30 行，依赖 P1-6 metrics 暴露 dropped 计数器。

### 3. `health` socket 命令

通过已有 Unix socket 协议暴露 daemon 健康状态。

**响应字段**：
```toml
[fiber]
ok = true
uptime_secs = 3600

[readers]
[reader.0]
alive = true
restarts = 2

[reader.1]
alive = false
restarts = 3

[channel]
depth = 42
dropped_total = 0

[cache]
dir_entries = 12345
proc_entries = 987
```

**收益**：
- systemd `ExecStartPost` 可以等探活成功
- 运维脚本可轮询
- 集成测试可验证 daemon 真正 ready

成本 ~50 行。

---

## P2 — 纵深防御

### 4. Systemd watchdog（可选，需用户自行配置 systemd service）

主循环死锁时 systemd 自动 kill + restart。fsmon 本身不依赖 systemd，此功能仅对将 daemon 托管在 systemd 下的用户可用。

**实现**：
```
libsystemd 可选依赖（feature flag "watchdog"）
WatchdogSec=30s  → 主循环每 15s 发 sd_notify(WATCHDOG=1)
无 systemd 时自动降级（检测 NOTIFY_SOCKET 环境变量）
```

成本 ~20 行 + Cargo.toml 可选依赖。

对应的 systemd unit 片段：
```ini
[Service]
WatchdogSec=30s
Restart=always
RestartSec=5
```

### 5. 磁盘空间预检

**启动时**：检查日志目录所在文件系统可用空间 < 10% → `[WARNING]`。

**运行时**：写入失败后标记 "磁盘不健康"，切换事件到内存环形缓冲（最多 10_000 条），定期重试写入，恢复后刷入。

成本 ~30 行。

---

## P3 — 可观测性

### 6. Metrics 计数器

不引入 HTTP/端口。通过 socket `health` 命令暴露，或可选写入 jsonl stats 文件。

**最小指标集**：
```
events_processed_total (per event_type)
events_dropped_total
reader_restarts_total
channel_depth (gauge)
cache_hit_rate (dir/proc/file)
uptime_secs
```

成本 ~60 行（与 P1-3 共用 response 结构）。

### 7. Channel backlog 告警

`channel_depth` 连续 3 次检查 > 10_000 → `[WARNING] Event backlog: N items`。

成本 ~10 行。

---

## 不适用/不做的

| 项 | 原因 |
|----|------|
| 复制 / 高可用 | 单机工具，监控本机文件系统 |
| AOF/RDB 持久化恢复 | 数据权威源在内核和 JSONL 磁盘日志，内存纯缓存 |
| 在线热升级 | Rust 不支持，systemd restart 已覆盖 |
| 断路器 | 无外部依赖 |
| 共识协议 | 单进程 |
| 数据校验和 | JSONL 逐行独立，坏行自动跳过 |

---

## 执行顺序

```
P1-2 Bounded channel 降级  ← 核心收益
P1-3 health 命令           ← 为 P2-4 铺路
P2-4 Systemd watchdog      ← 依赖 P1-3 验证存活
P2-5 磁盘空间预检          ← 独立，可随时做
P3-6 Metrics               ← 扩展 P1-3 响应结构
P3-7 Backlog 告警          ← 依赖 P1-2 bounded channel
```

每个 item 独立可测，不互相阻塞。
