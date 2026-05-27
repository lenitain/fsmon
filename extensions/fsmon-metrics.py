#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

fsmon metrics command — pull Prometheus-format metrics via Unix socket.

Demonstrates the pull-mode socket layer:
  1. Connect to socket
  2. Send TOML metrics command
  3. Read back Prometheus text format
  4. Display as-is or summary

No configuration needed — the socket metrics command is always available.

Usage:
  # Output Prometheus format (default)
  python3 fsmon-metrics.py

  # Show summary only
  python3 fsmon-metrics.py --summary

  # Watch mode: pull every N seconds (like Prometheus scrape)
  python3 fsmon-metrics.py --watch 15
"""

import argparse
import socket
import sys
import time


def pull_metrics(socket_path="/tmp/fsmon-1000.sock") -> str:
    """Connect to fsmon socket, send metrics command, return Prometheus text."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(socket_path)
    s.sendall(b'cmd = "metrics"\n\n')

    reader = s.makefile("r")
    return reader.read()


def parse_summary(text: str) -> dict:
    """Extract key metrics from Prometheus text format."""
    info = {}
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("fsmon_events_total{"):
            parts = line.split("}")
            labels = parts[0].replace("fsmon_events_total{", "")
            value = int(parts[1].strip() if len(parts) > 1 else 0)
            key = labels.replace('"', "").replace("event_type=", "").replace(",cmd=", " / ")
            info[key] = value
        elif line.startswith("fsmon_subscribers "):
            info["subscribers"] = int(line.split()[-1])
        elif line.startswith("fsmon_monitored_paths "):
            info["monitored_paths"] = int(line.split()[-1])
        elif line.startswith("fsmon_reader_groups "):
            info["reader_groups"] = int(line.split()[-1])
        elif line.startswith("fsmon_pending_paths "):
            info["pending_paths"] = int(line.split()[-1])
        elif line.startswith("fsmon_disk_buffer_events "):
            info["disk_buf"] = int(line.split()[-1])
    return info


def main():
    parser = argparse.ArgumentParser(description="Pull fsmon metrics via Unix socket")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--summary", action="store_true", help="Show human-readable summary")
    parser.add_argument("--watch", type=int, metavar="SECS", help="Watch mode: pull every N seconds")
    args = parser.parse_args()

    if args.watch:
        while True:
            try:
                text = pull_metrics(args.socket)
            except Exception as e:
                print(f"connection failed: {e}", file=sys.stderr)
                time.sleep(args.watch)
                continue

            if args.summary:
                info = parse_summary(text)
                total = sum(v for k, v in info.items() if isinstance(v, int) and k.startswith(("CREATE", "MODIFY", "DELETE", "ACCESS", "OPEN", "CLOSE", "MOVE", "ATTRIB", "FS_ERROR")))
                print(f"\n[{time.strftime('%H:%M:%S')}] events_total={total}  subscribers={info.get('subscribers', '?')}  paths={info.get('monitored_paths', '?')}", flush=True)
            else:
                print(text, end="")
            time.sleep(args.watch)
    else:
        try:
            text = pull_metrics(args.socket)
        except Exception as e:
            print(f"connection failed: {e}", file=sys.stderr)
            print("Is the daemon running? sudo fsmon daemon", file=sys.stderr)
            sys.exit(1)

        if args.summary:
            info = parse_summary(text)
            print(f"Subscribers:       {info.get('subscribers', 0)}")
            print(f"Monitored paths:   {info.get('monitored_paths', 0)}")
            print(f"Reader groups:     {info.get('reader_groups', 0)}")
            print(f"Pending paths:     {info.get('pending_paths', 0)}")
            print(f"Disk buffer:       {info.get('disk_buf', 0)}")
            print("\nEvent counts:")
            for k, v in sorted(info.items()):
                if isinstance(v, int) and k not in {"subscribers", "monitored_paths", "reader_groups", "pending_paths", "disk_buf"}:
                    print(f"  {k:30s} {v}")
        else:
            print(text, end="")


if __name__ == "__main__":
    main()
