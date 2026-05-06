# fsmon PROGRESS — Config/Store/Log Redesign

## 已完成

### 2026-05-06 — 移除数字 ID，路径即唯一标识

参考 chezmoi 设计，路径本身就是唯一标识符，`id: u64` 字段是多此一举。

#### 核心改动

| 领域 | 旧 | 新 |
|------|-----|-----|
| 日志文件名 | `log_{id}.toml` | 路径编码 → `_tmp.toml`（`/` → `_`） |
| 路径标识 | `id: u64` 字段 | 路径自身 (`PathBuf`) |
| CLI 删除 | `fsmon remove <ID>` | `fsmon remove <path>` |
| CLI 查询 | `--id 1,3-5` (范围语法) | `--path /tmp --path /var` (重复) |
| CLI 清理 | `--id 1,3-5` | `--path /tmp` (重复) |
| 唯一性保证 | `Store::next_id()` | 路径唯一 (HashSet dedup) |
| socket 协议 | `id: Option<u64>` | 无 `id` 字段 |

#### 文件改动

| 文件 | 改动 |
|------|------|
| `store.rs` | `PathEntry` 移除 `id`，`add_entry`/`remove_entry`/`get` 改用路径 |
| `socket.rs` | `SocketCmd` + `SocketResp` 移除 `id` 字段 |
| `utils.rs` | 新增 `path_to_log_name()` (路径编码 → 文件名，如 `_tmp.toml`) |
| `lib.rs` | `clean_logs` 签名改用 `paths: Option<&[PathBuf]>` |
| `monitor.rs` | 移除 `path_ids`，`write_event` 用路径编码日志文件，`matching_path` 取代 `entry_id_for_path` |
| `query.rs` | `ids: Option<Vec<u64>>` → `paths: Option<Vec<PathBuf>>`，日志扫描 `.toml` |
| `bin/fsmon.rs` | `remove` 取路径，`query --path`，`clean --path`，移除 `parse_query_ids` |
| `help.rs` | 所有 ID 相关帮助文本更新为路径 |
| `config.rs` | 配置模板注释更新 |

#### 构建状态

```
cargo build     ✅ 零警告
cargo clippy    ✅ -D warnings 通过
cargo fmt       ✅ 无差异
cargo test      ✅ 73 passed, 7 ignored (fanotify 需要 sudo)
```

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

### 2026-05-05 — 实时添加路径无事件 (mount_fd / cache / fan_fds 三重 bug)

- **问题**: `fsmon add` 实时添加路径后 daemon 不产生任何事件日志。三个 bug 叠加:
  1. `run()` 初始 fd 只存在 local `fan_groups`，未写入 `self.fan_fds`，
     导致 `add_path()` → `try_mark_on_existing()` 永远无 fd 可尝试
  2. `add_path()` 在 `spawn_fd_reader()` 之后才 push mount_fd，
     新 reader task 的快照里缺少该路径的目录 fd，`open_by_handle_at` 解析失败
  3. `add_path()` 缓存目录句柄到 `self.dir_cache`（已被 `run()` 的 `mem::take` 掏空），
     而非 `shared_dir_cache`，reader 的二阶段路径恢复也找不到句柄
  结果: 事件文件句柄无法解析 → 路径为空 → `is_path_in_scope` 过滤掉 → 无日志
- **修复**:
  - `run()`: 初始化后 `self.fan_fds.push(group.fd)` 存入所有 fd
  - `add_path()`: mount_fd + shared_dir_cache 写入移到 `spawn_fd_reader` 之前
  - `add_path()`: 目录句柄缓存写入 `shared_dir_cache` 而非 `self.dir_cache`

### 2026-05-05 — Store::load 自动校验 path/ID 唯一性 + next_id

- **问题**: 用户手动编辑 `store.toml` 可绕过所有应用层约束，产生重复 path、重复 ID、
  next_id 低于实际 max_id 等问题。
- **修复**: 新增 `Store::validate()`，在 `load()` 时自动修复：
  1. 重复 path：反向扫描，同 path 只保留最后一个（新配置覆盖旧配置）
  2. 重复 ID：第一个保留原 ID，后续重新分配 `next_id` 的递增 ID
  3. next_id：确保 `>= max(id) + 1`，空表时 `>= 1`
- 返回 `bool` 表示是否修复，调用方可重新 `save()`

### 2026-05-05 — 重复 `fsmon add <path>` 产生重复 entry

- **问题**: `Store::add_entry()` 直接 `push`，不检查 path 是否已存在。
  连续 `fsmon add ~/.config` 产生多个同路径 entry，daemon 启动时全部加载，
  `managed` 也全部列出。不同 add 的参数互不覆盖。
- **修复**: `add_entry()` 先 `retain(|e| e.path != entry.path)` 移除旧 entry，
  再 `push` 新 entry。重复 add 同一路径时后者取代前者，获得最新 ID。

### 2026-05-05 — store.next_id 因 daemon persist_config 回退导致 ID 不唯一

- **问题**: 三个 bug 叠加导致 ID 不唯一：
  1. CLI `fsmon add` 通过 socket 通知 daemon 时未发送已分配的 ID
     (`id: None`)，daemon 用 `max(path_ids) + 1` 自算，与 store 的 `next_id` 脱节
  2. `handle_socket_cmd("add")` 完全忽略 `cmd.id` 字段
  3. `persist_config()` 用 `store.next_id = max_id + 1` 覆写 store，
     当 entry 被删除后 `max_id` 降低，导致 `next_id` 回退，
     后续 `store.add_entry()` 分配已用过的 ID
- **修复**:
  - CLI 发送 `id: Some(assigned_id)` 给 daemon
  - daemon 优先使用 `cmd.id`，未提供时再 fallback
  - `persist_config()` 用 `store.next_id.max(calculated)` 避免回退

### 2026-05-05 — add/remove 打印不必要的 daemon 不可达警告

- **问题**: `fsmon add` / `fsmon remove` 在 store 操作成功后尝试 socket 通知 daemon，
  若 daemon 未运行则打印 "Daemon not reachable" + 重启提示，
  让用户误以为操作失败（实际 store 已正确修改）。
- **修复**: 静默忽略 `Err(_)`（daemon 未运行），store 已保存，重启后自动生效。
  仅保留 `Ok(resp)` 非 ok 分支的错误输出（daemon 运行但操作失败）。

### 2026-05-05 — 更新 README 与代码保持同步

两份 README（中英文）全面重写以匹配当前代码架构：

| 旧内容 | 新内容 |
|--------|--------|
| `~/.config/fsmon/config.toml` 存路径 | config 仅基础设施，路径在 `~/.local/share/fsmon/store.toml` |
| `fsmon remove /path` | `fsmon remove <ID>` — 按数字 ID 移除 |
| `~/.local/state/fsmon/history.log` | `~/.local/state/fsmon/log_<ID>.toml` 按 ID 分文件 |
| `--log-file` 选项 | `--id` 选项，支持逗号/范围/重复 |
| 配置示例含 `[[paths]]` | 配置示例为基础设施三段式（store/logging/socket） |
| 缺失 `fsmon generate` | 新增 `generate` 子命令说明 |
| 模块列表缺 config/store/systemd | 补全全部 14 个模块 |
| JSON 输出格式 | 实际为 TOML（`--format json` 别名） |
| 架构图中 config 放路径 | 架构图更新为 config + store 两层分离 |

## 下一阶段可能的改进

- 找回 query.rs 中丢失的 17 个二进制搜索测试 (代码逻辑未改, 测试函数需恢复)
- 标记 output.rs 可能不再使用
- 添加日志轮转/压缩 (未来考虑)
- web UI / dashboard
