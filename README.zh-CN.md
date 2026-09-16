# fsmon

实时监控文件变更，追溯进程操作。

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

🌍 **语言**: [English](./README.md) | [简体中文](./README.zh-CN.md)

## 概述

**fsmon** 是一款基于 Linux fanotify 的实时文件系统变更监控工具。它监视文件和目录，捕获每一次创建、修改、删除、移动、属性变更等事件，并追溯每个变更的来源进程 — 包括 PID、命令名、用户、父进程 PID、线程组 ID，和可选的完整进程祖先链。

进程跟踪是**事件驱动的**：内核的 cn_proc 事件流维护进程拓扑，无需轮询，也无需递归扫描 `/proc`；每 CPU 的消息序列号可量化丢失的事件。一次性 `/proc` 扫描仅作为启动时的基线。

### 为什么选择 fsmon？

与仅报告文件变更的传统监控工具不同，**fsmon** 增加了**进程追溯**功能 — 它能识别是哪个进程导致了每次变更。这使得在多进程环境中调试意外的文件修改变得更加容易。对于需要追踪文件系统变更源头的系统管理员和开发人员来说，fsmon 提供了传统工具无法比拟的深入洞察。

本工具仅支持 Linux，在其他平台编译将失败并给出明确的错误信息。

## 用法

```
Lightweight high-performance file change tracking tool

Usage: fsmon <COMMAND>

Commands:
  daemon     Run the fsmon daemon (needs CAP_SYS_ADMIN for pid attribution) [alias: d]
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

# 安装加固版 systemd 服务：以你的用户身份运行，只授予 CAP_SYS_ADMIN，
# 并用 seccomp 收窄系统调用面
sudo fsmon init --service
sudo systemctl enable --now fsmon

# 也可以手动运行以便调试
sudo fsmon daemon

# 在另一个终端，添加监控路径
fsmon add _global --path /var/www -r

# 查询事件
fsmon query _global | jq 'select(.cmd == "nginx")'
```

## 权限

fsmon 需要 **`CAP_SYS_ADMIN`，且只用于一件事**：创建*特权* fanotify group。
内核会把没有该能力时创建的 group 标记为 `FANOTIFY_UNPRIV`，随后把**其他进程**
造成的每一条事件的 `metadata.pid` 抹成 0
（`fs/notify/fanotify/fanotify_user.c`）—— 进程追溯是 fsmon 的立身之本，
却会**静默**退化成 `pid: 0`。

除此之外都不需要特权：打标记、读事件、FID→路径解析、进程追踪、写日志
在非特权下均可正常工作。

守护进程把这一个能力关得很紧：

- 启动时 fork 出一个极小的 **fanotify 工厂**子进程，由它继承 `CAP_SYS_ADMIN`。
  它的全部输入是 `(dirfd[, fan_fd], flags, mask)` —— 没有路径字符串、没有 JSON、
  没有事件流 —— 并且运行在只有 8 个系统调用的 seccomp 白名单下
  （`fanotify_init`、`fanotify_mark`、`recvmsg`、`sendmsg`、`read`、`write`、
  `close`、`exit_group`）。
- 随后主进程丢弃**全部**能力（`CapEff=0`、`PR_SET_NO_NEW_PRIVS`）。那些特权
  group 依然报告真实 pid，因为内核把 `FANOTIFY_UNPRIV` 存在 **group 对象**上。
- 该特权**不以磁盘文件的形式存在**，其他本地用户无法取得
  （这正是 `setcap` helper 二进制做不到的）。

`fsmon init --service` 生成的 unit 以你的用户身份运行，并设置
`AmbientCapabilities=CAP_SYS_ADMIN`、`CapabilityBoundingSet=CAP_SYS_ADMIN`、
`NoNewPrivileges=yes`、`SystemCallFilter=@system-service fanotify_init
fanotify_mark`，且只允许写 store、日志与 runtime 目录。注意
`fanotify_init`/`fanotify_mark` **不在** systemd 的 `@system-service` 集合里，
必须显式追加。

fsmon **故意不申请** `CAP_DAC_READ_SEARCH`。它只对 `open_by_handle_at`
路径回退有用，而实践中根本不会走到（目录句柄已通过无需特权的
`name_to_handle_at` 预热），委托它反而会造出一个真正的任意文件读取 oracle。
因此解析器拿到的是空的 mount-fd 列表，回退是零系统调用的立即失败；
`fsmon health` 用 `dir_cache_misses` 暴露其发生次数。

如果启动时没有 `CAP_SYS_ADMIN`，守护进程会**拒绝运行**，而不是静默记录
`pid: 0`。设置 `FSMON_ALLOW_UNPRIVILEGED=1` 可显式接受降级模式，
此时 `fsmon health` 会返回 `"unprivileged": true`。

## 从源码构建

需要 Rust 工具链（已测试 `rustc 1.78.0`）。

```bash
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo build --release
```

## 已知限制

### 短生命周期进程的 `comm`（内核限制）

fsmon 通过内核 cn_proc connector（fork/exec/exit 事件）跟踪进程。内核只在进程通过 `prctl(PR_SET_NAME)` 主动改名时发送 `COMM` 事件——**exec 时从不发送**（`proc_comm_connector` 仅在 `kernel/sys.c` 的 `PR_SET_NAME` 分支被调用）。这是内核设计，不是 fsmon 或其依赖的 bug。

对短生命周期进程（spawn → exec → 退出 <1ms，如 `touch`）的影响：

- 文件事件**不会遗漏**；pid/tgid/ppid/chain 完整。
- 记录事件中的 `comm`/`cmd` 字段为**空**——事件被处理时进程通常已退出，`/proc` 也来不及读取。
- `cmd=` 过滤组匹配不到此类进程。

长生命周期进程不受影响：其 comm 由启动时的 `/proc` 扫描捕获，并通过改名事件刷新。
