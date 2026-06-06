#!/bin/bash
# 压力测试：大量文件顺序修改

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common.sh"

passed=0
failed=0

# ── 主流程 ──

cleanup
mkdir -p "$BENCH_DIR"

# 先创建文件（不记录事件）
n=${1:-5000}
for i in $(seq 1 $n); do echo "stress" > "$BENCH_DIR/file_$i.dat"; done

# 启动 daemon + 注册监控
restart_daemon
register

info "=== 压力: 顺序修改 $n 个文件 ==="

# 顺序修改
t_start=$(date +%s%N)
for i in $(seq 1 $n); do
    echo "mod" >> "$BENCH_DIR/file_$i.dat"
done
t_end=$(date +%s%N)
ms=$(( (t_end - t_start) / 1000000 ))
info "修改耗时: ${ms}ms"
sleep 5

# 停止 daemon 确保 BufWriter flush
sudo killall fsmon 2>/dev/null || true
sleep 2

count=$(count_type "MODIFY")
info "MODIFY 捕获: $count / $n"
[ "$count" -eq "$n" ] && ok "压力 MODIFY" || fail "压力 MODIFY (期望 $n, 实际 $count)"

cleanup

echo ""
echo -e "  压力测试: ${GREEN}${passed} passed${NC}  ${RED}${failed} failed${NC}"
[ "$failed" -eq 0 ]
