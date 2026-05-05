# fsmon PROGRESS — Config/Store/Log Redesign

## 已完成

### 架构变更

旧: `~/.config/fsmon/config.toml` 同时存储基础设施路径和受监控路径。
新: 三层分离，各司其职。

| 文件 | 用途 | 管理方式 |
|------|------|----------|
| `~/.config/fsmon/config.toml` | 基础设施路径 (store/log/socket) | 手动编辑 + `fsmon generate` |
| `~/.local/share/fsmon/store.toml` | 受监控路径数据库 | `fsmon add` / `fsmon remove` |
| `~/.local/state/fsmon/log_<id>.toml` | 每个 entry 独立的日志文件 | daemon 写入, `fsmon clean` 管理 |

### config.toml 格式

```toml
[store]
file = "~/.local/share/fsmon/store.toml"

[logging]
dir = "~/.local/state/fsmon"

[socket]
path = "/tmp/fsmon-<UID>.sock"
```

### store.toml 格式

```toml
next_id = 3

[[entries]]
id = 1
path = "/tmp"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "1KB"
exclude = "*.tmp"
all_events = false
```

### CLI 改动

| 子命令 | 改动 |
|--------|------|
| `daemon` | 无参数, 只读 config.toml |
| `add <path>` | 写入 store.toml, socket 通知 daemon (非致命) |
| `remove <id>` | 从 store.toml 移除, socket 通知 daemon |
| `managed` | 优先从 daemon 读, 回退读 store.toml |
| `query` | `--id <ids>` 过滤 (逗号/范围/重复), 默认扫描全部 .log |
| `clean` | 同 query 的 `--id` 语法 |
| `generate` | 生成 config.toml 模板 (新格式) |

### 文件改动

- **config.rs**: 重写为 Config 结构体 (store/logging/socket 三段), 保留 resolve_uid/resolve_home/guess_home
- **store.rs**: 新建, Store + PathEntry 结构体, CRUD (load/save/add/remove/get)
- **monitor.rs**: 单文件输出 → 按 entry ID 分文件日志, 移除 OutputFormat/instance_name
- **bin/fsmon.rs**: 所有命令使用 Config + Store, 新增 `--id` 参数, parse_query_ids()
- **query.rs**: 支持 log_dir + ids 过滤, 多文件读取
- **lib.rs**: clean_logs 改为按目录 + ids 迭代, clean_single_log 保留原清理逻辑
- **help.rs**: 更新所有路径和参数文档
- **socket.rs**: 导入改为 `crate::store::PathEntry`
- **lib.rs**: 添加 `pub mod store;`

### 构建状态

```
cargo build        ✅ 零警告
cargo clippy       ✅ -D warnings 通过
cargo fmt          ✅ 无差异
cargo test         ✅ 67 passed, 7 ignored (fanotify 测试需要 sudo)
```

## Bug 修复

### 2026-05-05 — fanotify_mark EXDEV 跨挂载点路径导致 daemon 崩溃

- **问题**: 当监控路径位于不同挂载点（如 `/home` vs `/`），daemon 启动时文件系统级
  fanotify_mark 返回 EXDEV，回退到 inode 级 `mark_directory()` 仍失败（同样 EXDEV），
  `?` 传播错误导致 daemon 崩溃退出。同样问题出现在 `fsmon add` 实时添加路径时。
- **修复**:
  - `monitor.rs::run()`: inode 回退 `mark_directory` 从 `?` 改为 `if let Err(e)`，
    失败时仅 `[WARNING]`，daemon 继续运行。
  - `monitor.rs::add_path()`: 同上修复，live add 返回 `ok: true`。
- **额外**: 修复一个 `collapsible_if` clippy 警告。

### 2026-05-05 — `fsmon managed` 输出为空

- **问题**: `cmd_managed()` 用 `Config::load().ok()` 加载配置后未调用
  `resolve_paths()`，导致 socket 路径保持 `/tmp/fsmon-<UID>.sock`（`<UID>` 未替换），\  socket 连接失败；store 路径保持 `~/.local/...`（`~` 未展开），\  store 读取失败。两项都失败，最终输出空列表。
- **修复**: 改为 `let mut cfg = Config::load()?; cfg.resolve_paths()?;`
  与其他命令一致。

### 2026-05-05 — daemon 运行时 `fsmon managed` 输出为空

- **根因**: CLI 与 daemon 的 socket 协议用空行做消息分隔符，但 TOML 序列化
  `Vec<PathEntry>` 时会在 array-of-tables (`[[paths]]`) 之间插入空行。
  `send_cmd()` 读取响应时一遇到第一个空行就停止，只读到 `ok = true`
  就 break，`paths` 字段全部丢失。
- **修复**: `socket.rs::send_cmd()` 响应读取改为 EOF 终止，不再依赖空行分隔。
  TOML 本身可包含空行，解析器正确处理。

### 2026-05-05 — fanotify fd 跨文件系统 EXDEV 导致 log 目录为空

- **根因**: 内核不允许同一 fanotify fd 上有跨文件系统的**任何** mark（包括 inode mark）。
  第一个路径的 mark 成功后 fd 绑定在该文件系统上，其他文件系统的路径返回 EXDEV。
  单 fd 架构无法同时监控 `/tmp`（根文件系统）和 `/home/pilot/.config`（独立 /home 分区）。
- **修复**: 改为多 fd 架构 — 每个文件系统独立一个 fanotify fd，各 spawn tokio 读取任务，
  事件通过 mpsc channel 汇集到主循环处理。
  - 移除 `Monitor.fan_fd` / `mask` / `use_fs_mark` 字段，改为 `fan_fds: Vec<i32>`
  - `run()`: 每个路径探测已有 fd，EXDEV 则创建新 fd
  - `add_path()`: 遍历所有 fd，都不匹配则创新 fd + spawn reader
  - 主循环从 channel 收事件（代替原先单 AsyncFd readable）

### 2026-05-05 — 日志文件名格式改为 log_<id>.toml

- **原因**: 之前 `<id>.log` 格式不易区分，改为 `log_<id>.toml`（带前缀 + .toml 后缀更明确）
- **改动**: `monitor.rs`, `query.rs`, `lib.rs` 中 3 处文件名拼接 + `help.rs` 文档

### 2026-05-05 — 事件路径 canonical 不匹配导致 log 目录为空

- **问题**: 事件路径来自 `/proc/self/fd/{fd}` readlink（canonical 路径），而 `entry_id_for_path()`
  和 `get_matching_path_options()` 只用原始 store 路径比较。若监控路径是 symlink/bind-mount，
  事件路径永远匹配不上，`write_event()` 静默返回使 log 目录完全为空。
- **修复**:
  - `entry_id_for_path()`: 新增 canonical 路径 fallback
  - `get_matching_path_options()`: 新增 canonical 路径 fallback
  - `write_event()`: 无法匹配时印 [WARNING] 便于诊断
  - 事件循环: `is_path_in_scope` 检查改为始终开启（原来仅 fs_mark 模式），减少无关事件泄露

### 2026-05-05 — 实时添加路径不生成 .log 文件

- **问题**: 通过 `fsmon add` (socket 通知 daemon) 添加的路径不生成对应 ID 的 `.log` 文件。
  `add_path()` 未更新 `self.path_ids`，导致 `write_event()` → `entry_id_for_path()` 返回 `None`
  → 静默 return，事件全部丢失。同时 `handle_socket_cmd("add")` 硬编码 `id: 0`，
  进一步导致 persist 后 store 写入 `id = 0`。
- **修复**:
  - `monitor.rs::add_path()`: 新增 `self.path_ids.insert(path, entry.id)`
  - `monitor.rs::handle_socket_cmd("add")`: 用 `max(existing IDs) + 1` 分配正确 ID

## 下一阶段可能的改进

- 找回 query.rs 中丢失的 17 个二进制搜索测试 (代码逻辑未改, 测试函数需恢复)
- 标记 output.rs 可能不再使用
- 添加日志轮转/压缩 (未来考虑)
- web UI / dashboard
