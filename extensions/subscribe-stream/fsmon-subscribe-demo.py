#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
"""
fsmon Subscribe Demo — real-time event stream consumer.

Connect to fsmon daemon's Unix socket, subscribe to the event stream,
and print events as they arrive. Auto-reconnects if the daemon restarts.

Usage:
  uv run subscribe-stream/fsmon-subscribe-demo.py
  uv run subscribe-stream/fsmon-subscribe-demo.py --track-cmd nginx --types CLOSE_WRITE

Prerequisites:
  fsmon — file system monitor (https://github.com/xxx/fsmon)
  sudo fsmon daemon
"""

import argparse
import json
import logging
import os
import signal
import socket
import sys
import time

_shutdown = False


def _on_sigterm(signum, frame):
    global _shutdown
    _shutdown = True

# ── Socket path ─────────────────────────────────────────────────────

def get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


# ── Subscribe ───────────────────────────────────────────────────────

def subscribe(socket_path: str, track_cmd: str | None = None,
              type_filter: str | None = None):
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


# ── Main ────────────────────────────────────────────────────────────

def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )

    parser = argparse.ArgumentParser(description="Subscribe to fsmon real-time event stream")
    parser.add_argument("--socket", default=None, help="Socket path (auto-detected)")
    parser.add_argument("--track-cmd", default=None, help="Filter by cmd group")
    parser.add_argument("--types", default=None, help="Comma-separated event types")
    args = parser.parse_args()

    socket_path = args.socket or get_socket_path()
    logging.info("fsmon-subscribe-demo starting on %s", socket_path)
    signal.signal(signal.SIGTERM, _on_sigterm)

    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            print(f"[{ev['event_type']}] {ev['path']}  pid={ev['pid']}  cmd={ev['cmd']}")
    except KeyboardInterrupt:
        logging.info("stopped.")
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)


if __name__ == "__main__":
    main()
