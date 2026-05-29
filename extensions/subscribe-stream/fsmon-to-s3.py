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
  uv run extensions/subscribe-stream/fsmon-to-s3.py --bucket my-audit-bucket --prefix fsmon/nginx/
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
    import boto3
    HAS_S3 = True
except ImportError:
    HAS_S3 = False


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
    parser = argparse.ArgumentParser(description="fsmon -> S3 archiver")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--bucket", required=True, help="S3 bucket name")
    parser.add_argument("--prefix", default="fsmon/", help="S3 object key prefix")
    parser.add_argument("--flush-secs", type=int, default=60, help="Upload interval in seconds")
    parser.add_argument("--flush-count", type=int, default=10000, help="Upload after N events")
    parser.add_argument("--dlq-dir", default=None,
                        help="Dead letter queue directory (default: $TMPDIR/fsmon-dlq)")
    args = parser.parse_args()

    if not HAS_S3:
        logging.error("boto3 required. Install: pip install boto3")
        sys.exit(1)

    socket_path = args.socket or _get_socket_path()
    dlq_dir = args.dlq_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "fsmon-dlq"
    )
    os.makedirs(dlq_dir, exist_ok=True)

    s3 = boto3.client("s3", config=boto3.session.Config(
        connect_timeout=5, retries={"max_attempts": 1}
    ))
    prefix = args.prefix.rstrip("/") + "/"
    logging.info("listening on %s -> S3 s3://%s/%s", socket_path, args.bucket, prefix)
    signal.signal(signal.SIGTERM, _on_sigterm)
    if args.track_cmd:
        logging.info("  cmd filter: %s", args.track_cmd)
    if args.types:
        logging.info("  type filter: %s", args.types)
    logging.info("  dlq: %s", dlq_dir)

    buffer: list[dict] = []
    last_flush = time.time()
    last_stats = time.time()
    uploaded = 0
    errors = 0

    def flush():
        nonlocal last_flush, last_stats, uploaded, errors
        if not buffer:
            return
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        key = f"{prefix}{ts}.jsonl"
        body = "\n".join(json.dumps(ev) for ev in buffer) + "\n"
        for attempt in range(3):
            try:
                s3.put_object(
                    Bucket=args.bucket, Key=key,
                    Body=body.encode(), ContentType="application/x-ndjson"
                )
                uploaded += len(buffer)
                logging.info("uploaded %d events -> s3://%s/%s", len(buffer), args.bucket, key)
                break
            except Exception as e:
                if attempt < 2:
                    delay = 2 ** attempt
                    logging.warning("s3 upload attempt %d/3 failed: %s", attempt + 1, e)
                    time.sleep(delay)
                else:
                    logging.error("s3 upload failed: %s", e)
                    for ev in buffer:
                        _write_dlq(dlq_dir, {"event": ev, "error": str(e), "key": key})
                    errors += len(buffer)
        now = time.time()
        if now - last_stats >= 30:
            _print_stats(uploaded, errors)
            last_stats = now
        buffer.clear()
        last_flush = time.time()

    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            buffer.append(ev)
            if len(buffer) >= args.flush_count or time.time() - last_flush >= args.flush_secs:
                flush()
    except KeyboardInterrupt:
        logging.info("stopped.")
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)
    finally:
        flush()
        logging.info("done. uploaded: %d events, errors: %d", uploaded, errors)


if __name__ == "__main__":
    main()
