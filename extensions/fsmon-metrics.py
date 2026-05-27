#!/usr/bin/env python3
"""
fsmon metrics 命令 — 通过 Unix socket 拉取 Prometheus 格式指标。

这是 pull 模式的 socket 层演示：
  1. 连 socket
  2. 发送 TOML metrics 命令
  3. 读回 Prometheus text format
  4. 格式化或直接输出

无需配置任何东西，daemon 的 socket metrics 命令始终可用。

用法：
  # 输出 Prometheus 格式（默认）
  python3 fsmon-metrics.py

  # 只显示事件计数
  python3 fsmon-metrics.py --summary

  # 循环拉取（像 Prometheus 每 15s scrape）
  python3 fsmon-metrics.py --watch 15
"""

import argparse
import socket
import sys
import time


def pull_metrics(socket_path="/tmp/fsmon-1000.sock") -> str:
    """Connect to fsmon socket, send metrics command, return Prometheus text."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(socket_path)
    s.sendall(b'cmd = "metrics"\n\n')

    reader = s.makefile("r")
    return reader.read()


def parse_summary(text: str) -> dict:
    """Extract key metrics from Prometheus text format."""
    info = {}
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("fsmon_events_total{"):
            # fsmon_events_total{event_type="CREATE",cmd="nginx"} 42
            parts = line.split("}")
            labels = parts[0].replace("fsmon_events_total{", "")
            value = int(parts[1].strip() if len(parts) > 1 else 0)
            key = labels.replace('"', "").replace("event_type=", "").replace(",cmd=", " / ")
            info[key] = value
        elif line.startswith("fsmon_subscribers "):
            info["subscribers"] = int(line.split()[-1])
        elif line.startswith("fsmon_monitored_paths "):
            info["monitored_paths"] = int(line.split()[-1])
        elif line.startswith("fsmon_reader_groups "):
            info["reader_groups"] = int(line.split()[-1])
        elif line.startswith("fsmon_pending_paths "):
            info["pending_paths"] = int(line.split()[-1])
        elif line.startswith("fsmon_disk_buffer_events "):
            info["disk_buf"] = int(line.split()[-1])
    return info


def main():
    parser = argparse.ArgumentParser(description="Pull fsmon metrics via Unix socket")
    parser.add_argument("--socket", default="/tmp/fsmon-1000.sock")
    parser.add_argument("--summary", action="store_true", help="Show human-readable summary")
    parser.add_argument("--watch", type=int, metavar="SECS", help="Watch mode: pull every N seconds")
    args = parser.parse_args()

    if args.watch:
        while True:
            try:
                text = pull_metrics(args.socket)
            except Exception as e:
                print(f"连接失败: {e}", file=sys.stderr)
                time.sleep(args.watch)
                continue

            if args.summary:
                info = parse_summary(text)
                total = sum(v for k, v in info.items() if isinstance(v, int) and k.startswith(("CREATE", "MODIFY", "DELETE", "ACCESS", "OPEN", "CLOSE", "MOVE", "ATTRIB", "FS_ERROR")))
                print(f"\n[{time.strftime('%H:%M:%S')}] events_total={total}  subscribers={info.get('subscribers', '?')}  paths={info.get('monitored_paths', '?')}", flush=True)
            else:
                print(text, end="")
            time.sleep(args.watch)
    else:
        try:
            text = pull_metrics(args.socket)
        except Exception as e:
            print(f"连接失败: {e}", file=sys.stderr)
            print("daemon 是否已在运行？ sudo fsmon daemon", file=sys.stderr)
            sys.exit(1)

        if args.summary:
            info = parse_summary(text)
            print(f"订阅者:     {info.get('subscribers', 0)}")
            print(f"监控路径:   {info.get('monitored_paths', 0)}")
            print(f"reader 组:  {info.get('reader_groups', 0)}")
            print(f"待创建路径: {info.get('pending_paths', 0)}")
            print(f"磁盘缓冲:   {info.get('disk_buf', 0)}")
            print("\n事件计数:")
            for k, v in sorted(info.items()):
                if isinstance(v, int) and k not in {"subscribers", "monitored_paths", "reader_groups", "pending_paths", "disk_buf"}:
                    print(f"  {k:30s} {v}")
        else:
            print(text, end="")


if __name__ == "__main__":
    main()
