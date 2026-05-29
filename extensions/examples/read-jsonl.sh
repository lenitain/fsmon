#!/usr/bin/env bash
# fsmon JSONL 文件读取示例
# 查看文件事件的两种方式：tail 实时流 + jq 过滤
set -euo pipefail

LOGDIR="${XDG_STATE_HOME:-$HOME/.local/state}/fsmon"

echo "=== 查看最近 5 条事件 ==="
jq -s '.[-5:]' "$LOGDIR"/*_log.jsonl

echo
echo "=== 实时 tail（类似 tail -f） ==="
echo "按 Ctrl+C 退出"
tail -n0 -f "$LOGDIR"/*_log.jsonl | jq --unbuffered 'select(.cmd == "nginx")'
