# 核热审查：fsmon 代码质量审计

## 概述

对 `fsmon` 项目进行极其严格的可维护性审查，重点关注结构质量、抽象质量和大幅简化机会。

**审查日期**：2026-06-05  
**审查工具**：thermo-nuclear-code-quality-review skill

---

## 🔴 阻断级问题（必须修复）

### 1. 文件超 1000 行

| 文件 | 行数 | 超限幅度 |
|------|------|----------|
| `src/monitor/mod.rs` | 1673 | +67% |
| `src/bin/fsmon.rs` | 1130 | +13% |

**`src/monitor/mod.rs` 问题详述**：
- 混合了：结构体定义、初始化、事件循环、socket 处理、metrics、文件写入、信号处理、测试（700+ 行）
- `Monitor` 结构体有 **25+ 个字段**，大量 `Option<T>`，职责过多

**`src/bin/fsmon.rs` 问题详述**：
- 880 行是测试代码，应拆分到 `tests/` 目录

### 2. 函数过长

| 函数 | 行数 | 涉及职责 |
|------|------|----------|
| `Monitor::run()` | 600 | 16个：root检查、进程缓存、fanotify掩码、路径规范化、inotify、文件系统组、目录缓存、日志目录、磁盘检查、metrics初始化、reader任务、file writer、信号处理、proc connector、事件循环、清理 |

---

## 🟡 代码柔道机会（大幅简化路径）

### 机会 1：拆分 `run()` 方法

当前 600 行的 `run()` 可拆分为：
- `init_process_cache()` — 进程缓存初始化
- `init_fanotify()` — fanotify 文件系统组初始化
- `init_pending_paths()` — 待定路径和 inotify 初始化
- `init_logging()` — 日志目录和磁盘检查
- `spawn_tasks()` — reader/writer 任务启动
- `run_event_loop()` — 主事件循环（7+ 分支的 `tokio::select!`）

### 机会 2：提取 `CacheManager`

当前缓存管理散落在 `run()` 中：
```rust
// 当前：散落各处
self.proc_cache = Some(proc_cache.clone());
self.pid_tree = Some(pid_tree.clone());
// ... dir_cache, file_size_cache

// 建议：统一管理
struct CacheManager {
    proc_cache: ProcCache,
    pid_tree: PidTree,
    dir_cache: Cache<HandleKey, PathBuf>,
    file_size_cache: LruCache<PathBuf, u64>,
}
```

### 机会 3：提取 `PendingPathManager`

待定路径逻辑散落在三处：
- `add_path()` — 处理不存在路径
- `handle_inotify_events()` — 监听目录创建
- `check_pending()` — 重试路径

合并为独立的 `PendingPathManager`，封装 inotify 监听和重试逻辑。

### 机会 4：提取调试辅助函数

5 处重复模式：
```rust
// 当前：散落各处
if self.debug {
    eprintln!("[DEBUG] ...");
}

// 建议：统一辅助
fn debug_log(&self, msg: &str) {
    if self.debug { eprintln!("[DEBUG] {}", msg); }
}
```

### 机会 5：`MonitorConfig` 结构体

`Monitor::new()` 接受 **13 个参数**，应改为配置结构体：
```rust
struct MonitorConfig {
    paths_and_options: Vec<(PathBuf, PathOptions)>,
    log_dir: Option<PathBuf>,
    monitored_path: Option<PathBuf>,
    buffer_size: Option<usize>,
    // ... 其余配置
}
```

### 机会 6：事件循环分支提取

`tokio::select!` 有 **7+ 个分支**，每个应提取为独立方法：
- `handle_events()` — 事件处理
- `handle_signal()` — 信号处理
- `handle_socket()` — socket 命令
- `handle_metrics()` — metrics 报告
- `handle_proc_connector()` — 进程连接器
- `handle_inotify()` — inotify 事件
- `handle_reader_death()` — reader 重启

---

## ✅ 正面发现

| 项目 | 状态 |
|------|------|
| 编译 | ✅ 无警告 |
| Clippy | ✅ 通过 |
| 格式化 | ✅ 通过 |
| 测试 | ✅ 394 个全部通过 |
| Unsafe | ✅ 仅 1 处，有充分注释 |
| 模块结构 | ✅ `monitor/` 下已有 7 个子模块 |
| 类型安全 | ✅ 良好使用 Rust 类型系统 |

---

## 📊 代码统计

| 指标 | 数值 |
|------|------|
| 总代码行数 | 12,696 |
| 最大文件 | 1,673 行 (`monitor/mod.rs`) |
| 最长函数 | 600 行 (`Monitor::run()`) |
| 公开函数 | 115 个 |
| 结构体 | 41 个 |
| 枚举 | 11 个 |
| Trait | 3 个 |
| 测试数 | 394 个 |
| `unwrap()` 调用 | 410 次 |
| `clone()` 调用 | 153 次 |

---

## 审查结论

### ❌ 不通过核热审查标准

违反以下标准：
1. **文件超 1000 行** — `monitor/mod.rs` 超限 67%
2. **错过大幅简化机会** — 600 行 `run()` 可拆分为 6+ 个方法
3. **意大利面增长** — 事件循环混合过多关注点
4. **抽象质量** — Monitor 结构体过大，职责过多

### 通过的标准：
1. ✅ 无明显结构性回归
2. ✅ 无不必要的包装器/类型转换
3. ✅ 模块边界合理

---

## 优先修复建议

### 立即（阻断）
1. 拆分 `src/monitor/mod.rs` 为多个模块
2. 将 `Monitor::run()` 拆分为 6-10 个方法
3. 将 `src/bin/fsmon.rs` 的测试移到 `tests/`

### 高优先
4. 提取 `MonitorConfig` 减少构造器参数
5. 创建 `CacheManager` 统一缓存管理
6. 提取事件循环各分支为独立方法

### 中优先
7. 创建 `debug_log()` 辅助函数
8. 提取常量替换魔法数字
9. 统一错误处理模式

---

*审查人：MiMo-v2.5-pro*  
*审查技能：thermo-nuclear-code-quality-review*
