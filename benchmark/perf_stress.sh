#!/bin/bash
# 压力测试期间采集 perf 数据
# 用法: bash perf_stress.sh [stress_count]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COUNT="${1:-5000}"
PERF_OUTPUT="/tmp/perf_stress.data"

echo "========================================="
echo "  perf + stress 测试"
echo "========================================="

# 清理旧数据
rm -f "$PERF_OUTPUT"

# 确保 kptr_restrict 为 0（允许 perf 看内核符号）
echo 0 | sudo tee /proc/sys/kernel/kptr_restrict > /dev/null

# 启动 perf 记录（后台）
echo "[INFO] 启动 perf record..."
sudo perf record -g -a -p $(pgrep -x fsmon) -o "$PERF_OUTPUT" &
PERF_PID=$!

# 等待 perf 就绪
sleep 2

# 运行压力测试
echo "[INFO] 运行 stress.sh (count=$COUNT)..."
bash "$SCRIPT_DIR/tests/events/stress.sh" "$COUNT" || true

# 停止 perf
echo "[INFO] 压力测试完成，停止 perf..."
sudo kill -INT "$PERF_PID" 2>/dev/null || true
wait "$PERF_PID" 2>/dev/null || true

# 等待 perf 写入完成
sleep 2

echo ""
echo "========================================="
echo "  采集完成"
echo "========================================="
echo "  数据文件: $PERF_OUTPUT"
echo ""
echo "  查看报告:"
echo "    sudo perf report -i $PERF_OUTPUT"
echo ""
