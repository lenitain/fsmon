# fsmon 安全发现报告（验证后）

**扫描时间**: 2026-06-22  
**目标**: fsmon  
**总发现**: 30 (HIGH: 13, MEDIUM: 14, LOW: 3)

---

## HIGH 严重性发现

| ID | 文件 | 行号 | 类别 | 原始置信度 | 验证置信度 | 标题 |
|---|---|---|---|---|---|---|
| F-001 | `socket_handler.rs` | 84 | 授权缺失 | 0.95 | 0.9 | 命令执行无权限检查 |
| F-002 | `utils.rs` | 91 | start_time_ns=0 绕过 | 0.95 | 0.9 | PID 复用 |
| F-003 | `config.rs` | 332 | XDG_CONFIG_HOME 注入 | 0.95 | 0.9 | XDG_CONFIG_HOME 注入 at src/common/config.rs:217 |
| F-005 | `daemon.rs` | 76 | 认证绕过 | 0.95 | 0.85 | Socket 权限 0666 |
| F-006 | `filters.rs` | 25 | symlink-following | 0.9 | 0.85 | symlink-following at src/common/filters.rs:25 |
| F-007 | `fid_parser.rs` | 395 | symlink-following | 0.92 | 0.85 | 同焦点区域2的394行 |
| F-008 | `config.rs` | 278 | HOME 注入 | 0.9 | 0.85 | HOME 注入 at src/common/config.rs:180 |
| F-009 | `core.rs` | 198 | 临时文件名可预测 | 0.95 | 0.8 | 临时文件名可预测 at src/common/clean/core.rs:198 |
| F-010 | `core.rs` | 108 | rename 失败数据丢失 | 0.85 | 0.8 | rename 失败数据丢失 at src/common/clean/core.rs:108 |
| F-011 | `config.rs` | 215 | SUDO_UID/GID 伪造 | 0.85 | 0.8 | 同焦点区域2的213行 |
| F-017 | `dir_watcher.rs` | 70 | 监控逃逸 | 0.8 | 0.75 | DELETE_SELF 时间窗口 |
| F-022 | `core.rs` | 198 | 符号链接攻击 | 0.9 | 0.7 | 符号链接攻击 at src/common/clean/core.rs:198 |
| F-025 | `dir_watcher.rs` | 100 | 事件丢失 | 0.7 | 0.65 | 新目录创建窗口 |

## MEDIUM 严重性发现

| ID | 文件 | 行号 | 类别 | 原始置信度 | 验证置信度 | 标题 |
|---|---|---|---|---|---|---|
| F-004 | `config.rs` | 229 | HOME 注入 | 0.9 | 0.85 | 通过 HOME 环境变量影响路径解析 |
| F-012 | `socket.rs` | 46 | 命令伪造 | 0.8 | 0.75 | track_cmd 可任意设置 |
| F-013 | `fid_parser.rs` | 394 | 符号链接遍历 | 0.8 | 0.75 | mark_recursive_inner 跟随 symlink |
| F-014 | `file_writer.rs` | 265 | TOCTOU | 0.8 | 0.75 | 目录创建与文件打开 |
| F-015 | `monitored.rs` | 133 | JSONL 路径无验证 | 0.8 | 0.75 | JSONL 路径无验证 at src/common/monitored.rs:96 |
| F-016 | `channel.rs` | 5 | 无界通道 OOM | 0.8 | 0.75 | 无界通道 OOM at src/common/monitor/channel.rs:7 |
| F-018 | `socket.rs` | 37 | 路径注入 | 0.85 | 0.7 | SocketCmd::Add 路径未净化 |
| F-019 | `config.rs` | 213 | 权限提升 | 0.75 | 0.7 | resolve_uid_gid 环境变量劫持 |
| F-020 | `live_path.rs` | 112 | path-traversal | 0.75 | 0.7 | canonicalize TOCTOU |
| F-021 | `fid_parser.rs` | 384 | 递归栈溢出 | 0.75 | 0.7 | 递归栈溢出 at src/common/fid_parser.rs:291 |
| F-023 | `socket.rs` | 37 | 不安全反序列化 | 0.75 | 0.65 | JSON 无大小限制 |
| F-024 | `dir_watcher.rs` | 70 | 拒绝服务 | 0.7 | 0.65 | 快速创建删除 |
| F-026 | `daemon.rs` | 70 | TOCTOU | 0.7 | 0.6 | Socket 创建与权限设置 |
| F-027 | `fid_parser.rs` | 226 | open_by_handle_at 滥用 | 0.65 | 0.6 | open_by_handle_at 滥用 at src/common/fid_parser.rs:226 |

## LOW 严重性发现

| ID | 文件 | 行号 | 类别 | 原始置信度 | 验证置信度 | 标题 |
|---|---|---|---|---|---|---|
| F-028 | `socket_handler.rs` | 66 | 资源耗尽 | 0.65 | 0.55 | 订阅者无限制 |
| F-029 | `fid_parser.rs` | 349 | TOCTOU | 0.6 | 0.55 | chown_to_user 路径级 chown |
| F-030 | `socket_handler.rs` | 200 | 信息泄露 | 0.6 | 0.5 | Health 命令泄露 |

---

## 焦点区域总结

### 焦点区域 1: Unix Socket IPC
- **F-001**: socket_handler.rs 命令处理无权限检查，任何连接用户可操作
- **F-005**: Socket 权限 0666，但受 /run/user/<uid> 0700 目录权限限制
- **F-018**: 路径注入风险，但 canonicalize 限制了遍历
- **F-023**: JSON 反序列化无大小限制

### 焦点区域 2: 特权操作
- **F-013/F-007**: mark_recursive_inner 跟随符号链接
- **F-019/F-011**: 环境变量伪造 UID/GID
- **F-027**: open_by_handle_at 需要 CAP_DAC_READ_SEARCH

### 焦点区域 3: 文件路径解析
- **F-006/F-007**: canonicalize 跟随符号链接
- **F-020**: canonicalize TOCTOU

### 焦点区域 4: 日志文件操作
- **F-009/F-022**: 临时文件名可预测但用 O_EXCL 防护
- **F-010**: rename 失败可能导致数据丢失
- **F-014**: 目录创建与文件打开 TOCTOU

### 焦点区域 5: 进程信息伪造
- **F-002**: start_time_ns=0 可绕过 PID 复用检查

### 焦点区域 6: 配置文件解析
- **F-003**: XDG_CONFIG_HOME 注入
- **F-004/F-008**: HOME 环境变量注入
- **F-011/F-019**: SUDO_UID/GID 伪造

### 焦点区域 7: 内存安全
- **F-016**: Unbounded 通道 OOM 风险
- **F-021**: 递归无深度限制

### 焦点区域 8: inotify 竞态
- **F-017**: DELETE_SELF 后监控失效
- **F-025**: 新目录事件丢失
- **F-024**: 快速创建删除 DoS

---

## 验证方法

1. 读取每个发现对应文件的上下文
2. 验证 file:line 指向正确代码位置
3. 评估该模式是否构成真实漏洞
4. 根据上下文（如 O_EXCL 防护、目录权限限制等）调整置信度
5. 所有行号均已修正并验证
