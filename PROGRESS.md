# fsmon 重构

## 总目标

最小改动，实现进程树感知。

## 改动清单

### 1. `--exclude-cmd` 重命名为 `--cmd`

```bash
# 旧语法（废弃）
fsmon add /home --exclude-cmd '!openclaw'
fsmon add /home --exclude-cmd rsync

# 新语法
fsmon add /home --cmd openclaw       # positive: 只追踪 openclaw 及其子树
fsmon add /home --cmd '!openclaw'    # inverted: 排除 openclaw（等价于旧 --exclude-cmd）
```

### 2. FileEvent 加 ppid, tgid（始终记录）

```rust
// 无 --cmd 或 --cmd '!'：只记录这两项
pub struct FileEvent {
    // ... 已有字段
    pub ppid: u32,    // 新增，来自 /proc/{pid}/status
    pub tgid: u32,    // 新增，来自 /proc/{pid}/status
}
```

来源：`/proc/{pid}/status` 的 `PPid:` 和 `Tgid:` 行（已打开的文件，多解析两行）。

旧日志反序列化时 ppid=0, tgid=0，向后兼容。

### 3. `--cmd openclaw`（positive）启用进程树

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

### 4. `--cmd '!openclaw'`（inverted）只查一层

等价于当前的 `--exclude-cmd openclaw`（默认排除语义）。不启用进程树，不构建 chain，只匹配 `event.cmd`。

**但** `ppid` 和 `tgid` 仍然记录。

## 判定逻辑汇总

| `--cmd` 参数 | 进程树 | chain | ppid/tgid | 匹配规则 |
|-------------|--------|-------|-----------|---------|
| 未指定 | ❌ | ❌ | ✅ | cmd 匹配不做筛选 |
| `openclaw`（positive） | ✅ | ✅ | ✅ | pid 在 openclaw 子树中（等价旧 `--exclude-cmd '!openclaw'` + 进程树） |
| `'!openclaw'`（inverted） | ❌ | ❌ | ✅ | cmd 不匹配 openclaw（等价旧 `--exclude-cmd openclaw`） |


## 文件改动

| 文件 | 改动 |
|------|------|
| `src/lib.rs` | FileEvent +ppid, +tgid |
| `src/proc_cache.rs` | 处理 Fork/Exec/Exit，提供 `is_descendant(pid, cmd)` + `build_chain(pid)` |
| `src/monitor.rs` | 集成 proc connector Fork/Exit；build_file_event 中查进程树 |
| `src/filters.rs` | PathOptions 增加 `cmd_positive: bool` 标识，`should_output` 查进程树 |
| `src/managed.rs` | PathEntry 增加 `cmd_positive` 字段 |
| `src/socket.rs` | SocketCmd 调整 |
| `src/bin/fsmon.rs` | `--exclude-cmd` → `--cmd` |
| `src/bin/commands/` | 传递新参数 |

## 实施顺序

1. FileEvent +ppid +tgid（lib.rs + proc_cache 读取逻辑）
2. `--exclude-cmd` → `--cmd` 重命名（CLI + 配置 + 测试）
3. 进程树缓存（proc connector Fork/Exec/Exit + 启动快照）
4. `build_chain` + `is_descendant` 接口
5. 事件处理路径集成（positive 模式用进程树，inverted 模式保持原逻辑）

## 非目标

❌ 不创建 ProcessEntry / ProcessStore 新类型
❌ 不改监控路径的数据模型
❌ 不破坏现有配置格式
