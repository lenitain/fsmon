# fsmon PROGRESS

## 当前状态

✅ **最小权限重构完成**（2026-05-05）：config 从 `/etc/fsmon/fsmon.toml` 迁移到用户路径。
所有测试通过，clippy clean。

### V3 架构变更

**移除 `/etc/fsmon/fsmon.toml`**，不再需要 root 读写配置。

**新配置文件**：`~/.config/fsmon/config.toml`
```toml
[[paths]]
path = "/home/user/project"
recursive = true
all_events = false
```

**日志文件**：`~/.local/share/fsmon/history.log`
**Socket**：`$XDG_RUNTIME_DIR/fsmon.sock` 或 `~/.local/share/fsmon/fsmon.sock`

**权限分离**：
- **需要 sudo**：`fsmon daemon`（fanotify 必须 root），`fsmon install/uninstall`（systemd 必须 root）
- **不需要 sudo**：`fsmon add/remove/managed/query/clean`

**自动迁移**：`fsmon daemon` 首次启动时自动从旧的 `/etc/fsmon/fsmon.toml` 读取 `[[paths]]` 并写入新的用户配置，然后删除（设计上保留旧文件不做破坏）。

## 实现计划

### P0 — 基础设施
- [x] 重写 `config.rs`：Config + PathEntry 结构体，`[[paths]]` TOML 格式
- [x] 创建 `socket.rs`：SocketCmd/SocketResp 协议、send_cmd 客户端、listen 监听器
- [x] 重构 `monitor.rs`：PathOptions 按路径过滤、动态 add/remove socket 集成

### P1 — 单二进制入口
- [x] 删除 `src/bin/fsmon.rs` 和 `src/bin/fsmon-cli.rs`，新建 `src/bin/fsmon.rs`
- [x] 重新组织 CLI 命令树（使用 clap enum dispatch）
- [x] 更新 `Cargo.toml` 为单 binary

### P1 — 配置系统
- [x] 重写 `config.rs`：单配置 `/etc/fsmon/fsmon.toml`，`[[paths]]` 条目
- [x] 改进测试覆盖：添加 save/add/remove 的 IO 测试，修复 `load()` 测试，移除琐碎测试
- [x] 添加 `#[must_use]` 和 `generate_default()` 成功消息
- [x] `install` 子命令：生成 service 文件 + 默认配置
- [x] 合并原有的 `InstanceConfig` 逻辑

### P2 — daemon 子系统
- [x] `fsmon daemon` 命令入口，加载配置，`fanotify_init`
- [x] 适配现有 `monitor.rs` 为 daemon 模式（多 path fanotify_mark、PathOptions 逐路径过滤）
- [x] unix socket listener（tokio::select! 集成到主循环）
- [x] add/remove/list 命令处理 + 动态 fanotify_mark（FAN_MARK_ADD/FAN_MARK_REMOVE）
- [x] SIGHUP 重载配置（reload_config 方法）
- [x] 优雅关闭（SIGTERM/SIGINT）

### P3 — CLI 子系统
- [x] `add <path>`：连 socket → 发 add 命令 → 显示结果
- [x] `remove <path>`：连 socket → 发 remove 命令 → 显示结果
- [x] `managed`：连 socket → list → 表格输出
- [x] CLI 在 socket 不可用时给出明确提示

### P4 — query / clean（移植现有代码）
- [x] `query`：读配置获取 log_file 路径，其余逻辑不变
- [x] `clean`：同上
- [x] 保留 `--log-file` override

### P5 — systemd 集成
- [x] 重写 `systemd.rs`：单 `fsmon.service`，`RuntimeDirectory`，`CapabilityBoundingSet` + `AmbientCapabilities`
- [x] `install(force)` 创建配置目录、生成默认配置、systemctl daemon-reload
- [x] `uninstall()` 移除 service 文件 + daemon-reload
- [x] 更新 `cmd_install` 调用签名

### P6 — 清理
- [x] 删除旧 `fsmon-cli.rs` 相关逻辑
- [x] 删除 `InstanceConfig`（已合并到主配置）
- [x] 删除 `generate` 命令
- [x] 删除死代码（`DEFAULT_LOG_PATH`, `parse_output_format`, `parse_sort_by`）
- [x] 移除未使用依赖（`directories`, `dirs`）
- [x] 更新 AGENTS.md 反映单二进制架构
- [x] 修复 clippy warning（空 `println!`）
- [x] 测试通过（78 passed, 7 ignored）
- [x] 同步 README.md 和 README.zh-CN.md 到最新代码（2026-05-05）

### V7 — 最小权限重构
- [x] config.rs 改为 `UserConfig`，路径为 `~/.config/fsmon/config.toml`
- [x] log_file/socket_path 使用 XDG 标准路径
- [x] systemd.rs 不再生成配置，只写 unit 文件
- [x] add/remove/managed/query/clean 不再需要 sudo
- [x] 自动迁移旧 `/etc/fsmon/fsmon.toml` 中的 paths
- [x] 测试通过（75 passed, 7 ignored），clippy clean
- [ ] 更新 README 反映新配置架构
