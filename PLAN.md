# PLAN: Extensions 代码质量审计与修复（v2）

## 设计原则

每个桥接脚本**完全自包含**。用户可以单独复制 `fsmon-kafka.py` 到任意机器，一行命令即可运行，无需安装 extras/ 项目的其他任何文件。用 Kafka 的人不关心 Elasticsearch 怎么桥接，也不该被迫接触它的代码或依赖。

**实现手段**:
- **PEP 723 inline script metadata**：每个 `.py` 文件头部声明自身依赖，`uv run script.py` 自动创建隔离 venv
- **接受有意的重复**：`subscribe()` 函数在 7 个文件中各自存在（~20 行），不提取公共模块。代价 < 隔离性的收益
- **`_templates/` 目录**：放置经审查的「canonical 实现片段」，新增 bridge 时从此复制，不作为 import 目标
- **`pyproject.toml` 仅用于开发**：pytest、mypy 等 dev dependency，与脚本运行时无关

## 受审查文件清单

| 子目录 | 文件 | 行数(估) | 运行时依赖 |
|--------|------|----------|-----------|
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

## 阶段 0: PEP 723 inline script metadata

### 目标

每个脚本 `uv run` 即用，无需先 `uv sync`。

### 方案

每个脚本文件顶部增加 PEP 723 metadata block：

```python
#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "kafka-python>=2.0",
# ]
# ///
"""
fsmon -> Kafka bridge
...
"""
```

stdlib-only 脚本（metrics, log-tail, admin, subscribe-demo, webhook, custom-format）可省略 `dependencies` 或写空数组：

```python
#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
```

### 需新增/修改

| 文件 | 操作 | 说明 |
|------|------|------|
| `extensions/pyproject.toml` | 新建 | dev dependencies (pytest, mypy) + requires-python >= 3.11 |
| 全部 10 个 `.py` | 修改 | 顶部加 PEP 723 block |

### .python-version & pyproject.toml

```
extensions/
├── pyproject.toml     # dev-only: pytest, mypy, requires-python >= 3.11
├── _templates/        # canonical 实现片段（阶段 1）
├── tests/             # 测试文件（阶段 1）
└── ...
```

```toml
# pyproject.toml —— 仅用于开发
[project]
name = "fsmon-extensions-dev"
version = "0.1.0"
requires-python = ">=3.11"

[tool.uv]
dev-dependencies = [
    "mypy>=1.0",
    "pytest>=8.0",
]

[tool.pytest.ini_options]
testpaths = ["tests"]

[tool.mypy]
strict = true
python_version = "3.11"
```

---

## 问题清单（按严重程度分级）

### 🔴 P0 — 数据安全 / 崩溃风险（8 项）

#### P0-1. `subscribe()` 在 7 个文件中重复，且存在分歧

**范围**: `subscribe-stream/` 下全部 7 个文件

**现状**: 每个文件实现一个 ~25 行的 `subscribe()` 生成器。核心逻辑完全相同，但已产生微小分歧：
- `fsmon-subscribe-demo.py`：对 warning 行做了 `print(f"[!] ...")` 处理
- 其余 6 个：对 warning 行 `continue`（静默跳过）

**决策**: **不提取公共模块**。隔离性优先级高于 DRY。代价是 7 份代码需独立维护，收益是每个脚本可单独复制使用。

**修复**:
1. 在 `_templates/subscribe.py` 中提供 canonical 实现（经审查、有类型标注、有错误处理）
2. 将 7 个脚本的 `subscribe()` 统一对齐到 canonical 版本
3. 新增 bridge 时从此 template 复制
4. demo.py 的特殊 warning 处理作为参数化选项加入 canonical 实现

---

#### P0-2. JSON 解析失败静默丢弃数据

**范围**: `subscribe-stream/` 下全部 7 个文件 + `fsmon-log-tail.py`

**具体位置**:

| 文件 | 函数 | 代码 | 后果 |
|------|------|------|------|
| 7x subscribe-stream | `subscribe()` | `except json.JSONDecodeError: pass` | 损坏行无任何日志 |
| `fsmon-log-tail.py` | `read_events()` | `except json.JSONDecodeError: continue` (3 处) | 同上 |

**风险**: 数据缺口不可检测。

**修复**: 每个脚本独立添加：
```python
except json.JSONDecodeError:
    print(f"[{time.strftime('%H:%M:%S')}] JSON decode error: {line[:120]}", file=sys.stderr)
    json_errors += 1
```
并在定期 stats 中输出 `json_errors` 计数。

---

#### P0-3. 外部写入失败无重试，数据永久丢失

**范围**: `fsmon-to-es.py`, `fsmon-to-s3.py`, `fsmon-to-influxdb.py`, `fsmon-webhook.py`, `fsmon-kafka.py`

| 文件 | 问题 |
|------|------|
| `fsmon-to-es.py` | `streaming_bulk(raise_on_error=False)` — 部分失败 doc 静默丢弃 |
| `fsmon-to-s3.py` | `put_object()` except 后 `buffer.clear()` — 整个 batch 丢失 |
| `fsmon-to-influxdb.py` | `write()` 无 try/except — 异常终止事件循环 |
| `fsmon-webhook.py` | HTTP 5s 超时 → except → 跳过 |
| `fsmon-kafka.py` | `producer.send()` 不调用 `.get()` — 发送失败不可知 |

**修复**: 每个 bridge 独立实现重试逻辑（形式因后端而异）：

| Bridge | 重试策略 |
|--------|---------|
| Webhook | 指数退避，max 3 次 (1s/2s/4s)，失败写死信文件 |
| InfluxDB | 同上 |
| Kafka | `send().get(timeout=5)` + retry on KafkaError |
| ES | bulk 失败项收集，单独重试 1 次 |
| S3 | upload 重试 3 次，失败写死信文件 |

死信文件路径通过 `--dlq-dir` 指定（默认 `$TMPDIR/fsmon-dlq`），不使用 `/var/log/`（避开 root 权限要求）。

---

#### P0-4. 内存缓冲区无持久化，进程崩溃全丢

**范围**: `fsmon-to-es.py` (buffer max ~1000), `fsmon-to-s3.py` (buffer max ~10000)

**修复**: 可选 WAL。每个事件到达时先追加本地 JSONL WAL 文件，flush 成功后 truncate。通过 `--wal-dir` 指定路径。如果未指定则跳过（保持简单）。

---

#### P0-5. InfluxDB line protocol 注入风险

**范围**: `fsmon-to-influxdb.py`

**现状**: `path` 字段仅转义空格和逗号，未处理 `=`、换行符；`event_type`/`cmd` 完全未转义。文件路径来自内核 fanotify，可能含任意字符。

**修复**: 实现完整 line protocol 转义：
- tag key/value: `,` → `\,`, `=` → `\=`, ` ` → `\ `
- field string: `"` → `\"`, `\` → `\\`
- measurement: `,` → `\,`, ` ` → `\ `

---

#### P0-6. socket 路径硬编码 UID=1000

**范围**: 8 个文件 `default="/tmp/fsmon-1000.sock"`

**对比**: `fsmon-admin.py` 正确使用 `SUDO_UID` / `os.getuid()` 动态计算。

**修复**: 每个脚本的 `get_socket_path()` 实现改为与 `fsmon-admin.py` 一致的逻辑。此函数将纳入 `_templates/socket_helpers.py`。

---

#### P0-7. `sys.exit()` 在非 main 函数中调用

**范围**: `fsmon-admin.py` 的 `send_cmd()` (5 处), `fsmon-log-tail.py` 的 `find_log_files()` (2 处)

**修复**: 改为 raise 自定义异常 (`FsmonError` 子类)，在 `main()` 中统一 catch + exit。

---

#### P0-8. `subscribe()` 断连后不重连（新增）

**范围**: subscribe-stream 下全部 7 个文件

**现状**: daemon 重启或 socket 断开时，`for line in reader` 静默退出，bridge 进程终止。运维中 daemon 重启是常态。

**修复**: `subscribe()` 外层包裹重连循环：
```python
while True:
    try:
        for ev in _subscribe_inner(socket_path, ...):
            yield ev
    except (ConnectionError, socket.timeout, BrokenPipeError):
        print(f"[reconnect] waiting {delay}s...", file=sys.stderr)
        time.sleep(delay)
        delay = min(delay * 2, 60)  # exponential backoff capped at 60s
```

---

### 🟠 P1 — 可靠性 / 可运维性（7 项）

#### P1-1. 缺少优雅关闭

**范围**: subscribe-stream 下全部 7 个文件

**现状**: `Ctrl+C` → `KeyboardInterrupt` → 直接终止。socket 不 close，Kafka producer 的最后 `flush()/close()` 不可达。

**修复**: 每个脚本添加 `signal(SIGTERM, handler)` + `try/finally` 确保：
1. socket.close()
2. 外部连接 cleanup（producer.flush + close, ES/S3 final flush）
3. 最后一条 stats line 输出

---

#### P1-2. 无 Self-monitoring / 健康指标

**范围**: subscribe-stream 下全部 7 个文件

**修复**: 每个脚本定期输出一行 JSON stats 到 stderr：
```json
{"ts":"...","events":12345,"errors":3,"json_errors":0,"reconnects":1}
```
可被 `jq` 或监控脚本消费。不引入 HTTP 端点（与隔离性冲突）。

---

#### P1-3. `datetime.fromisoformat()` 依赖 Python 3.11+

**范围**: 所有文件

**修复**: `.python-version` 锁定 3.11，PEP 723 声明 `requires-python = ">=3.11"`。现有 try/except 回退保留作为防御性编程。

---

#### P1-4. `fsmon-log-tail.py` 读取位置不持久化

**范围**: `fsmon-log-tail.py` `tail_events()`

**现状**: 重启后从头读取全部日志或靠 `--last` 滤波。没有持久化 offset。

**修复**: 添加可选 `--pos-file` 参数，持久化 {inode: offset} 到 JSON 文件。默认不使用（保持简单）。

---

#### P1-5. `fsmon-webhook.py` 同步阻塞 HTTP

**范围**: `fsmon-webhook.py`

**现状**: 阻塞式 `urlopen(timeout=5)`，事件产生速度快于 HTTP 响应速度时，背压传导到 fsmon daemon。

**修复**: 用 `queue.Queue` + `threading.Thread` 做异步发送（仍然 stdlib，不引入 aiohttp）。

---

#### P1-6. Kafka `producer.send()` 后 flush 不可达

**范围**: `fsmon-kafka.py`

**修复**: `producer.flush(); producer.close()` 移入 `finally` 块（与 P1-1 优雅关闭合并）。

---

#### P1-7. 全部用 `print()` 而非 `logging`

**范围**: 全部 10 个文件

**修复**: 每个脚本使用 `logging` 模块。stderr StreamHandler 为默认，可选 `--log-file` 参数。不影响自包含性（logging 是 stdlib）。

---

### 🟡 P2 — 代码质量 / 可维护性（8 项）

#### P2-1. 零类型标注（9/10 文件）

**范围**: `fsmon-admin.py` 有部分标注，其余 9 个无。

**修复**: 所有函数添加类型注解，达到 mypy strict。

---

#### P2-2. 缺少输入校验

| 文件 | 输入 | 问题 |
|------|------|------|
| `fsmon-admin.py` | `args.path` | 无路径合法性校验 |
| `fsmon-admin.py` | `args.types` | 未验证 FANOTIFY 事件类型 |
| `fsmon-log-tail.py` | `args.last` | `parse_duration("0s")` 无意义，空字符串 crash |
| subscribe-stream | `args.types` | 无效 type 被 daemon 静默忽略 |

**修复**: argparse `choices` / 自定义 `type=` 函数。

---

#### P2-3. 手动 TOML 序列化脆弱

**范围**: `fsmon-admin.py` + 所有 subscribe 脚本

**现状**: 手工拼接 TOML 无转义。风险受控（仅简单配置），但值中含 `"` 会破坏格式。

**修复**: `_templates/toml_helpers.py` 提供 canonical `dict_to_toml()` / `parse_toml()` 实现（零依赖，用 `str.replace` 做最小转义）。各脚本复制使用。

---

#### P2-4. socket 未使用 context manager

**范围**: 所有 socket 脚本

**修复**: `with socket.socket(...) as s:` 或 `contextlib.closing`。

---

#### P2-5. 错误信息无结构化错误码

**范围**: `fsmon-admin.py`

**现状**: `socket.timeout`、`FileNotFoundError`、`ConnectionRefusedError` 均 `sys.exit(1)`，无区分。

**修复**: 自定义异常层次 + 不同 exit code (1=连接失败, 2=协议错误, 3=超时)。

---

#### P2-6. `--types` 与内部变量名不一致

**范围**: subscribe-stream 下全部 7 个文件

**现状**: argparse `--types`（复数），内部变量 `type_filter`（单数）。

**修复**: 统一为 `--types` → `types_filter`。

---

#### P2-7. `fsmon-metrics.py` `pull_metrics()` 不关闭 socket

**范围**: `fsmon-metrics.py`

**修复**: `with` 语句或显式 `s.close()`。

---

#### P2-8. `fsmon-metrics.py` `parse_summary()` 中 `int("")` 崩溃风险

**范围**: `fsmon-metrics.py` L~55

**现状**:
```python
value = int(parts[1].strip() if len(parts) > 1 else 0)
```
`parts[1].strip()` 可能为空字符串 `""`，`int("")` → `ValueError` 崩溃。

**修复**: `int(parts[1].strip() or "0")`。

---

### 🔵 P3 — 文档 / 使用体验（8 项）

#### P3-1. docstring 示例路径硬编码

**范围**: `fsmon-log-tail.py` docstring 写死 `/var/log/fsmon`

**修复**: 用 `<LOG_DIR>` 占位符，或引用 `--log-dir`。

---

#### P3-2. `fsmon-metrics.py` event type 硬编码列表

**范围**: `fsmon-metrics.py` `parse_summary()` 硬编码 9 种事件类型名

**修复**: 排除法：排除已知 gauge 名 (`subscribers`, `monitored_paths`, `reader_groups`, `pending_paths`, `disk_buf`)，其余全部累加。

---

#### P3-3. `fsmon-to-es.py` `to_es_doc()` 字段选择性映射

**范围**: `fsmon-to-es.py`

**现状**: 手动列举 10 个字段。daemon 新增字段会被静默丢弃。

**修复**: 动态映射（排除内部字段如 `_index`）。

---

#### P3-4. ES 索引粒度仅日级

**范围**: `fsmon-to-es.py`

**修复**: 添加 `--index-granularity {daily,hourly}` 可选参数。

---

#### P3-5. Loki 输出格式不完整

**范围**: `fsmon-custom-format.py` `format_loki()`

**现状**: 输出 logfmt 文本行，文档建议直接 curl 到 Loki push API。但 Loki API 期望 JSON `{"streams": [...]}` 结构。

**决策**: **不做 Loki push client**（超出格式转换职责）。改为：
- `format_loki()` 保持输出 logfmt 行
- 文档改为「用 Promtail 采集此输出」或「pipe 到 Loki docker driver」

---

#### P3-6. `fsmon-metrics.py` `--watch` 无首次延迟

**范围**: `fsmon-metrics.py`

**修复**: 低优先级，可选 `--once` flag。

---

#### P3-7. daemon 依赖说明不完整

**范围**: subscribe-stream 下全部 7 个文件 + `fsmon-metrics.py`

**现状**: docstring 的 Prerequisites 写了 `sudo fsmon daemon`，但如果用户是初次接触 fsmon，他不知道 fsmon 是什么、去哪安装。subscribe 脚本连接失败时只输出 `subscribe failed:...`，不提示 daemon 未运行。

**风险**: 用户拿到脚本后卡在第一步，不知道依赖链路。

**修复**:
1. 每个脚本 docstring Prerequisites 顶部加：`fsmon — file system monitor (https://github.com/xxx/fsmon)`
2. subscribe 脚本 connect 失败时输出：`Is the daemon running? Start with: sudo fsmon daemon`
3. socket 文件不存在时额外提示：`Install fsmon: https://github.com/xxx/fsmon`

---

#### P3-8. 各脚本 docstring 需统一 PEP 723 运行示例

**范围**: 全部 10 个文件

**现状**: docstring 的 Quick Start 用 `python3 script.py`

**修复**: 改为 `uv run script.py`（推荐）+ 保留 `python3` 备选说明。

---

## 修复策略

### 阶段 1: PEP 723 + `_templates/` 基础设施（P0-1, P0-6, P2-3）

创建 `extensions/_templates/` 目录，放入经审查的 canonical 实现：

```
extensions/_templates/
├── subscribe.py       # canonical subscribe() 生成器（含重连逻辑 P0-8）
├── socket_helpers.py  # get_socket_path(), send_cmd() + 异常层次
├── toml_helpers.py    # dict_to_toml(), parse_toml_response()
├── retry.py           # retry 装饰器/函数（指数退避）
└── stats.py           # JSON stats line 输出
```

**注意**: 这些文件**不被 import**。它们是「源码级别的 canonical 参考」——新增 bridge 时从此复制，避免重新发明并引入分歧。现有 7 个 subscribe 脚本对齐到 canonical 实现后，各自内联所有代码。

同时：全部 10 个脚本顶部加 PEP 723 block，创建 `.python-version`、`pyproject.toml`。

---

### 阶段 2: 数据可靠性（P0-2, P0-3, P0-4, P0-5, P0-7, P0-8）

每个 bridge 脚本独立实现：
- JSON 解析错误 → log + count
- 外部写入重试 + 死信队列
- WAL（可选）
- InfluxDB line protocol 完整转义
- `subscribe()` 内层重连循环
- 异常替换 `sys.exit()`

---

### 阶段 3: 可靠性与可观测性（P1-1 ~ P1-7）

- signal handler + finally cleanup
- stderr JSON stats
- webhook 异步队列
- logging 模块

---

### 阶段 4: 代码质量（P2-1 ~ P2-8）

- 类型标注
- 输入校验
- socket context manager
- 变量命名统一
- `int("")` bugfix

---

### 阶段 5: 文档（P3-1 ~ P3-8）

- docstring 示例更新
- Loki 方向修正
- 事件类型排除法
- ES 动态映射

---

## 文件变更总览

| 操作 | 文件 | 阶段 |
|------|------|------|
| **新建** | `extensions/pyproject.toml` | 1 |

| **新建** | `extensions/_templates/subscribe.py` | 1 |
| **新建** | `extensions/_templates/socket_helpers.py` | 1 |
| **新建** | `extensions/_templates/toml_helpers.py` | 1 |
| **新建** | `extensions/_templates/retry.py` | 1 |
| **新建** | `extensions/_templates/stats.py` | 1 |
| **新建** | `extensions/tests/` (目录) | 1 |
| **修改** | `extensions/subscribe-stream/fsmon-subscribe-demo.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-webhook.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-kafka.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-to-es.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-to-influxdb.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-to-s3.py` | 1-5 |
| **修改** | `extensions/subscribe-stream/fsmon-custom-format.py` | 1-5 |
| **修改** | `extensions/socket-admin/fsmon-admin.py` | 1-5 |
| **修改** | `extensions/http-metrics/fsmon-metrics.py` | 1-5 |
| **修改** | `extensions/jsonl-logs/fsmon-log-tail.py` | 1-5 |
| **修改** | `extensions/README.md` | 1-5 |

---

## 实施顺序

| 顺序 | 阶段 | 核心产出 |
|------|------|---------|
| 1 | 阶段 1 | `_templates/` 5 文件, PEP 723 blocks, `pyproject.toml` |
| 2 | 阶段 2 | 重试 + 死信 + WAL + 重连 + 转义 |
| 3 | 阶段 3 | signal handler + stats + 异步 webhook + logging |
| 4 | 阶段 4 | 类型标注 + 校验 + 命名统一 + bugfix |
| 5 | 阶段 5 | doc + Loki + ES 映射 + 排除法 |

## 测试计划

| 测试 | 覆盖 |
|------|------|
| `uv run` 各脚本 `--help` | PEP 723 元数据 + 依赖解析 + arg 定义 |
| `test_templates.py` | `_templates/` 中 canonical 实现的单元测试 |
| `test_influxdb_escape.py` | line protocol 转义边界 |
| `test_retry.py` | 退避时间计算、死信写入 |
| mypy strict | 全部脚本通过类型检查 |

## 时间估算

| 阶段 | 预估 |
|------|------|
| 阶段 1（基础设施） | 主要工作 |
| 阶段 2（数据安全） | 主要工作 |
| 阶段 3（可靠性） | 中等 |
| 阶段 4（代码质量） | 较快 |
| 阶段 5（文档） | 快速 |
