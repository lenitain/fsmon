#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

fsmon -> InfluxDB bridge (line protocol).

Receives events from fsmon subscribe, writes to InfluxDB as time-series data.
Each event becomes a point with event_type and cmd as tags, file_size as a field.

Dependency:
  pip install influxdb-client

Usage:
  export INFLUXDB_TOKEN=xxx
  python3 fsmon-to-influxdb.py --url http://localhost:8086 --org my-org --bucket fsmon
"""

import argparse
import json
import os
import socket
import sys
from datetime import datetime, timezone

try:
    from influxdb_client import InfluxDBClient
    from influxdb_client.client.write_api import SYNCHRONOUS
    HAS_INFLUX = True
except ImportError:
    HAS_INFLUX = False


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
