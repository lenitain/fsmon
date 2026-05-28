#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

fsmon -> Elasticsearch bridge

Receives real-time events from fsmon subscribe, bulk-indexes to Elasticsearch.
Uses the bulk API, flushing every 5s or 1000 events.

Dependency:
  pip install elasticsearch

Usage:
  python3 extensions/subscribe-stream/fsmon-to-es.py --host localhost:9200 --index fsmon-events

  # With authentication
  python3 extensions/subscribe-stream/fsmon-to-es.py --host https://es.example.com:9200 --user admin --pass secret
"""

import argparse
import json
import socket
import sys
import time
from datetime import datetime, timezone

try:
    from elasticsearch import Elasticsearch, helpers
    HAS_ES = True
except ImportError:
    HAS_ES = False


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


def to_es_doc(ev: dict) -> dict:
    """Convert fsmon event to ES document with timestamp index."""
    return {
        "_index": None,  # set by caller
        "_source": {
            "time": ev["time"],
            "event_type": ev["event_type"],
            "path": ev["path"],
            "pid": ev.get("pid"),
            "cmd": ev.get("cmd", ""),
            "user": ev.get("user", ""),
            "file_size": ev.get("file_size", 0),
            "ppid": ev.get("ppid"),
            "tgid": ev.get("tgid"),
            "chain": ev.get("chain", ""),
        }
    }


def main():
    parser = argparse.ArgumentParser(description="fsmon -> Elasticsearch bridge")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--host", default="http://localhost:9200", help="ES host URL")
    parser.add_argument("--user", help="ES username")
    parser.add_argument("--pass", dest="pwd", help="ES password")
    parser.add_argument("--index", default="fsmon-events", help="ES index name (date-rolled)")
    parser.add_argument("--flush-secs", type=int, default=5, help="Bulk flush interval")
    parser.add_argument("--flush-count", type=int, default=1000, help="Bulk flush count")
    args = parser.parse_args()

    if not HAS_ES:
        print("Error: elasticsearch required. Install: pip install elasticsearch", file=sys.stderr)
        sys.exit(1)

    # ES connection
    es_kwargs = {"hosts": [args.host]}
    if args.user:
        es_kwargs["basic_auth"] = (args.user, args.pwd or "")
    es = Elasticsearch(**es_kwargs)

    if not es.ping():
        print(f"Error: cannot connect to ES {args.host}", file=sys.stderr)
        sys.exit(1)

    print(f"Listening on {args.socket} -> ES {args.host}/{args.index}-YYYY.MM.DD")

    buffer = []
    last_flush = time.time()

    def flush():
        nonlocal last_flush
        if not buffer:
            return
        done = 0
        for ok, info in helpers.streaming_bulk(es, buffer, raise_on_error=False):
            if ok:
                done += 1
        if done > 0:
            print(f"[es] indexed {done} docs", flush=True)
        buffer.clear()
        last_flush = time.time()

    try:
        for ev in subscribe(args.socket, args.track_cmd, args.types):
            # Daily index: fsmon-events-2026.05.27
            try:
                ts = datetime.fromisoformat(ev["time"])
            except (ValueError, KeyError):
                ts = datetime.now(timezone.utc)
            idx = f"{args.index}-{ts:%Y.%m.%d}"
            doc = to_es_doc(ev)
            doc["_index"] = idx
            doc["_id"] = f"{ts.timestamp()}-{ev.get('pid', 0)}-{hash(ev.get('path', ''))}"
            buffer.append(doc)

            if len(buffer) >= args.flush_count or time.time() - last_flush >= args.flush_secs:
                flush()
    finally:
        flush()


if __name__ == "__main__":
    main()
