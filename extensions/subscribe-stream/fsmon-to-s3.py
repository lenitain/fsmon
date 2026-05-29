#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.30"]
# ///
"""

fsmon -> S3 archiver bridge

Receives real-time events from fsmon subscribe, batches them, and uploads
to S3 periodically. Does not write one-by-one: buffers in memory and flushes
every 60s or 10000 events.

Dependency:
  pip install boto3

S3 credentials are read from environment variables or ~/.aws/credentials.

Usage:
  export AWS_ACCESS_KEY_ID=xxx
  export AWS_SECRET_ACCESS_KEY=xxx
  python3 extensions/subscribe-stream/fsmon-to-s3.py --bucket my-audit-bucket --prefix fsmon/nginx/
"""

import argparse
import json
import os
import socket
import sys
import time
import logging
from datetime import datetime, timezone

try:
    import boto3
    HAS_S3 = True
except ImportError:
    HAS_S3 = False


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
    parser = argparse.ArgumentParser(description="fsmon -> S3 archiver")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--bucket", required=True, help="S3 bucket name")
    parser.add_argument("--prefix", default="fsmon/", help="S3 object key prefix")
    parser.add_argument("--flush-secs", type=int, default=60, help="Upload interval in seconds")
    parser.add_argument("--flush-count", type=int, default=10000, help="Upload after N events")
    args = parser.parse_args()

    if not HAS_S3:
        print("Error: boto3 required. Install: pip install boto3", file=sys.stderr)
        sys.exit(1)

    s3 = boto3.client("s3")
    prefix = args.prefix.rstrip("/") + "/"
    print(f"Listening on {args.socket} -> S3 s3://{args.bucket}/{prefix}")

    buffer = []
    last_flush = time.time()

    def flush():
        nonlocal last_flush
        if not buffer:
            return
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        key = f"{prefix}{ts}.jsonl"
        body = "\n".join(json.dumps(ev) for ev in buffer) + "\n"
        try:
            s3.put_object(Bucket=args.bucket, Key=key, Body=body.encode(), ContentType="application/x-ndjson")
            print(f"[s3] uploaded {len(buffer)} events -> s3://{args.bucket}/{key}", flush=True)
        except Exception as e:
            print(f"[s3] upload failed: {e}", file=sys.stderr)
        buffer.clear()
        last_flush = time.time()

    try:
        for ev in subscribe(args.socket, args.track_cmd, args.types):
            buffer.append(ev)
            if len(buffer) >= args.flush_count or time.time() - last_flush >= args.flush_secs:
                flush()
    finally:
        flush()


if __name__ == "__main__":
    main()
