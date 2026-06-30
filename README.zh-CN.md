# fsmon

实时监控文件变更，追溯进程操作。

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

🌍 **语言**: [English](./README.md) | [简体中文](./README.zh-CN.md)

## 概述

**fsmon** 是一款基于 Linux fanotify 的实时文件系统变更监控工具。它监视文件和目录，捕获每一次创建、修改、删除、移动、属性变更等事件，并追溯每个变更的来源进程 — 包括 PID、命令名、用户、父进程 PID、线程组 ID，和可选的完整进程祖先链。

### 为什么选择 fsmon？

与仅报告文件变更的传统监控工具不同，**fsmon** 增加了**进程追溯**功能 — 它能识别是哪个进程导致了每次变更。这使得在多进程环境中调试意外的文件修改变得更加容易。对于需要追踪文件系统变更源头的系统管理员和开发人员来说，fsmon 提供了传统工具无法比拟的深入洞察。

## 用法

```
Note: If installed via 'cargo install', copy to system path for sudo compatibility:
  sudo cp ~/.cargo/bin/fsmon /usr/local/bin/

Config:  ~/.config/fsmon/fsmon.toml (created by 'fsmon init')
Monitor: ~/.local/share/fsmon/monitored.jsonl
Logs:    ~/.local/state/fsmon/
Socket:  /run/user/<UID>/fsmon/daemon.sock

Usage: fsmon <COMMAND>

Commands:
  daemon     Run the fsmon daemon (requires sudo for fanotify) [aliases: d]
  add        Add a path to the monitoring list [aliases: a]
  remove     Remove one or more paths from the monitoring list [aliases: r]
  monitored  List all monitored paths with their configuration [aliases: m]
  query      Query historical file change events from log files [aliases: q]
  clean      Clean historical log files, retain by time or size [aliases: cl]
  changes    Show the most recent event per path (deduplicated changes) [aliases: ch]
  init       Create the config file (directories created on first use) [aliases: i]
  cd         Open a subshell in the monitored path or log directory
  health     Query daemon health status [aliases: h]
  help       Print this message or the help of the given subcommand(s)

Options:
  -v, --version  Print version
  -h, --help     Print help
```

### 快速开始

```bash
# 安装
cargo install fsmon

# 启动守护进程（需要 root 权限以使用 fanotify）
sudo fsmon daemon

# 在另一个终端，添加监控路径
fsmon add _global --path /var/www -r

# 查询事件
fsmon query _global | jq 'select(.cmd == "nginx")'
```

## 从源码构建

需要 Rust 工具链（已测试 `rustc 1.78.0`）。

```bash
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo build --release
```
