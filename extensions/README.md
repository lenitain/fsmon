# fsmon Extensions — 示例

fsmon 的文件事件以标准 **JSONL** 格式导出。以下示例展示如何使用已有工具对接。

## 出口 ①：JSONL 文件

默认写入 `~/.local/state/fsmon/*_log.jsonl`。

```bash
# 终端查看
jq 'select(.cmd == "nginx")' ~/.local/state/fsmon/*_log.jsonl

# 实时 tail
tail -f ~/.local/state/fsmon/*_log.jsonl | jq --unbuffered 'select(.event_type == "CREATE")'

# Filebeat → Kafka/ES（filebeat.yml）
filebeat.inputs:
  - type: log
    paths: ["/home/*/.local/state/fsmon/*_log.jsonl"]
    json.keys_under_root: true

# Vector → 任意目标
```

## 出口 ②：Unix socket（连接即流）

连接 daemon socket 直接收 JSONL 事件，无需握手协议。

```bash
# 所有事件
nc -U /tmp/fsmon-$(id -u).sock

# 过滤（用 jq）
nc -U /tmp/fsmon-$(id -u).sock | jq 'select(.cmd == "nginx")'

# 转发到 Kafka
nc -U /tmp/fsmon-$(id -u).sock | kafkacat -b localhost:9092 -t fsmon-events

# 转发到 Webhook
nc -U /tmp/fsmon-$(id -u).sock |
  while read -r line; do
    curl -s -X POST -d "$line" https://hooks.example.com/fsmon
  done
```

## examples/

```bash
# 查看 JSONL 文件示例
bash extensions/examples/read-jsonl.sh

# socket 实时流示例
bash extensions/examples/subscribe-socket.sh

# 转发到 Kafka 示例
bash extensions/examples/forward-to-kafka.sh localhost:9092 fsmon-events
```

Socket 路径规则：`/tmp/fsmon-<UID>.sock`（和 daemon 同一用户的 UID）。
