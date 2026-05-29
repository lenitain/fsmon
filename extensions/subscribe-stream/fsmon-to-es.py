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
  uv run extensions/subscribe-stream/fsmon-to-es.py --host localhost:9200 --index fsmon-events

  # With authentication
  uv run extensions/subscribe-stream/fsmon-to-es.py --host https://es.example.com:9200 --user admin --pass secret
"""

import argparse
import json
import logging
import os
import signal
import socket
import sys
import time
from datetime import datetime, timezone
from collections.abc import Generator
from typing import Any

_shutdown = False


def _on_sigterm(signum: int, frame: Any) -> None:
    global _shutdown
    _shutdown = True

try:
    from elasticsearch import Elasticsearch, helpers
    HAS_ES = True
except ImportError:
    HAS_ES = False


def subscribe(socket_path: str, track_cmd: str | None = None,
              type_filter: str | None = None) -> Generator[dict[str, Any], None, None]:
    """Yield fsmon events with auto-reconnect and error logging."""
    _log = logging.getLogger("fsmon.subscribe")
    delay = 1.0
    while not _shutdown:
        try:
            yield from _subscribe_inner(socket_path, track_cmd, type_filter)
            delay = 1.0
        except (ConnectionRefusedError, FileNotFoundError, BrokenPipeError,
                ConnectionError, socket.timeout, OSError) as e:
            if _shutdown:
                return
            _log.warning("disconnected, reconnecting in %.0fs... (%s)", delay, e)
            time.sleep(delay)
            delay = min(delay * 2, 60)
        else:
            if _shutdown:
                return
            _log.warning("daemon closed connection, reconnecting in %.0fs...", delay)
            time.sleep(delay)
            delay = min(delay * 2, 60)


def _subscribe_inner(socket_path: str, track_cmd: str | None,
                     type_filter: str | None) -> Generator[dict[str, Any], None, None]:
    """Single subscribe connection. Raises on disconnect."""
    _log = logging.getLogger("fsmon.subscribe")
    cmd: dict = {"cmd": "subscribe"}
    if track_cmd:
        cmd["track_cmd"] = track_cmd
    if type_filter:
        cmd["types"] = [t.strip() for t in type_filter.split(",")]
    payload = _dict_to_toml(cmd) + "\n\n"

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


def _dict_to_toml(d: dict[str, Any]) -> str:
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


def to_es_doc(ev: dict[str, Any]) -> dict[str, Any]:
    """Convert fsmon event to ES document with all fields preserved."""
    return {
        "_index": None,  # set by caller
        "_source": {k: v for k, v in ev.items()},
    }


def _get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


def _write_dlq(directory: str, item: dict[str, Any]) -> None:
    today = time.strftime("%Y-%m-%d")
    path = os.path.join(directory, f"dlq-{today}.jsonl")
    try:
        with open(path, "a") as f:
            f.write(json.dumps(item, default=str) + "\n")
    except OSError as exc:
        logging.error("dead-letter write failed: %s", exc)


def _print_stats(count: int, errors: int) -> None:
    print(json.dumps({
        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "events": count,
        "errors": errors,
    }), file=sys.stderr, flush=True)


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )
    parser = argparse.ArgumentParser(description="fsmon -> Elasticsearch bridge")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--host", default="http://localhost:9200", help="ES host URL")
    parser.add_argument("--user", help="ES username")
    parser.add_argument("--pass", dest="pwd", help="ES password")
    parser.add_argument("--index", default="fsmon-events", help="ES index name (date-rolled)")
    parser.add_argument("--index-granularity", default="daily", choices=["daily", "hourly"],
                        help="Index rotation granularity")
    parser.add_argument("--flush-secs", type=int, default=5, help="Bulk flush interval")
    parser.add_argument("--flush-count", type=int, default=1000, help="Bulk flush count")
    parser.add_argument("--dlq-dir", default=None,
                        help="Dead letter queue directory (default: $TMPDIR/fsmon-dlq)")
    args = parser.parse_args()

    if not HAS_ES:
        logging.error("elasticsearch required. Install: pip install elasticsearch")
        sys.exit(1)

    socket_path = args.socket or _get_socket_path()
    dlq_dir = args.dlq_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "fsmon-dlq"
    )
    os.makedirs(dlq_dir, exist_ok=True)

    es_kwargs: dict = {"hosts": [args.host]}
    if args.user:
        es_kwargs["basic_auth"] = (args.user, args.pwd or "")
    try:
        es = Elasticsearch(**es_kwargs)
        if not es.ping():
            raise ConnectionError("ping failed")
    except Exception as e:
        logging.error("cannot connect to ES %s: %s", args.host, e)
        sys.exit(1)

    logging.info("listening on %s -> ES %s/%s-YYYY.MM.DD", socket_path, args.host, args.index)
    signal.signal(signal.SIGTERM, _on_sigterm)
    if args.track_cmd:
        logging.info("  cmd filter: %s", args.track_cmd)
    if args.types:
        logging.info("  type filter: %s", args.types)
    logging.info("  dlq: %s", dlq_dir)

    buffer: list[dict] = []
    last_flush = time.time()
    last_stats = time.time()
    errors = 0
    indexed = 0

    def flush():
        nonlocal last_flush, errors, indexed, last_stats
        if not buffer:
            return
        failed: list[dict] = []
        for ok, info in helpers.streaming_bulk(es, buffer, raise_on_error=False):
            if ok:
                indexed += 1
            else:
                failed.append(info)
        for doc_info in failed:
            try:
                es.index(index=doc_info.get("_index", ""),
                         id=doc_info.get("_id"),
                         body=doc_info.get("_source"),
                         timeout="5s")
                indexed += 1
            except Exception as e:
                _write_dlq(dlq_dir, {"doc": doc_info, "error": str(e)})
                errors += 1
        if indexed > 0 or errors > 0:
            logging.info("indexed %d docs (%d errors)", indexed, errors)
        now = time.time()
        if now - last_stats >= 30:
            _print_stats(indexed, errors)
            last_stats = now
        buffer.clear()
        last_flush = time.time()

    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            try:
                ts = datetime.fromisoformat(ev["time"])
            except (ValueError, KeyError):
                ts = datetime.now(timezone.utc)
            idx = f"{args.index}-{ts:%Y.%m.%d}" if args.index_granularity == "daily" \
                else f"{args.index}-{ts:%Y.%m.%d.%H}"
            doc = to_es_doc(ev)
            doc["_index"] = idx
            doc["_id"] = f"{ts.timestamp()}-{ev.get('pid', 0)}-{hash(ev.get('path', ''))}"
            buffer.append(doc)
            if len(buffer) >= args.flush_count or time.time() - last_flush >= args.flush_secs:
                flush()
    except KeyboardInterrupt:
        logging.info("stopped.")
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)
    finally:
        flush()
        logging.info("done. indexed: %d docs, errors: %d", indexed, errors)


if __name__ == "__main__":
    main()
