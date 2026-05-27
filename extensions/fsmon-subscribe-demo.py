#!/usr/bin/env python3
"""
fsmon subscribe 协议示例 — 连接 daemon，接收实时事件流。

这是 subscribe 协议的最小演示，展示如何：
  1. 连 Unix socket
  2. 发送 TOML subscribe 命令
  3. 读 TOML 响应
  4. 持续接收 JSONL 事件

文件写入是 daemon 内置 FileLogWriter 的职责，不需要外部脚本做。
如果要自定义输出，用这个脚本的框架接入 Kafka / S3 / webhook 等。

用法：
  # 确保 daemon 已在运行
  sudo fsmon daemon

  # 查看所有实时事件
  python3 extensions/fsmon-subscribe-demo.py

  # 只看 nginx 的 CLOSE_WRITE 事件
  python3 extensions/fsmon-subscribe-demo.py --track-cmd nginx --types CLOSE_WRITE
"""

import argparse
import json
import socket


def main():
    parser = argparse.ArgumentParser(description="Subscribe to fsmon real-time event stream")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--track-cmd", default=None, help="Filter by cmd group")
    parser.add_argument("--types", default=None, help="Comma-separated event types")
    args = parser.parse_args()

    # 构造 TOML 命令
    lines = ['cmd = "subscribe"']
    if args.track_cmd:
        lines.append(f'track_cmd = "{args.track_cmd}"')
    if args.types:
        types = ", ".join(f'"{t.strip()}"' for t in args.types.split(","))
        lines.append(f"types = [{types}]")
    payload = "\n".join(lines) + "\n\n"

    # 连 socket
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(args.socket)
    s.sendall(payload.encode())

    # 读初始 TOML 响应
    reader = s.makefile("r")
    resp = reader.readline()
    if "ok = true" not in resp:
        print(f"subscribe 失败: {resp.strip()}")
        return

    print(f"已连接 {args.socket}，等待事件... (Ctrl+C 退出)")

    # 持续读 JSONL 事件
    for line in reader:
        line = line.strip()
        if not line:
            continue
        if '"warning"' in line:
            print(f"[!] {json.loads(line).get('warning', line)}")
            continue
        try:
            ev = json.loads(line)
            print(f"[{ev['event_type']}] {ev['path']}  pid={ev['pid']}  cmd={ev['cmd']}")
        except json.JSONDecodeError:
            print(f"[raw] {line}")


if __name__ == "__main__":
    main()
