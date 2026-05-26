# fsmon 自愈体系完善计划

## 已完成

- [x] **Reader task 崩溃恢复** — 死亡通知 + 自动重启 + 退避（3次/60s）
- [x] **PID 回收去重** — `start_time_ns` 校验，已在 `get_process_info_by_pid` 中实现
- [x] **日志标签规范** — `[debug]` → `[DEBUG]`，与 `[ERROR]`/`[WARNING]`/`[INFO]` 统一
- [x] **SIGTERM 排空 event channel** — 关闭时 `try_recv` 耗尽 channel 中已排队事件后再退出
- [x] **Bounded channel + 事件降级** — 默认 unbounded，可选 `--channel-capacity N` 启用 bounded，reader task 满时自然阻塞
- [x] **`health` socket 命令** — `fsmon health` 通过 Unix socket 获取 daemon 健康状态
- [x] **磁盘空间预检 + 运行时缓冲** — 可配置阈值，写入失败自动切内存缓冲（最多 10K 条），每 10s 重试刷盘
- [x] **`sudo fsmon init --service`** — 自动生成 systemd service（`Type=simple` + `Restart=always`），崩溃自动重启

---

## systemd 集成分析（P2-4）

以 Redis 为参照，逐项分析各 systemd feature 对 fsmon 的价值：

| 能力 | Redis | fsmon 现状 | 对 fsmon 的价值 | 决策 |
|------|-------|-----------|----------------|:----:|
| **崩溃重启** | `Restart=always` | `init --service` 已做 | 进程 panic/kill -9 后自动拉起来，核心收益 | **✅ 已做** |
| **READY=1 通知** | `Type=notify` + `sd_notify(READY=1)` | ✅ `notify_sd_ready()` 已实现 | 启动时通过 Unix socket 向 systemd 发 READY=1。零外部依赖，纯标准库。`NOTIFY_SOCKET` 不存在时自动降级 | **✅ 已做** |
| **WATCHDOG 心跳** | `--supervised systemd` 自带 | 无 | 检测主循环死锁。但 fsmon 是 tokio 事件驱动（`select!`），所有操作都是非阻塞 async，死锁概率极低。复杂度 > 收益 | 🔴 **不做** |
| **显式 `--supervised` 开关** | 必须加这个参数 | 无此设计 | Redis 用显式开关是因为有 3 种模式（no/upstart/systemd），fsmon 只有 1 种。而且 NOTIFY_SOCKET 不存在时 sd_notify 自动降级，不需要人工开关 | 🔴 **不做** |
| **安全硬化** | `User=` + `Protect*` + `Private*` | 全 root 运行 | fanotify 需要 `CAP_SYS_ADMIN`，不能降权。`ProtectSystem=full` 阻止写日志，`ProtectHome=true` 阻止读配置 | 🔴 **不能做** |
| **超时控制** | `TimeoutStartSec/StopSec` | 无 | 启动/停止都是亚秒级操作，默认值足够 | 🔴 **没必要** |

### 结论

崩溃重启已经落袋。WATCHDOG 不做不是因为难，而是**死锁在事件驱动的 tokio 代码里几乎不可能发生**——所有操作都是非阻塞的 `select!` 分支，没有等待外部锁的同步代码路径。加 watchdog 等于为不可能发生的事引入一个依赖。

唯一可考虑的是 `READY=1` 通知（~5 行 + 可选依赖 `libsystemd`），但当前没有外部脚本依赖 `After=fsmon.service`，不是刚需。

### 不适用/不做的

| 项 | 原因 |
|----|------|
| 复制 / 高可用 | 单机工具，监控本机文件系统 |
| AOF/RDB 持久化恢复 | 数据权威源在内核和 JSONL 磁盘日志，内存纯缓存 |
| 在线热升级 | Rust 不支持，systemd restart 已覆盖 |
| 断路器 | 无外部依赖 |
| 共识协议 | 单进程 |
| 数据校验和 | JSONL 逐行独立，坏行自动跳过 |
