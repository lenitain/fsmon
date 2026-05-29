"""
Canonical fsmon Unix socket helpers.

DO NOT import this file. Copy the functions you need into your bridge script.
This is a reference implementation — each script should be self-contained.

Provides:
  - get_socket_path()   — resolve fsmon daemon socket path (SUDO_UID aware)
  - FsmonError hierarchy — typed exceptions for socket operations
  - send_cmd()          — send a TOML command to the daemon, return parsed response
"""

import os
import socket
from typing import Any


# ── Exceptions ──────────────────────────────────────────────────────

class FsmonError(Exception):
    """Base exception for fsmon client errors."""


class FsmonConnectionError(FsmonError):
    """Cannot connect to fsmon daemon socket."""


class FsmonTimeoutError(FsmonError):
    """Socket operation timed out."""


class FsmonProtocolError(FsmonError):
    """Invalid or unexpected response from daemon."""


# ── Socket path ─────────────────────────────────────────────────────

def get_socket_path() -> str:
    """Return the fsmon daemon Unix socket path.

    The daemon names its socket /tmp/fsmon-<UID>.sock where UID is
    the original user's UID (from SUDO_UID if running under sudo).
    """
    sudo_uid: str | None = os.environ.get("SUDO_UID")
    uid: str = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


# ── Command interface ───────────────────────────────────────────────

def send_cmd(cmd: dict[str, Any], socket_path: str | None = None) -> dict[str, Any]:
    """Send a TOML command to fsmon daemon and return parsed response.

    Args:
        cmd: Dict with at minimum {"cmd": "..."}. See _templates/toml_helpers.py
             for dict_to_toml() serialization.
        socket_path: Override socket path. Defaults to get_socket_path().

    Returns:
        Parsed response dict. Always contains "ok": bool.

    Raises:
        FsmonConnectionError: socket not found or connection refused.
        FsmonTimeoutError: operation timed out.
        FsmonProtocolError: empty or unparseable response.
    """
    if socket_path is None:
        socket_path = get_socket_path()

    # Build TOML payload (copy dict_to_toml from _templates/toml_helpers.py)
    toml_payload = _dict_to_toml(cmd) + "\n"

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(10)
            s.connect(socket_path)
            s.sendall(toml_payload.encode())

            reader = s.makefile("r")
            response = reader.read()
    except FileNotFoundError:
        raise FsmonConnectionError(
            f"socket not found: {socket_path}\n"
            f"Is the daemon running? Start with: sudo fsmon daemon"
        )
    except ConnectionRefusedError:
        raise FsmonConnectionError(
            f"connection refused: {socket_path}\n"
            f"The daemon may have stopped. Restart with: sudo fsmon daemon"
        )
    except socket.timeout:
        raise FsmonTimeoutError(f"connection timed out: {socket_path}")

    if not response.strip():
        raise FsmonProtocolError("empty response from daemon")

    # Parse TOML response (copy parse_toml_response from _templates/toml_helpers.py)
    return _parse_toml_response(response)


# ── Minimal TOML (inlined — copy from _templates/toml_helpers.py) ──

def _dict_to_toml(d: dict[str, Any]) -> str:
    """Serialize a flat dict to TOML (subset used by fsmon wire protocol)."""
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
    """Escape a TOML scalar string value."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def _parse_toml_response(text: str) -> dict[str, Any]:
    """Parse fsmon SocketResp TOML subset (scalars + array-of-tables)."""
    result: dict[str, Any] = {"ok": False}
    current_path: dict[str, Any] | None = None
    in_paths: bool = False

    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue

        if line.startswith("[[paths]]"):
            in_paths = True
            if "paths" not in result:
                result["paths"] = []
            current_path = {}
            result["paths"].append(current_path)
            continue

        if "=" in line:
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()
            parsed = _parse_toml_value(value)

            if in_paths and current_path is not None:
                current_path[key] = parsed
            else:
                result[key] = parsed
                if key != "paths":
                    in_paths = False

    return result


def _parse_toml_value(value: str) -> Any:
    """Parse a TOML scalar value."""
    value = value.strip()
    if value == "true":
        return True
    if value == "false":
        return False
    if (value.startswith('"') and value.endswith('"')) or \
       (value.startswith("'") and value.endswith("'")):
        return value[1:-1]
    if value.startswith("[") and value.endswith("]"):
        inner = value[1:-1]
        if not inner.strip():
            return []
        return [_parse_toml_value(item) for item in inner.split(",")]
    try:
        return int(value)
    except ValueError:
        pass
    return value
