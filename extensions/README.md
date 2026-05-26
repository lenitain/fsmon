# fsmon Extensions — Bridge Examples

fsmon daemon exposes **4 data exit points**. This directory is organized by exit point,
each subdirectory provides example code showing how to bridge fsmon to your existing tools.

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
```

## Quick Navigation

| I want to... | Go to |
|-------------|-------|
| Analyze logs after the fact, grep specific events | `jsonl-logs/` |
| Push real-time events to Webhook / Kafka / ES / S3 | `subscribe-stream/` |
| Dynamically add/remove monitored paths from code | `socket-admin/` |
| Hook into Prometheus + Grafana for dashboards | `http-metrics/` |

## Naming Convention

All example scripts are prefixed with `fsmon-`. They are **example code** (not production-ready).
Adapt parameters to your environment before deploying.

## Dependencies

All examples use Python 3 stdlib (`socket`, `json`, `argparse`) as the baseline.
Some advanced examples (Kafka, ES, etc.) require extra `pip install` — the scripts
print install instructions if the dependency is missing.
