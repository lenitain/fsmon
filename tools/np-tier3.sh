#!/bin/bash
# Measure how often fsmon actually needs open_by_handle_at (tier-3 fallback).
export HOME=/tmp/np-home XDG_CONFIG_HOME=/tmp/np-home/.config XDG_RUNTIME_DIR=/tmp/np-run
FSMON="${FSMON:-/tmp/fsmon-np/target/release/fsmon}"   # patched build, see README
W=/tmp/np-watch3
rm -rf "$W" /tmp/np-home/.local/state/fsmon; mkdir -p "$W"

"$FSMON" daemon >/tmp/np-daemon3.log 2>&1 &
D=$!; sleep 2
"$FSMON" add _global --path "$W" -r >/dev/null 2>&1; sleep 1

# A demanding workload: many new nested dirs, files at depth, renames, deletes.
for i in $(seq 1 20); do
  mkdir -p "$W/d$i/sub/deeper"
  echo hi > "$W/d$i/sub/deeper/f$i.txt"
  echo hi > "$W/d$i/root$i.txt"
  mv "$W/d$i/root$i.txt" "$W/d$i/moved$i.txt"
  rm -f "$W/d$i/sub/deeper/f$i.txt"
done
# Burst of short-lived single-file events
for i in $(seq 1 40); do touch "$W/burst$i"; done
# A directory tree with a file created immediately after each mkdir (race window)
for i in $(seq 1 10); do mkdir -p "$W/race$i/x/y"; echo z > "$W/race$i/x/y/z.txt"; done
sleep 3

echo "### tier-3 fallback attempts:"
grep -c "TIER3-PROBE" /tmp/np-daemon3.log 2>/dev/null || echo 0
grep "TIER3-PROBE" /tmp/np-daemon3.log 2>/dev/null | tail -3
echo
echo "### events resolved vs total:"
"$FSMON" query _global 2>/dev/null | python3 -c "
import sys,json
tot=0;ok=0
for l in sys.stdin:
    l=l.strip()
    if not l.startswith('{'): continue
    try: d=json.loads(l)
    except: continue
    tot+=1
    if d['path'] and not d['path'].startswith('<') and d['path'].count('/')>=1: ok+=1
print(f'  total events     : {tot}')
print(f'  with real path   : {ok}')
print(f'  pid attributed   : {sum(1 for l in open(\"/tmp/np-home/.local/state/fsmon/_global_log.jsonl\") if \"\\\"pid\\\":0\" not in l)}')
"
kill -INT $D 2>/dev/null; sleep 1; kill -9 $D 2>/dev/null
