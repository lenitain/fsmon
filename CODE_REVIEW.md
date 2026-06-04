# 🔥 热核代码质量审计报告：fsmon

> 审计日期：2026-06-04
> 审计工具：thermo-nuclear-code-quality-review skill

---

## 1. 结构性代码质量退化

### 1.1 文件超过 1k 行阈值

| 文件 | 行数 | 状态 |
|------|------|------|
| `query.rs` | 1078 | 🔴 超过阈值 |
| `clean.rs` | 1014 | 🔴 超过阈值 |
| `config.rs` | 981 | 🟡 逼近阈值 |

`query.rs` 和 `clean.rs` 都已突破 1k 行。这两个文件混合了数据模型、业务逻辑和 I/O 操作，应当拆分。

**`query.rs` 治愈方案**：把 `TimeFilter` 求值提取为 `TimeFilter` 的方法（见下方第 3 节）。`read_events_from` 和 `execute_changes` 共享大量逻辑，可以通过统一的迭代器/过滤管线消除重复。

**`clean.rs` 治愈方案**：`clean_single_log` 函数 80+ 行，同时做了时间过滤、大小裁剪、dry-run 输出、统计打印。应拆为：(1) 纯过滤函数、(2) 大小裁剪器、(3) 输出格式化器。

### 1.2 Monitor 上帝对象

`Monitor` 结构体有 **30+ 字段**，同时承担以下职责：

- fanotify fd 生命周期管理
- inotify 监视
- 路径管理
- 事件路由
- 广播通道
- proc connector
- socket 服务
- 文件日志写入
- 指标统计
- watchdog
- reader 任务监管

这是典型的上帝对象。更干净的拆分方案：

```
Monitor（协调器）
├── FanotifyManager    (fs_groups, dir_cache, path_to_group)
├── InotifyManager     (inotify, _inotify_watches, pending_paths)
├── EventBus           (event_tx, event_stream_tx, broadcast)
├── SocketServer       (socket_listener, subscribe 处理)
├── ProcManager        (proc_cache, pid_tree)
└── Metrics            (metrics, started_at)
```

这样每个关注点可独立测试，`run()` 函数也能从 200+ 行缩减为清晰的调度循环。

---

## 2. 错过的大幅简化机会（Code Judo）

### 2.1 TimeFilter 求值 —— 最大的赢面

时间过滤求值模式在 **3 个位置** 出现了完全相同的 match 块：

**位置 1** — `query.rs:read_events_from`（约第 113 行）：
```rust
let pass = self.time_filters.iter().all(|f| match f.op {
    TimeOp::Gt => event.time > f.time,
    TimeOp::Ge => event.time >= f.time,
    TimeOp::Lt => event.time < f.time,
    TimeOp::Le => event.time <= f.time,
    TimeOp::Eq => event.time == f.time,
});
```

**位置 2** — `query.rs:extract_since`（约第 76 行）：
```rust
match f.op {
    TimeOp::Gt | TimeOp::Ge => { ... }
    _ => {}
}
```

**位置 3** — `clean.rs:clean_single_log`（约第 48 行）：
```rust
let passes_time = time_filter.as_ref().is_none_or(|f| match f.op {
    TimeOp::Gt => event.time > f.time,
    TimeOp::Ge => event.time >= f.time,
    TimeOp::Lt => event.time < f.time,
    TimeOp::Le => event.time <= f.time,
    TimeOp::Eq => event.time == f.time,
});
```

**Code Judo 招式**：给 `TimeFilter` 加一个方法：

```rust
impl TimeFilter {
    pub fn matches(&self, time: DateTime<Utc>) -> bool {
        match self.op {
            TimeOp::Gt => time > self.time,
            TimeOp::Ge => time >= self.time,
            TimeOp::Lt => time < self.time,
            TimeOp::Le => time <= self.time,
            TimeOp::Eq => time == self.time,
        }
    }

    pub fn is_lower_bound(&self) -> bool {
        matches!(self.op, TimeOp::Gt | TimeOp::Ge)
    }

    pub fn is_upper_bound(&self) -> bool {
        matches!(self.op, TimeOp::Lt | TimeOp::Le)
    }
}
```

这一招删掉 30+ 行重复的 match 块，且让逻辑集中在一处可测试。`extract_since` / `extract_until` 变成一行代码。

### 2.2 重复的 PID 状态读取

`utils.rs` 中的 `read_proc_status_fields`（约第 88 行）和 `proc_cache.rs` 中的 `read_proc_info`（约第 255 行）几乎完全一样。

**Code Judo 招式**：删掉 `proc_cache.rs` 中的 `read_proc_info`，复用 `utils.rs` 中的版本。消除 15 行纯重复代码。

### 2.3 重复的 PathEntry → PathOptions 转换

PathEntry → PathOptions 的转换在至少 4 个地方重复：`commands/mod.rs:parse_path_options`、`monitor/events.rs:matching_opts_for_event`、`monitor/live_path.rs:add_path`、`monitor/live_path.rs:add_temp_parent_mark`。

**Code Judo 招式**：添加 `impl From<&PathEntry> for PathOptions`，所有地方统一使用。消除 ~40 行散落的转换逻辑。

### 2.4 重复的 chown 逻辑

`config.rs` 中的 `chown_to_original_user` 和 `fid_parser.rs` 中的 `chown_to_user` 做同样的事情，但返回类型和错误处理不同。

**Code Judo 招式**：保留 `chown_to_user`（更健壮）作为规范辅助函数，让 `chown_to_original_user` 委托给它。

---

## 3. 意面代码 / 分支复杂度增长

### 3.1 Monitor::run() 事件循环

`run()` 方法约 200 行，`tokio::select!` 块包含 **7 个分支**。主事件处理分支本身就有 30+ 行：排空 proc 事件 → 处理批次 → 二次排空 → 修补事件 → 发送 → 检查 pending。

**治愈方案**：把事件处理提取为独立的 `EventProcessor`，封装两次排空再修补的模式。`select!` 循环变成清晰的调度表。

### 3.2 add_path 120 行函数

`add_path` 函数 120+ 行，处理 3 个完全不同的代码路径：路径已监控、路径不存在、路径存在。

**治愈方案**：拆分为 3 个聚焦方法。

---

## 4. 边界 / 抽象 / 类型契约问题

### 4.1 PathEntry 身兼多职

`PathEntry` 被用于 JSONL 序列化、Socket 协议传输、内部 monitor 状态、CLI 参数表示。导致到处都是 `Option<String>` 字段。

**治愈方案**：创建领域专用类型 `StoredEntry`、`SocketEntry`、`MonitorEntry`。

### 4.2 EventSender/EventReceiver 重复

`channel.rs` 中的枚举重复了 bounded/unbounded 逻辑，薄抽象没带来太多价值。

**治愈方案**：使用统一的 `EventChannel` 结构体。

### 4.3 Monitor 上的 #[cfg(test)] 方法

`get_matching_path_options`、`should_output`、`is_path_in_scope` 仅在测试中使用但位于 Monitor impl 块中。

**治愈方案**：移到文件底部单独的 `#[cfg(test)] impl Monitor` 块中。

---

## 5. 文件大小和拆分问题

### 5.1 query.rs（1078 行）

测试占了 ~60%。核心逻辑可以提取到 `query/core.rs`，或把二分搜索 + 时间过滤逻辑做成通用的 `LogReader` 工具。

### 5.2 clean.rs（1014 行）

`clean_single_log` 在一个函数中处理时间过滤、大小裁剪、dry-run 输出和统计打印。

---

## 6. 模块化和抽象问题

### 6.1 monitor/ 子模块边界

所有子模块通过 `impl Monitor` 块实现方法，不是真正的独立模块。应提取拥有自己状态的结构体。

### 6.2 filters.rs 职责混合

混合了数据类型和纯函数。`PathOptions` 应独立，匹配函数可放在 `PathMatcher` 结构体上。

---

## 7. 可读性和可维护性问题

### 7.1 to_jsonl_string_local 的字符串手术

手动 JSON 字符串替换很脆弱。应使用结构化序列化。

### 7.2 Monitor::new() 有 13 个参数

应使用 builder 模式或 `MonitorConfig` 结构体。

---

## 8. 审计总结

### 推定阻塞项

1. **🔴 `query.rs` 和 `clean.rs` 超 1k 行** — 拆分或提取共享逻辑。
2. **🔴 Monitor 上帝对象（30+ 字段）** — 拆分为聚焦的子组件。
3. **🔴 TimeFilter 求值重复 3 次** — 提取为 TimeFilter 的方法。
4. **🔴 `read_proc_status_fields` / `read_proc_info` 重复** — 复用一个。

### 高价值改进

5. **🟡 Monitor::run() 事件循环复杂度** — 提取事件处理为 EventProcessor。
6. **🟡 add_path 120 行函数** — 拆分为 3 个聚焦方法。
7. **🟡 Monitor::new() 13 个参数** — 使用 builder 或 config 结构体。
8. **🟡 PathEntry 身兼存储/传输/内部三职** — 创建领域专用类型。
9. **🟡 重复的 PathEntry→PathOptions 转换** — 单一 `From` impl。

### 美化但低价值

10. **🟢 Monitor impl 上的 `#[cfg(test)]` 方法** — 移到测试专用 impl 块。
11. **🟢 `to_jsonl_string_local` 手动字符串手术** — 使用结构化序列化。

### 做得好的地方

- `fid_parser.rs` 的三层路径解析设计考虑周全，文档完善。
- `FileLogWriter` 正确地隔离为独立的异步任务。
- `MetricsRegistry` 干净且测试充分。
- Socket 协议类型安全，错误分类清晰。
- `DirCache` / `ProcCache` / `PidTree` 分离干净。
- 所有模块的测试覆盖率都很高。
- 项目在 CLI（commands/）和库（src/）之间有清晰的分离。
