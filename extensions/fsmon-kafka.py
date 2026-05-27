#!/usr/bin/env python3
"""
fsmon → Kafka 桥接

从 fsmon subscribe 接收实时事件，转发到 Kafka topic。

依赖：
  pip install kafka-python

如果 kafka-python 未安装，脚本会提示安装命令。

用法：
  # 所有事件发到 Kafka
  python3 fsmon-kafka.py --broker localhost:9092 --topic fsmon-events

  # 只看 nginx 的 CLOSE_WRITE
  python3 fsmon-kafka.py --broker localhost:9092 --topic nginx-writes --track-cmd nginx --types CLOSE_WRITE

Prometheus / Grafana 可以读 Kafka 里的 fsmon_events_total counter。
"""

import argparse
import json
import socket
import sys
import time

try:
    from kafka import KafkaProducer
    from kafka.errors import NoBrokersAvailable
    HAS_KAFKA = True
except ImportError:
    HAS_KAFKA = False


def subscribe(socket_path, track_cmd=None, type_filter=None):
    """Generator yielding events from fsmon subscribe socket."""
    lines = ['cmd = "subscribe"']
    if track_cmd:
        lines.append(f'track_cmd = "{track_cmd}"')
    if type_filter:
        types = ", ".join(f'"{t.strip()}"' for t in type_filter.split(","))
        lines.append(f"types = [{types}]")
    payload = "\n".join(lines) + "\n\n"

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(socket_path)
    s.sendall(payload.encode())

    reader = s.makefile("r")
    resp = reader.readline()
    if "ok = true" not in resp:
        return
    for line in reader:
        line = line.strip()
        if not line or '"warning"' in line:
            continue
        try:
            yield json.loads(line)
        except json.JSONDecodeError:
            pass


def main():
    parser = argparse.ArgumentParser(description="fsmon → Kafka bridge")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--broker", default="localhost:9092", help="Kafka broker address")
    parser.add_argument("--topic", default="fsmon-events", help="Kafka topic")
    args = parser.parse_args()

    if not HAS_KAFKA:
        print("错误: 需要 kafka-python。安装: pip install kafka-python", file=sys.stderr)
        sys.exit(1)

    print(f"连接 Kafka: {args.broker} topic={args.topic}")

    # 创建 producer
    try:
        producer = KafkaProducer(
            bootstrap_servers=args.broker,
            value_serializer=lambda v: json.dumps(v).encode("utf-8"),
            key_serializer=lambda v: v.encode("utf-8") if v else None,
            acks=1,
            retries=3,
        )
    except NoBrokersAvailable:
        print(f"错误: 无法连接 Kafka broker {args.broker}", file=sys.stderr)
        sys.exit(1)

    print(f"监听 {args.socket} → Kafka topic {args.topic}")
    if args.track_cmd:
        print(f"  过滤 cmd: {args.track_cmd}")
    if args.types:
        print(f"  过滤 types: {args.types}")

    count = 0
    for ev in subscribe(args.socket, args.track_cmd, args.types):
        # 用 cmd+event_type 做 key，同 key 的事件进同一 partition（保持顺序）
        key = f"{ev.get('cmd', '?')}:{ev['event_type']}"
        producer.send(args.topic, value=ev, key=key)
        count += 1
        if count % 1000 == 0:
            print(f"[kafka] 已发送 {count} 个事件", flush=True)

    producer.flush()
    producer.close()


if __name__ == "__main__":
    main()
