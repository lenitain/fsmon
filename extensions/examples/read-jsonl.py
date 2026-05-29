#!/usr/bin/env python3
"""Read fsmon JSONL files — tail + filter"""

import glob, json, subprocess, sys

files = sorted(glob.glob(f"{sys.argv[1] if len(sys.argv) > 1 else '~/.local/state/fsmon'}/*_log.jsonl"))

print("=== last 5 events ===")
events = []
for f in files:
    with open(f) as fh:
        for line in fh:
            events.append(json.loads(line))
for ev in events[-5:]:
    print(ev["event_type"], ev["cmd"], ev["path"])

print("=== real-time (nginx only) ===")
proc = subprocess.Popen(["tail", "-n0", "-f"] + files, stdout=subprocess.PIPE, text=True)
try:
    for line in proc.stdout:
        ev = json.loads(line)
        if ev["cmd"] == "nginx":
            print("nginx:", ev["event_type"], ev["path"], flush=True)
except KeyboardInterrupt:
    proc.terminate()
