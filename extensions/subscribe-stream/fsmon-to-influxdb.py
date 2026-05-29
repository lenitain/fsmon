#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["influxdb-client>=1.40"]
# ///
"""

fsmon -> InfluxDB bridge (line protocol).

Receives events from fsmon subscribe, writes to InfluxDB as time-series data.
Each event becomes a point with event_type and cmd as tags, file_size as a field.

Dependency:
  pip install influxdb-client

Usage:
  export INFLUXDB_TOKEN=xxx
  uv run extensions/subscribe-stream/fsmon-to-influxdb.py --url http://localhost:8086 --org my-org --bucket fsmon
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
    from influxdb_client import InfluxDBClient
    from influxdb_client.client.write_api import SYNCHRONOUS
    HAS_INFLUX = True
except ImportError:
    HAS_INFLUX = False


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


# ── InfluxDB line protocol escaping ──────────────────────────────

def _escape_tag(value: str) -> str:
    """Escape a tag value for InfluxDB line protocol."""
    return value.replace("\\", "\\\\").replace(" ", "\\ ").replace(",", "\\,").replace("=", "\\=")


def _to_line_protocol(ev: dict[str, Any], timestamp_ns: int) -> str:
    """Convert a fsmon event to InfluxDB line protocol with proper escaping."""
    event_type = _escape_tag(ev.get("event_type", "?"))
    cmd_name = _escape_tag(ev.get("cmd", "?"))
    path = _escape_tag(ev.get("path", "?"))
    pid = ev.get("pid", 0)
    file_size = ev.get("file_size", 0)
    return (
        f"fsmon_events,"
        f"event_type={event_type},"
        f"cmd={cmd_name},"
        f"path={path} "
        f"pid={pid}i,"
        f"file_size={file_size}i "
        f"{timestamp_ns}"
    )


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
    parser = argparse.ArgumentParser(description="fsmon -> InfluxDB bridge")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--url", default="http://localhost:8086", help="InfluxDB URL")
    parser.add_argument("--org", default=os.environ.get("INFLUXDB_ORG", "my-org"), help="InfluxDB org")
    parser.add_argument("--bucket", default="fsmon", help="InfluxDB bucket")
    parser.add_argument("--token", default=os.environ.get("INFLUXDB_TOKEN", ""), help="InfluxDB token")
    parser.add_argument("--dlq-dir", default=None,
                        help="Dead letter queue directory (default: $TMPDIR/fsmon-dlq)")
    args = parser.parse_args()

    if not HAS_INFLUX:
        logging.error("influxdb-client required. Install: pip install influxdb-client")
        sys.exit(1)
    if not args.token:
        logging.error("set INFLUXDB_TOKEN or pass --token")
        sys.exit(1)

    socket_path = args.socket or _get_socket_path()
    dlq_dir = args.dlq_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "fsmon-dlq"
    )
    os.makedirs(dlq_dir, exist_ok=True)

    client = InfluxDBClient(url=args.url, token=args.token, org=args.org)
    write_api = client.write_api(write_options=SYNCHRONOUS)

    logging.info("listening on %s -> InfluxDB %s/%s", socket_path, args.url, args.bucket)
    signal.signal(signal.SIGTERM, _on_sigterm)
    if args.track_cmd:
        logging.info("  cmd filter: %s", args.track_cmd)
    if args.types:
        logging.info("  type filter: %s", args.types)
    logging.info("  dlq: %s", dlq_dir)

    count = 0
    errors = 0
    last_stats = time.time()
    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            try:
                ts = datetime.fromisoformat(ev["time"])
            except (ValueError, KeyError):
                ts = datetime.now(timezone.utc)
            timestamp_ns = int(ts.timestamp() * 1_000_000_000)
            line = _to_line_protocol(ev, timestamp_ns)
            for attempt in range(3):
                try:
                    write_api.write(bucket=args.bucket, record=line)
                    break
                except Exception as e:
                    if attempt < 2:
                        delay = 2 ** attempt
                        logging.warning("influx write attempt %d/3 failed: %s", attempt + 1, e)
                        time.sleep(delay)
                    else:
                        logging.error("influx write failed: %s", e)
                        _write_dlq(dlq_dir, {"event": ev, "error": str(e), "line": line})
                        errors += 1
            count += 1
            if count % 1000 == 0:
                logging.info("wrote %d points (%d errors)", count, errors)
            now = time.time()
            if now - last_stats >= 30:
                _print_stats(count, errors)
                last_stats = now
    except KeyboardInterrupt:
        logging.info("stopped. total: %d points, %d errors", count, errors)
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)
    finally:
        client.close()


if __name__ == "__main__":
    main()
