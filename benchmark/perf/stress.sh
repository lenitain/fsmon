#!/bin/bash
# 压力测试期间采集 perf 数据（仅 fsmon 进程）
# 用法: bash perf_stress.sh [stress_count]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

COUNT="${1:-5000}"
PERF_OUTPUT="/tmp/perf_stress.data"

cleanup_perf() { rm -f "$PERF_OUTPUT"; }

# ── 清理 ──
cleanup
cleanup_perf

echo "========================================="
echo "  perf + stress 测试"
echo "========================================="

# ── 准备测试文件（fsmon 未启动，不记录事件）──
mkdir -p "$BENCH_DIR"
info "创建 $COUNT 个测试文件..."
for i in $(seq 1 "$COUNT"); do echo "stress" > "$BENCH_DIR/file_$i.dat"; done

# ── 启动 fsmon ──
sudo killall fsmon 2>/dev/null || true
sleep 1
sudo rm -f "$LOG_FILE"
sudo fsmon daemon &>/dev/null &
sleep 3
if ! fsmon monitored &>/dev/null; then
    echo "[ERROR] fsmon daemon 启动失败"
    exit 1
fi
info "fsmon daemon 已启动"

# ── 注册监控 ──
fsmon add _global --path "$BENCH_DIR" -r -t all
sleep 2

# ── 获取 fsmon PID，启动 perf ──
FSMON_PID=$(pgrep -x fsmon)
info "fsmon PID=$FSMON_PID, 启动 perf record..."
sudo perf record -g -p "$FSMON_PID" -o "$PERF_OUTPUT" &
PERF_PID=$!
sleep 1
if ! kill -0 "$PERF_PID" 2>/dev/null; then
    echo "[ERROR] perf record 启动失败"
    exit 1
fi

# ── 压力测试：顺序修改 ──
info "=== 压力: 顺序修改 $COUNT 个文件 ==="
t_start=$(date +%s%N)
for i in $(seq 1 "$COUNT"); do
    echo "mod" >> "$BENCH_DIR/file_$i.dat"
done
t_end=$(date +%s%N)
ms=$(( (t_end - t_start) / 1000000 ))
info "修改耗时: ${ms}ms"

# ── 等待事件落盘 ──
sleep 5

# ── 停止 perf ──
info "停止 perf..."
sudo kill -INT "$PERF_PID" 2>/dev/null || true
wait "$PERF_PID" 2>/dev/null || true
sleep 2

# ── 停止 fsmon ──
sudo killall fsmon 2>/dev/null || true
sleep 2

# ── 验证结果 ──
count=$(count_type "MODIFY")
info "MODIFY 捕获: $count / $COUNT"
[ "$count" -eq "$COUNT" ] && ok "压力 MODIFY" || fail "压力 MODIFY (期望 $COUNT, 实际 $count)"

cleanup

echo ""
echo "========================================="
echo "  采集完成"
echo "========================================="
echo "  数据文件: $PERF_OUTPUT"
echo ""
echo "  查看报告:"
echo "    sudo perf report -i $PERF_OUTPUT"
echo ""
