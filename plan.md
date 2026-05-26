# fsmon 自愈体系完善计划

## 已完成

- [x] **Reader task 崩溃恢复** — 死亡通知 + 自动重启 + 退避（3次/60s）
- [x] **PID 回收去重** — `start_time_ns` 校验，已在 `get_process_info_by_pid` 中实现
- [x] **日志标签规范** — `[debug]` → `[DEBUG]`，与 `[ERROR]`/`[WARNING]`/`[INFO]` 统一
- [x] **SIGTERM 排空 event channel** — 关闭时 `try_recv` 耗尽 channel 中已排队事件后再退出
- [x] **Bounded channel + 事件降级** — 默认 unbounded，可选 `--channel-capacity N` / `cache.channel_capacity` 启用 bounded，reader task 满时自然阻塞，fanotify overflow 兜底
- [x] **`health` socket 命令** — `fsmon health` 通过 Unix socket 获取 daemon 健康状态（uptime、reader 存活/重启数、channel type、路径数）
- [x] **磁盘空间预检 + 运行时缓冲** — 可配置阈值（CLI/TOML），启动时 `statvfs` 检查，写入失败自动切内存缓冲（最多 10K 条），每 10s 重试刷盘。运行时目录被删自动重建 + chown

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
P2-4 Systemd watchdog      ← 可选，需用户自行配置 systemd service
```

每个 item 独立可测，不互相阻塞。
