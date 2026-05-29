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
  python3 extensions/subscribe-stream/fsmon-to-influxdb.py --url http://localhost:8086 --org my-org --bucket fsmon
"""

import argparse
import json
import os
import socket
import sys
import logging
import time
from datetime import datetime, timezone

try:
    from influxdb_client import InfluxDBClient
    from influxdb_client.client.write_api import SYNCHRONOUS
    HAS_INFLUX = True
except ImportError:
    HAS_INFLUX = False


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
    parser = argparse.ArgumentParser(description="fsmon -> InfluxDB bridge (EXAMPLE)")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--url", default="http://localhost:8086", help="InfluxDB URL")
    parser.add_argument("--org", default=os.environ.get("INFLUXDB_ORG", "my-org"), help="InfluxDB org")
    parser.add_argument("--bucket", default="fsmon", help="InfluxDB bucket")
    parser.add_argument("--token", default=os.environ.get("INFLUXDB_TOKEN", ""), help="InfluxDB token")
    args = parser.parse_args()

    if not HAS_INFLUX:
        print("Error: influxdb-client required. Install: pip install influxdb-client", file=sys.stderr)
        sys.exit(1)

    if not args.token:
        print("Error: set INFLUXDB_TOKEN or pass --token", file=sys.stderr)
        sys.exit(1)

    client = InfluxDBClient(url=args.url, token=args.token, org=args.org)
    write_api = client.write_api(write_options=SYNCHRONOUS)

    print(f"Listening on {args.socket} -> InfluxDB {args.url}/{args.bucket}")

    count = 0
    for ev in subscribe(args.socket, args.track_cmd, args.types):
        try:
            ts = datetime.fromisoformat(ev["time"])
        except (ValueError, KeyError):
            ts = datetime.now(timezone.utc)

        # InfluxDB line protocol:
        # measurement,tag1=val1,tag2=val2 field1=val1,field2=val2 timestamp
        line = (
            f"fsmon_events,"
            f"event_type={ev.get('event_type','?')},"
            f"cmd={ev.get('cmd','?')},"
            f"path={ev.get('path','?').replace(' ', '\\ ').replace(',', '\\,')} "
            f"pid={ev.get('pid',0)}i,"
            f"file_size={ev.get('file_size',0)}i "
            f"{int(ts.timestamp() * 1_000_000_000)}"
        )
        write_api.write(bucket=args.bucket, record=line)
        count += 1
        if count % 1000 == 0:
            print(f"[influx] wrote {count} points", flush=True)

    client.close()


if __name__ == "__main__":
    main()
