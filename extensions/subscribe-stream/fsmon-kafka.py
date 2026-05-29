#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["kafka-python>=2.0"]
# ///
"""

fsmon -> Kafka bridge

Receives real-time events from fsmon subscribe, forwards to a Kafka topic.

Dependency:
  pip install kafka-python

If kafka-python is not installed, the script prints install instructions and exits.

Usage:
  # All events to Kafka
  python3 extensions/subscribe-stream/fsmon-kafka.py --broker localhost:9092 --topic fsmon-events

  # Only nginx CLOSE_WRITE events
  python3 extensions/subscribe-stream/fsmon-kafka.py --broker localhost:9092 --topic nginx-writes --track-cmd nginx --types CLOSE_WRITE

Prometheus / Grafana can consume the fsmon_events_total counter from Kafka.
"""

import argparse
import json
import socket
import sys
import time
import logging

try:
    from kafka import KafkaProducer
    from kafka.errors import NoBrokersAvailable
    HAS_KAFKA = True
except ImportError:
    HAS_KAFKA = False


def subscribe(socket_path: str, track_cmd: str | None = None,
              type_filter: str | None = None):
    """Yield fsmon events with auto-reconnect and error logging."""
    _log = logging.getLogger("fsmon.subscribe")
    delay = 1.0
    while True:
        try:
            yield from _subscribe_inner(socket_path, track_cmd, type_filter)
        except (ConnectionRefusedError, FileNotFoundError, BrokenPipeError,
                ConnectionError, socket.timeout, OSError) as e:
            _log.warning("disconnected, reconnecting in %.0fs... (%s)", delay, e)
            time.sleep(delay)
            delay = min(delay * 2, 60)
        else:
            _log.warning("daemon closed connection, reconnecting in %.0fs...", delay)
            time.sleep(delay)
            delay = min(delay * 2, 60)


def _subscribe_inner(socket_path: str, track_cmd: str | None,
                     type_filter: str | None):
    """Single subscribe connection. Raises on disconnect."""
    _log = logging.getLogger("fsmon.subscribe")
    cmd: dict = {"cmd": "subscribe"}
    if track_cmd:
        cmd["track_cmd"] = track_cmd
    if type_filter:
        cmd["types"] = [t.strip() for t in type_filter.split(",")]
    payload = _dict_to_toml(cmd) + "\n"

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)
        s.connect(socket_path)
        s.sendall(payload.encode())
        reader = s.makefile("r")
        resp = reader.readline()
        if "ok = true" not in resp:
            raise ConnectionError(
                f"subscribe rejected: {resp.strip()}\n"
                f"Is the daemon running? Start with: sudo fsmon daemon"
            )
        _log.info("connected to %s", socket_path)
        json_errors = 0
        for line in reader:
            line = line.strip()
            if not line:
                continue
            if '"warning"' in line:
                try:
                    ev = json.loads(line)
                    _log.warning("daemon: %s", ev.get("warning", line))
                except json.JSONDecodeError:
                    pass
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                json_errors += 1
                _log.error("JSON decode error (#%d): %.120s", json_errors, line)


def _dict_to_toml(d: dict) -> str:
    """Serialize flat dict to TOML subset."""
    def _esc(s: str) -> str:
        return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'
    lines: list[str] = []
    for key, value in d.items():
        if value is None:
            continue
        if isinstance(value, bool):
            lines.append(f"{key} = {'true' if value else 'false'}")
        elif isinstance(value, list):
            items = ", ".join(
                _esc(v) if isinstance(v, str) else str(v) for v in value
            )
            lines.append(f"{key} = [{items}]")
        elif isinstance(value, str):
            lines.append(f"{key} = {_esc(value)}")
        else:
            lines.append(f"{key} = {value}")
    return "\n".join(lines)


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
