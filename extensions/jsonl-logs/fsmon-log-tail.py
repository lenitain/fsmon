#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

fsmon Log File Reader — process JSONL log files produced by fsmon daemon.

fsmon daemon writes all events to JSONL files under the log directory
(e.g. /var/log/fsmon/). This exit point is the simplest to consume:
just read the files with any JSON-capable tool.

This script demonstrates common patterns:
  1. tail -f equivalent: follow new events as they are written
  2. grep by event type or command name
  3. time-range filtering
  4. aggregate statistics

No external dependencies (stdlib only).

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: daemon running and logging
  sudo fsmon daemon --log-dir /var/log/fsmon

  # Tail all log files (like tail -f *.jsonl)
  python3 extensions/jsonl-logs/fsmon-log-tail.py --log-dir /var/log/fsmon

  # Only show DELETE events
  python3 extensions/jsonl-logs/fsmon-log-tail.py --log-dir /var/log/fsmon --type DELETE

  # Show events from last 5 minutes
  python3 extensions/jsonl-logs/fsmon-log-tail.py --log-dir /var/log/fsmon --last 5m

  # Aggregate: count events by type
  python3 extensions/jsonl-logs/fsmon-log-tail.py --log-dir /var/log/fsmon --aggregate

  # Watch a specific command group's log
  python3 extensions/jsonl-logs/fsmon-log-tail.py --log-dir /var/log/fsmon --cmd nginx

── How It Works ────────────────────────────────────────────────────
  The daemon writes one JSON line per event to files named {cmd}_log.jsonl.
  This script reads those files directly — no socket connection needed.
  It's the simplest integration: works even if the daemon is stopped,
  and you can process historical data.

── Bridge To ────────────────────────────────────────────────────────
  - grep/jq for ad-hoc investigation
  - logrotate + S3/GCS archival
  - logstash / filebeat → Elasticsearch
  - cron job that aggregates hourly stats → Slack
  - any tool that can read JSON lines from disk
"""

import argparse
import json
import os
import re
import sys
import time
from collections import Counter
from datetime import datetime, timezone, timedelta


# ── File discovery ──────────────────────────────────────────────────

def find_log_files(log_dir: str, cmd_filter: str = None) -> list:
    """Find all *_log.jsonl files in log_dir, optionally filtered by cmd name."""
    if not os.path.isdir(log_dir):
        print(f"Error: log directory not found: {log_dir}", file=sys.stderr)
        sys.exit(1)

    files = sorted(
        os.path.join(log_dir, f)
        for f in os.listdir(log_dir)
        if f.endswith("_log.jsonl")
    )
    if cmd_filter:
        target = f"{cmd_filter}_log.jsonl"
        files = [f for f in files if os.path.basename(f) == target]

    if not files:
        print(f"No *_log.jsonl files found in {log_dir}", file=sys.stderr)
        sys.exit(1)
    return files


# ── Time parsing ────────────────────────────────────────────────────

def parse_duration(s: str) -> timedelta:
    """Parse human-readable duration like '5m', '1h', '30s', '2d'."""
    m = re.match(r"^(\d+)(s|m|h|d)$", s)
    if not m:
        raise ValueError(f"Invalid duration: {s} (use 30s, 5m, 1h, 2d)")
    value = int(m.group(1))
    unit = m.group(2)
    unit_map = {"s": "seconds", "m": "minutes", "h": "hours", "d": "days"}
    return timedelta(**{unit_map[unit]: value})


def parse_event_time(ev: dict) -> datetime:
    """Parse the 'time' field from a fsmon event. Returns datetime in UTC."""
    ts = ev.get("time", "")
    try:
        # ISO 8601 with Z or +HH:MM offset
        # Handle Z suffix
        if ts.endswith("Z"):
            ts = ts[:-1] + "+00:00"
        return datetime.fromisoformat(ts)
    except (ValueError, TypeError):
        return datetime.min.replace(tzinfo=timezone.utc)


# ── Event reading ───────────────────────────────────────────────────

def read_events(files: list, type_filter: str = None, since: datetime = None):
    """Generator: yield events from log files, optionally filtered."""
    for fpath in files:
        try:
            with open(fpath, "r") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        ev = json.loads(line)
                    except json.JSONDecodeError:
                        continue

                    # Time filter
                    if since:
                        ev_time = parse_event_time(ev)
                        if ev_time < since:
                            continue

                    # Type filter
                    if type_filter and ev.get("event_type") != type_filter:
                        continue

                    yield ev
        except FileNotFoundError:
            continue


def tail_events(log_dir: str, cmd_filter: str = None, type_filter: str = None):
    """Generator: like tail -f — follow new events as they are appended."""
    known_files = set(find_log_files(log_dir, cmd_filter))
    file_positions = {f: os.path.getsize(f) for f in known_files}

    # Read existing content first
    for fpath in sorted(known_files):
        with open(fpath, "r") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    ev = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if type_filter and ev.get("event_type") != type_filter:
                    continue
                yield ev

    # Poll for new content and new files
    while True:
        current_files = set(find_log_files(log_dir, cmd_filter))
        # Check for new files
        for fpath in current_files - known_files:
            file_positions[fpath] = os.path.getsize(fpath)
        known_files = current_files

        for fpath in sorted(known_files):
            try:
                size = os.path.getsize(fpath)
                if size > file_positions.get(fpath, 0):
                    with open(fpath, "r") as f:
                        f.seek(file_positions.get(fpath, 0))
                        for line in f:
                            line = line.strip()
                            if not line:
                                continue
                            try:
                                ev = json.loads(line)
                            except json.JSONDecodeError:
                                continue
                            if type_filter and ev.get("event_type") != type_filter:
                                continue
                            yield ev
                    file_positions[fpath] = size
            except FileNotFoundError:
                pass

        time.sleep(0.5)  # poll interval — adjust based on event rate


# ── Formatting ──────────────────────────────────────────────────────

def format_event(ev: dict) -> str:
    """Human-readable one-line event display."""
    try:
        ts = parse_event_time(ev)
        tstr = ts.strftime("%H:%M:%S")
    except Exception:
        tstr = "??:??:??"
    return (
        f"[{tstr}] {ev.get('event_type', '?'):12s} "
        f"pid={ev.get('pid', '?'):>6d}  "
        f"cmd={ev.get('cmd', '?'):15s}  "
        f"user={ev.get('user', '?'):10s}  "
        f"size={ev.get('file_size', 0):>8d}  "
        f"{ev.get('path', '?')}"
    )


def format_chain(ev: dict) -> str:
    """Display process chain if available."""
    chain = ev.get("chain", "")
    if chain:
        return f"  chain: {chain}"
    return ""


# ── Aggregation ─────────────────────────────────────────────────────

def aggregate_events(files: list, since: datetime = None):
    """Read all events and print aggregate statistics."""
    counter_type = Counter()
    counter_cmd = Counter()
    counter_user = Counter()
    total = 0

    for ev in read_events(files, since=since):
        total += 1
        counter_type[ev.get("event_type", "?")] += 1
        counter_cmd[ev.get("cmd", "?")] += 1
        counter_user[ev.get("user", "?")] += 1

    print(f"\n{'='*60}")
    print(f"  Event Summary  (total: {total})")
    print(f"{'='*60}")

    print(f"\n  By Event Type:")
    for etype, count in counter_type.most_common():
        pct = count / total * 100 if total else 0
        print(f"    {etype:20s} {count:>8d}  ({pct:5.1f}%)")

    print(f"\n  By Command:")
    for cmd, count in counter_cmd.most_common(10):
        pct = count / total * 100 if total else 0
        print(f"    {cmd:20s} {count:>8d}  ({pct:5.1f}%)")

    print(f"\n  By User:")
    for user, count in counter_user.most_common(10):
        pct = count / total * 100 if total else 0
        print(f"    {user:20s} {count:>8d}  ({pct:5.1f}%)")

    print()


# ── Main ────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="fsmon log file reader — read and analyze JSONL event logs",
        epilog="""
Examples:
  %(prog)s --log-dir /var/log/fsmon                        # tail all logs
  %(prog)s --log-dir /var/log/fsmon --type DELETE          # only deletions
  %(prog)s --log-dir /var/log/fsmon --cmd nginx            # only nginx events
  %(prog)s --log-dir /var/log/fsmon --last 5m              # last 5 minutes
  %(prog)s --log-dir /var/log/fsmon --aggregate            # summary stats
  %(prog)s --log-dir /var/log/fsmon --show-chain           # show process chains
        """,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--log-dir", default="/var/log/fsmon",
                        help="Directory containing *_log.jsonl files")
    parser.add_argument("--cmd", help="Filter by command group name (e.g. nginx)")
    parser.add_argument("--type", dest="event_type", help="Filter by event type (e.g. DELETE)")
    parser.add_argument("--last", help="Time range: 30s, 5m, 1h, 2d")
    parser.add_argument("--aggregate", action="store_true",
                        help="Show aggregate statistics instead of event stream")
    parser.add_argument("--show-chain", action="store_true",
                        help="Display process ancestry chain for each event")
    parser.add_argument("--no-follow", action="store_true",
                        help="Read existing logs and exit (don't tail)")
    args = parser.parse_args()

    since = None
    if args.last:
        since = datetime.now(timezone.utc) - parse_duration(args.last)

    if args.aggregate:
        files = find_log_files(args.log_dir, args.cmd)
        aggregate_events(files, since)
        return

    if args.no_follow:
        files = find_log_files(args.log_dir, args.cmd)
        for ev in read_events(files, args.event_type, since):
            print(format_event(ev))
            if args.show_chain:
                print(format_chain(ev))
        return

    # Default: tail mode
    print(f"Tailing logs in {args.log_dir} ... (Ctrl+C to stop)")
    if args.cmd:
        print(f"  cmd filter: {args.cmd}")
    if args.event_type:
        print(f"  type filter: {args.event_type}")
    print()

    try:
        for ev in tail_events(args.log_dir, args.cmd, args.event_type):
            print(format_event(ev))
            if args.show_chain:
                chain = format_chain(ev)
                if chain:
                    print(chain)
    except KeyboardInterrupt:
        print("\nStopped.")


if __name__ == "__main__":
    main()
