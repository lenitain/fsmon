#!/bin/bash
# 事件测试编排器

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "========================================="
echo "  fsmon events benchmark"
echo "========================================="

total_pass=0
total_fail=0
suites=("create" "modify" "delete" "move" "recursive" "stress")

for suite in "${suites[@]}"; do
    echo ""
    echo "--- $suite ---"
    if bash "$SCRIPT_DIR/tests/events/$suite.sh"; then
        total_pass=$((total_pass + 1))
    else
        total_fail=$((total_fail + 1))
    fi
done

echo ""
echo "========================================="
echo "  事件测试: $total_pass/${#suites[@]} 套件通过"
echo "========================================="
[ "$total_fail" -eq 0 ]
