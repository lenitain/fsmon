#!/usr/bin/env python3
"""Subscribe to fsmon event stream via Unix socket.

Protocol: send JSON command → receive JSON OK → stream JSONL events.
Pipe output to jq for filtering.
"""

import os, socket, json, sys

SOCKET = f"/tmp/fsmon-{os.getuid()}.sock"

if not os.path.exists(SOCKET):
    sys.exit("daemon not running? missing " + SOCKET)

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(SOCKET)

# Send subscribe command (JSON)
sock.sendall(b'{"Subscribe":{}}\n')

# Use a single buffered reader for both JSON response and JSONL stream.
# (Avoid mixing sock.recv() and sock.makefile() — recv consumes bytes
#  that makefile can't see, causing event loss.)
f = sock.makefile("rb")
resp = f.readline()

resp_str = resp.decode().strip()
print(f"[subscribed] {resp_str}", file=sys.stderr)

if resp_str != '"Ok"':
    sys.exit(f"subscribe failed: {resp_str}")

# Stream JSONL events to stdout
for line in f:
    line = line.decode().strip()
    if not line:
        continue
    try:
        ev = json.loads(line)
        # Filter examples — uncomment or pipe to jq:
        # if ev.get("cmd") == "nginx":
        print(json.dumps(ev), flush=True)
    except json.JSONDecodeError:
        print(line)
