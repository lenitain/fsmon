#!/usr/bin/env bash
# fsmon JSONL log file query examples
# Uses standard tools: tail, jq
set -euo pipefail

LOGDIR="${XDG_STATE_HOME:-$HOME/.local/state}/fsmon"

if [ ! -d "$LOGDIR" ]; then
    echo "log directory not found: $LOGDIR" >&2
    echo "Tip: start the daemon first: sudo fsmon daemon &" >&2
    exit 1
fi

echo "=== recent 5 events (last line of each log) ==="
for f in "$LOGDIR"/*_log.jsonl; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    echo "  [$name]"
    tail -1 "$f" | jq -r '"    \(.time)  \(.event_type)  \(.path)"'
done

echo
echo "=== real-time tail (nginx only) ==="
echo "  tail -f \"$LOGDIR\"/nginx_log.jsonl | jq --unbuffered '.'  # Ctrl+C to stop"
echo "  # or use: fsmon query nginx -t '>1h'"
