"""
Canonical subscribe() generator for fsmon real-time event stream.

DO NOT import this file. Copy the function into your bridge script.
This is a reference implementation — each script should be self-contained.

Usage:
    for event in subscribe(socket_path, track_cmd="nginx", types_filter="CREATE,DELETE"):
        process(event)
"""

import json
import logging
import socket
import time
from collections.abc import Generator
from typing import Any

logger = logging.getLogger(__name__)


def subscribe(
    socket_path: str,
    track_cmd: str | None = None,
    types_filter: str | None = None,
    reconnect: bool = True,
) -> Generator[dict[str, Any], None, None]:
    """Subscribe to fsmon real-time event stream.

    Connects to fsmon daemon's Unix socket, sends a subscribe TOML command,
    and yields parsed JSON events as they arrive.

    Args:
        socket_path: Path to fsmon daemon socket (e.g. /tmp/fsmon-1000.sock).
        track_cmd: Optional cmd group filter (e.g. "nginx").
        types_filter: Optional comma-separated event types (e.g. "CREATE,DELETE").
        reconnect: If True, auto-reconnect on daemon restart (exponential backoff).

    Yields:
        Parsed event dicts with keys: time, event_type, path, pid, cmd, etc.
        Warning messages yield a dict with a "warning" key.
    """
    delay = 1  # seconds, for reconnection backoff

    while True:
        try:
            yield from _subscribe_inner(socket_path, track_cmd, types_filter)
        except (ConnectionRefusedError, FileNotFoundError, BrokenPipeError,
                ConnectionError, socket.timeout, OSError) as e:
            if not reconnect:
                logger.error("subscribe connection lost: %s", e)
                return
            logger.warning("disconnected, reconnecting in %ds... (%s)", delay, e)
            time.sleep(delay)
            delay = min(delay * 2, 60)
        else:
            # Normal EOF (daemon shut down gracefully)
            if not reconnect:
                return
            logger.warning("daemon closed connection, reconnecting in %ds...", delay)
            time.sleep(delay)
            delay = min(delay * 2, 60)

    # Reset delay on successful reconnection happens implicitly —
    # _subscribe_inner raises on disconnect, outer loop catches, backoff applies.


def _subscribe_inner(
    socket_path: str,
    track_cmd: str | None,
    types_filter: str | None,
) -> Generator[dict[str, Any], None, None]:
    """Single subscribe connection. Raises on disconnect."""
    # Build TOML payload (copy dict_to_toml from _templates/toml_helpers.py)
    cmd: dict[str, Any] = {"cmd": "subscribe"}
    if track_cmd:
        cmd["track_cmd"] = track_cmd
    if types_filter:
        cmd["types"] = [t.strip() for t in types_filter.split(",")]

    payload = _dict_to_toml(cmd) + "\n"

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)  # read timeout — daemon sends keepalive on idle
        s.connect(socket_path)
        s.sendall(payload.encode())

        reader = s.makefile("r")
        resp = reader.readline()
        if "ok = true" not in resp:
            raise ConnectionError(f"subscribe rejected: {resp.strip()}")

        logger.info("connected to %s", socket_path)

        json_errors = 0
        for line in reader:
            line = line.strip()
            if not line:
                continue

            # Warning messages from daemon
            if '"warning"' in line:
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    logger.warning("unparseable warning: %s", line[:120])
                continue

            # Event lines
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                json_errors += 1
                logger.error(
                    "JSON decode error (#%d): %s", json_errors, line[:120]
                )


# ── Minimal TOML serialization (inlined — copy from _templates/toml_helpers.py) ──

def _dict_to_toml(d: dict[str, Any]) -> str:
    """Serialize a flat dict to TOML."""
    lines: list[str] = []
    for key, value in d.items():
        if value is None:
            continue
        if isinstance(value, bool):
            lines.append(f"{key} = {'true' if value else 'false'}")
        elif isinstance(value, list):
            items = ", ".join(_toml_escape_scalar(v) for v in value)
            lines.append(f"{key} = [{items}]")
        elif isinstance(value, str):
            lines.append(f"{key} = {_toml_escape_scalar(value)}")
        else:
            lines.append(f"{key} = {value}")
    return "\n".join(lines)


def _toml_escape_scalar(value: str) -> str:
    """Escape a TOML string value."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'
