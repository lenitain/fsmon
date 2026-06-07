#!/bin/bash
# 查询性能测试：不同数据量下的 query 耗时

set -euo pipefail

BENCH_DIR="/tmp/fsmon_benchmark"
passed=0
failed=0

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[PASS]${NC} $*"; passed=$((passed + 1)); }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; failed=$((failed + 1)); }

cleanup() {
    rm -rf "$BENCH_DIR"
    fsmon remove _global 2>/dev/null || true
    : > ~/.local/state/fsmon/_global_log.jsonl 2>/dev/null
}

setup() {
    cleanup
    mkdir -p "$BENCH_DIR"
    fsmon add _global --path "$BENCH_DIR" -r -t all
    sleep 0.5
}

# 生成 N 个事件
generate_events() {
    local n=$1
    info "生成 $n 个事件..."
    for i in $(seq 1 $n); do
        echo "data_$i" > "$BENCH_DIR/q_$i.txt"
    done
    sleep 5
}

# 测量 query 耗时
bench_query() {
    local label="$1"
    shift
    local t_start t_end ms
    t_start=$(date +%s%N)
    "$@" > /dev/null 2>&1
    t_end=$(date +%s%N)
    ms=$(( (t_end - t_start) / 1000000 ))
    echo "$ms"
}

# ─────────────────────────────────────────────
# 测试 1: 小数据量查询 (100 条)
# ─────────────────────────────────────────────
test_query_small() {
    info "=== 查询测试: 100 条事件 ==="
    generate_events 100

    local ms
    ms=$(bench_query "query_all" fsmon query _global -p "$BENCH_DIR" -t ">5m")
    info "全量查询: ${ms}ms"
    [ "$ms" -lt 5000 ] && ok "小数据量查询 <5s" || fail "小数据量查询超时"

    ms=$(bench_query "query_filter" fsmon query _global -p "$BENCH_DIR" -t ">5m")
    info "过滤查询: ${ms}ms"
    [ "$ms" -lt 5000 ] && ok "小数据量过滤 <5s" || fail "小数据量过滤超时"
}

# ─────────────────────────────────────────────
# 测试 2: 中数据量查询 (1000 条)
# ─────────────────────────────────────────────
test_query_medium() {
    info "=== 查询测试: 1000 条事件 ==="
    generate_events 1000

    local ms
    ms=$(bench_query "query_all" fsmon query _global -p "$BENCH_DIR" -t ">5m")
    info "全量查询: ${ms}ms"
    [ "$ms" -lt 10000 ] && ok "中数据量查询 <10s" || fail "中数据量查询超时"
}

# ─────────────────────────────────────────────
# 测试 3: 大数据量查询 (5000 条)
# ─────────────────────────────────────────────
test_query_large() {
    info "=== 查询测试: 5000 条事件 ==="
    generate_events 5000

    local ms
    ms=$(bench_query "query_all" fsmon query _global -p "$BENCH_DIR" -t ">5m")
    info "全量查询: ${ms}ms"
    [ "$ms" -lt 30000 ] && ok "大数据量查询 <30s" || fail "大数据量查询超时"
}

# ─────────────────────────────────────────────
# 测试 4: jq 管道性能
# ─────────────────────────────────────────────
test_jq_pipeline() {
    info "=== jq 管道性能测试 ==="
    generate_events 1000

    local ms t_start t_end
    t_start=$(date +%s%N)
    fsmon query _global -p "$BENCH_DIR" -t ">5m" 2>/dev/null | jq -s 'length' > /dev/null
    t_end=$(date +%s%N)
    ms=$(( (t_end - t_start) / 1000000 ))
    info "jq -s length: ${ms}ms"
    [ "$ms" -lt 15000 ] && ok "jq 管道 <15s" || fail "jq 管道超时"

    t_start=$(date +%s%N)
    fsmon query _global -p "$BENCH_DIR" -t ">5m" 2>/dev/null | jq -s '[.[] | select(.event_type == "CREATE")] | length' > /dev/null
    t_end=$(date +%s%N)
    ms=$(( (t_end - t_start) / 1000000 ))
    info "jq select+length: ${ms}ms"
    [ "$ms" -lt 15000 ] && ok "jq select <15s" || fail "jq select 超时"
}

# ─────────────────────────────────────────────
# 主流程
# ─────────────────────────────────────────────
if ! pgrep -x fsmon > /dev/null; then
    echo "[ERROR] fsmon daemon 未运行"
    exit 1
fi

setup

test_query_small
test_query_medium
test_query_large
test_jq_pipeline

echo ""
echo "========================================="
echo -e "  query 测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
echo "========================================="

[ "$failed" -eq 0 ]
