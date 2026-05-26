#!/usr/bin/env python3
"""
fsmon 本地日志订阅器 — 通过 subscribe 协议接收事件并写入 JSONL 文件。

与 daemon 内置的 FileLogWriter 逻辑等价，但作为独立进程运行。
可以用来：
  1. 替代 daemon 内置的文件写入（daemon 启动时不配 log_dir）
  2. 自定义日志路径/格式
  3. 调试/开发时观察实时事件流

用法：
  # 先启动 daemon（不配 log_dir 则无内置文件写入）
  sudo fsmon daemon

  # 运行此订阅器
  python3 extensions/fsmon-log-subscriber.py

  # 或指定日志目录和过滤条件
  python3 extensions/fsmon-log-subscriber.py \
      --log-dir /tmp/my-logs \
      --cmd nginx \
      --types CLOSE_WRITE,DELETE
"""

import argparse
import json
import os
import socket
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description="Subscribe to fsmon events and write to JSONL files")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock",
                        help="fsmon daemon Unix socket path")
    parser.add_argument("--log-dir", default=os.path.expanduser("~/.local/state/fsmon"),
                        help="Output directory for JSONL log files")
    parser.add_argument("--cmd", default=None,
                        help="Only subscribe to this cmd group (e.g. nginx)")
    parser.add_argument("--types", default=None,
                        help="Comma-separated event types filter (e.g. CLOSE_WRITE,DELETE)")
    parser.add_argument("--subscribe-buf", type=int, default=4096,
                        help="Broadcast buffer capacity (must match daemon --subscribe-buf)")
    args = parser.parse_args()

    log_dir = Path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)

    # 构造 subscribe 命令
    cmd_parts = ['cmd = "subscribe"']
    if args.cmd:
        cmd_parts.append(f'track_cmd = "{args.cmd}"')
    if args.types:
        types_list = [f'"{t.strip()}"' for t in args.types.split(",")]
        cmd_parts.append(f"types = [{', '.join(types_list)}]")
    subscribe_cmd = "\n".join(cmd_parts) + "\n\n"

    # 连接 daemon socket
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(args.socket)
    s.sendall(subscribe_cmd.encode())

    # 跳过 TOML ok 响应
    buf = b""
    while True:
        buf += s.recv(1)
        if buf.endswith(b"\n"):
            line = buf.decode().strip()
            if "ok = true" in line or "ok =" in line:
                break
            buf = b""

    print(f"[fsmon-log-subscriber] Connected to {args.socket}")
    print(f"[fsmon-log-subscriber] Writing logs to {log_dir}")
    if args.cmd:
        print(f"[fsmon-log-subscriber] Filter: cmd={args.cmd}")
    if args.types:
        print(f"[fsmon-log-subscriber] Filter: types={args.types}")
    print("[fsmon-log-subscriber] Listening for events... (Ctrl+C to stop)")

    # 维护各 cmd 的文件句柄缓存
    file_handles: dict[str, object] = {}

    try:
        reader = s.makefile("r", buffering=1)
        for line in reader:
            line = line.strip()
            if not line:
                continue

            # 检查是否是警告行
            if '"warning"' in line:
                print(f"[WARNING] {line}", file=sys.stderr)
                continue

            try:
                event = json.loads(line)
            except json.JSONDecodeError as e:
                print(f"[ERROR] Failed to parse event: {e}", file=sys.stderr)
                continue

            # 从 event 获取 cmd group（chain 的最后一个元素）
            chain = event.get("chain", "")
            cmd_name = "_global"
            if chain and " → " in chain:
                parts = chain.split(" → ")
                cmd_name = parts[-1].strip()

            # 写入 {cmd}_log.jsonl
            log_path = log_dir / f"{cmd_name}_log.jsonl"
            with open(log_path, "a") as f:
                f.write(line + "\n")

    except KeyboardInterrupt:
        print("\n[fsmon-log-subscriber] Stopped")
    finally:
        s.close()


if __name__ == "__main__":
    main()
