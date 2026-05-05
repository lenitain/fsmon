# fsmon PROGRESS

## 当前状态

重构中：从双二进制（`fsmon` + `fsmon-cli`）合并为单二进制，单 daemon 架构。

### V2 新设计

**架构**：单 `fsmon` 二进制，一个常驻 daemon（systemd 管理），通过 unix socket 接受 CLI 命令。

**命令树**：
```
fsmon
├── daemon          # systemd ExecStart，常驻监控
├── add <path>      # 添加监控路径（--recursive/-r, --types, --min-size, --exclude, --all-events）
├── remove <path>   # 移除监控路径
├── managed         # 列出所有受管路径及详情
├── query           # 查询历史事件（--since, --until, --pid, --cmd, --user, --types, --min-size, --format, --sort）
├── clean           # 清理旧日志（--keep-days, --max-size, --dry-run）
├── install         # 安装 systemd 服务 + 初始化配置
└── uninstall       # 卸载 systemd 服务
```

**配置文件**：`/etc/fsmon/fsmon.toml`
```toml
log_file = "/var/log/fsmon/history.log"
socket_path = "/var/run/fsmon/fsmon.sock"
[[paths]]
path = "/var/www"
recursive = true
types = ["MODIFY", "CREATE"]
min_size = "100MB"
exclude = "*.tmp"
all_events = false
```

**Socket 协议**（`/var/run/fsmon/fsmon.sock`，JSON 行）：
```
→ {"cmd":"add","path":"/var/www","recursive":true,"types":["MODIFY"]}
← {"ok":true}
→ {"cmd":"remove","path":"/var/www"}
← {"ok":true}
→ {"cmd":"list"}
← {"paths":[{"path":"/var/www","recursive":true,...}]}
```

**daemon 生命周期**：启动 → 读配置 → fanotify_init → mark(paths) → bind socket → 主循环（fan_fd + socket + SIGHUP + SIGTERM）。add/remove 不重启 fanotify，动态 mark_add/mark_remove。

**保留原有 flag**：monitor 的参数保留到 `add` 中（`-r`, `--types`, `--min-size`, `--exclude`, `--all-events`）。query/clean 保留所有原参数。

---

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
- [x] 重写 `systemd.rs`：单 `fsmon.service`（非 template），`RuntimeDirectory`，`CapabilityBoundingSet` + `AmbientCapabilities`
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
