# Pull Metrics 端点 — 实现方案

## 目标

为 fsmon 补齐 Pull 输出模式，同时保持通用：不锁格式、不锁传输、不引入重量依赖。

## 三层分离

```
┌─ 计数器层 ────────────────────────────────────┐
│  MetricsRegistry                               │
│  ├─ events_total: CounterVec(event_type, cmd)  │  ← AtomicU64, 零锁
│  ├─ subscribers: Gauge                         │
│  ├─ monitored_paths: Gauge                     │
│  ├─ reader_groups: Gauge                       │
│  ├─ pending_paths: Gauge                       │
│  └─ disk_buffer_events: Gauge                  │
│  gather() → Vec<MetricFamily>                  │  ← 结构化输出
└────────────────────────────────────────────────┘
         │                        │
         ▼                        ▼
┌─ 格式层 ───────────┐   ┌─ 格式层 ───────────┐
│ PrometheusText     │   │ (未来) JSON/OpenMet │
│ format() → String  │   │ format() → String   │
└────────────────────┘   └────────────────────┘
         │                        │
         ▼                        ▼
┌─ 传输层 ───────────────┐   ┌─ 传输层 ─────────────┐
│ socket cmd "metrics"   │   │ TCP HTTP /metrics    │
│ (复用现有 Unix socket) │   │ (可选, tokio task)   │
│ 默认可用, 零开销       │   │ 配 listen 即开启     │
└────────────────────────┘   └──────────────────────┘
```

---

## 设计

### 配置

```toml
# fsmon.toml
# [metrics]
#   TCP HTTP 桥接地址。存在 → 开启；注释掉 → 关闭。
#   socket `metrics` 命令始终可用，无需配置。
# listen = "127.0.0.1:9845"
```

CLI: `--metrics-listen 127.0.0.1:9845`

| config.listen | `--metrics-listen` | socket `metrics` | TCP HTTP |
|---|---|---|---|
| 无 | 无 | ✅ 始终可用 | ❌ |
| `127.0.0.1:9845` | 无 | ✅ | ✅ |
| 无 | `127.0.0.1:9845` | ✅ | ✅ CLI 覆盖 |

### 计数器层（手搓，零依赖）

```rust
// src/metrics.rs

/// 带 label 的 Counter（Prometheus CounterVec 等价）
pub struct CounterVec {
    counters: Arc<RwLock<HashMap<Vec<String>, AtomicU64>>>,
}

/// 简单 IntGauge
pub struct IntGauge {
    value: Arc<AtomicI64>,
}

/// 收集的 metric 快照
pub struct MetricFamily {
    pub name: String,
    pub help: String,
    pub metric_type: MetricType,
    pub metrics: Vec<Metric>,
}

pub struct Metric {
    pub labels: Vec<(String, String)>,
    pub value: MetricValue,
}
```

- `CounterVec` — RwLock over HashMap，读多写少，实际开销微小
- `IntGauge` — AtomicI64，连锁都没有
- `gather()` — 收集所有注册 counter/gauge 的快照
- **不依赖任何外部 crate**

### 格式层

```rust
impl MetricsRegistry {
    /// 输出 Prometheus text format（当前唯一格式，后续可加）
    pub fn format_prometheus(&self) -> String {
        // 遍历 gather() 结果，拼接 Prometheus text
    }
}
```

Prometheus text format 极其简单：
```
# HELP fsmon_events_total Total file system events processed
# TYPE fsmon_events_total counter
fsmon_events_total{event_type="CREATE",cmd="nginx"} 42
```

### 传输层

**Socket `metrics` 命令**（复用现有 socket，和 subscribe 协议一致）：

```
→ cmd = "metrics"
← # HELP fsmon_events_total ...
← fsmon_events_total{...} 42
← (EOF，连接关闭)
```

**TCP HTTP `/metrics`**（可选，一个 tokio task）：

```
GET /metrics HTTP/1.1
↓
HTTP/1.1 200 OK
Content-Type: text/plain; version=0.0.4

fsmon_events_total{...} 42
...
```

---

## 暴露的 Metrics

### Counter（per event_type × cmd）

```
fsmon_events_total{event_type="CREATE",cmd="nginx"} 42
```

- `event_type`: 14 种（CREATE, MODIFY, CLOSE_WRITE ...）
- `cmd`: cmd 组名，`global` 表示无过滤
- 基数 ≤ 14 × ~20 = 280，可控

### Gauge

| 名称 | 含义 | 更新时机 |
|---|---|---|
| `fsmon_subscribers` | 活动 subscribe 连接数 | 连/断时 ±1 |
| `fsmon_monitored_paths` | 监控路径数 | add_path/remove_path |
| `fsmon_reader_groups` | fanotify fd 组数 | 组增减时 |
| `fsmon_pending_paths` | 待创建路径数 | check_pending 后 |
| `fsmon_disk_buffer_events` | 磁盘满时缓冲事件数 | buffer 大小变化 |

---

## 架构（在 Monitor 中的位置）

```
fanotify → reader → mpsc → process_event_batch()
                                │
                   ┌────────────┼────────────┐
                   │            │            │
            broadcast     metrics.      metrics.
            channel       events_total.  gauges
            (subscribe    inc()         set()
             & fw)         │              │
                           └──────┬───────┘
                                  │
                        MetricsRegistry.gather()
                                  │
                          .format_prometheus()
                                  │
                        ┌─────────┴──────────┐
                        │                    │
                  socket handler       TCP HTTP server
                  ("metrics" cmd)     (tokio task, 可选)
```

### Gauge 更新点

| Gauge | 代码位置 |
|---|---|
| `fsmon_subscribers` | `handle_subscribe()` spawn 时 +1，`subscriber_task` 退出时 -1 |
| `fsmon_monitored_paths` | `add_path()` 后 set(len)，`remove_path()` 后 set(len) |
| `fsmon_reader_groups` | `restart_reader()` 后，reader death 后 |
| `fsmon_pending_paths` | `check_pending()` 后 |
| `fsmon_disk_buffer_events` | FileLogWriter `write_event()` 故障时，`flush_disk_buf()` 后 |

---

## 文件改动

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | 无新依赖 |
| `src/config.rs` | 加 `MetricsConfig { listen: Option<String> }` |
| `src/metrics.rs` | **新文件** — CounterVec, IntGauge, MetricsRegistry, format, socket handler, TCP server |
| `src/lib.rs` | `pub mod metrics` |
| `src/socket.rs` | `SocketCmd` 路由 `"metrics"` |
| `src/monitor.rs` | Monitor 加 `metrics: MetricsRegistry`，process 中 inc，各 gauge 更新点 |
| `src/bin/commands/daemon.rs` | `--metrics-listen` CLI arg |
| `src/bin/commands/mod.rs` | 参数传递 |

---

## 注意事项

1. **零依赖** — 不引入 `prometheus` 或任何 metrics crate，CounterVec ~100 行，IntGauge ~30 行
2. **零锁热路径** — CounterVec 用 AtomicU64，读多写少只在 gather 时短暂锁
3. **格式可换** — gather() 返回结构化数据，想加 JSON 格式只需加 format 函数
4. **端口冲突不 crash** — TCP listen 失败 warning，socket `metrics` 不受影响
5. **TCP server 极简** — 手写 HTTP 响应，不加 web framework
