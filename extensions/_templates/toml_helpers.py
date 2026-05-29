"""
Canonical TOML helpers for fsmon wire protocol.

DO NOT import this file. Copy the functions you need into your bridge script.
The fsmon wire protocol uses a minimal TOML subset (scalars + array-of-tables).
No external dependencies.

Provides:
  - dict_to_toml()          — serialize flat dict to TOML string
  - parse_toml_response()   — parse fsmon SocketResp TOML
"""

from typing import Any


def dict_to_toml(d: dict[str, Any]) -> str:
    """Serialize a flat dict to TOML subset used by fsmon wire protocol.

    Handles bool, str, int, list[str]. None values are skipped.

    >>> dict_to_toml({"cmd": "subscribe", "track_cmd": "nginx"})
    'cmd = "subscribe"\\ntrack_cmd = "nginx"'
    """
    lines: list[str] = []
    for key, value in d.items():
        if value is None:
            continue
        if isinstance(value, bool):
            lines.append(f"{key} = {'true' if value else 'false'}")
        elif isinstance(value, list):
            items = ", ".join(escape_toml_string(v) if isinstance(v, str) else str(v)
                              for v in value)
            lines.append(f"{key} = [{items}]")
        elif isinstance(value, str):
            lines.append(f"{key} = {escape_toml_string(value)}")
        else:
            lines.append(f"{key} = {value}")
    return "\n".join(lines)


def escape_toml_string(value: str) -> str:
    """Escape a string for TOML double-quoted value.

    >>> escape_toml_string('hello "world"')
    '"hello \\"world\\""'
    """
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def parse_toml_response(text: str) -> dict[str, Any]:
    """Parse fsmon SocketResp TOML subset.

    Handles scalars (bool, int, string) and array-of-tables ([[paths]]).
    Does NOT handle nested tables, inline tables, or multi-line strings.

    Returns a dict with at minimum {"ok": False}. On parse errors,
    returns what was successfully parsed.
    """
    result: dict[str, Any] = {"ok": False}
    current_path: dict[str, Any] | None = None
    in_paths: bool = False

    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue

        # Array of tables: [[paths]]
        if line.startswith("[[") and line.endswith("]]"):
            table_name = line[2:-2]
            in_paths = True
            if table_name not in result:
                result[table_name] = []
            current_path = {}
            result[table_name].append(current_path)
            continue

        if "=" in line:
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()
            parsed = _parse_scalar(value)

            if in_paths and current_path is not None:
                current_path[key] = parsed
            else:
                result[key] = parsed
                if key != "paths":
                    in_paths = False

    return result


def _parse_scalar(value: str) -> Any:
    """Parse a TOML scalar: bool, int, string, or array of scalars."""
    value = value.strip()
    if value == "true":
        return True
    if value == "false":
        return False
    # Quoted string
    if (value.startswith('"') and value.endswith('"')) or \
       (value.startswith("'") and value.endswith("'")):
        return value[1:-1]
    # Array
    if value.startswith("[") and value.endswith("]"):
        inner = value[1:-1]
        if not inner.strip():
            return []
        return [_parse_scalar(item) for item in inner.split(",")]
    # Integer
    try:
        return int(value)
    except ValueError:
        pass
    # Fallback: raw string
    return value
