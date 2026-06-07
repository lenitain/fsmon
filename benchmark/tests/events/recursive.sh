#!/bin/bash
# 递归监控性能测试

set -o pipefail

BENCH_DIR="/tmp/fsmon_recursive"
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
cleanup() { rm -rf "$BENCH_DIR"; }

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
