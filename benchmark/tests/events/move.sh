#!/bin/bash
# MOVE 事件性能测试

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

# ── 主流程 ──

cleanup
mkdir -p "$BENCH_DIR/move_from" "$BENCH_DIR/move_to"

# 先创建文件（不记录事件）
n=30
for i in $(seq 1 $n); do echo "movable" > "$BENCH_DIR/move_from/file_$i.txt"; done

# 启动 daemon + 注册监控
restart_daemon
register

info "=== MOVE: 移动 30 个文件 (from → to) ==="
for i in $(seq 1 $n); do mv "$BENCH_DIR/move_from/file_$i.txt" "$BENCH_DIR/move_to/file_$i.txt"; done
sleep 5

# 停止 daemon 确保 BufWriter flush
sudo killall fsmon 2>/dev/null || true
sleep 2

moved_from=$(count_type "MOVED_FROM")
moved_to=$(count_type "MOVED_TO")
info "MovedFrom: $moved_from / $n"
info "MovedTo:   $moved_to / $n"

[ "$moved_from" -eq "$n" ] && ok "MovedFrom 数量" || fail "MovedFrom 数量 (期望 $n, 实际 $moved_from)"
[ "$moved_to" -eq "$n" ]   && ok "MovedTo 数量"   || fail "MovedTo 数量 (期望 $n, 实际 $moved_to)"

cleanup

echo ""
echo -e "  MOVE 测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
[ "$failed" -eq 0 ]
