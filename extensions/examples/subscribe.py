#!/usr/bin/env python3
"""Subscribe to fsmon event stream via Unix socket.

Protocol: send TOML command → receive TOML OK → stream JSONL events.
Pipe output to jq for filtering.
"""

import os, socket, json, sys

SOCKET = f"/tmp/fsmon-{os.getuid()}.sock"

if not os.path.exists(SOCKET):
    sys.exit("daemon not running? missing " + SOCKET)

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(SOCKET)

# Send subscribe command (TOML, blank line terminated)
sock.sendall(b'cmd = "subscribe"\n\n')

# Read TOML response (single line)
resp = b""
while True:
    c = sock.recv(1)
    if c == b"\n":
        break
    resp += c

resp_str = resp.decode().strip()
print(f"[subscribed] {resp_str}", file=sys.stderr)

if not resp_str.startswith("ok = true"):
    sys.exit(f"subscribe failed: {resp_str}")

# Stream JSONL events to stdout
for line in sock.makefile("r"):
    try:
        ev = json.loads(line)
        # Filter examples — uncomment or pipe to jq:
        # if ev.get("cmd") == "nginx":
        print(json.dumps(ev))
    except json.JSONDecodeError:
        print(line.rstrip())
