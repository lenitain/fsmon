#!/usr/bin/env bash
# Subscribe to fsmon daemon event stream.
#
# Protocol: send TOML → receive TOML OK → stream JSONL events.
# Pipe to jq for filtering.
set -euo pipefail

SOCKET="/tmp/fsmon-$(id -u).sock"
[ -S "$SOCKET" ] || { echo "daemon not running? missing $SOCKET" >&2; exit 1; }

# Method 1: if socat is available (recommended)
if command -v socat &>/dev/null; then
    echo 'cmd = "subscribe"' | socat - "UNIX-CONNECT:$SOCKET" | {
        read -r ok_line
        echo "[subscribed] $ok_line" >&2
        jq --unbuffered '.'
    }
    exit 0
fi

# Method 2: fallback — embedded python helper (same as subscribe.py)
python3 -c '
import os, socket, sys
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[1])
sock.sendall(b"cmd = \"subscribe\"\n\n")
resp = b""
while True:
    c = sock.recv(1)
    if c == b"\n": break
    resp += c
import json
print(f"[subscribed] {resp.decode().strip()}", file=sys.stderr)
for line in sock.makefile("r"):
    try:
        print(json.dumps(json.loads(line)))
    except json.JSONDecodeError:
        print(line.rstrip())
' "$SOCKET" | jq --unbuffered '.'
