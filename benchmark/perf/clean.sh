#!/bin/bash
# clean 测试期间采集 perf 数据（仅 fsmon 进程）
# 用法: bash perf_clean.sh [event_count]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

COUNT="${1:-5000}"
PERF_OUTPUT="/tmp/perf_clean.data"

cleanup_perf() { rm -f "$PERF_OUTPUT"; }

generate_events() {
    local n=$1
    info "生成 $n 个事件..."
    for i in $(seq 1 "$n"); do echo "data_$i" > "$BENCH_DIR/c_$i.txt"; done
    sleep 5
}

bench() {
    local t_start t_end ms
    t_start=$(date +%s%N)
    "$@" > /dev/null 2>&1
    t_end=$(date +%s%N)
    ms=$(( (t_end - t_start) / 1000000 ))
    echo "$ms"
}

# ── 清理 ──
cleanup
cleanup_perf

echo "========================================="
echo "  perf + clean 测试 (count=$COUNT)"
echo "========================================="

# ── 准备测试文件（fsmon 未启动，不记录事件）──
mkdir -p "$BENCH_DIR"
info "创建 $COUNT 个测试文件..."
for i in $(seq 1 "$COUNT"); do echo "base" > "$BENCH_DIR/b_$i.dat"; done

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

# ── clean 测试 ──
info "=== clean 测试 ==="

generate_events "$COUNT"
ms=$(bench fsmon clean _global)
info "clean $COUNT: ${ms}ms"

generate_events "$COUNT"
ms=$(bench fsmon clean _global -t ">1h")
info "clean by time: ${ms}ms"

generate_events "$COUNT"
ms=$(bench fsmon clean _global --dry-run)
info "clean dry-run: ${ms}ms"

# ── 停止 perf ──
info "停止 perf..."
sudo kill -INT "$PERF_PID" 2>/dev/null || true
wait "$PERF_PID" 2>/dev/null || true
sleep 2

# ── 停止 fsmon ──
sudo killall fsmon 2>/dev/null || true

echo ""
echo "========================================="
echo "  采集完成"
echo "========================================="
echo "  数据文件: $PERF_OUTPUT"
echo ""
echo "  查看报告:"
echo "    sudo perf report -i $PERF_OUTPUT"
echo ""
