#!/bin/bash
# MODIFY 事件性能测试

set -o pipefail

BENCH_DIR="/tmp/fsmon_modify"
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

count_type() {
    fsmon query _global -p "$BENCH_DIR" 2>/dev/null | jq -s "[.[] | select(.event_type == \"$1\")] | length"
}

# ── 主流程 ──

cleanup
mkdir -p "$BENCH_DIR"

# 先创建文件（不记录事件）
n=50
for i in $(seq 1 $n); do echo "original" > "$BENCH_DIR/file_$i.txt"; done

# 启动 daemon + 注册监控
restart_daemon
register

info "=== MODIFY: 修改 50 个文件 ==="
for i in $(seq 1 $n); do echo "modified" >> "$BENCH_DIR/file_$i.txt"; done
sleep 5

# 停止 daemon 确保 BufWriter flush
sudo killall fsmon 2>/dev/null || true
sleep 2

count=$(count_type "MODIFY")
info "期望 = $n，实际: $count"
[ "$count" -eq "$n" ] && ok "MODIFY 数量" || fail "MODIFY 数量 (期望 $n, 实际 $count)"

cleanup

echo ""
echo -e "  MODIFY 测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
[ "$failed" -eq 0 ]
