#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
"""

fsmon → Webhook Bridge — forward file events to HTTP endpoints.

Receives real-time events from fsmon's subscribe stream and POSTs them
as JSON to any HTTP webhook URL. No external dependencies (stdlib only).

── Use Cases ───────────────────────────────────────────────────────
  - Slack / Discord / Feishu / DingTalk alerting on file changes
  - Trigger CI/CD pipeline on suspicious file operations
  - Custom HTTP callback to your own service
  - Audit trail forwarding to a central collector

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: start the daemon
  sudo fsmon daemon

  # Forward ALL events to a webhook receiver
  python3 extensions/subscribe-stream/fsmon-webhook.py \
      --webhook http://localhost:8080/alert

  # Only nginx write events → webhook
  python3 extensions/subscribe-stream/fsmon-webhook.py \
      --track-cmd nginx --types MODIFY,CLOSE_WRITE \
      --webhook https://hooks.slack.com/services/xxx

  # Also print events to stdout for debugging
  python3 extensions/subscribe-stream/fsmon-webhook.py \
      --webhook http://localhost:8080/alert --print

── Slack Example ───────────────────────────────────────────────────
  python3 extensions/subscribe-stream/fsmon-webhook.py \
      --track-cmd nginx \
      --types DELETE \
      --webhook https://hooks.slack.com/services/T.../B.../xxx

  Then configure Slack Incoming Webhook app to receive the JSON payload.

── Bridge To ────────────────────────────────────────────────────────
  - Any HTTP webhook receiver (Slack, Discord, Teams, custom)
  - CI/CD triggers (Jenkins, GitHub Actions via repository_dispatch)
  - Serverless functions (AWS Lambda URL, Cloud Functions)
"""

import argparse
import json
import logging
import os
import signal
import socket
import sys
import time
import urllib.request
from datetime import datetime, timezone

_shutdown = False


def _on_sigterm(signum, frame):
    global _shutdown
    _shutdown = True


# ── Socket path ──────────────────────────────────────────────────

def get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


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


# ── Dead Letter Queue ────────────────────────────────────────────

class DeadLetterQueue:
    """Append failed items to a daily JSONL file."""
    def __init__(self, directory: str):
        self.directory = directory
        os.makedirs(directory, exist_ok=True)

    def append(self, item: dict) -> None:
        today = time.strftime("%Y-%m-%d")
        path = os.path.join(self.directory, f"dlq-{today}.jsonl")
        try:
            with open(path, "a") as f:
                f.write(json.dumps(item, default=str) + "\n")
        except OSError as exc:
            logging.error("dead-letter write failed: %s", exc)


# ── Webhook sender ───────────────────────────────────────────────

def send_webhook(url: str, event: dict, dlq: DeadLetterQueue | None = None) -> bool:
    """Send event as JSON to webhook URL with retry."""
    data = json.dumps(event).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    for attempt in range(3):
        try:
            urllib.request.urlopen(req, timeout=5)
            return True
        except Exception as e:
            if attempt < 2:
                delay = 2 ** attempt
                logging.warning("webhook attempt %d/3 failed: %s, retrying in %ds...",
                                attempt + 1, e, delay)
                time.sleep(delay)
            else:
                logging.error("webhook failed after 3 attempts: %s", e)
                if dlq:
                    dlq.append({"event": event, "error": str(e), "url": url})
                return False
    return False


def format_for_print(ev):
    ts = datetime.fromisoformat(ev["time"])
    return f"[{ts:%H:%M:%S}] {ev['event_type']:12s} {ev['path']}  pid={ev['pid']}  cmd={ev['cmd']}"


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )
    parser = argparse.ArgumentParser(description="fsmon -> Webhook bridge")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--webhook", required=True, help="Webhook URL")
    parser.add_argument("--print", action="store_true", help="Also print events to stdout")
    parser.add_argument("--dlq-dir", default=None,
                        help="Dead letter queue directory (default: $TMPDIR/fsmon-dlq)")
    args = parser.parse_args()

    socket_path = args.socket or get_socket_path()
    dlq_dir = args.dlq_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "fsmon-dlq"
    )
    dlq = DeadLetterQueue(dlq_dir)

    logging.info("listening on %s -> webhook %s", socket_path, args.webhook)
    signal.signal(signal.SIGTERM, _on_sigterm)
    if args.track_cmd:
        logging.info("  cmd filter: %s", args.track_cmd)
    if args.types:
        logging.info("  type filter: %s", args.types)
    logging.info("  dlq: %s", dlq_dir)

    count = 0
    errors = 0
    try:
        for ev in subscribe(socket_path, args.track_cmd, args.types):
            count += 1
            if not send_webhook(args.webhook, ev, dlq):
                errors += 1
            if args.print:
                print(format_for_print(ev))
            if count % 100 == 0:
                logging.info("sent %d events (%d errors)", count, errors)
    except KeyboardInterrupt:
        logging.info("stopped. total: %d events, %d errors", count, errors)
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)


if __name__ == "__main__":
    main()
