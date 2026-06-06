#!/bin/bash
# 快速 perf 采集（手动用）
set -euo pipefail

echo 0 | sudo tee /proc/sys/kernel/kptr_restrict > /dev/null

FSMON_PID=$(pgrep -x fsmon)
if [ -z "$FSMON_PID" ]; then
    echo "[ERROR] fsmon 未运行"
    exit 1
fi

sudo perf record -g -p "$FSMON_PID"
