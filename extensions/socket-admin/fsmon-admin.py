#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# ///
"""

fsmon Admin Client — programmatic daemon management via Unix socket.

fsmon daemon listens on a Unix socket for management commands:
  - add:     dynamically add a new monitored path
  - remove:  stop monitoring a path
  - list:    list all currently monitored paths
  - health:  get daemon health status (uptime, readers, restarts)

These are the same commands available via the `fsmon` CLI. This script
shows how to call them programmatically — useful for:
  - Auto-monitoring new directories created by your app
  - Health-check integration with your monitoring system (Nagios, etc.)
  - Dynamic configuration from a control plane or orchestration tool

No external dependencies (stdlib only).

── Quick Start ─────────────────────────────────────────────────────
  # Prerequisites: daemon must be running
  sudo fsmon daemon

  # List monitored paths
  python3 extensions/socket-admin/fsmon-admin.py list

  # Add a new path to monitor (all events, recursive)
  python3 extensions/socket-admin/fsmon-admin.py add /var/www --recursive

  # Add path with filter: only nginx CLOSE_WRITE events
  python3 extensions/socket-admin/fsmon-admin.py add /var/log/nginx --track-cmd nginx --types CLOSE_WRITE

  # Remove a path
  python3 extensions/socket-admin/fsmon-admin.py remove /var/log/nginx

  # Health check
  python3 extensions/socket-admin/fsmon-admin.py health

  # Health check with JSON output (for monitoring systems)
  python3 extensions/socket-admin/fsmon-admin.py health --json

── Protocol ────────────────────────────────────────────────────────
  The socket protocol is TOML-over-Unix-stream:
    1. Connect to /tmp/fsmon-<UID>.sock
    2. Send TOML document followed by blank line
    3. Read TOML response

  Example wire format:
    Client sends:
      cmd = "add"
      path = "/var/www"
      recursive = true

    Server responds:
      ok = true

    Client sends:
      cmd = "list"

    Server responds:
      ok = true
      [[paths]]
      path = "/var/www"
      recursive = true

── Bridge To ────────────────────────────────────────────────────────
  - Configuration management (Ansible, Puppet, Terraform)
  - Container orchestration: add volume paths when containers start
  - CI/CD: monitor build artifacts directories
  - Health monitoring: Nagios / Icinga / Datadog agent check
  - Custom control plane / admin dashboard
"""

import argparse
import json
import os
import socket
import sys


class FsmonError(Exception):
    """Connection or protocol error with fsmon daemon."""


# ── Socket helpers ──────────────────────────────────────────────────

def get_socket_path() -> str:
    """Get the fsmon daemon socket path for the current user.
    
    The daemon socket is named /tmp/fsmon-<UID>.sock where UID is
    the original user's UID (set via SUDO_UID if running under sudo).
    """
    # When fsmon runs under sudo, it uses SUDO_UID to name the socket.
    # We try the same logic for client connections.
    sudo_uid = os.environ.get("SUDO_UID")
    uid = sudo_uid if sudo_uid else str(os.getuid())
    return f"/tmp/fsmon-{uid}.sock"


def send_cmd(cmd_dict: dict, socket_path: str | None = None) -> dict:
    """Send a TOML command to the daemon and return the parsed response.

    Returns a dict with at least {"ok": bool}.

    Raises:
        FsmonError: connection failed, timeout, or empty response.
    """
    if socket_path is None:
        socket_path = get_socket_path()

    # Build TOML manually for zero-dependency operation
    toml_lines = []
    for key, value in cmd_dict.items():
        if value is None:
            continue
        if isinstance(value, bool):
            toml_lines.append(f"{key} = {'true' if value else 'false'}")
        elif isinstance(value, list):
            items = ", ".join(f'"{v}"' for v in value)
            toml_lines.append(f"{key} = [{items}]")
        elif isinstance(value, str):
            toml_lines.append(f'{key} = "{value}"')
        else:
            toml_lines.append(f"{key} = {value}")
    payload = "\n".join(toml_lines) + "\n\n"

    # Connect and send
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(socket_path)
        s.sendall(payload.encode())
    except FileNotFoundError:
        raise FsmonError(
            f"socket not found: {socket_path}\n"
            f"Is the daemon running? Start with: sudo fsmon daemon"
        )
    except ConnectionRefusedError:
        raise FsmonError(
            f"connection refused: {socket_path}\n"
            f"The daemon may have stopped. Restart with: sudo fsmon daemon"
        )
    except socket.timeout:
        raise FsmonError(f"connection timed out: {socket_path}")

    # Read response
    reader = s.makefile("r")
    response = reader.read()
    s.close()

    if not response.strip():
        raise FsmonError("empty response from daemon")

    # Parse TOML response manually for zero-dependency operation
    result = _parse_toml_response(response)
    return result


def _parse_toml_response(text: str) -> dict:
    """Minimal TOML parser for SocketResp format.
    
    Handles the subset of TOML that fsmon's SocketResp uses:
    scalars (bool, string, int) and array-of-tables ([[paths]]).
    """
    result = {"ok": False}
    current_path = None
    in_paths = False

    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue

        # Array of tables header: [[paths]]
        if line.startswith("[[paths]]"):
            in_paths = True
            if "paths" not in result:
                result["paths"] = []
            current_path = {}
            result["paths"].append(current_path)
            continue

        # Key = value
        if "=" in line:
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()

            # Parse value
            parsed = _parse_toml_value(value)

            if in_paths and current_path is not None:
                current_path[key] = parsed
            else:
                result[key] = parsed
                if key != "paths":
                    in_paths = False

    return result


def _parse_toml_value(value: str):
    """Parse a TOML scalar value."""
    value = value.strip()
    if value == "true":
        return True
    if value == "false":
        return False
    if value.startswith('"') and value.endswith('"'):
        return value[1:-1]
    if value.startswith("'") and value.endswith("'"):
        return value[1:-1]
    # Array
    if value.startswith("[") and value.endswith("]"):
        inner = value[1:-1]
        if not inner.strip():
            return []
        items = []
        for item in inner.split(","):
            items.append(_parse_toml_value(item))
        return items
    # Integer
    try:
        return int(value)
    except ValueError:
        pass
    return value


# ── Command implementations ─────────────────────────────────────────

def cmd_list(args):
    """List all monitored paths."""
    resp = send_cmd({"cmd": "list"})
    if not resp.get("ok"):
        print(f"Error: {resp.get('error', 'unknown')}", file=sys.stderr)
        sys.exit(1)

    paths = resp.get("paths", [])
    if not paths:
        print("No paths currently monitored.")
        return

    print(f"Monitored paths ({len(paths)}):\n")
    for entry in paths:
        path = entry.get("path", "?")
        cmd = entry.get("cmd") or "global"
        recursive = entry.get("recursive")
        types = entry.get("types")
        size = entry.get("size")

        flags = []
        if recursive:
            flags.append("recursive")
        if types:
            flags.append(f"types={','.join(types)}")
        if size:
            flags.append(f"size={size}")

        flag_str = ", ".join(flags) if flags else "all events"
        print(f"  [{cmd}] {path}")
        print(f"         {flag_str}")
    print()


def cmd_add(args):
    """Add a new monitored path."""
    if not os.path.exists(args.path):
        print(f"Warning: path '{args.path}' does not exist. "
              f"fsmon will start monitoring when it's created.")

    cmd = {
        "cmd": "add",
        "path": args.path,
    }
    if args.recursive:
        cmd["recursive"] = True
    if args.track_cmd:
        cmd["track_cmd"] = args.track_cmd
    if args.types:
        cmd["types"] = args.types.split(",")

    resp = send_cmd(cmd)
    if resp.get("ok"):
        print(f"✓ Added: {args.path}")
        if args.track_cmd:
            print(f"  cmd group: {args.track_cmd}")
        if args.recursive:
            print(f"  mode: recursive")
        if args.types:
            print(f"  types: {args.types}")
    else:
        error = resp.get("error", "unknown")
        error_kind = resp.get("error_kind", "")
        kind_str = f" [{error_kind}]" if error_kind else ""
        print(f"✗ Failed to add '{args.path}': {error}{kind_str}", file=sys.stderr)
        sys.exit(1)


def cmd_remove(args):
    """Remove a monitored path."""
    cmd = {"cmd": "remove", "path": args.path}
    if args.track_cmd:
        cmd["track_cmd"] = args.track_cmd

    resp = send_cmd(cmd)
    if resp.get("ok"):
        print(f"✓ Removed: {args.path}")
    else:
        error = resp.get("error", "unknown")
        print(f"✗ Failed to remove '{args.path}': {error}", file=sys.stderr)
        sys.exit(1)


def cmd_health(args):
    """Get daemon health status."""
    resp = send_cmd({"cmd": "health"})
    health = resp.get("health")
    if not health:
        print(f"Error: no health data in response", file=sys.stderr)
        sys.exit(1)

    if args.json:
        # Output JSON for machine consumption (monitoring systems)
        print(json.dumps(health, indent=2))
        return

    # Human-readable output
    uptime_m = health.get("uptime_secs", 0) // 60
    uptime_s = health.get("uptime_secs", 0) % 60
    print(f"Daemon Health:")
    print(f"  Uptime:           {uptime_m}m {uptime_s}s")
    print(f"  Channel type:     {health.get('channel_type', '?')}")
    print(f"  Monitored paths:  {health.get('monitored_paths', 0)}")
    print(f"  Reader groups:    {health.get('reader_groups', 0)}")
    print()

    readers = health.get("readers", [])
    if readers:
        print(f"  Readers ({len(readers)}):")
        for i, r in enumerate(readers):
            status = "✓ alive" if r.get("alive") else "✗ dead"
            restarts = r.get("restarts", 0)
            fd = r.get("fd", "?")
            restart_str = f", restarts={restarts}" if restarts > 0 else ""
            print(f"    [{i}] fd={fd}  {status}{restart_str}")

    # Exit non-zero if any reader is dead (for health check integration)
    if any(not r.get("alive", True) for r in readers):
        print("\n⚠ Some readers are dead — daemon may need restart", file=sys.stderr)
        sys.exit(2)


# ── Main ────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="fsmon admin client — manage daemon programmatically",
        epilog="""
Examples:
  %(prog)s list                                     List all monitored paths
  %(prog)s add /var/www --recursive                  Add recursive monitor
  %(prog)s add /var/log/nginx --track-cmd nginx      Add with cmd filter
  %(prog)s add /tmp --types CREATE,DELETE            Add with type filter
  %(prog)s remove /var/log/nginx                     Remove a path
  %(prog)s health                                    Human-readable health
  %(prog)s health --json                             Machine-readable health
        """,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--socket", default=None,
                        help=f"Socket path (default: /tmp/fsmon-<UID>.sock)")

    sub = parser.add_subparsers(dest="command", required=True)

    # list
    p_list = sub.add_parser("list", help="List monitored paths")

    # add
    p_add = sub.add_parser("add", help="Add a monitored path")
    p_add.add_argument("path", help="Path to monitor")
    p_add.add_argument("--recursive", action="store_true",
                        help="Monitor subdirectories recursively")
    p_add.add_argument("--track-cmd", default=None,
                        help="Only track events from this cmd group (e.g. nginx)")
    p_add.add_argument("--types", default=None,
                        help="Comma-separated event types (e.g. CREATE,DELETE)")

    # remove
    p_remove = sub.add_parser("remove", help="Remove a monitored path")
    p_remove.add_argument("path", help="Path to stop monitoring")
    p_remove.add_argument("--track-cmd", default=None,
                           help="Only remove this cmd group's entry (leave others)")

    # health
    p_health = sub.add_parser("health", help="Daemon health status")
    p_health.add_argument("--json", action="store_true",
                           help="Output as JSON (for monitoring systems)")

    args = parser.parse_args()

    commands = {
        "list": cmd_list,
        "add": cmd_add,
        "remove": cmd_remove,
        "health": cmd_health,
    }
    try:
        commands[args.command](args)
    except FsmonError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
