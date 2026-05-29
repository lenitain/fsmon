#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
"""

fsmon Metrics Client — pull Prometheus-format metrics via Unix socket.

fsmon exposes metrics through two transport layers:
  1. Unix socket: cmd="metrics" → returns Prometheus text (this script)
  2. TCP HTTP:    GET /metrics → returns Prometheus text (for Prometheus scraper)

Both return the same Prometheus text format. The socket layer is always
available; the TCP layer requires --metrics-listen at daemon startup.

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: install fsmon (https://github.com/xxx/fsmon)
  # Start daemon with TCP metrics endpoint
  sudo fsmon daemon --metrics-listen 127.0.0.1:9845

  # Pull metrics once via socket (always available, no --metrics-listen needed)
  uv run extensions/http-metrics/fsmon-metrics.py

  # Human-readable summary
  uv run extensions/http-metrics/fsmon-metrics.py --summary

  # Watch mode: pull every 15s (like a lightweight Prometheus scraper)
  uv run extensions/http-metrics/fsmon-metrics.py --watch 15

  # Or point Prometheus directly at the TCP endpoint:
  # See prometheus.yml in this directory for scrape config.

── Metrics Explained ───────────────────────────────────────────────
  fsmon_events_total{event_type,cmd}   Counter: total events processed
  fsmon_subscribers                    Gauge: active subscribe connections
  fsmon_monitored_paths                Gauge: number of monitored path entries
  fsmon_reader_groups                  Gauge: number of fanotify fd groups
  fsmon_pending_paths                  Gauge: paths waiting for creation
  fsmon_disk_buffer_events             Gauge: events buffered (disk full)

── Bridge To ────────────────────────────────────────────────────────
  - Prometheus (scrape TCP /metrics or use this script as custom exporter)
  - Grafana (import fsmon-grafana.json dashboard from this directory)
  - Datadog / New Relic (custom metrics via their agent or API)
  - Any monitoring system that speaks Prometheus text format
  - Shell scripts: parse --summary output for Nagios/Icinga checks
"""

import argparse
import os
import socket
import sys
import time


def get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


def pull_metrics(socket_path: str | None = None) -> str:
    """Connect to fsmon socket, send metrics command, return Prometheus text."""
    if socket_path is None:
        socket_path = get_socket_path()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
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
            value = int((parts[1].strip() or "0") if len(parts) > 1 else 0)
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


_GAUGE_KEYS = {"subscribers", "monitored_paths", "reader_groups", "pending_paths", "disk_buf"}


def main() -> None:
    parser = argparse.ArgumentParser(description="Pull fsmon metrics via Unix socket")
    parser.add_argument("--socket", default=None, help="Socket path (auto-detected)")
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
                total = sum(v for k, v in info.items() if isinstance(v, int) and k not in _GAUGE_KEYS)
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
                if isinstance(v, int) and k not in _GAUGE_KEYS:
                    print(f"  {k:30s} {v}")
        else:
            print(text, end="")


if __name__ == "__main__":
    main()
