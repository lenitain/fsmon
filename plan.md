# fsmon 自愈体系完善计划

## 已完成

- [x] **Reader task 崩溃恢复** — 死亡通知 + 自动重启 + 退避（3次/60s）
- [x] **PID 回收去重** — `start_time_ns` 校验，已在 `get_process_info_by_pid` 中实现
- [x] **日志标签规范** — `[debug]` → `[DEBUG]`，与 `[ERROR]`/`[WARNING]`/`[INFO]` 统一
- [x] **SIGTERM 排空 event channel** — 关闭时 `try_recv` 耗尽 channel 中已排队事件后再退出
- [x] **Bounded channel + 事件降级** — 默认 unbounded，可选 `--channel-capacity N` / `cache.channel_capacity` 启用 bounded，reader task 满时自然阻塞，fanotify overflow 兜底
- [x] **`health` socket 命令** — `fsmon health` 通过 Unix socket 获取 daemon 健康状态（uptime、reader 存活/重启数、channel type、路径数）
- [x] **磁盘空间预检 + 运行时缓冲** — 可配置阈值（CLI/TOML），启动时 `statvfs` 检查，写入失败自动切内存缓冲（最多 10K 条），每 10s 重试刷盘。运行时目录被删自动重建 + chown

---

## systemd 集成（P2-4）

### 现状

fsmon 当前有两种启动方式：

```bash
# 前台运行（Ctrl+C 退出）
sudo fsmon daemon

# 后台运行
sudo fsmon daemon &
```

没有 `.service` 文件，没有 pidfile，没有 systemd 通知。进程崩溃 = 事件永久丢失，需人工发现并重启。

### 设计方案

分三个子项，独立可做：

```
① fsmon.service 文件          → 让 systemctl 可用
② sd_notify 心跳 + feature     → 崩溃自动重启 + 死锁检测
③ 路径适配 + README 文档       → 常规用户配置可用
```

---

### ① `.service` 文件（无代码改动）

新建 `fsmon.service`：

```ini
[Unit]
Description=fsmon - File System Change Monitor
Documentation=man:fsmon(1)
After=local-fs.target

[Service]
# Type=notify + sd_notify(READY=1) 确保 systemd 直到初始化完成后才认为服务就绪
Type=notify

# 核心命令
ExecStart=/usr/local/bin/fsmon daemon

# 进程级自愈 — 崩溃后 5s 自动重启
Restart=always
RestartSec=5

# watchdog — 主循环死锁 >30s 则 kill+restart
WatchdogSec=30

# 将 HOME 指向常规用户目录，才能读到 ~/.config/fsmon/fsmon.toml
# 如果不设，daemon 作为 root 运行会去读 /root/.config/...（找不到就用默认路径）
# 用户根据实际情况修改
Environment=HOME=/home/pilot

# 安全硬化（可选）
ProtectHome=read-only
ProtectSystem=strict
ReadWritePaths=/home/pilot/.local/share/fsmon /home/pilot/.local/state/fsmon /tmp

[Install]
WantedBy=multi-user.target
```

**安装方式**：
```bash
sudo cp scripts/fsmon.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now fsmon
```

**路径问题**：systemd 下 `SUDO_UID` 不存在。`config.rs` 的 `resolve_uid_gid()` / `guess_home()` 不会走 sudo 分支，最终用 `getpwuid(euid)`，euid=0 → root 家目录。
- 解法：service 里 `Environment=HOME=/home/pilot` → config.rs 的 `expand_tilde` 正确展开
- `guess_home()` 检测到 `SUDO_UID` 不存在且 EUID=0 时，已做了 `getpwuid` 回退，不崩

**交互变化**：
```bash
# 旧：sudo fsmon daemon &               # bash 后台
# 新：
sudo systemctl start fsmon              # 启动
sudo systemctl stop fsmon               # 停止 → SIGTERM → drain → exit
sudo systemctl restart fsmon            # 重启
sudo systemctl status fsmon             # 状态（含 sd_notify 消息）
journalctl -u fsmon -f                  # 看日志
```

---

### ② `sd_notify` 心跳（~30 行代码）

**依赖**：`libsystemd` crate，Linux-only，可选。

```toml
# Cargo.toml
[features]
default = []
watchdog = ["dep:libsystemd"]

[dependencies]
libsystemd = { version = "0.7", optional = true }
```

**实现位置**：`monitor.rs` 的 `run()` 方法。

```
主循环 tokio::select! 中新增一个分支：

    _ = watchdog_tick.tick() => {
        #[cfg(feature = "watchdog")]
        if env!("NOTIFY_SOCKET").is_ok() {
            sd_notify(false, "WATCHDOG=1");
        }
    }
```

**三处通知点**：

| 时机 | 消息 | 效果 |
|------|------|------|
| 初始化完成（fanotify + reader 已启动） | `READY=1` | systemd 知道服务就绪了 |
| 主循环每 15s | `WATCHDOG=1` | 防死锁检测 |
| 退出前 | `STOPPING=1` | 通知正在关闭（可选） |

**`watchdog_tick` 的创建**：
```rust
// 在 run() 中，在进入主循环前
let mut watchdog_tick = tokio::time::interval(Duration::from_secs(15));
watchdog_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

即使 `#[cfg(not(feature = "watchdog"))]`，timer 本身的开销可忽略（tokio 空 timer）。

**NOTIFY_SOCKET 检测**：
- systemd `Type=notify` 时会设 `NOTIFY_SOCKET` 环境变量
- 没有 systemd 时（用户直接 `sudo fsmon daemon`），该变量不存在 → `sd_notify` 静默跳过
- 不需要额外 feature gate，运行时检测即可

**READY=1 的发送时机**：
```rust
// 在 run() 中，fanotify 初始化完毕、reader 已 spawn、尚未进入主循环时
#[cfg(feature = "watchdog")]
if std::env::var("NOTIFY_SOCKET").is_ok() {
    let _ = libsystemd::sd_notify(false, "READY=1");
}
```

---

### ③ 路径适配 + README 文档

**当前路径解析逻辑**（`config.rs`）：

```rust
pub fn resolve_uid_gid() -> (u32, u32) {
    // 1. 如果 SUDO_UID 存在（sudo 执行）→ 用 SUDO_UID
    // 2. 否则 → geteuid()
}
```

**systemd 下的行为**：没有 `SUDO_UID`、euid=0（root）。
- `resolve_uid_gid()` → root
- `chown_to_user(dir)` → root（已经是 root，no-op）
- `guess_home()` → `getpwuid(0)` → `/root`
- 路径展开 → `/root/.local/...`
- 普通用户读不到 /root 下的 socket/config/log

**解法**：service 文件 `Environment=HOME=/home/pilot` 即可。`expand_tilde` 会把它展开成 `/home/pilot/.config/...`。不需要改 `config.rs`。

**README 新增一节**：

```markdown
## Systemd 集成（可选）

1. 安装 `--features watchdog` 版本
2. 复制 service 文件，修改 HOME 路径
3. systemctl enable --now

# 然后日常用
systemctl status fsmon    # 看运行状态和 watchdog 心跳
journalctl -u fsmon -f    # 看守护进程日志
```

---

### 执行

```
步骤一：scripts/fsmon.service 文件        ~30 行，零代码
步骤二：Cargo.toml feature + sd_notify    ~30 行
步骤三：更新 README                        ~30 行
```

每步独立可测。
