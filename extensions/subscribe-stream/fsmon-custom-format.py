#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
"""

fsmon Custom Format Converter — subscribe stream → any text format.

Receives events from fsmon's subscribe stream and converts them to
various text-based output formats. No external dependencies (stdlib only).

── Built-in Formats ────────────────────────────────────────────────
  csv      Comma-separated values (import to Excel, pandas)
  tsv      Tab-separated values
  syslog   RFC 5424 syslog format (forward to rsyslog/syslog-ng)
  loki     Grafana Loki logfmt (label=value pairs for Loki ingestion)
  json     Pretty-printed JSON
  human    Human-readable one-line summary (default)

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: start the daemon
  sudo fsmon daemon

  # CSV output → pipe to file or another tool
  python3 extensions/subscribe-stream/fsmon-custom-format.py --format csv > events.csv

  # Syslog format → forward to local syslog via logger command
  python3 extensions/subscribe-stream/fsmon-custom-format.py --format syslog | logger -t fsmon

  # Loki format → send to Loki's push API
  python3 extensions/subscribe-stream/fsmon-custom-format.py --format loki \
      | curl -H "Content-Type: text/plain" --data-binary @- http://loki:3100/loki/api/v1/push

  # Only nginx events in syslog format
  python3 extensions/subscribe-stream/fsmon-custom-format.py \
      --format syslog --track-cmd nginx

── Bridge To ────────────────────────────────────────────────────────
  - rsyslog / syslog-ng (via pipe to logger command)
  - Grafana Loki (via logfmt → Loki push API)
  - CSV consumers (Excel, pandas, R, database import)
  - Any tool that reads from stdin or supports text-based ingestion
"""

import argparse
import csv
import io
import json
import socket
import sys
from datetime import datetime, timezone


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


# -- Format functions --

def format_csv(ev):
    """Flat CSV: time,event_type,path,pid,cmd,file_size,chain"""
    out = io.StringIO()
    w = csv.writer(out)
    w.writerow([
        ev.get("time", ""),
        ev.get("event_type", ""),
        ev.get("path", ""),
        str(ev.get("pid", "")),
        ev.get("cmd", ""),
        str(ev.get("file_size", "")),
        ev.get("chain", ""),
    ])
    return out.getvalue().rstrip()


def format_tsv(ev):
    return "\t".join([
        ev.get("time", ""),
        ev.get("event_type", ""),
        ev.get("path", ""),
        str(ev.get("pid", "")),
        ev.get("cmd", ""),
    ])


def format_syslog(ev):
    """RFC 5424 style: <PRI>VERSION TIMESTAMP HOST APP PID MSGID STRUCTURED-DATA MSG"""
    try:
        ts = datetime.fromisoformat(ev.get("time", ""))
    except Exception:
        ts = datetime.now(timezone.utc)
    pri = "14"  # user.info
    if ev.get("event_type") == "DELETE" or ev.get("event_type") == "DELETE_SELF":
        pri = "12"  # user.warning
    elif ev.get("event_type") == "FS_ERROR":
        pri = "11"  # user.err
    host = socket.gethostname()
    msg = f"fsmon [{ev.get('event_type', '?')}] {ev.get('path', '?')} pid={ev.get('pid', '?')} cmd={ev.get('cmd', '?')}"
    return f"<{pri}>1 {ts:%Y-%m-%dT%H:%M:%S.%fZ} {host} fsmon {ev.get('pid', '-')} - [fsmon event_type=\"{ev.get('event_type', '')}\" path=\"{ev.get('path', '')}\"] {msg}"


def format_loki(ev):
    """Grafana Loki logfmt: timestamp {labels} message"""
    labels = {
        "event_type": ev.get("event_type", ""),
        "cmd": ev.get("cmd", ""),
        "path": ev.get("path", ""),
    }
    label_str = ",".join(f'{k}="{v}"' for k, v in labels.items() if v)
    return f"{{ {label_str} }} pid={ev.get('pid', '')} user={ev.get('user', '')} size={ev.get('file_size', '')}"


def format_json(ev):
    return json.dumps(ev)


def format_human(ev):
    try:
        ts = datetime.fromisoformat(ev.get("time", ""))
        tstr = ts.strftime("%H:%M:%S")
    except Exception:
        tstr = "??:??:??"
    return f"[{tstr}] {ev.get('event_type', '?'):12s} {ev.get('path', '?')}  pid={ev.get('pid', '?')}  cmd={ev.get('cmd', '?')}"


FORMATTERS = {
    "csv": format_csv,
    "tsv": format_tsv,
    "syslog": format_syslog,
    "loki": format_loki,
    "json": format_json,
    "human": format_human,
}


def main():
    parser = argparse.ArgumentParser(description="fsmon -> custom format converter")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--format", default="human", choices=list(FORMATTERS.keys()), help="Output format")
    args = parser.parse_args()

    fmt = FORMATTERS[args.format]
    for ev in subscribe(args.socket, args.track_cmd, args.types):
        print(fmt(ev), flush=True)


if __name__ == "__main__":
    main()
