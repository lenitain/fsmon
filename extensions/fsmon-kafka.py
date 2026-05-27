#!/usr/bin/env python3
"""
fsmon -> Kafka bridge

Receives real-time events from fsmon subscribe, forwards to a Kafka topic.

Dependency:
  pip install kafka-python

If kafka-python is not installed, the script prints install instructions and exits.

Usage:
  # All events to Kafka
  python3 fsmon-kafka.py --broker localhost:9092 --topic fsmon-events

  # Only nginx CLOSE_WRITE events
  python3 fsmon-kafka.py --broker localhost:9092 --topic nginx-writes --track-cmd nginx --types CLOSE_WRITE

Prometheus / Grafana can consume the fsmon_events_total counter from Kafka.
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
    parser = argparse.ArgumentParser(description="fsmon -> Kafka bridge")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--broker", default="localhost:9092", help="Kafka broker address")
    parser.add_argument("--topic", default="fsmon-events", help="Kafka topic")
    args = parser.parse_args()

    if not HAS_KAFKA:
        print("Error: kafka-python required. Install: pip install kafka-python", file=sys.stderr)
        sys.exit(1)

    print(f"Connecting to Kafka: {args.broker} topic={args.topic}")

    # Create producer
    try:
        producer = KafkaProducer(
            bootstrap_servers=args.broker,
            value_serializer=lambda v: json.dumps(v).encode("utf-8"),
            key_serializer=lambda v: v.encode("utf-8") if v else None,
            acks=1,
            retries=3,
        )
    except NoBrokersAvailable:
        print(f"Error: cannot connect to Kafka broker {args.broker}", file=sys.stderr)
        sys.exit(1)

    print(f"Listening on {args.socket} -> Kafka topic {args.topic}")
    if args.track_cmd:
        print(f"  cmd filter: {args.track_cmd}")
    if args.types:
        print(f"  type filter: {args.types}")

    count = 0
    for ev in subscribe(args.socket, args.track_cmd, args.types):
        # Use cmd+event_type as key to keep same-key events in order
        key = f"{ev.get('cmd', '?')}:{ev['event_type']}"
        producer.send(args.topic, value=ev, key=key)
        count += 1
        if count % 1000 == 0:
            print(f"[kafka] sent {count} events", flush=True)

    producer.flush()
    producer.close()


if __name__ == "__main__":
    main()
