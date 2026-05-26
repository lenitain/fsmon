#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

fsmon Subscribe Demo — real-time event stream consumer.

This is the most common integration pattern: connect to fsmon's Unix socket,
subscribe to the event stream, and process events as they happen.

The subscribe protocol is:
  1. Connect to Unix socket (/tmp/fsmon-<UID>.sock)
  2. Send TOML subscribe command with optional filters (track_cmd, types)
  3. Read TOML response (ok=true means subscribed)
  4. Continuously read JSONL event lines until disconnected

No external dependencies (stdlib only). Use this as the base template for
building your own bridges (Kafka, webhook, S3, etc.).

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: start the daemon
  sudo fsmon daemon

  # Watch all real-time events
  python3 extensions/subscribe-stream/fsmon-subscribe-demo.py

  # Only watch nginx CLOSE_WRITE events
  python3 extensions/subscribe-stream/fsmon-subscribe-demo.py \
      --track-cmd nginx --types CLOSE_WRITE

  # Watch specific path events (global group)
  python3 extensions/subscribe-stream/fsmon-subscribe-demo.py \
      --types CREATE,DELETE

── Wire Protocol ───────────────────────────────────────────────────
  Client -> Server:
    cmd = "subscribe"
    track_cmd = "nginx"
    types = ["CREATE", "DELETE"]
    <blank line>

  Server -> Client:
    ok = true
    {"time":"2026-05-28T10:00:00Z","event_type":"CREATE",...}
    {"time":"2026-05-28T10:00:01Z","event_type":"DELETE",...}
    ...

── Bridge To ────────────────────────────────────────────────────────
  This is the foundation for all real-time integrations:
    fsmon-webhook.py      → HTTP webhooks (Slack, Discord, custom)
    fsmon-kafka.py        → Apache Kafka topics
    fsmon-to-es.py         → Elasticsearch indexing
    fsmon-to-s3.py          → S3 object storage archiving
    fsmon-to-influxdb.py    → InfluxDB time-series database
    fsmon-custom-format.py  → CSV, syslog, Loki, etc.
"""

import argparse
import json
import socket


def main():
    parser = argparse.ArgumentParser(description="Subscribe to fsmon real-time event stream")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", default=None, help="Filter by cmd group")
    parser.add_argument("--types", default=None, help="Comma-separated event types")
    args = parser.parse_args()

    # Build TOML command
    lines = ['cmd = "subscribe"']
    if args.track_cmd:
        lines.append(f'track_cmd = "{args.track_cmd}"')
    if args.types:
        types = ", ".join(f'"{t.strip()}"' for t in args.types.split(","))
        lines.append(f"types = [{types}]")
    payload = "\n".join(lines) + "\n\n"

    # Connect to socket
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(args.socket)
    s.sendall(payload.encode())

    # Read initial TOML response
    reader = s.makefile("r")
    resp = reader.readline()
    if "ok = true" not in resp:
        print(f"subscribe failed: {resp.strip()}")
        return

    print(f"Connected to {args.socket}, waiting for events... (Ctrl+C to exit)")

    # Continuously read JSONL events
    for line in reader:
        line = line.strip()
        if not line:
            continue
        if '"warning"' in line:
            print(f"[!] {json.loads(line).get('warning', line)}")
            continue
        try:
            ev = json.loads(line)
            print(f"[{ev['event_type']}] {ev['path']}  pid={ev['pid']}  cmd={ev['cmd']}")
        except json.JSONDecodeError:
            print(f"[raw] {line}")


if __name__ == "__main__":
    main()
