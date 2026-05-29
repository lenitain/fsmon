#!/usr/bin/env bash
# Connect to fsmon socket and filter events
set -euo pipefail

SOCKET="/tmp/fsmon-$(id -u).sock"
[ -S "$SOCKET" ] || { echo "daemon not running? missing $SOCKET" >&2; exit 1; }

echo "=== all events (first 5) ==="
nc -U "$SOCKET" | head -5

echo "=== nginx CREATE events only ==="
nc -U "$SOCKET" | jq --unbuffered 'select(.cmd == "nginx" and .event_type == "CREATE")'
