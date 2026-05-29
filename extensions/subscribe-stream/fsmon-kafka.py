#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["kafka-python>=2.0"]
# ///
"""

fsmon -> Kafka bridge

Receives real-time events from fsmon subscribe, forwards to a Kafka topic.

Dependency:
  pip install kafka-python

If kafka-python is not installed, the script prints install instructions and exits.

Usage:
  # All events to Kafka
  python3 extensions/subscribe-stream/fsmon-kafka.py --broker localhost:9092 --topic fsmon-events

  # Only nginx CLOSE_WRITE events
  python3 extensions/subscribe-stream/fsmon-kafka.py --broker localhost:9092 --topic nginx-writes --track-cmd nginx --types CLOSE_WRITE

Prometheus / Grafana can consume the fsmon_events_total counter from Kafka.
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
import logging

try:
    from kafka import KafkaProducer
    from kafka.errors import NoBrokersAvailable
    HAS_KAFKA = True
except ImportError:
    HAS_KAFKA = False


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


def _get_socket_path() -> str:
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


def _write_dlq(directory: str, item: dict) -> None:
    """Append a failed item to the daily dead-letter file."""
    today = time.strftime("%Y-%m-%d")
    path = os.path.join(directory, f"dlq-{today}.jsonl")
    try:
        with open(path, "a") as f:
            f.write(json.dumps(item, default=str) + "\n")
    except OSError as exc:
        logging.error("dead-letter write failed: %s", exc)


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )
    parser = argparse.ArgumentParser(description="fsmon -> Kafka bridge")
    parser.add_argument("--socket", default=None, help="fsmon daemon socket (auto-detected)")
    parser.add_argument("--track-cmd", help="Filter by cmd group")
    parser.add_argument("--types", help="Comma-separated event types")
    parser.add_argument("--broker", default="localhost:9092", help="Kafka broker address")
    parser.add_argument("--topic", default="fsmon-events", help="Kafka topic")
    parser.add_argument("--dlq-dir", default=None,
                        help="Dead letter queue directory (default: $TMPDIR/fsmon-dlq)")
    args = parser.parse_args()

    if not HAS_KAFKA:
        logging.error("kafka-python required. Install: pip install kafka-python")
        sys.exit(1)

    socket_path = args.socket or _get_socket_path()
    dlq_dir = args.dlq_dir or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "fsmon-dlq"
    )
    os.makedirs(dlq_dir, exist_ok=True)

    logging.info("connecting to Kafka: %s topic=%s", args.broker, args.topic)

    try:
        producer = KafkaProducer(
            bootstrap_servers=args.broker,
            value_serializer=lambda v: json.dumps(v).encode("utf-8"),
            key_serializer=lambda v: v.encode("utf-8") if v else None,
            acks=1,
            retries=3,
        )
    except NoBrokersAvailable:
        logging.error("cannot connect to Kafka broker %s", args.broker)
        sys.exit(1)

    logging.info("listening on %s -> Kafka topic %s", socket_path, args.topic)
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
            key = f"{ev.get('cmd', '?')}:{ev['event_type']}"
            future = producer.send(args.topic, value=ev, key=key)
            # Confirm delivery with retry
            for attempt in range(3):
                try:
                    future.get(timeout=5)
                    break
                except Exception as e:
                    if attempt < 2:
                        delay = 2 ** attempt
                        logging.warning("kafka send attempt %d/3 failed: %s", attempt + 1, e)
                        time.sleep(delay)
                    else:
                        logging.error("kafka send failed: %s", e)
                        _write_dlq(dlq_dir, {"event": ev, "error": str(e)})
                        errors += 1
            count += 1
            if count % 1000 == 0:
                logging.info("sent %d events (%d errors)", count, errors)
    except KeyboardInterrupt:
        logging.info("shutting down...")
    except ConnectionError as e:
        logging.error("%s", e)
        sys.exit(1)
    finally:
        logging.info("flushing producer...")
        producer.flush()
        producer.close()
        logging.info("done. total: %d events, %d errors", count, errors)


if __name__ == "__main__":
    main()
