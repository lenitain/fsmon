#!/bin/bash
# 共享配置和工具函数
# 被 benchmark 下所有脚本 source

# ── 从 fsmon.toml 读取配置 ──
FSMON_CONFIG="${HOME}/.config/fsmon/fsmon.toml"

if [ ! -f "$FSMON_CONFIG" ]; then
    echo "[ERROR] fsmon 配置文件不存在: $FSMON_CONFIG"
    exit 1
fi

# 解析 TOML [logging].path（支持 # 注释、空行、引号）
get_config_value() {
    local section="$1" key="$2"
    sed -n "/^\[${section}\]/,/^\[/p" "$FSMON_CONFIG" \
        | grep -E "^[[:space:]]*${key}[[:space:]]*=" \
        | head -1 \
        | sed "s/.*=[[:space:]]*\"\(.*\)\"/\1/" \
        | sed "s|~|${HOME}|"
}

LOG_DIR=$(get_config_value logging path)
if [ -z "$LOG_DIR" ]; then
    echo "[ERROR] fsmon.toml 中未找到 [logging].path"
    exit 1
fi
LOG_FILE="${LOG_DIR}/_global_log.jsonl"

# ── 公共变量 ──
BENCH_DIR="/tmp/fsmon_benchmark"

# ── 颜色 ──
RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[PASS]${NC} $*"; passed=$((passed + 1)); }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; failed=$((failed + 1)); }

# ── 公共函数 ──
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

register() {
    fsmon add _global --path "$BENCH_DIR" -r -t all
    sleep 2
}

cleanup() {
    rm -rf "$BENCH_DIR"
}

count_type() {
    fsmon query _global -p "$BENCH_DIR" 2>/dev/null \
        | jq -s "[.[] | select(.event_type == \"$1\")] | length"
}
