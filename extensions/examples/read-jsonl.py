#!/usr/bin/env python3
"""Read fsmon JSONL log files — query + tail"""

import glob, json, os, sys

LOGDIR = os.path.expanduser(
    os.environ.get("XDG_STATE_HOME", "~/.local/state") + "/fsmon"
)

if not os.path.isdir(LOGDIR):
    sys.exit(f"log directory not found: {LOGDIR}")

files = sorted(glob.glob(f"{LOGDIR}/*_log.jsonl"))
if not files:
    sys.exit(f"no log files in {LOGDIR}")

# Show last 5 events across all log files
print("=== last 5 events ===")
events = []
for f in files:
    with open(f) as fh:
        for line in fh:
            events.append(json.loads(line))
for ev in events[-5:]:
    print(f"  {ev['time']}  {ev['event_type']:12}  {ev['cmd']:10}  {ev['path']}")

# Show how to tail specific cmd
print()
print("=== how to tail (e.g., nginx only) ===")
print(f"  tail -f {LOGDIR}/nginx_log.jsonl | jq '.'")
print(f"  # or use fsmon query:")
print(f"  fsmon query nginx -t '>1h'")
