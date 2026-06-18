# fsmon

实时监控文件变更，追溯进程操作。

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

🌍 **语言**: [English](./README.md) | [简体中文](./README.zh-CN.md)

## 概述

**fsmon** 是一款基于 Linux fanotify 的实时文件系统变更监控工具。它监视文件和目录，捕获每一次创建、修改、删除、移动、属性变更等事件，并追溯每个变更的来源进程 — 包括 PID、命令名、用户、父进程 PID、线程组 ID，和可选的完整进程祖先链。

## 特性

- **进程追溯**：追踪每个变更由哪个进程（及其子进程）执行。
- **实时监控**：捕获 14 种 fanotify 事件类型（默认 8 种核心事件）。
- **递归监控**：监控整个目录树。
- **完整删除捕获**：完整捕获 `rm -rf` 中的每个文件。
- **捕获时过滤**：按事件类型和文件大小过滤。

## 用法

```
Usage: fsmon [OPTIONS] <COMMAND>

Commands:
  daemon, d       启动守护进程
  add, a          添加监控路径
  remove, r       移除监控路径
  monitored, m    列出监控路径
  query, q        查询历史事件
  clean, cl       清理日志文件
  changes, ch     显示最新变更
  init, i         创建配置文件
  cd              打开子 shell
  health, h       查询守护进程健康状态

Options:
  -h, --help      帮助信息
  -V, --version   版本信息
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

## 与 inotifywait 的比较

[inotifywait](https://man7.org/linux/man-pages/man1/inotifywait.1.html) 是 Linux 标准的文件系统监控工具。但它只报告哪个文件发生了变更，而不报告是哪个进程导致的变更。**fsmon** 增加了进程追溯功能，使得在多进程环境中调试意外的文件修改变得更加容易。

虽然 inotifywait 更简单且更广泛可用，但 fsmon 为系统管理员和开发人员提供了更深入的洞察，帮助他们追踪文件系统变更的源头。

## 许可证

[MIT License](./LICENSE)