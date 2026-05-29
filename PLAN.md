# Extensions 测试最终报告

## 运行时验证结果

### ✅ 完整通过（连接 daemon，处理真实事件）

| 脚本 | 验证项 |
|------|--------|
| `fsmon-metrics.py` | socket ✅ TCP curl ✅ --summary ✅ --watch ✅ |
| `fsmon-admin.py` | list ✅ health ✅ health --json ✅ add/remove ✅ --socket 修复 ✅ |
| `fsmon-subscribe-demo.py` | 连接 ✅ 事件接收 ✅ --types 过滤 ✅ |
| `fsmon-webhook.py` | 异步队列 ✅ 3次重试 ✅ DLQ 写入 ✅ |
| `fsmon-custom-format.py` | csv ✅ tsv ✅ syslog ✅ loki ✅ json ✅ human ✅ |
| `fsmon-log-tail.py` | --no-follow ✅ --aggregate ✅ --type ✅ --last ✅ tail ✅ |

### ⚠️ 部分通过（无后端，仅验证连接拒绝和参数校验）

| 脚本 | 已验证 | 未验证 |
|------|--------|--------|
| `fsmon-kafka.py` | clean error | 写入 topic、key 分区、重试+DLQ |
| `fsmon-to-es.py` | clean error | bulk 索引、--index-granularity、重试+DLQ |
| `fsmon-to-influxdb.py` | 连接成功 | line protocol 转义、写入、重试+DLQ |

### ❌ 已删除

`fsmon-to-s3.py` — 依赖 AWS，boto3 下载超时。

## 待补测

```bash
docker run -d --name kafka -p 9092:9092 apache/kafka
docker run -d --name es -p 9200:9200 elasticsearch:8
docker run -d --name influxdb -p 8086:8086 influxdb:2

touch /tmp/fsmon_ext_test/kafka_event
touch /tmp/fsmon_ext_test/es_event
touch /tmp/fsmon_ext_test/influx_event

# 验证数据落盘 + 容错（停后端→DLQ、杀 daemon→重连）
```

## 修复的 bug

1. subscribe payload `\n`→`\n\n`
2. TOML `[health]` 表头解析
3. ES 9.x 构造函数崩溃
4. admin.py `--socket` 被忽略
5. log-tail 默认路径 `/var/log/fsmon`→`~/.local/state/fsmon`
