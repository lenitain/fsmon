# fsmon Extensions

Extra tooling for common fsmon integration patterns.

## 文件事件对接

fsmon 的事件出口全部使用标准 **JSONL** 格式，下游可以用任意已有工具消费。

```
fanotify 事件 → JSONL 格式
    │
    ├── JSONL 文件 (可选)  →  Filebeat / Vector / fluent-bit → Kafka / ES / ...
    │                         tail / jq / grep 直接查看
    │
    └── Unix socket        →  nc -U /tmp/fsmon-$(id -u).sock | jq
                                nc -U socket | kafkacat -b broker -t topic
```

### JSONL 文件

默认写入 `~/.local/state/fsmon/*_log.jsonl`。可通过 `[logging].path` 配置或设为 `none` 关闭。

### Unix socket 实时流

连接 daemon socket 后自动推送 JSONL 事件，无需握手：

```bash
# 终端查看
nc -U /tmp/fsmon-$(id -u).sock | jq

# 转发到 Kafka（需安装 kafkacat）
nc -U /tmp/fsmon-$(id -u).sock | kafkacat -b localhost:9092 -t fsmon-events
```

Socket 路径规则：`/tmp/fsmon-<UID>.sock`

### 无额外依赖

不需要 Python 脚本、不需要安装额外包。`nc` 系统自带，`jq` 可选（用于格式化过滤）。
