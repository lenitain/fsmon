# PLAN: Extensions 代码质量审计与修复

## 背景

`extensions/` 目录包含 10 个 Python 脚本，README 标注为 "example code (not production-ready)"。经全面审查，存在 6 大类共 28 个具体问题。本计划逐一列举、评估风险、制定修复策略。

## 受审查文件清单

| 子目录 | 文件 | 行数(估) | 依赖 |
|--------|------|----------|------|
| `http-metrics/` | `fsmon-metrics.py` | ~120 | stdlib |
| `jsonl-logs/` | `fsmon-log-tail.py` | ~250 | stdlib |
| `socket-admin/` | `fsmon-admin.py` | ~290 | stdlib |
| `subscribe-stream/` | `fsmon-subscribe-demo.py` | ~70 | stdlib |
| `subscribe-stream/` | `fsmon-webhook.py` | ~110 | stdlib |
| `subscribe-stream/` | `fsmon-kafka.py` | ~110 | kafka-python |
| `subscribe-stream/` | `fsmon-to-es.py` | ~150 | elasticsearch |
| `subscribe-stream/` | `fsmon-to-influxdb.py` | ~120 | influxdb-client |
| `subscribe-stream/` | `fsmon-to-s3.py` | ~110 | boto3 |
| `subscribe-stream/` | `fsmon-custom-format.py` | ~160 | stdlib |

---

## 问题清单（按严重程度分级）

### 🔴 P0 — 数据安全 / 崩溃风险（7 项）

#### P0-1. 7 个文件复制了完全相同的 `subscribe()` 函数

**范围**: `subscribe-stream/` 下全部 7 个文件

**现状**: 每个文件独立实现一个 ~25 行的 `subscribe()` 生成器，内容完全相同（socket 连接、TOML 命令构造、响应解析、JSONL 行迭代）。唯一差异是 `fsmon-subscribe-demo.py` 对 warning 行做了特殊打印，其余 6 个完全一致。

**风险**: 任何 socket 协议变更需修改 7 处；已实际产生分歧（demo 打印 warning，其他跳过）；新增 bridge 会复制第 8 份。

**修复**: 创建 `extensions/lib/fsmon_client.py` 公共模块，提供 `subscribe()` 和 `send_cmd()` 两个核心函数。所有 bridge 脚本改为 import。

---

#### P0-2. JSON 解析失败静默丢弃数据

**范围**: `subscribe-stream/` 下全部 7 个文件 + `fsmon-log-tail.py`

**具体位置**:

| 文件 | 函数 | 代码 | 后果 |
|------|------|------|------|
| 7x subscribe-stream | `subscribe()` | `except json.JSONDecodeError: pass` | 损坏行无任何日志，事件永久丢失 |
| `fsmon-log-tail.py` | `read_events()` L~95 | `except json.JSONDecodeError: continue` | 同上 |
| `fsmon-log-tail.py` | `tail_events()` L~115 | `except json.JSONDecodeError: continue` | 同上 |
| `fsmon-log-tail.py` | `tail_events()` L~135 | `except json.JSONDecodeError: continue` | 同上 |

**风险**: 一行损坏 → 一个事件丢失且无感知。在日志聚合场景中，这会产生不可检测的数据缺口。

**修复**: 至少 `print(f"JSON decode error: {line[:80]}...", file=sys.stderr)`，并增加 `json_errors` 计数器用于 metrics。

---

#### P0-3. 外部写入失败无重试，缓冲区清空后数据永久丢失

**范围**: `fsmon-to-es.py`, `fsmon-to-s3.py`, `fsmon-to-influxdb.py`, `fsmon-webhook.py`, `fsmon-kafka.py`

**具体位置**:

| 文件 | 行 | 问题 |
|------|-----|------|
| `fsmon-to-es.py` | L~128 | `helpers.streaming_bulk()` 返回 `(ok, info)`，不 `ok` 的文档静默丢弃，`done` 只计成功的 |
| `fsmon-to-s3.py` | L~98 | `s3.put_object()` except 后 `buffer.clear()`，所有积压数据永久丢失 |
| `fsmon-to-influxdb.py` | L~129 | `write_api.write()` 无 try/except，写入失败会抛异常终止事件循环 |
| `fsmon-webhook.py` | L~82 | `urllib.request.urlopen(timeout=5)` except 后跳过，事件丢失 |
| `fsmon-kafka.py` | L~107 | `producer.send()` 只返回 Future 不调用 `.get()`，发送失败不可知 |

**风险**:
- ES bulk 部分失败不重试（`raise_on_error=False`）
- S3 上传失败丢弃整个 batch（最多 10000 events）
- InfluxDB 写入异常直接终止脚本
- Webhook 5 秒超时后丢弃事件（无退避、无死信队列）
- Kafka 发送异步无确认

**修复**: 为每个 external writer 添加：
1. 指数退避重试（max 3 次, interval 1s/2s/4s）
2. 重试耗尽后写入死信文件（`/var/log/fsmon-bridge-dlq.jsonl`）
3. 死信大小限制 + 轮转

---

#### P0-4. 内存缓冲区无持久化，进程崩溃全丢

**范围**: `fsmon-to-es.py`, `fsmon-to-s3.py`

**具体位置**:

| 文件 | 变量 | 问题 |
|------|------|------|
| `fsmon-to-es.py` | `buffer = []` | buffer 最大 `flush_count` (default 1000)，崩溃时最多丢 1000 events |
| `fsmon-to-s3.py` | `buffer = []` | buffer 最大 `flush_count` (default 10000)，崩溃时最多丢 10000 events |

**风险**: OOM kill、SIGKILL、电源故障 → 所有未 flush 数据永久丢失。

**修复**: 添加可选的 WAL（write-ahead log）机制：每个事件到达时先追加到本地 JSONL WAL 文件，flush 成功后 truncate。

---

#### P0-5. InfluxDB line protocol 注入风险

**范围**: `fsmon-to-influxdb.py` L~121-127

**现状**:
```python
line = (
    f"fsmon_events,"
    f"event_type={ev.get('event_type','?')},"
    f"cmd={ev.get('cmd','?')},"
    f"path={ev.get('path','?').replace(' ', '\\ ').replace(',', '\\,')} "
    ...
)
```

**问题**:
- `event_type` 未转义：空格、逗号、`=` 会破坏 measurement/tag 结构
- `cmd` 同上
- `path` 仅转义了空格和逗号，未处理 `=` 和换行符
- 文件路径可能包含任意字符（来自内核 `fanotify` 事件）

**风险**: InfluxDB 拒绝写入或数据损坏。恶意构造的路径名可注入任意 line protocol 字段。

**修复**: 实现完整的 InfluxDB line protocol 转义函数：measurement 中 `,` ` ` → `\`；tag key/value 中 `,` `=` ` ` → `\`；field string 中 `"` → `\"`。

---

#### P0-6. socket 路径硬编码 UID=1000

**范围**: `fsmon-metrics.py`, `fsmon-subscribe-demo.py`, `fsmon-webhook.py`, `fsmon-kafka.py`, `fsmon-to-es.py`, `fsmon-to-influxdb.py`, `fsmon-to-s3.py`, `fsmon-custom-format.py`

**现状**: 全部 8 个文件 hardcode `default="/tmp/fsmon-1000.sock"`

**对比**: `fsmon-admin.py` 是**唯一**正确处理的：
```python
sudo_uid = os.environ.get("SUDO_UID")
uid = sudo_uid if sudo_uid else str(os.getuid())
return f"/tmp/fsmon-{uid}.sock"
```

**修复**: 统一使用 `extensions/lib/fsmon_client.py` 中的 `get_socket_path()` 函数。

---

#### P0-7. `sys.exit()` 在非 main 函数中调用

**范围**: `fsmon-admin.py` L~81-92, `fsmon-log-tail.py` L~48-54

**具体位置**:

| 文件 | 函数 | 代码 |
|------|------|------|
| `fsmon-admin.py` | `send_cmd()` | `sys.exit(1)` (5 处) |
| `fsmon-log-tail.py` | `find_log_files()` | `sys.exit(1)` (2 处) |

**风险**: `send_cmd()` 作为工具函数被复用时会意外终止整个进程。`sys.exit()` 只应在 `main()` 或 `if __name__` 块中调用。

**修复**: 改为 `raise` 自定义异常，在 `main()` 中 catch 并 `sys.exit()`。

---

### 🟠 P1 — 可靠性 / 可运维性（7 项）

#### P1-1. 所有 subscribe 脚本缺少优雅关闭

**范围**: subscribe-stream 下全部 7 个文件

**现状**: `for ev in subscribe(...):` 是无限循环，`Ctrl+C` 触发 `KeyboardInterrupt` 后直接终止：
- socket 不 close → 服务端残留连接
- Kafka producer 在 `fsmon-kafka.py` 中有 `producer.flush(); producer.close()` 但写在循环**之后**，`KeyboardInterrupt` 时不会执行
- ES / S3 的 `finally: flush()` 会执行（好的），但 socket 不关

**修复**: 所有脚本添加 `signal.signal(SIGTERM, handler)` + `finally` 块确保 socket 关闭和外部连接 cleanup。

---

#### P1-2. 无 Self-monitoring / 健康指标

**范围**: subscribe-stream 下全部 7 个文件

**现状**: 各 bridge 脚本没有暴露任何自身指标。运维时无法回答：
- 这个 bridge 还在运行吗？
- 处理速率是多少？
- 错误率多少？
- 队列积压多少？

唯一的外部可见信号是每 N events 的 print（如 `[webhook] sent 100 events`），但这只是 stdout 文本，需要人工观察。

**修复**: 可选方案：
- a) 定期输出 JSON 格式的 stats line 到 stderr（最简）
- b) 暴露一个本地 HTTP `/health` 端点（推荐）
- c) 写入 statsd/prometheus pushgateway

---

#### P1-3. 时间解析不兼容 Python < 3.11

**范围**: 所有文件的 `datetime.fromisoformat()`

**现状**: `fromisoformat()` 对任意 ISO 8601 偏移（如 `+08:00`）的完整支持在 Python 3.11 引入。3.10 及以下可能抛出异常。所有调用处虽然包裹了 `except`，但回退策略是 `datetime.min` 或 `datetime.now()`，可能产生语义错误的静默数据。

**修复**: 文档说明最低 Python 版本要求为 3.11，或实现兼容层（手动解析 Z/+offset）。

---

#### P1-4. `fsmon-log-tail.py` 的 `tail_events()` 没有持久化读取位置

**范围**: `fsmon-log-tail.py` L~105-145

**现状**: 每次启动重新从文件头读取全部已有日志，然后用 `file_positions` 字典在内存中跟踪进度。重启后进度丢失 → 要么重新读取全部日志（默认行为），要么通过 `--last 5m` 滤波。没有像 `tail -F` 那样持久化 inode/offset。

**风险**: 重启后会重放所有历史事件，对下游系统造成重复。

**修复**: 可选，至少文档说明此行为。更完善的方案是用 `seek` offset 持久化到 `.fsmon-tail-pos` 文件。

---

#### P1-5. `fsmon-webhook.py` 无重试、无队列、无背压

**范围**: `fsmon-webhook.py` L~76-82

**现状**:
```python
def send_webhook(url, event):
    try:
        urllib.request.urlopen(req, timeout=5)
    except Exception as e:
        print(...)
```
- 5 秒超时硬编码，高延迟网络不适用
- 阻塞式同步 HTTP，事件产生速度 > HTTP 响应速度 → socket 缓冲区满 → 背压到 fsmon daemon
- 无并发、无队列

**修复**: 添加 ThreadPoolExecutor + 内部队列 + 重试。

---

#### P1-6. Kafka `producer.send()` 后无 `flush()` 间隔

**范围**: `fsmon-kafka.py` L~107

**现状**: `producer.send()` 每事件调用一次但从不 `flush()`，只在最后（不可达代码）`producer.flush(); producer.close()`。Kafka 内部按 `linger_ms` / `batch_size` 自动批处理，这是正确的，但缺少显式 flush 意味着关闭时的逻辑不可达。

**修复**: 将 `flush`/`close` 移到 `finally` 块。

---

#### P1-7. 缺少日志框架，全部 `print(..., file=sys.stderr)`

**范围**: 全部 10 个文件

**现状**: 所有输出用 `print()`。无法按级别过滤、无时间戳前缀、无法输出到 syslog/文件。

**修复**: 添加 `logging` 模块集成，至少区分 INFO/WARNING/ERROR。

---

### 🟡 P2 — 代码质量 / 可维护性（7 项）

#### P2-1. 零类型标注（9/10 文件）

**范围**: `fsmon-admin.py` 有部分标注（`-> dict`, `-> str`等），其余 9 个文件完全没有类型注解。

**修复**: 为所有公共函数和核心逻辑添加类型注解（`mypy --strict` 兼容）。

---

#### P2-2. 缺少输入校验

**范围**: 多个文件

| 文件 | 输入 | 问题 |
|------|------|------|
| `fsmon-admin.py` | `args.path` | 直接放入 TOML，无路径合法性校验 |
| `fsmon-admin.py` | `args.types` | 未验证是否为有效 FANOTIFY 事件类型 |
| `fsmon-log-tail.py` | `args.last` | `parse_duration("0s")` 合法但无意义；空字符串会 crash |
| subscribe-stream | `args.types` | 同上，无效 type 发给 daemon 会被静默忽略 |

**修复**: 添加 `choices` 或自定义 `type` 函数做校验。

---

#### P2-3. 硬编码常量分散

**范围**: 全部文件

| 常量 | 多文件出现 | 应集中 |
|------|-----------|--------|
| `"cmd"`, `"subscribe"`, `"add"`, `"remove"`, `"list"`, `"health"` | 所有 socket 脚本 | `fsmon_client.py` |
| `"ok = true"` | 7x subscribe | 同上 |
| `"event_type"`, `"path"`, `"pid"`, `"cmd"`, `"time"` | 全 10 文件 | `fsmon_client.py` |
| `"/tmp/fsmon-1000.sock"` | 8 文件 | `get_socket_path()` |

**修复**: 集中到 `lib/` 模块。

---

#### P2-4. session/connection 泄漏风险

**范围**: 所有 socket 脚本

**现状**: `s = socket.socket(...)` 创建后未在 `finally` 中 `close()`。虽然在 subscribe 模式下 socket 是长连接（直到进程退出），但良好的实践应当处理。

**修复**: 使用 `with socket.socket(...) as s:` 或 `contextlib.closing`。

---

#### P2-5. 手动 TOML 序列化脆弱

**范围**: `fsmon-admin.py` L~65-81 + 所有 subscribe 脚本的 TOML 构造

**现状**: 手工拼接 TOML 字符串：
```python
toml_lines.append(f'{key} = "{value}"')   # 如果 value 包含 " 会破坏 TOML
toml_lines.append(f"types = [{types}]")   # 如果 types 元素含特殊字符会错误
```

虽然有 `fsmon-admin.py` 的 `_parse_toml_value()` 做反序列化，但序列化侧没有转义保护。不过由于 fsmon 的 TOML 仅用于简单配置（路径、cmd 名），实际风险受控。

**修复**: 低成本方案：在值中 `"` → `\"`。更完善方案：用 `tomllib`/`tomli`（Python 3.11+ 内置 `tomllib`）。

---

#### P2-6. `fsmon-admin.py` 中 `_parse_toml_response()` 与 `send_cmd()` 耦合

**范围**: `fsmon-admin.py`

**现状**: `send_cmd()` 构建 TOML 请求，`_parse_toml_response()` 解析 TOML 响应。两者都有 mini TOML 实现，逻辑碎片化。且 `_parse_toml_response` 仅处理 fsmon 返回的子集（无嵌套表、无多行字符串）。

**修复**: 与 TOML 序列化一体重构，集中到 `lib/` 模块。

---

#### P2-7. 错误信息不区分场景

**范围**: `fsmon-admin.py` L~81-92

**现状**: `socket.timeout`、`FileNotFoundError`、`ConnectionRefusedError` 各有一个 `sys.exit(1)`，均给出同一级别的错误信息，无结构化错误码。

**修复**: 使用自定义异常层次：`FsmonConnectionError(ConnectionRefusedError)`, `FsmonTimeoutError(socket.timeout)` 等，`main()` 中统一处理并映射 exit code。

---

### 🔵 P3 — 文档 / 使用体验（7 项）

#### P3-1. docstring 中 `--log-dir` 硬编码 `/var/log/fsmon`

**范围**: `fsmon-log-tail.py` docstring

**现状**: 顶部的 Quick Start 示例写死了 `/var/log/fsmon`，但代码默认值也是它。对非标准安装的用户需要改代码。

**修复**: docstring 中用占位符 `<LOG_DIR>` 或引用 `--log-dir` 参数。

---

#### P3-2. `fsmon-metrics.py` `--watch` 模式下无首次延迟

**范围**: `fsmon-metrics.py` L~80

**现状**: `while True: pull_metrics(); sleep(watch)`. 如果 daemon 响应极快，每分钟可能产生大量输出。且首次 pull 后立即 sleep，用户体验无差异。

**修复**: 低优先级，可选加 `--once` 显式语义。

---

#### P3-3. `fsmon-metrics.py` `parse_summary()` 中 magic string 过滤

**范围**: `fsmon-metrics.py` L~60-65

**现状**:
```python
total = sum(v for k, v in info.items() if isinstance(v, int) 
    and k.startswith(("CREATE", "MODIFY", "DELETE", "ACCESS", "OPEN", "CLOSE", "MOVE", "ATTRIB", "FS_ERROR")))
```
硬编码了 9 种事件类型。如果 daemon 增加新类型（如 `FS_OPEN_PERM`），这里不会累加到 total。

**修复**: 改为排除法：排除 `subscribers`, `monitored_paths`, `reader_groups`, `pending_paths`, `disk_buf`，其余全部累加。

---

#### P3-4. `fsmon-to-es.py` 的 `to_es_doc()` 选择性映射字段

**范围**: `fsmon-to-es.py` L~85-95

**现状**: 映射了 `time, event_type, path, pid, cmd, user, file_size, ppid, tgid, chain`。如果 daemon 未来新增字段（如 `gid`, `inode`），会被静默丢弃。

**修复**: 改为动态映射（`**{k: v for k, v in ev.items() if k != "_index"}`）或至少记录 unmapped 字段。

---

#### P3-5. 所有 subscribe 脚本使用 `--types` 参数名不一致

**范围**: subscribe-stream 下全部 7 个文件

**现状**: argparse 用 `--types`（复数），内部变量叫 `type_filter`（单数）。虽然不是 bug，但命名一致性差。

**修复**: 统一为 `--types` → `types_filter`。

---

#### P3-6. ES date-rolling index 格式不够粒度

**范围**: `fsmon-to-es.py` L~133

**现状**: `{args.index}-{ts:%Y.%m.%d}` — 每日一个索引。高吞吐场景（如监控整个文件系统）一天可能产生百万级文档，单索引过大影响查询性能。

**修复**: 可选，提供 `--index-granularity {daily,hourly}` 参数。

---

#### P3-7. `fsmon-custom-format.py` 的 Loki 格式不完整

**范围**: `fsmon-custom-format.py` `format_loki()` L~144

**现状**: Loki 推送 API 期望的是 `{"streams": [{"stream": {...labels...}, "values": [["<nanosec>", "<line>"]]}]}` JSON 结构，而当前输出是纯文本 logfmt 行。文档中说 `| curl ... --data-binary @-` 可行，但 `@-` 传的是原始文本，Loki 的 `/loki/api/v1/push` 实际需要上述 JSON。

**修复**: 修正为输出正确的 Loki push JSON 格式，或改为输出 logfmt 并更新文档中的消费方式（用 Promtail 而非直接 curl）。

---

## 修复策略

### 阶段 1: 提取公共模块（P0-1, P0-6, P2-3, P2-5, P2-6）

创建 `extensions/lib/fsmon_client.py`：

```
extensions/lib/
├── __init__.py
└── fsmon_client.py   # subscribe(), send_cmd(), get_socket_path(), TOML helpers
```

**`fsmon_client.py` 导出的 API**:
- `get_socket_path() -> str` — 统一 socket 路径计算
- `subscribe(socket_path, track_cmd, types_filter) -> Generator[dict]` — 事件流生成器
- `send_cmd(cmd_dict, socket_path) -> dict` — 发送管理命令并解析响应
- `FsmonConnectionError`, `FsmonTimeoutError`, `FsmonProtocolError` — 异常层次

**影响范围**: subscribe-stream 下 7 个文件的 `subscribe()` 函数删除，改为 import；`socket-admin/fsmon-admin.py` 的 socket 逻辑委托给 `send_cmd()`。

---

### 阶段 2: 数据可靠性（P0-2 ~ P0-5, P0-7）

#### 2a. 添加 `ErrorCounter` + 结构化错误输出

在 `fsmon_client.py` 中：
- `subscribe()` 中 JSON 解析失败 → `log_error("json_decode", line[:100])`
- 暴露 `get_error_stats() -> dict`

#### 2b. 添加 `RetryWriter` 基类

```python
class RetryWriter:
    """Base for external writers with retry + dead letter queue."""
    def write_with_retry(self, data, max_retries=3, backoff_base=1.0):
        ...
```

各 bridge 继承并实现 `_do_write()`。

#### 2c. 修复 InfluxDB line protocol 转义

独立的 `escape_influxdb_tag()`, `escape_influxdb_field()` 函数。

#### 2d. P0-7 异常重构

`fsmon-admin.py` 和 `fsmon-log-tail.py` 中 `sys.exit()` 替换为 raise 异常。

---

### 阶段 3: 可靠性与可观测性（P1-1 ~ P1-7）

#### 3a. 优雅关闭

所有 subscribe 脚本添加 `signal` handler + `finally` 清理块。

#### 3b. 健康检查

为每个 bridge 添加 `--health-port` 可选参数，暴露 `/health` 端点：
```json
{"status": "ok", "events_processed": 12345, "errors": 3, "last_event_at": "..."}
```

#### 3c. 日志框架

`logging` 替代 `print(..., file=sys.stderr)`，至少 stderr handler + 可选 `--log-file`。

---

### 阶段 4: 代码质量（P2-1 ~ P2-4, P2-7）

- 全部函数添加类型注解
- 输入校验（argparse `choices` / `type=`）
- 常量集中

---

### 阶段 5: 文档与边缘情况（P3-1 ~ P3-7）

- 修正 docstring 示例
- 修正 Loki 格式
- `fsmon-metrics.py` 事件类型改为排除法
- ES mapping 改为动态

---

## 文件变更总览

| 操作 | 文件 |
|------|------|
| **新建** | `extensions/lib/__init__.py` |
| **新建** | `extensions/lib/fsmon_client.py` |
| **新建** | `extensions/lib/retry_writer.py` |
| **修改** | `extensions/subscribe-stream/fsmon-subscribe-demo.py` |
| **修改** | `extensions/subscribe-stream/fsmon-webhook.py` |
| **修改** | `extensions/subscribe-stream/fsmon-kafka.py` |
| **修改** | `extensions/subscribe-stream/fsmon-to-es.py` |
| **修改** | `extensions/subscribe-stream/fsmon-to-influxdb.py` |
| **修改** | `extensions/subscribe-stream/fsmon-to-s3.py` |
| **修改** | `extensions/subscribe-stream/fsmon-custom-format.py` |
| **修改** | `extensions/socket-admin/fsmon-admin.py` |
| **修改** | `extensions/http-metrics/fsmon-metrics.py` |
| **修改** | `extensions/jsonl-logs/fsmon-log-tail.py` |
| **修改** | `extensions/README.md` |

---

## 测试计划

| 测试 | 覆盖 |
|------|------|
| `test_fsmon_client.py` | `get_socket_path()`, `send_cmd()`, TOML 序列化/反序列化, 异常层次 |
| `test_retry_writer.py` | 重试逻辑、退避时间、死信队列写入 |
| `test_influxdb_escape.py` | line protocol 转义边界情况 |
| 各 bridge 脚本的 import 测试 | 确保 import 路径正确 |

---

## 时间估算

| 阶段 | 预估 |
|------|------|
| P0（公共模块 + 数据安全） | 主要工作 |
| P1（可靠性 + 可观测性） | 中等 |
| P2（代码质量） | 较快 |
| P3（文档） | 快速 |
