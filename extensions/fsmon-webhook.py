#!/usr/bin/env python3
"""
fsmon -> Webhook / Alerting bridge

Receives real-time events from fsmon subscribe, matches conditions,
then calls an HTTP webhook. No external dependencies (stdlib only).

Use cases:
  - File change alerts to Slack / Discord / Feishu / DingTalk
  - Trigger CI/CD on suspicious file operations
  - Custom HTTP callbacks

Usage:
  # Send all events to webhook
  python3 fsmon-webhook.py --webhook http://localhost:8080/alert

  # Only nginx log changes
  python3 fsmon-webhook.py --track-cmd nginx --types MODIFY,CLOSE_WRITE --webhook http://...
"""

import argparse
import json
import socket
import sys
import urllib.request
from datetime import datetime, timezone


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


def send_webhook(url: str, event: dict):
    """Send event as JSON to webhook URL."""
    data = json.dumps(event).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        urllib.request.urlopen(req, timeout=5)
    except Exception as e:
        print(f"webhook send failed: {e}", file=sys.stderr)


def format_for_print(ev):
    ts = datetime.fromisoformat(ev["time"])
    return f"[{ts:%H:%M:%S}] {ev['event_type']:12s} {ev['path']}  pid={ev['pid']}  cmd={ev['cmd']}"


def main():
    parser = argparse.ArgumentParser(description="fsmon -> Webhook bridge")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock", help="fsmon daemon socket")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--webhook", required=True, help="Webhook URL")
    parser.add_argument("--print", action="store_true", help="Also print events to stdout")
    args = parser.parse_args()

    print(f"Listening on {args.socket} -> webhook {args.webhook}")
    if args.track_cmd:
        print(f"  cmd filter: {args.track_cmd}")
    if args.types:
        print(f"  type filter: {args.types}")

    count = 0
    for ev in subscribe(args.socket, args.track_cmd, args.types):
        count += 1
        send_webhook(args.webhook, ev)
        if args.print:
            print(format_for_print(ev))
        if count % 100 == 0:
            print(f"[webhook] sent {count} events", flush=True)


if __name__ == "__main__":
    main()
