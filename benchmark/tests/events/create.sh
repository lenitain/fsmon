#!/bin/bash
# CREATE 事件性能测试

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

# ── 主流程 ──

restart_daemon
cleanup
mkdir -p "$BENCH_DIR"
register

info "=== CREATE: 创建 100 个文件 ==="
n=100
for i in $(seq 1 $n); do echo "content_$i" > "$BENCH_DIR/file_$i.txt"; done
sleep 5

# 停止 daemon 确保 BufWriter flush
sudo killall fsmon 2>/dev/null || true
sleep 2

count=$(count_type "CREATE")
info "期望 = $n，实际: $count"
if [ "$count" -ne "$n" ]; then
    fail "CREATE 数量 (期望 $n, 实际 $count)"
    echo "--- 实际查到的 CREATE 文件 ---"
    fsmon query _global -p "$BENCH_DIR" 2>/dev/null | jq -r 'select(.event_type == "CREATE") | .path' | sort
    echo "--- 期望的文件 ---"
    for i in $(seq 1 $n); do echo "$BENCH_DIR/file_$i.txt"; done | sort
    echo "--- 差异 ---"
    diff <(fsmon query _global -p "$BENCH_DIR" 2>/dev/null | jq -r 'select(.event_type == "CREATE") | .path' | sort) <(for i in $(seq 1 $n); do echo "$BENCH_DIR/file_$i.txt"; done | sort) || true
else
    ok "CREATE 数量"
fi

cleanup

echo ""
echo -e "  CREATE 测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
[ "$failed" -eq 0 ]
