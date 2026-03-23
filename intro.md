# fsmon - 学习 Rust 的实战项目

## 这是什么？

`fsmon` 是一个**文件变更溯源工具**，用 Rust 编写。它的功能是监控文件系统的变化（创建、删除、修改等），并记录是哪个进程触发了这些变化。

简单来说：当你想知道"谁动了我的文件"时，这个工具可以告诉你。

## 为什么适合学习 Rust？

这个项目涵盖了 Rust 编程的核心知识点，但代码量适中（约 600 行），适合从入门到进阶的学习者。

### 你将学到什么？

| 知识点 | 在项目中如何体现 |
|--------|-----------------|
| **所有权和借用** | 事件数据在监控器、查询器之间传递 |
| **结构体和枚举** | `FileEvent` 存储事件，`Commands` 定义 CLI 命令 |
| **模式匹配** | 处理不同的文件事件类型 |
| **错误处理** | 使用 `anyhow::Result` 统一处理错误 |
| **异步编程** | 基于 `tokio` 的异步文件监控 |
| **Trait 和泛型** | 序列化/反序列化的通用处理 |
| **模块系统** | 清晰的模块划分（monitor/query/daemon/utils） |
| **外部 crate 使用** | clap(命令行)、serde(序列化)、notify(文件监控) |

## 项目结构

```
src/
├── main.rs      # 入口点，定义 CLI 命令和参数
├── monitor.rs   # 文件监控核心逻辑
├── query.rs     # 历史日志查询
├── daemon.rs    # 后台守护进程管理
└── utils.rs     # 工具函数（时间/大小解析）
```

## 快速开始

### 1. 构建项目

```bash
cargo build --release
```

生成的二进制文件在 `./target/release/fsmon`

### 2. 试试基本功能

```bash
# 查看帮助
./target/release/fsmon --help

# 监控一个目录（开一个新终端执行）
./target/release/fsmon monitor /tmp

# 在另一个终端创建文件，观察输出
touch /tmp/test.txt
```

### 3. 阅读代码建议顺序

1. **先从 `main.rs` 开始** - 了解程序整体结构和 CLI 设计
2. **然后看 `utils.rs`** - 最简单的工具函数，理解辅助代码
3. **接着看 `monitor.rs`** - 核心功能，学习如何使用 notify 库
4. **再看 `query.rs`** - 学习文件读取和过滤逻辑
5. **最后看 `daemon.rs`** - 了解进程管理和状态检查

## 关键代码片段解析

### 1. 命令行定义（main.rs）

```rust
#[derive(Parser)]
#[command(name = "fsmon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 实时监控文件变更
    Monitor { ... },
    /// 查询历史监控日志
    Query { ... },
}
```

使用 `clap` 库的派生宏，简洁地定义 CLI 结构。

### 2. 异步事件循环（monitor.rs）

```rust
while running.load(Ordering::SeqCst) {
    match timeout(Duration::from_millis(100), rx.recv()).await {
        Ok(Some(event)) => {
            if self.should_output(&event) {
                self.output_event(&event, &mut output_file).await?;
            }
        }
        _ => continue,
    }
}
```

典型的 tokio 异步模式：循环 + 超时 + 通道接收。

### 3. 错误处理

整个项目使用 `anyhow::Result`，避免繁琐的错误类型定义，专注于业务逻辑。

## 可以如何扩展？

学有余力？试试这些挑战：

- [ ] 添加单元测试覆盖 `monitor.rs`
- [ ] 支持正则表达式过滤路径
- [ ] 添加 TUI 界面（使用 `ratatui` crate）
- [ ] 实现告警功能（文件大小超过阈值时通知）
- [ ] 支持 macOS/Windows 跨平台

## 依赖说明

| Crate | 用途 |
|-------|------|
| `tokio` | 异步运行时 |
| `notify` | 跨平台文件监控 |
| `clap` | 命令行解析 |
| `chrono` | 时间处理 |
| `serde` + `serde_json` | JSON 序列化 |
| `anyhow` | 错误处理 |
| `regex` | 正则表达式 |

## 下一步

读完这个项目后，你可以尝试：

1. **ripgrep** - Rust 编写的高性能 grep 工具，学习更复杂的代码组织
2. **bat** - cat 命令的现代替代品，学习测试和文档
3. **starship** - 跨平台 shell 提示符，学习配置系统

## 有问题？

- Rust 官方文档：https://doc.rust-lang.org/book/
- Rust By Example: https://doc.rust-lang.org/rust-by-example/
- Tokio 文档：https://tokio.rs/tokio/tutorial

---

**祝学习愉快！** 🦀
