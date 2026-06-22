# 安全扫描报告

**目标**: /home/pilot/.projects/fsmon  
**时间**: 2026-06-22  
**焦点区域**: Unix Socket IPC 认证与注入、特权操作与 fanotify 系统调用、文件路径解析与符号链接、日志文件操作与临时文件竞态、进程信息伪造与 PID 复用、配置文件解析与路径注入、内存安全与缓存溢出、inotify 监控窗口与竞态条件

---

## 摘要

| 严重性 | 数量 |
|--------|------|
| HIGH   | 13   |
| MEDIUM | 15   |
| LOW    | 3    |
| **合计** | **31** |

---

## 发现详情

### [F-001] Socket 权限设置为 0666 允许任意用户连接 (HIGH, confidence: 0.90)

**文件**: src/bin/fsmon/commands/daemon.rs:76  
**类别**: 认证绕过

**描述**: daemon.rs 第 76 行将 socket 权限设置为 0666，允许系统上任何用户连接到 daemon socket

**攻击场景**: 任意本地用户可通过连接 socket 发送 Add/Remove/Subscribe 命令控制 daemon

**修复建议**: 使用 SO_PEERCRED 验证连接者 UID/GID，或将权限设为 0600

---

### [F-002] 命令执行无权限检查 (HIGH, confidence: 0.90)

**文件**: src/common/monitor/socket_handler.rs:110  
**类别**: 授权缺失

**描述**: handle_socket_cmd() 直接执行所有命令，无基于发送者身份的权限检查

**攻击场景**: 普通用户可发送 Add 命令监控 /etc/shadow 或其他敏感文件

**修复建议**: 使用 SO_PEERCRED 获取连接者 UID，根据 UID 决定允许的操作

---

### [F-003] start_time_ns=0 绕过导致返回伪造进程信息 (HIGH, confidence: 0.90)

**文件**: src/common/utils.rs:91  
**类别**: PID 复用绕过

**描述**: 当 cached_start == current_start || current_start == 0 时信任过时缓存

**攻击场景**: Fork 事件缓存 start_time=0 → 进程退出 → PID 复用 → 返回伪造信息

**修复建议**: 移除 current_start == 0 条件，或在 start_time=0 时标记为不可信

---

### [F-007] 路径过滤器跟随符号链接 (HIGH, confidence: 0.90)

**文件**: src/common/filters.rs:25  
**类别**: 符号链接跟随

**描述**: 路径规范化使用 canonicalize() 跟随符号链接

**攻击场景**: 通过符号链接使监控指向敏感目录

**修复建议**: 使用 symlink_metadata() 检查是否为符号链接

---

### [F-009] 临时文件名可预测，存在符号链接攻击风险 (HIGH, confidence: 0.90)

**文件**: src/common/clean/core.rs:198  
**类别**: 临时文件竞态

**描述**: truncate_from_start 使用 PID 生成临时文件名 .fsmon_trunc_\<PID\>

**攻击场景**: 预测文件名并创建 symlink -> /etc/passwd，rename 时覆盖目标文件

**修复建议**: 使用不可预测的随机文件名（如 tempfile crate）

---

### [F-004] XDG_CONFIG_HOME 环境变量注入导致恶意配置加载 (HIGH, confidence: 0.90)

**文件**: src/common/config.rs:332  
**类别**: 环境变量注入

**描述**: Config::user_path() 使用 XDG_CONFIG_HOME 环境变量确定配置文件路径

**攻击场景**: 设置 XDG_CONFIG_HOME=/tmp/evil，加载被篡改的 fsmon.toml

**修复建议**: 以 root 运行时忽略该环境变量，或验证路径合法性

---

### [F-005] HOME 环境变量注入导致路径解析错误 (HIGH, confidence: 0.85)

**文件**: src/common/config.rs:278  
**类别**: 环境变量注入

**描述**: guess_home() 在 SUDO_UID 未设置时回退到 HOME 环境变量

**攻击场景**: 设置 HOME=/tmp/evil-user，加载攻击者控制的配置文件

**修复建议**: 使用 getpwuid() 系统调用获取真实用户主目录

---

### [F-006] SUDO_UID/SUDO_GID 环境变量可被伪造 (HIGH, confidence: 0.85)

**文件**: src/common/config.rs:215  
**类别**: 权限提升

**描述**: resolve_uid_gid() 依赖 SUDO_UID 和 SUDO_GID 环境变量

**攻击场景**: 设置 SUDO_UID=0 伪装成 root，导致权限提升

**修复建议**: 验证 SUDO_UID 对应的用户是否实际存在

---

### [F-008] mark_recursive_inner 以 root 权限跟随符号链接遍历目录 (HIGH, confidence: 0.85)

**文件**: src/common/fid_parser.rs:394  
**类别**: 符号链接遍历

**描述**: is_dir() 通过 stat 而非 lstat 判断类型，默认跟随符号链接

**攻击场景**: 在监控目录下创建 symlink 指向 /etc，扩展监控到系统敏感目录

**修复建议**: 使用 entry.file_type()?.is_symlink() 跳过符号链接

---

### [F-010] 未检查目标文件是否为符号链接 (HIGH, confidence: 0.85)

**文件**: src/common/clean/core.rs:198  
**类别**: 符号链接攻击

**描述**: truncate_from_start 直接对 path 执行操作，未检查是否为符号链接

**攻击场景**: 将日志文件替换为指向 /etc/passwd 的 symlink，截断时破坏系统文件

**修复建议**: 使用 lstat 检查文件是否为符号链接

---

### [F-019] 配置文件路径字段无验证 (MEDIUM, confidence: 0.85)

**文件**: src/common/config.rs:229  
**类别**: 路径注入

**描述**: Config::load() 直接从 TOML 读取路径字段，无验证

**攻击场景**: 配置路径指向 /etc/shadow，导致读写敏感文件

**修复建议**: 实现路径白名单验证

---

### [F-011] rename 操作失败时原始日志文件可能已损坏 (HIGH, confidence: 0.80)

**文件**: src/common/clean/core.rs:108  
**类别**: 数据丢失

**描述**: clean_single_log 先写入临时文件再 rename，rename 失败时数据可能丢失

**攻击场景**: 跨文件系统操作时 rename 失败，原始文件和临时文件都丢失

**修复建议**: rename 前备份原始文件，失败时恢复

---

### [F-012] DELETE_SELF 到 fanotify 标记建立的时间窗口可绕过监控 (HIGH, confidence: 0.80)

**文件**: src/common/monitor/dir_watcher.rs:118  
**类别**: 监控逃逸

**描述**: DELETE_SELF 事件处理和 fanotify 临时标记生效之间存在时间窗口

**攻击场景**: 在窗口内创建同名目录并执行操作，绕过监控

**修复建议**: 立即添加 fanotify 临时标记，然后再调用 remove_path

---

### [F-014] SocketCmd::Add 路径字段未净化 (MEDIUM, confidence: 0.80)

**文件**: src/common/socket.rs:37  
**类别**: 路径注入

**描述**: path 字段直接使用 PathBuf，无路径净化或规范化

**攻击场景**: 提交包含 ../ 或符号链接的路径，监控意外目录

**修复建议**: 对路径进行规范化并验证在允许范围内

---

### [F-018] 目录创建与文件打开之间存在符号链接攻击风险 (MEDIUM, confidence: 0.75)

**文件**: src/common/monitor/file_writer.rs:265  
**类别**: TOCTOU 竞态

**描述**: create_dir_all 成功后、open 文件前，可创建符号链接

**攻击场景**: 创建 symlink 指向 /etc/shadow，程序向敏感文件追加数据

**修复建议**: 使用 O_NOFOLLOW 标志打开文件

---

### [F-015] track_cmd 字段可被任意设置 (MEDIUM, confidence: 0.75)

**文件**: src/common/socket.rs:46  
**类别**: 命令伪造

**描述**: Subscribe 的 track_cmd 可被任意设置，用于过滤或操纵事件流

**攻击场景**: 设置为管理员命令组名称窃取事件，或 CMD_GLOBAL 获取所有事件

**修复建议**: 验证 track_cmd 值是否是有效的命令组名称

---

### [F-022] Unbounded 通道无背压，可导致内存耗尽 (MEDIUM, confidence: 0.75)

**文件**: src/common/monitor/channel.rs:5  
**类别**: 资源耗尽

**描述**: EventSender::Unbounded 使用无界通道，无容量上限

**攻击场景**: 触发大规模文件系统事件风暴，内存耗尽被 OOM killer 终止

**修复建议**: 强制使用有界通道并设置合理容量

---

### [F-023] 文件存在性检查与重命名之间存在竞态窗口 (MEDIUM, confidence: 0.75)

**文件**: src/common/clean/core.rs:33  
**类别**: TOCTOU 竞态

**描述**: 检查 log_file.exists() 后执行 rename，时间窗口内文件可能被删除

**攻击场景**: 检查通过后文件被删除，rename 失败或创建新文件

**修复建议**: 使用文件锁或原子性操作

---

### [F-020] JSONL 路径字段无验证 (MEDIUM, confidence: 0.75)

**文件**: src/common/monitored.rs:133  
**类别**: JSONL 注入

**描述**: Monitored::load() 解析 JSONL 时路径字段无验证

**攻击场景**: 注入指向敏感目录的路径

**修复建议**: 加载后验证所有路径

---

### [F-013] 新目录创建后 fanotify 标记建立前的事件丢失窗口 (HIGH, confidence: 0.70)

**文件**: src/common/monitor/dir_watcher.rs:178  
**类别**: 事件丢失

**描述**: on_new_subdirectory 中 fanotify 标记建立前可能已有文件事件发生

**攻击场景**: 快速创建目录并立即写入文件，操作可能不被捕获

**修复建议**: 减少标记建立时间窗口，使用 FAN_MARK_FLUSH 确保立即生效

---

### [F-017] canonicalize() 与后续操作之间的 TOCTOU 竞态 (MEDIUM, confidence: 0.70)

**文件**: 
- src/common/monitor/live_path.rs:112
- src/common/monitor/temp_marks.rs:23
- src/common/monitor/temp_marks.rs:140
- src/common/monitor/dir_watcher.rs:206

**类别**: TOCTOU 竞态

**描述**: 多处代码先 canonicalize() 获取规范路径，再用该路径调用 mark_directory()、metadata() 或 fanotify_mark()。攻击者可在 canonicalize 和后续操作之间替换符号链接目标，导致操作作用于非预期路径。

**攻击场景**: 
1. 监控路径 `/tmp/link`（指向 `/safe`）
2. canonicalize() 解析为 `/safe`
3. 攻击者将 `/tmp/link` 改为指向 `/etc`
4. 后续 mark_directory() 作用于 `/safe`（已解析），但实际监控可能被绕过或指向错误位置

**修复建议**: 
- 使用 open(O_DIRECTORY|O_NOFOLLOW) 获取文件描述符，后续通过 fd 操作
- 或使用 `std::fs::OpenOptions` 的 `custom_flags(libc::O_NOFOLLOW)` 打开目录
- 对于已存在的路径，立即获取 fd 并基于 fd 进行 metadata 和 mark 操作

---


### [F-021] 递归目录遍历无深度限制 (MEDIUM, confidence: 0.70)

**文件**: src/common/fid_parser.rs:384  
**类别**: 递归栈溢出

**描述**: mark_recursive_inner 使用递归遍历，无最大深度限制

**攻击场景**: 创建深度嵌套结构触发栈溢出崩溃

**修复建议**: 改用迭代方式或添加最大递归深度限制

---

### [F-025] cmd 字段无长度和字符验证 (MEDIUM, confidence: 0.70)

**文件**: src/common/monitored.rs:44  
**类别**: JSONL 注入

**描述**: CmdGroup 的 cmd 字段直接反序列化，无长度限制或字符过滤

**攻击场景**: 插入超长字符串消耗内存，或特殊字符导致日志注入

**修复建议**: 限制 cmd 字段长度并过滤特殊字符

---

### [F-026] 临时文件权限过宽 (MEDIUM, confidence: 0.70)

**文件**: src/common/clean/core.rs:198  
**类别**: 临时文件权限

**描述**: fs::File::create_new 使用默认 umask 权限创建临时文件

**攻击场景**: umask 为 0000 时权限为 0666，其他用户可读取日志内容

**修复建议**: 使用 OpenOptions 显式设置权限 0600

---

### [F-027] monitored.jsonl 路径可被符号链接替换 (MEDIUM, confidence: 0.70)

**文件**: src/common/monitored.rs:60  
**类别**: 符号链接攻击

**描述**: JSONL 文件路径未检查是否为符号链接

**攻击场景**: 创建 symlink ~/.local/share/fsmon -> /etc，读写系统配置

**修复建议**: 使用 canonicalize() 解析符号链接并验证路径

---

### [F-016] mount_fd 使用 O_DIRECTORY 权限过大 (MEDIUM, confidence: 0.65)

**文件**: src/common/fid_parser.rs:226  
**类别**: open_by_handle_at 滥用

**描述**: open_dir() 使用 O_DIRECTORY 但未使用 O_PATH，权限过大

**攻击场景**: 如果 mount_fd 被获取，可用于 getdents 枚举目录内容

**修复建议**: 改用 O_DIRECTORY | O_PATH

---

### [F-024] 快速创建/删除目录导致 pending_paths 膨胀 (MEDIUM, confidence: 0.65)

**文件**: src/common/monitor/dir_watcher.rs:178  
**类别**: 拒绝服务

**描述**: 快速创建删除目录导致 pending_paths 和 temp_parent_marks 列表增长

**攻击场景**: 循环创建删除目录，列表无限增长耗尽内存

**修复建议**: 设置大小限制，定期清理过期标记

---

### [F-028] 配置文件位置可预测且无完整性检查 (MEDIUM, confidence: 0.65)

**文件**: src/common/config.rs:278  
**类别**: 配置伪造

**描述**: 配置文件路径 ~/.config/fsmon/fsmon.toml 是可预测的

**攻击场景**: 提前创建或篡改配置文件，注入恶意配置

**修复建议**: 检查配置文件权限和所有者

---

### [F-029] Health 命令泄露内部实现细节 (LOW, confidence: 0.55)

**文件**: src/common/monitor/socket_handler.rs:198  
**类别**: 信息泄露

**描述**: health() 返回详细内部状态，包括文件描述符号、重启次数等

**攻击场景**: 获取 daemon 内部状态用于侦察

**修复建议**: 限制 Health 命令返回的信息

---

### [F-030] pending_paths 可能包含重复条目 (LOW, confidence: 0.50)

**文件**: src/common/monitor/dir_watcher.rs:75  
**类别**: 状态不一致

**描述**: 目录删除时添加到 pending_paths 但未检查是否已存在

**攻击场景**: 多次删除创建同一目录导致重复条目

**修复建议**: 添加前检查是否已存在

---

### [F-031] strip_deleted_suffix 全局替换可能误删合法路径 (LOW, confidence: 0.50)

**文件**: src/common/fid_parser.rs:105  
**类别**: 路径混淆

**描述**: 使用 replace(" (deleted)", "") 全局替换，可能误删合法文件名

**攻击场景**: 创建名为 "dir (deleted)" 的目录，路径被错误截断

**修复建议**: 仅替换路径末尾的 " (deleted)"

---

## 关键风险总结

### 最严重的问题（需优先修复）

1. **Socket 认证绕过** (F-001, F-002): daemon socket 权限为 0666 且无命令执行权限检查，任何本地用户可完全控制 daemon
2. **符号链接攻击链** (F-007, F-008, F-009, F-010): 多处代码跟随符号链接，组合可实现任意文件读写
3. **环境变量注入** (F-004, F-005, F-006): 依赖可被篡改的环境变量进行配置加载和权限判断
4. **PID 复用绕过** (F-003): 可返回伪造进程信息

### 攻击面分类

| 类别 | 数量 | 典型发现 |
|------|------|----------|
| 认证/授权缺陷 | 2 | F-001, F-002 |
| 符号链接攻击 | 6 | F-007, F-008, F-009, F-010, F-018, F-027 |
| 环境变量注入 | 3 | F-004, F-005, F-006 |
| TOCTOU 竞态 | 4 | F-017, F-018, F-023, F-012 |
| 路径注入 | 4 | F-014, F-019, F-020, F-031 |
| 资源耗尽/DoS | 3 | F-021, F-022, F-024 |
| 信息泄露 | 1 | F-029 |
