# fsmon Extensions — Bridge Scripts

将 fsmon daemon 的文件事件桥接到你的基础设施：Kafka、Elasticsearch、S3、Webhook、InfluxDB 等。

```
fanotify kernel events
       │
       ▼
  fsmon daemon
       │
       ├── ① jsonl-logs/         Disk log files    → grep / aggregate / replay
       ├── ② subscribe-stream/   Real-time stream  → Webhook / Kafka / ES / S3 / ...
       ├── ③ socket-admin/       Management cmds   → programmatic add/remove/list/health
       └── ④ http-metrics/       Metrics endpoint  → Prometheus / Grafana / any TCP consumer

  _templates/                     Canonical reference implementations (dev only)
  tests/                          Test suite
```

## 快速开始

### 前置条件

- **[uv](https://docs.astral.sh/uv/)** — Python 包管理器（`curl -LsSf https://astral.sh/uv/install.sh | sh`）
- **fsmon daemon** — 本项目已包含，一行安装：

```bash
# 在本仓库根目录
git clone https://github.com/xxx/fsmon
cd fsmon
cargo install --path .
```

### 三步跑起来

```bash
# 1. 启动 daemon
sudo fsmon daemon

# 2. 添加监控路径
sudo fsmon add /var/log/nginx --track-cmd nginx --types CLOSE_WRITE

# 3. 运行任意 bridge 脚本
uv run extensions/subscribe-stream/fsmon-subscribe-demo.py
```

## 设计理念

每个脚本**完全自包含**——不 import 其他 Python 文件、不依赖其他 bridge 的第三方包。你可以将单个 `.py` 复制到任意已安装 fsmon daemon 的机器即可运行。

- **PEP 723 inline metadata**：脚本头部声明自身依赖，`uv run` 自动创建隔离 venv
- **零跨文件 import**：所有逻辑内联在单个文件中
- **独立可复制**：将 `fsmon-kafka.py` 复制到任意已装 daemon 的机器即可运行
- **前提**：脚本连接 fsmon daemon Unix socket，目标机器必须先装好 fsmon

## 所有 Bridge 一览

### subscribe-stream/ — 实时事件流

| 脚本 | 依赖 | 用途 |
|------|------|------|
| `fsmon-subscribe-demo.py` | stdlib | 终端查看实时事件流 |
| `fsmon-webhook.py` | stdlib | 转发到 HTTP webhook（Slack、Discord 等） |
| `fsmon-kafka.py` | kafka-python | 发布到 Kafka topic |
| `fsmon-to-es.py` | elasticsearch | 索引到 Elasticsearch |
| `fsmon-to-influxdb.py` | influxdb-client | 写入 InfluxDB 时序数据库 |
| `fsmon-custom-format.py` | stdlib | 转换为 CSV/TSV/syslog/Loki/JSON 格式 |

### socket-admin/ — 管理命令

| 脚本 | 依赖 | 用途 |
|------|------|------|
| `fsmon-admin.py` | stdlib | 动态添加/移除监控路径，健康检查 |

### jsonl-logs/ — 日志文件分析

| 脚本 | 依赖 | 用途 |
|------|------|------|
| `fsmon-log-tail.py` | stdlib | 读取 JSONL 日志（tail、grep、聚合） |

### http-metrics/ — Prometheus 指标

| 脚本 | 依赖 | 用途 |
|------|------|------|
| `fsmon-metrics.py` | stdlib | 拉取 Prometheus 格式指标 |

## 使用示例

### Kafka bridge

```bash
# daemon 已安装的前提下，从仓库直接运行
uv run extensions/subscribe-stream/fsmon-kafka.py \
    --broker kafka.internal:9092 --topic fsmon-events

# 或者只复制这一个 .py 到其他机器
# （前提：目标机器已安装 fsmon daemon + uv）
scp extensions/subscribe-stream/fsmon-kafka.py other-host:/opt/bridges/
ssh other-host
cd /opt/bridges
./fsmon-kafka.py --broker kafka.internal:9092 --topic fsmon-events

# 筛选特定事件
./fsmon-kafka.py --broker localhost:9092 --topic nginx-writes \
    --track-cmd nginx --types CLOSE_WRITE
```

### Elasticsearch bridge

```bash
./fsmon-to-es.py --host https://es.example.com:9200 --user admin --pass secret
```

### Webhook bridge（Slack 告警）

```bash
./fsmon-webhook.py --webhook https://hooks.slack.com/services/T.../B.../xxx \
    --track-cmd nginx --types DELETE
```

## 运维

### systemd 部署示例

```ini
[Unit]
Description=fsmon -> Kafka bridge
After=network.target

[Service]
ExecStart=/opt/fsmon-bridges/fsmon-kafka.py \
    --broker kafka.prod:9092 \
    --topic fsmon-events
Restart=always
User=fsmon

[Install]
WantedBy=multi-user.target
```

### 优雅关闭

所有 bridge 脚本响应 `Ctrl+C` / `SIGTERM`，会 flush 缓冲、关闭连接后退出。

### 死信队列

外部写入失败时，事件写入 `$TMPDIR/fsmon-dlq/` 下的死信文件。可通过 `--dlq-dir` 指定路径。

### 健康监控

每个 bridge 定期向 stderr 输出一行 JSON stats：

```json
{"ts":"2026-05-29T15:30:10","events":2000,"errors":0,"json_errors":0,"reconnects":0}
```

## 传统 pip 用户

如果不用 uv，也可以传统方式安装依赖：

```bash
# stdlib 脚本 —— 直接运行
python3 fsmon-subscribe-demo.py

# 有外部依赖的脚本 —— 手动 pip install
pip install kafka-python
python3 fsmon-kafka.py --broker localhost:9092 --topic fsmon-events
```

所有脚本保持 `#!/usr/bin/env python3` shebang，兼容两种方式。
