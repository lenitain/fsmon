#!/usr/bin/env bash
# fsmon socket 实时流示例
# 连接 daemon socket 直接收事件，无需握手
set -euo pipefail

SOCKET="/tmp/fsmon-$(id -u).sock"

if [ ! -S "$SOCKET" ]; then
    echo "daemon 未运行？没找到 socket: $SOCKET" >&2
    exit 1
fi

echo "=== 连接 $SOCKET ==="
echo "按 Ctrl+C 退出"

# 方式 1: 所有事件
nc -U "$SOCKET" | head -5

echo
echo "=== 只保留 nginx 的 CREATE 事件 ==="
nc -U "$SOCKET" | jq --unbuffered 'select(.cmd == "nginx" and .event_type == "CREATE")'
