#!/usr/bin/env python3
"""
EXAMPLE ONLY — NOT FOR PRODUCTION USE.
Adapt this script to your environment before deploying.

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
  python3 fsmon-to-s3.py --bucket my-audit-bucket --prefix fsmon/nginx/
"""

import argparse
import json
import os
import socket
import sys
import time
from datetime import datetime, timezone

try:
    import boto3
    HAS_S3 = True
except ImportError:
    HAS_S3 = False


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
