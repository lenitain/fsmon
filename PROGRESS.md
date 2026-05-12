# fsmon 重构

## 总目标

最小改动，实现进程树感知。

## 改动清单

### 1. `--exclude-cmd` 重命名为 `--cmd`

```bash
# 旧语法（废弃）
fsmon add /home --exclude-cmd '!openclaw'
fsmon add /home --exclude-cmd rsync

# 新语法（只有两种）
fsmon add /home                     # 默认：所有事件，只记 ppid/tgid
fsmon add /home --cmd openclaw      # 包含 openclaw 及其子树，带 chain
```

### 2. FileEvent 加 ppid, tgid（始终记录）

```rust
pub struct FileEvent {
    // ... 已有字段
    pub ppid: u32,    // 新增，来自 /proc/{pid}/status
    pub tgid: u32,    // 新增，来自 /proc/{pid}/status
}
```

来源：`/proc/{pid}/status` 的 `PPid:` 和 `Tgid:` 行（已打开的文件，多解析两行）。

旧日志反序列化时 ppid=0, tgid=0，向后兼容。

### 3. `--cmd openclaw` 启用进程树

```rust
// 只有在 --cmd 为 positive 匹配时才做的事：
//
// a) proc connector 订阅 Fork/Exec/Exit → 维护 pid → {cmd, ppid, user} 树
// b) 事件到来时先查进程树：pid 是否属于 openclaw 的子树？
//    → 否 → 直接丢弃
//    → 是 → 构建 chain，写入日志
//
// chain 格式: "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
```

**没有排除模式**。`--cmd` 只有包含语义，不设 `!` 排除语法。
排除噪音请用 `--exclude`（文件路径）或 `--cmd` 自然排除（不指定 = 不追踪）。

## 判定逻辑汇总

| `--cmd` 参数 | 进程树 | chain | ppid/tgid | 匹配规则 |
|-------------|--------|-------|-----------|---------|
| 未指定 | ❌ | ❌ | ✅ | 所有事件通过，只记 ppid/tgid |
| `openclaw` | ✅ | ✅ | ✅ | pid 在 openclaw 子树中 |


## 文件改动

| 文件 | 改动 |
|------|------|
| `src/lib.rs` | FileEvent +ppid, +tgid |
| `src/proc_cache.rs` | 处理 Fork/Exec/Exit，提供 `is_descendant(pid, cmd)` + `build_chain(pid)` |
| `src/monitor.rs` | 集成 proc connector Fork/Exit；build_file_event 中查进程树 |
| `src/filters.rs` | PathOptions 增加 `cmd: Option<String>`，`should_output` 查进程树 |
| `src/monitored.rs` | PathEntry 增加 `cmd_positive` 字段 |
| `src/socket.rs` | SocketCmd 调整 |
| `src/bin/fsmon.rs` | `--exclude-cmd` → `--cmd` |
| `src/bin/commands/` | 传递新参数 |

## 实施顺序

1. FileEvent +ppid +tgid（lib.rs + proc_cache 读取逻辑）
2. `--exclude-cmd` → `--cmd` 重命名（CLI + 配置 + 测试）
3. 进程树缓存（proc connector Fork/Exec/Exit + 启动快照）
4. `build_chain` + `is_descendant` 接口
5. 事件处理路径集成（`--cmd` 开启动用进程树，无 `--cmd` 保持默认）

## 非目标

❌ 不创建 ProcessEntry / ProcessStore 新类型
❌ 不改监控路径的数据模型
❌ 不破坏现有配置格式
