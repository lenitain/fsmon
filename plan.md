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

---

## P1 小修小补（低代价，高收益）

### 建议 1: `procfs` 依赖的去留

`Cargo.toml` 里引入了 `procfs = "0.16"`，但 `proc_cache.rs` 和 `utils.rs` 中仍然手动解析 `/proc/{pid}/status`。`procfs::process::Process` 一行就能拿到 ppid / comm / tgid / starttime，比手动解析更可靠（处理了线程名中的空格、字段格式变化等边界）。

- **要么用 `procfs` 替代所有手动 `/proc` 解析**，减少代码量并提高健壮性
- **要么从 Cargo.toml 里删掉 `procfs`**，避免引入但不用增加编译时间

### 建议 2: 日志写入 flush + fsync 策略

当前写入走 `BufWriter`，不是每次 flush。如果 daemon 被 `kill -9`（非 SIGTERM），最后几秒的事件可能丢失。在"用 fsmon 找到是谁删了文件"的场景下，威胁模型需要覆盖"监控工具自身被干掉"。

- 加 `--sync-interval N`（默认 5s），每 N 秒对日志文件做一次 `fdatasync`
- SIGTERM 接收时做最后一次 sync 后再退出
- 代价：几十毫秒磁盘 IO / 每 5s — 可忽略

### 建议 3: `fsmon diff` 命令

运维中高频场景："上次部署以后哪些文件被改了？" `fsmon query` 只能按时间和路径过滤，需要用户用 `jq` 写复杂的去重聚合脚本。

```bash
# 新命令：按 path 去重，取最后一次更改
fsmon diff _global --since '2026-05-25 08:00' --until 'now'
```

实现上就是 `query` 的结果按 path dedup 取 `max(time)`，但有了独立子命令用户心智负担小很多。新增功能里 ROI 最高的一个。

---

## P2 中型改进（需要投入，但收益明显）

### 事件去重/合并 (`--coalesce`)

单个操作（如 `echo "hello" > file.txt`）产生 3 个 fanotify 事件：CREATE → MODIFY → CLOSE_WRITE。用 `(pid, path)` 做 key，固定时间窗口（~100ms）内合并为一个带事件列表 + 持续时间的记录。

- 可用 `moka::Cache` 做（项目已有依赖），在 `process_event_batch` 末尾加 flush timer
- 高频写入场景下日志量可减少 60-70%
- 作为**可选**功能（`--coalesce`），默认不开启，保持兼容

### `fanotify_mark` 中消除不必要的堆分配

`path.as_ref().as_bytes().to_vec()` 每次都分配新 Vec。可用 `CString::new` 或 `OsStr::as_encoded_bytes()`（Rust 1.74+）避免。mark 操作不频繁，但作为 crates.io 公共 API，零开销是承诺。

### HandleCache 的脏数据问题

moka TTL 淘汰被删除目录的 handle entry 但无通知。Phase 1 本地 `handle_map` 会走 `resolve_file_handle` 回退，不影响正确性，但浪费空间。未来可考虑把 HandleCache trait 化，让用户自由选择后端。

---

## P3 架构级（如果做大）

### 日志格式 trait 化

`FileEvent` 硬编码 JSONL 序列化。如果团队用 protobuf / Avro 做日志管道，就得 fork 项目。

```rust
pub trait EventSink {
    fn write(&mut self, event: &FileEvent) -> Result<()>;
}
```

~20 行抽象，让"接 Kafka / S3 / protobuf 输出"成为可能，且不影响默认 JSONL 路径。不急着做，但架构上预留位置。

### fanotify-fid 支持 io-uring

Linux 6.0+ 下 fanotify 可配合 io-uring 做异步事件读取。当前 `AsyncFd` 方案在 tokio 下完全正确，但极致低延迟场景（微秒级）io-uring 的 submission queue polling 更优。不急，但 crate README 的 "Future Work" 里提一句让后来者知道这条路被考虑过。

---

## 不应建议的方向

| 方向 | 原因 |
|------|------|
| "用 SQLite 存事件" | fsmon 哲学是 Unix 管道 + `jq`，JSONL 是故意选的 |
| "加 Web Dashboard" | CLI 工具不是平台，`fsmon query | jq` 已给用户一切 |
| "支持 Windows/macOS" | fanotify 是 Linux 独有的，这是 feature 不是 bug |
| "换掉 moka 用自己的缓存" | moka 经过大量生产验证，自己写大概率更差 |
| "支持分布式集群监控" | 单机工具，每台机器的文件系统变更是独立的 |
