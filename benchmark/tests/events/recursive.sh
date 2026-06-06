#!/bin/bash
# 递归监控性能测试

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

check_path() {
    local c
    c=$(fsmon query _global -p "$BENCH_DIR" 2>/dev/null | jq -s "[.[] | select(.path == \"$1\")] | length")
    [ "$c" -gt 0 ]
}

# ── 主流程 ──

restart_daemon
cleanup
mkdir -p "$BENCH_DIR/a/b"
register

info "=== 递归: 2 层子目录 ==="
echo "root" > "$BENCH_DIR/root.txt"
echo "l1"   > "$BENCH_DIR/a/level1.txt"
echo "l2"   > "$BENCH_DIR/a/b/level2.txt"
sleep 5

# 停止 daemon 确保 BufWriter flush
sudo killall fsmon 2>/dev/null || true
sleep 2

check_path "$BENCH_DIR/root.txt"       && ok "递归 root.txt"       || fail "递归 root.txt"
check_path "$BENCH_DIR/a/level1.txt"   && ok "递归 a/level1.txt"   || fail "递归 a/level1.txt"
check_path "$BENCH_DIR/a/b/level2.txt" && ok "递归 a/b/level2.txt" || fail "递归 a/b/level2.txt"

cleanup

echo ""
echo -e "  递归测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
[ "$failed" -eq 0 ]
