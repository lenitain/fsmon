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
