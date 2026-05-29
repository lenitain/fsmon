#!/usr/bin/env python3
"""fsmon socket 实时流 — 最小 Python 示例（5 行核心代码）"""

import os, socket, json

sock = socket.socket(socket.AF_UNIX)
sock.connect(f"/tmp/fsmon-{os.getuid()}.sock")
for line in sock.makefile("r"):
    ev = json.loads(line)
    print(ev["event_type"], ev["cmd"], ev["path"])
