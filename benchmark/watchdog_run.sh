#!/usr/bin/env bash
# Test systemd watchdog hang detection.
#
# Prerequisites:
#   - fsmon.service installed with WatchdogSec configured
#   - gdb installed (for hang simulation)
#
# Usage:
#   sudo bash tests/test_watchdog.sh

set -euo pipefail

echo "=== fsmon watchdog hang test ==="

# Step 1: Verify daemon is running under systemd
if ! systemctl is-active --quiet fsmon; then
  echo "ERROR: fsmon.service is not running. Start it first:"
  echo "  sudo systemctl start fsmon"
  exit 1
fi

DAEMON_PID=$(pgrep -f "fsmon daemon" | head -1)
if [[ -z "$DAEMON_PID" ]]; then
  echo "ERROR: cannot find fsmon daemon PID"
  exit 1
fi
echo "[1/4] Daemon running, PID=$DAEMON_PID"

# Step 2: Check watchdog config
WATCHDOG_SEC=$(systemctl show fsmon -p WatchdogUSec --value | sed 's/[^0-9]//g')
if [[ -z "$WATCHDOG_SEC" || "$WATCHDOG_SEC" == "0" ]]; then
  echo "ERROR: WatchdogSec not configured in fsmon.service"
  echo "  Add WatchdogSec=10 to [Service] section, then:"
  echo "  sudo systemctl daemon-reload && sudo systemctl restart fsmon"
  exit 1
fi
echo "[2/4] WatchdogSec=${WATCHDOG_SEC}s"

# Step 3: Simulate hang via gdb (blocks main thread without freezing the whole process)
echo "[3/4] Injecting sleep(3600) via gdb to simulate event loop hang..."
echo "       (this blocks the main thread, preventing heartbeat)"

# -batch: run command and exit
# -ex: execute command
# -p: attach to process
gdb -batch -ex "call (int)sleep(3600)" -ex "detach" -p "$DAEMON_PID" 2>/dev/null || {
  echo "ERROR: gdb failed. Is gdb installed?"
  echo "  sudo apt install gdb"
  exit 1
}

# Step 4: Wait for watchdog timeout and observe
echo "[4/4] Waiting for systemd watchdog timeout (~${WATCHDOG_SEC}s)..."
echo "      Watch: journalctl -u fsmon -f --no-pager"
echo ""

# Monitor journal for watchdog-related messages
timeout=$((WATCHDOG_SEC + 10))
journalctl -u fsmon -f --no-pager --since "now" &
JOURNAL_PID=$!

sleep "$timeout"
kill $JOURNAL_PID 2>/dev/null || true

echo ""
echo "=== Test complete ==="
echo "Expected to see in journal:"
echo "  - 'Watchdog timeout' or 'killed by watchdog'"
echo "  - Automatic restart of fsmon.service"
echo ""
echo "Verify with: systemctl status fsmon"
