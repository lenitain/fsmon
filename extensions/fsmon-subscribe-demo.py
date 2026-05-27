#!/usr/bin/env python3
"""
fsmon subscribe protocol demo — connect to daemon, receive real-time event stream.

Minimal demonstration of the subscribe protocol:
  1. Connect to Unix socket
  2. Send TOML subscribe command
  3. Read TOML response
  4. Continuously receive JSONL events

File writing is handled by the built-in FileLogWriter. No external script needed.
For custom output, use this framework to integrate with Kafka / S3 / webhook / etc.

Usage:
  # Ensure daemon is running
  sudo fsmon daemon

  # Watch all real-time events
  python3 extensions/fsmon-subscribe-demo.py

  # Only watch nginx CLOSE_WRITE events
  python3 extensions/fsmon-subscribe-demo.py --track-cmd nginx --types CLOSE_WRITE
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
