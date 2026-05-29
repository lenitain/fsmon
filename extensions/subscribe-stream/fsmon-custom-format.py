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
import logging
import os
import signal
import socket
import sys
import time
from collections.abc import Generator
from datetime import datetime, timezone
from typing import Any

_shutdown = False


def _on_sigterm(signum: int, frame: Any) -> None:
    global _shutdown
    _shutdown = True


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


def _get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )
    parser = argparse.ArgumentParser(description="fsmon -> custom format converter")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--format", default="human", choices=list(FORMATTERS.keys()), help="Output format")
    args = parser.parse_args()

    socket_path = args.socket or _get_socket_path()
    signal.signal(signal.SIGTERM, _on_sigterm)
    logging.info("listening on %s -> format=%s", socket_path, args.format)
    if args.track_cmd:
        logging.info("  cmd filter: %s", args.track_cmd)
    if args.types:
        logging.info("  type filter: %s", args.types)

    fmt = FORMATTERS[args.format]
    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            print(fmt(ev), flush=True)
    except KeyboardInterrupt:
        logging.info("stopped.")
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)


if __name__ == "__main__":
    main()
