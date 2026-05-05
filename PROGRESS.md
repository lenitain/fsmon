# fsmon PROGRESS

## 当前状态

基础功能已完成：fanotify 实时监控、进程缓存、日志查询、systemd 集成、日志清理。

**D1/D2 已完成**：配置错误统一 exit(78) + `RestartPreventExitStatus` 保护 + 醒目提示。

---

## 后台模式（systemd 模板）边界问题与解决方案

### D1 [高] 实例配置不存在 ⇒ systemd 无限重启 ✅

**方案**：
1. **区分退出码语义**：在生成的 systemd template 中添加 `RestartPreventExitStatus=78`，阻止 systemd 重启配置类错误
2. **醒目提示**：启动失败时用醒目的格式（=== BORDER ===）打印修复指引，告诉用户运行 `fsmon generate --instance <name>`

**涉及文件**：`src/systemd.rs`、`src/main.rs:398-410`

### D2 [高] 实例配置路径为空 / TOML 格式错误 ⇒ 无限重启 ✅

**方案**：
- 和 D1 统一处理：`RestartPreventExitStatus=78` 覆盖所有配置类错误
- 错误消息中明确区分"格式错误"和"路径未配置"两种场景

**涉及文件**：`src/config.rs:219-236`、`src/main.rs:398-410`

### D3 [高] 监控路径不存在 ⇒ 启动失败 ⇒ 无限重启

**问题**：实例配置中 `paths = ["/nonexistent"]` → `fanotify_mark` 在不存在路径上失败 → `bail!` → `exit(1)` → 触发重启循环。

**方案**：
- 在 `Monitor::run` 启动阶段显式检查路径存在性
- 不存在的路径给出清晰错误（exit 78 而非 exit 1），纳入 RestartPreventExitStatus
- 同时检查所有 paths，一次性报告所有问题路径

**涉及文件**：`src/monitor.rs:203-210`

### D4 [高] 未配置 output ⇒ 事件日志静默丢失

**问题**：实例模式下未设 `output` → 事件仅输出到 stdout (journald) → journald 有大小限制和自动清理 → 用户用 `fsmon query` 查不到历史事件，且无任何提示。

**方案**：
- 实例模式启动时，若 `output` 为 None，向 stderr 打印 WARNING 提示用户事件不会被持久化
- 可选：实例模式下默认使用 `/var/log/fsmon/{name}.log` 作为 output

**涉及文件**：`src/main.rs:448-450`、`src/monitor.rs:258-314`

### D5 [中] 多实例写同一 output 文件 ⇒ 日志交错损坏

**问题**：两个 systemd 实例（如 `fsmon@web`、`fsmon@db`）若配置了相同 `output` 路径 → 两个进程同时 append 写 → 无文件锁 → JSON 行交错 → 文件损坏。

**方案**：
- 启动时检查 output 文件是否可写，尝试获取 `flock`（文件锁）
- 或在 output 路径中自动注入 instance name：`/var/log/fsmon/{name}.log`，在 header 中推荐此用法
- 添加启动时冲突检测：读取 output 文件最后一行 header，若 instance name 不同则 warning

**涉及文件**：`src/monitor.rs:258-314`

### D6 [中] SIGTERM 无优雅关闭

**问题**：systemd 停服务发 SIGTERM，但代码只处理了 `ctrl_c()` (SIGINT)。SIGTERM 直接杀进程，`fan_fd`/`mount_fds` 清理代码不执行。

**方案**：
- 添加 `tokio::signal::sigterm()` 处理，与 ctrl_c 走同一清理路径

**涉及文件**：`src/monitor.rs:392-395`

### D7 [中] Proc connector 超时 ⇒ 启动失败

**问题**：`proc_cache` 订阅最多等 2s，在 systemd 冷启动或高负载环境下可能超时 → `bail!` → exit(1) → 触发重启。

**方案**：
- 超时后不 bail，改为 warn 降级：打印 warning 继续运行（proc_cache 是优化组件，非核心功能）
- 短生命周期进程的进程名归因在超时期间可能不准确，但监控仍然正常工作

**涉及文件**：`src/monitor.rs:136-152`

### D8 [中] 日志文件无限增长

**问题**：`output` 日志文件只 append 不轮转。`fsmon clean` 是手动 CLI 命令，无人值守运行时日志持续膨胀直至占满磁盘。

**方案**：
- 实例模式下支持自动轮转配置（如 `max_log_size = "500MB"`），写入前检查文件大小
- 或提供可选的 `log_rotate` 配置项，超出大小后自动 truncate 尾部

**涉及文件**：`src/config.rs`、`src/monitor.rs`

### D9 [中] Restart=on-abnormal 语义与配置错误不匹配 ✅

**方案**：
- `Restart=on-abnormal` → `Restart=on-failure` + `RestartPreventExitStatus=78`

**涉及文件**：`src/systemd.rs:13-34`

### D10 [低] 容器/非标准环境 fanotify 不可用

**问题**：fanotify FID 模式需要 Linux 5.9+ 且容器需 `CAP_SYS_ADMIN`。在不支持的环境下 `fanotify_init` 失败 → bail → exit(1) → 重启循环。

**方案**：
- 检测到 `fanotify_init` 失败时明确提示内核版本要求和缺少的 capability
- 纳入 D9 的 `RestartPreventExitStatus` 处理

**涉及文件**：`src/monitor.rs:154-164`

---

## 优先级与工作顺序

| 优先级 | 类别 | 问题 | 工作量 | 状态 |
|--------|------|------|--------|------|
| P0 | 配置错误 | D1/D2/D9 systemd 无限重启 | 小 | ✅ |
| P0 | 事件丢失 | D4 output 未配置静默丢事件 | 小 | ✅ |
| P1 | 启动验证 | D3 路径不存在检查 | 小 | ✅ |
| P1 | 优雅关闭 | D6 SIGTERM 处理 | 小 | ⏳ |
| P1 | 启动降级 | D7 proc_cache 超时降级 | 小 | ⏳ |
| P2 | 文件冲突 | D5 多实例 output 冲突 | 中 | ⏳ |
| P2 | 磁盘管理 | D8 日志自动轮转 | 中 | ⏳ |
| P3 | 可移植性 | D10 容器环境检测 | 小 | ⏳ |

## 下一步

建议按顺序推进：D4 → D3 → D6 → D7 → D5 → D8 → D10
