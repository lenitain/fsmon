# fsmon PROGRESS — Podman 模式重构

## 架构变更

放弃 systemd 服务模式，改为**用户自启 daemon（Podman 模式）**。

```
之前: sudo systemctl start fsmon → root daemon → /etc/fsmon/fsmon.toml
                                              → /var/log/fsmon/history.log
                                              → /var/run/fsmon/fsmon.sock
      fsmon add /tmp → 需要 sudo

之后: sudo fsmon daemon & → root daemon → ~/.config/fsmon/config.toml（读用户配置）
                                         → ~/.local/state/fsmon/history.log
                                         → /tmp/fsmon-<UID>.sock（0666 权限）
      fsmon add /tmp → 不需要 sudo
```

路径一律基于**原始用户**（SUDO_UID）解析，不依赖 HOME 环境变量。

### 文件路径

| 用途 | 路径 | 创建者 | 权限 |
|---|---|---|---|
| 监控路径配置 | `~/.config/fsmon/config.toml` | fsmon add/remove | 用户 |
| 事件日志 | `~/.local/state/fsmon/history.log` | daemon (root) | 644 |
| Unix socket | `/tmp/fsmon-<UID>.sock` | daemon (root) | 666 |

### 权限分离

| 命令 | 需要 sudo | 原因 |
|---|---|---|
| `fsmon daemon` | ✅ | fanotify 必须 root |
| `fsmon add /tmp` | ❌ | 写用户 config + socket |
| `fsmon remove /tmp` | ❌ | 同上 |
| `fsmon managed` | ❌ | 读用户 config + socket |
| `fsmon query --since 1h` | ❌ | 读日志文件（644） |
| `fsmon clean --keep-days 7` | ❌ | 写日志文件 |

### daemon 启动流

1. 检查 root（fanotify 要求）
2. 获取原始用户 UID（SUDO_UID → getpwuid_r）
3. 根据 UID 构建 config/log/socket 路径
4. 读 `~/.config/fsmon/config.toml` 获取监控路径
5. fanotify_init → fanotify_mark(paths) → bind socket (0666) → 主循环

## 实现计划

### P0 — config.rs 重构 ✅
- [x] 新增 `resolve_uid()` / `resolve_home()` / `guess_home()` 函数
- [x] `default_log_file()` → `~/.local/state/fsmon/history.log`
- [x] `default_socket_path()` → `/tmp/fsmon-<UID>.sock`
- [x] 删除 `migrate_from_etc()`（不再需要）
- [x] 测试覆盖

### P1 — bin/fsmon.rs 改动 ✅
- [x] `cmd_daemon` 使用新路径 + socket 0666 权限
- [x] 删除 install / uninstall 子命令
- [x] 所有 CLI 用 `UserConfig::default_socket/log` 解析路径

### P2 — systemd.rs 清理 ✅
- [x] install/uninstall 改为 bail 提示用户不再使用 systemd
- [x] 保留模块不报错

### P3 — 帮助文档 ✅
- [x] help.rs 更新所有文本
- [x] 删除 install/uninstall 相关帮助

### P4 — 测试通过 + 清理 ✅
- [x] cargo test 全部通过（75 passed, 7 ignored）
- [x] cargo clippy clean
- [x] cargo fmt clean
