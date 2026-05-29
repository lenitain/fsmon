#!/usr/bin/env python3
"""Connect to fsmon socket and print events — 5 lines"""

import os, socket, json

sock = socket.socket(socket.AF_UNIX)
sock.connect(f"/tmp/fsmon-{os.getuid()}.sock")
for line in sock.makefile("r"):
    ev = json.loads(line)
    if ev["cmd"] == "nginx":
        print(ev["event_type"], ev["path"])
