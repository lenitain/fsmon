#!/bin/bash
# query 测试期间采集 perf 数据（仅 fsmon 进程）
# 用法: bash perf_query.sh [event_count]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

COUNT="${1:-5000}"
PERF_OUTPUT="/tmp/perf_query.data"

cleanup_perf() { rm -f "$PERF_OUTPUT"; }

generate_events() {
    local n=$1
    info "生成 $n 个事件..."
    for i in $(seq 1 "$n"); do echo "data_$i" > "$BENCH_DIR/q_$i.txt"; done
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
echo "  perf + query 测试 (count=$COUNT)"
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

# ── 生成事件 ──
generate_events "$COUNT"

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

# ── query 测试 ──
info "=== query 测试 ==="

ms=$(bench fsmon query _global -p "$BENCH_DIR" -t ">5m")
info "全量查询 ${COUNT} 条: ${ms}ms"

ms=$(bench bash -c "fsmon query _global -p \"$BENCH_DIR\" -t \">5m\" | jq -s 'length'")
info "jq -s length: ${ms}ms"

ms=$(bench bash -c "fsmon query _global -p \"$BENCH_DIR\" -t \">5m\" | jq -s '[.[] | select(.event_type == \"CREATE\")] | length'")
info "jq select+length: ${ms}ms"

ms=$(bench bash -c "fsmon query _global -p \"$BENCH_DIR\" -t \">5m\" | jq -s 'group_by(.event_type) | map({type: .[0].event_type, count: length})'")
info "jq group_by: ${ms}ms"

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
