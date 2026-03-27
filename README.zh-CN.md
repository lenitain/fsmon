<h1 align="center">
  <samp>fsmon</samp>
</h1>

<h3 align="center">实时文件系统变更监控工具，精准追溯进程操作。</h3>

🌍 **选择语言 | Language**
- [简体中文](./README.zh-CN.md)
- [English](./README.md)

[![Release](https://img.shields.io/github/v/release/lenitain/fsmon)](https://github.com/lenitain/fsmon/releases)
[![Build](https://img.shields.io/github/actions/workflow/status/lenitain/fsmon/ci.yml?branch=main)](https://github.com/lenitain/fsmon/actions)
[![License](https://img.shields.io/github/license/lenitain/fsmon)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)

<div align="center">
<img width="1200" alt="fsmon demo" src="./images/fsmon.png" />
</div>

## 特性

- **实时监控**: 默认捕获 8 种核心 fanotify 事件（CREATE、DELETE、CLOSE_WRITE、ATTRIB 等）
- **进程追溯**: 追踪每个文件变更的 PID、命令名和用户 — 即使是 `touch`、`rm`、`mv` 等短命进程
- **递归监控**: 监控整个目录树，自动追踪新建的子目录
- **完整删除捕获**: 再也不怕 `rm -rf` 丢失事件 — 递归删除中的每个文件都能被捕获
- **高性能**: Rust 编写，内存占用 <5MB，零拷贝事件解析
- **灵活过滤**: 支持按时间、大小、进程、用户和事件类型过滤
- **多种格式**: 人类可读、JSON、CSV 三种输出格式
- **守护进程模式**: 后台运行，持久化日志，支持长期审计

## 为什么选择 fsmon

是否曾想知道"谁修改了这个文件？"这正是 fsmon 要解决的问题。

传统的文件监控工具只给你事件本身，而没有上下文 — fsmon 桥接了这段空白，将每个文件变更归因到对应的进程。无论是恶意脚本、自动化部署还是配置错误的服务，你都能准确知道发生了什么、何时发生的、以及是谁（或什么）导致的。

## 快速开始

### 前置要求

- **操作系统**: Linux 5.9+（需要 fanotify FID 模式）
- **文件系统**: ext4、XFS、tmpfs（btrfs 部分支持）
- **构建工具**: Rust 工具链（`cargo`）

```bash
# 验证内核版本
uname -r  # 需要 ≥ 5.9

# 如未安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 安装

```bash
# 从源码构建
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo install --path .

# 或从 crates.io 安装
cargo install fsmon
```

**注意：复制到系统路径以便 sudo 使用：**
```bash
sudo cp ~/.cargo/bin/fsmon /usr/local/bin/
```

### 基础用法

```bash
# 监控目录
sudo fsmon monitor /etc --types MODIFY

# 递归监控
fsmon monitor ~/myproject --recursive

# 守护进程模式长期审计
sudo fsmon monitor /var/log /etc --recursive --daemon --output /var/log/fsmon-audit.log

# 查询历史事件
fsmon query --since 1h --cmd nginx

# 查看守护进程状态
fsmon status
```

## 示例

### 排查配置文件变更

```bash
# 监控 /etc 的修改
sudo fsmon monitor /etc --types MODIFY --output /tmp/etc-monitor.log

# 另一个终端执行修改
echo "192.168.1.100 newhost" | sudo tee -a /etc/hosts

# 查询结果
fsmon query --log-file /tmp/etc-monitor.log --since 1h --types MODIFY
```

### 追踪大文件创建

```bash
# 监控大于 50MB 的文件创建
fsmon monitor /tmp --types CREATE --min-size 50MB --format json

# 触发
dd if=/dev/zero of=/tmp/large_test.bin bs=1M count=100
```

### 审计删除操作

```bash
# 捕获完整的递归删除
fsmon monitor ~/test-project --types DELETE --recursive --output /tmp/deletes.log

# 触发
rm -rf ~/test-project/build/

# 输出显示每个被删除的文件（包括子目录中的）
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build/output.o (PID: 34567, CMD: rm)
[2024-05-01 16:00:00] [DELETE] /home/pilot/test-project/build (PID: 34567, CMD: rm)
```

## 命令参考

```bash
fsmon monitor --help    # 实时监控
fsmon query --help      # 查询历史日志
fsmon status --help     # 查看守护进程状态
fsmon stop --help       # 停止守护进程
fsmon clean --help      # 清理旧日志
```

## 技术架构

- **fanotify (FID 模式)**: Linux 内核级文件监控
- **Proc Connector**: 在进程 `exec()` 时缓存进程信息，确保准确归因
- **name_to_handle_at**: 目录句柄缓存，实现完整的删除追踪
- **Rust + Tokio**: 异步运行时，高并发低延迟

### 事件类型

默认捕获 8 种核心事件。使用 `--all-events` 可开启全部 14 种。

**默认事件（8 种）：**

| 事件 | 说明 |
|------|------|
| CLOSE_WRITE | 文件写完后关闭（最佳"已修改"信号） |
| ATTRIB | 元数据变更（权限、时间戳、所有者） |
| CREATE | 文件/目录已创建 |
| DELETE | 文件/目录已删除 |
| DELETE_SELF | 被监控对象自身被删除 |
| MOVED_FROM | 文件从监控目录移出 |
| MOVED_TO | 文件移入监控目录 |
| MOVE_SELF | 被监控对象自身被移动 |

**额外事件（6 种，使用 --all-events）：**

| 事件 | 说明 |
|------|------|
| ACCESS | 文件被读取 |
| MODIFY | 文件内容被写入（非常频繁） |
| OPEN | 文件/目录被打开 |
| OPEN_EXEC | 文件被打开用于执行 |
| CLOSE_NOWRITE | 只读文件被关闭 |
| FS_ERROR | 文件系统错误（Linux 5.16+） |

## 许可证

MIT License
