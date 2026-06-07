#!/bin/bash
# 清理性能测试：不同数据量下的 clean 耗时

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

cleanup() {
    rm -rf "$BENCH_DIR"
    fsmon remove _global 2>/dev/null || true
    sudo rm -f "$LOG_FILE" 2>/dev/null || true
}

setup() {
    restart_daemon
    mkdir -p "$BENCH_DIR"
    fsmon add _global --path "$BENCH_DIR" -r -t all
    sleep 0.5
}

# 生成 N 个事件
generate_events() {
    local n=$1
    info "生成 $n 个事件..."
    for i in $(seq 1 $n); do
        echo "data_$i" > "$BENCH_DIR/c_$i.txt"
    done
    sleep 5
}

# 测量 clean 耗时
bench_clean() {
    local label="$1"
    shift
    local t_start t_end ms
    t_start=$(date +%s%N)
    "$@" > /dev/null 2>&1
    t_end=$(date +%s%N)
    ms=$(( (t_end - t_start) / 1000000 ))
    echo "$ms"
}

# 获取日志文件大小
log_size() {
    if [ -d "$LOG_DIR" ]; then
        du -sh "$LOG_DIR" 2>/dev/null | cut -f1
    else
        echo "0"
    fi
}

# ─────────────────────────────────────────────
# 测试 1: 小数据量清理 (100 条)
# ─────────────────────────────────────────────
test_clean_small() {
    info "=== 清理测试: 100 条事件 ==="
    generate_events 100

    local before_size
    before_size=$(log_size)
    info "清理前日志大小: $before_size"

    local ms
    ms=$(bench_clean "clean_all" fsmon clean _global)
    info "清理耗时: ${ms}ms"
    [ "$ms" -lt 5000 ] && ok "小数据量清理 <5s" || fail "小数据量清理超时"

    local after_size
    after_size=$(log_size)
    info "清理后日志大小: $after_size"
}

# ─────────────────────────────────────────────
# 测试 2: 中数据量清理 (1000 条)
# ─────────────────────────────────────────────
test_clean_medium() {
    info "=== 清理测试: 1000 条事件 ==="
    generate_events 1000

    local before_size
    before_size=$(log_size)
    info "清理前日志大小: $before_size"

    local ms
    ms=$(bench_clean "clean_all" fsmon clean _global)
    info "清理耗时: ${ms}ms"
    [ "$ms" -lt 10000 ] && ok "中数据量清理 <10s" || fail "中数据量清理超时"
}

# ─────────────────────────────────────────────
# 测试 3: 大数据量清理 (5000 条)
# ─────────────────────────────────────────────
test_clean_large() {
    info "=== 清理测试: 5000 条事件 ==="
    generate_events 5000

    local before_size
    before_size=$(log_size)
    info "清理前日志大小: $before_size"

    local ms
    ms=$(bench_clean "clean_all" fsmon clean _global)
    info "清理耗时: ${ms}ms"
    [ "$ms" -lt 30000 ] && ok "大数据量清理 <30s" || fail "大数据量清理超时"
}

# ─────────────────────────────────────────────
# 测试 4: 按时间清理
# ─────────────────────────────────────────────
test_clean_by_time() {
    info "=== 按时间清理测试 ==="
    generate_events 1000

    local ms
    ms=$(bench_clean "clean_time" fsmon clean _global -t ">1h")
    info "保留 >1h 清理: ${ms}ms"
    [ "$ms" -lt 10000 ] && ok "按时间清理 <10s" || fail "按时间清理超时"
}

# ─────────────────────────────────────────────
# 测试 5: dry-run 性能
# ─────────────────────────────────────────────
test_clean_dry_run() {
    info "=== dry-run 性能测试 ==="
    generate_events 1000

    local ms
    ms=$(bench_clean "dry_run" fsmon clean _global --dry-run)
    info "dry-run 耗时: ${ms}ms"
    [ "$ms" -lt 10000 ] && ok "dry-run <10s" || fail "dry-run 超时"
}

# ─────────────────────────────────────────────
# 主流程
# ─────────────────────────────────────────────
setup

test_clean_small
test_clean_medium
test_clean_large
test_clean_by_time
test_clean_dry_run

echo ""
cleanup

echo ""
echo "========================================="
echo -e "  clean 测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
echo "========================================="

[ "$failed" -eq 0 ]
