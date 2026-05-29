#!/usr/bin/env bash
# 通过 socket 将事件实时转发到 Kafka（示例）
# 前提: 安装 kafkacat (或 kcat)
set -euo pipefail

SOCKET="/tmp/fsmon-$(id -u).sock"
BROKER="${1:-localhost:9092}"
TOPIC="${2:-fsmon-events}"

if ! command -v kafkacat &>/dev/null && ! command -v kcat &>/dev/null; then
    echo "请先安装 kafkacat (或 kcat)" >&2
    exit 1
fi

KCAT=$(command -v kafkacat || command -v kcat)

echo "转发 $SOCKET → $BROKER/$TOPIC"
nc -U "$SOCKET" | "$KCAT" -b "$BROKER" -t "$TOPIC" -P
