# 清理完成

## 已删除

### 扩展脚本（6 个 Python 桥接脚本）
- `extensions/subscribe-stream/` — fsmon-kafka.py, fsmon-to-es.py, fsmon-to-influxdb.py, fsmon-webhook.py, fsmon-custom-format.py, fsmon-subscribe-demo.py
- `extensions/http-metrics/` — fsmon-metrics.py, prometheus.yml, fsmon-grafana.json
- `extensions/jsonl-logs/` — fsmon-log-tail.py
- `extensions/socket-admin/` — fsmon-admin.py
- `extensions/_templates/` — 参考实现模板
- `extensions/tests/` — 空测试目录
- `extensions/pyproject.toml` — 开发依赖

### Rust 代码删除
- `--metrics-listen` CLI 参数
- `MetricsConfig` 配置结构体
- `serve_metrics_tcp()` TCP HTTP 服务器
- `handle_metrics_socket()` socket 命令处理器
- 对应的全部 TCP 集成测试

## 保留的架构

```
fanotify 事件 → JSONL 格式
    │
    ├── ① JSONL 文件 (可选)
    │       持久化，Filebeat/Vector/jq
    │
    └── ② Unix socket (连接即流)
            nc -U socket | jq
```

两个出口，同一种数据（JSONL），无自定义协议。
