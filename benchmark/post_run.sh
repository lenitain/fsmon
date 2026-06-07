#!/bin/bash
# 后期处理性能测试入口（query / clean）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "========================================="
echo "  fsmon post-process benchmark"
echo "========================================="

if ! pgrep -x fsmon > /dev/null; then
    echo "[ERROR] fsmon daemon 未运行"; exit 1
fi

total_pass=0
total_fail=0
suites=("query" "clean")

for suite in "${suites[@]}"; do
    echo ""
    echo "--- $suite ---"
    if bash "$SCRIPT_DIR/tests/post/$suite.sh"; then
        total_pass=$((total_pass + 1))
    else
        total_fail=$((total_fail + 1))
    fi
done

echo ""
echo "========================================="
echo "  后期测试: $total_pass/${#suites[@]} 套件通过"
echo "========================================="
[ "$total_fail" -eq 0 ]
