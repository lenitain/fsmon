#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["elasticsearch>=8.0"]
# ///
"""

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
import logging
from datetime import datetime, timezone

try:
    from elasticsearch import Elasticsearch, helpers
    HAS_ES = True
except ImportError:
    HAS_ES = False


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
