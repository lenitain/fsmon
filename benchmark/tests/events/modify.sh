#!/bin/bash
# MODIFY 事件性能测试

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

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
