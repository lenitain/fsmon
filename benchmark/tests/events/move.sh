#!/bin/bash
# MOVE 事件性能测试

set -o pipefail

BENCH_DIR="/tmp/fsmon_move"
LOG_FILE="$HOME/.local/state/fsmon/_global_log.jsonl"
passed=0
failed=0

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[PASS]${NC} $*"; passed=$((passed + 1)); }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; failed=$((failed + 1)); }

restart_daemon() {
    sudo killall fsmon 2>/dev/null || true
    sleep 1
    sudo rm -f "$LOG_FILE"
    sudo fsmon daemon &>/dev/null &
    sleep 3
    if ! fsmon monitored &>/dev/null; then
        echo "[ERROR] daemon 启动失败，请确保有 sudo 权限"
        exit 1
    fi
}

register() { fsmon add _global --path "$BENCH_DIR" -r -t all; sleep 2; }
unregister() { fsmon remove _global --path "$BENCH_DIR" 2>/dev/null || true; }
cleanup() { rm -rf "$BENCH_DIR"; }

count_type() {
    fsmon query _global -p "$BENCH_DIR" 2>/dev/null | jq -s "[.[] | select(.event_type == \"$1\")] | length"
}

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
