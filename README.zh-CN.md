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

本工具仅支持 Linux，在其他平台编译将失败并给出明确的错误信息。

## 用法

```
Lightweight high-performance file change tracking tool

Usage: fsmon <COMMAND>

Commands:
  daemon     Run the fsmon daemon (requires sudo for fanotify) [alias: d]
  add        Add a path to the monitoring list [alias: a]
  remove     Remove one or more paths from the monitoring list [alias: r]
  monitored  List all monitored paths with their configuration [alias: m]
  query      Query historical file change events from log files [alias: q]
  clean      Clean historical log files, retain by time or size [alias: cl]
  changes    Show the most recent event per path (deduplicated changes) [alias: ch]
  init       Create the config file (directories created on first use) [alias: i]
  cd         Open a subshell in the monitored path or log directory
  health     Query daemon health status [alias: h]
  help       Print this message or the help of the given subcommand(s)

Options:
  -v, --version  Print version
  -h, --help     Print help (see more with '--help')
```

可通过以下命令生成 man 手册和 Shell 补全脚本（bash、fish、zsh、nushell）：
```
fsmon init -c
```

详细文档请查看 `fsmon --help` 或 `man fsmon`。

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

## 已知限制

### 短生命周期进程的 `comm`（内核限制）

fsmon 通过内核 cn_proc connector（fork/exec/exit 事件）跟踪进程。内核只在进程
通过 `prctl(PR_SET_NAME)` 主动改名时发送 `COMM` 事件——**exec 时从不发送**
（`proc_comm_connector` 仅在 `kernel/sys.c` 的 `PR_SET_NAME` 分支被调用）。
这是内核设计，不是 fsmon 或其依赖的 bug。

对短生命周期进程（spawn → exec → 退出 <1ms，如 `touch`）的影响：

- 文件事件**不会遗漏**；pid/tgid/ppid/chain 完整。
- 记录事件中的 `comm`/`cmd` 字段为**空**——事件被处理时进程通常已退出，
  `/proc` 也来不及读取。
- `cmd=` 过滤组匹配不到此类进程（与基于轮询的 0.5 版本行为一致——0.5 同样
  看不到它们，只是显示 `unknown` 而非空串）。

长生命周期进程不受影响：其 comm 由启动时的 `/proc` 扫描捕获，并通过改名事件刷新。
