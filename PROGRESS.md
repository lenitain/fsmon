# fsmon 重构：进程优先的追踪系统

## 现状问题

当前设计以**路径**为锚点，进程只是附属过滤条件：

```bash
fsmon add /home                    # 路径优先
fsmon add --exclude-cmd apt /home  # 再从路径事件里排除某些进程
```

**根因**：fanotify 按路径标记 → 用户空间收到所有事件 → 再过滤掉不要的。这导致：
- 即使只关心一个进程，也要处理整个文件系统的噪音
- `--exclude-cmd` 在事件**到达用户空间后**才过滤，不省底层开销
- chain 构建（5× /proc 读）对每条事件执行，不匹配的事件也在浪费

## 新架构：进程优先

```bash
fsmon add openclaw                # 进程优先：追踪 openclaw 的所有文件活动
fsmon add openclaw --path /workspace  # 再限制路径范围
fsmon add openclaw --path /data
```

### 核心变化

| 维度 | 当前（路径优先） | 新（进程优先） |
|------|----------------|----------------|
| **配置锚点** | `PathEntry`（路径是唯一标识） | `ProcessEntry`（进程名是唯一标识） |
| **过滤方向** | "排除不要的" | "只包含要的" |
| **内核过滤** | 无（全量事件到用户空间） | 无（fanotify 无法按进程过滤） |
| **用户空间过滤** | 先匹配路径，再 `exclude_cmd` | **先匹配进程**，不匹配直接丢弃，不读 /proc |
| **子进程继承** | 无（只查 cmd，不查树） | 有（Fork/Exec 事件维护进程树） |
| **chain** | 对每条事件构建 | 只对匹配的进程构建 |

### ProcessEntry 数据结构

```rust
pub struct ProcessEntry {
    /// 进程名（唯一的标识符，如 "openclaw"）
    pub name: String,
    /// 允许的文件路径列表（不指定 = 所有路径）
    pub paths: Option<Vec<PathBuf>>,
    /// 排除的路径列表（优先级高于 paths）
    pub exclude_paths: Option<Vec<String>>,
    /// 事件类型过滤
    pub types: Option<Vec<String>>,
    /// 大小过滤
    pub size: Option<String>,
    /// 递归子进程（fork 出来的子进程自动算入）
    pub recursive: Option<bool>,
}
```

### 存储（ProcessStore）

```rust
/// 进程监控配置，取代 Managed/PathEntry
pub struct ProcessStore {
    pub entries: Vec<ProcessEntry>,
}
```

## 实现路径

### Phase 1：进程树缓存（核心基础设施）

**目标**：在 daemon 启动后维护一个准确的 `pid → {cmd, ppid, user}` 进程树，
能支撑两个需求：
1. **快速判断"这个 PID 是不是我关心的进程或其后代"**
2. **在事件发生时构建 chain**

**实现**：

```
proc connector 订阅 Fork/Exec/Exit 事件：

FORK parent=100, child=101 → 缓存 {101, cmd:null, ppid:100}
EXEC pid=101 → 读 /proc/101/comm + status → 更新 {101, cmd:sh, ppid:100, user:root}
EXIT pid=101 → 可选：标记 exited=true，或延迟删除

启动时一次快照（补齐已存在的进程）：
  → 读 /proc/*/stat 一次（O(n) n=进程数，~25ms/500进程）
  → 补齐 pid_cache
```

**变更文件**：
- `src/proc_cache.rs` → 重写，处理 Fork/Exec/Exit，提供 `pid_is_descendant_of(pid, cmd)` 和 `build_chain(pid)` 接口
- `src/monitor.rs` → 集成 proc connector 的 Fork/Exit 处理

### Phase 2：进程优先过滤器

**目标**：在 `should_output` 的事件处理路径中，**先于 /proc 读取**判断事件是否被任何 `ProcessEntry` 监控。

**实现**：

```
fanotify 事件 → pid=102
  → 查进程树: pid=102 是否属于 openclaw 的子树？
    → 否 → 直接丢弃（零 /proc 读）
    → 是 → 检查 path 是否在允许列表中？
      → 否 → 丢弃
      → 是 → 构建 chain → 写入日志
```

**数据类型**：`DashMap<String, Vec<ProcessEntry>>`（cmd → entry 列表，快速判断）

**变更文件**：
- `src/filters.rs` → 新增 `ProcessEntry` 和进程匹配逻辑
- `src/managed.rs` → 改为 `ProcessStore`，或在旁边并行
- 保留 `PathOptions`/`PathEntry` 兼容（旧配置自动迁移）

### Phase 3：CLI 重构

```bash
fsmon add openclaw                # 新格式
fsmon add openclaw --path /workspace --path /data
fsmon add openclaw --exclude '*.tmp'
fsmon remove openclaw
fsmon list                        # 显示进程列表而非路径列表
```

**变更文件**：
- `src/bin/fsmon.rs` → 修改 `AddArgs`、`Commands`
- `src/bin/commands/add.rs` → 新逻辑
- `src/bin/commands/remove.rs` → 新逻辑
- `src/bin/commands/mod.rs` → `parse_process_entries`

### Phase 4：日志格式升级

```json
// 新 FileEvent（向后兼容旧格式，解析时 chain 不存在则 = ""）
{
  "time": "...",
  "event_type": "CREATE",
  "path": "/workspace/foo.txt",
  "pid": 102,
  "cmd": "touch",
  "user": "root",
  "file_size": 0,
  "chain": "102|touch|root;101|sh|root;100|openclaw|root;1|systemd|root"
}
```

## 向后兼容策略

1. 旧 `PathEntry` 配置自动转换为 `ProcessEntry`（反向不保证）
2. 旧日志格式（无 chain）正常读取，`chain=""`
3. `--exclude-cmd` 过渡期保留，底层映射为进程排除

## 权衡

### 进程树缓存的开销

| 操作 | 代价 | 频率 |
|------|------|------|
| 启动快照（读 `/proc/*/stat`） | ~25ms（500 进程） | 1 次/daemon 启动 |
| Fork 事件处理 | DashMap insert，~200ns | 每次 fork |
| Exec 事件处理 | DashMap update + 读 /proc，~50µs | 每次 exec |
| Exit 事件处理 | DashMap 标记，~200ns | 每次 exit |
| **PID 匹配查询** | DashMap get，~200ns | **每条 fanotify 事件** |

相比当前方案（每条事件构建 chain = 250µs），进程优先模式下**不匹配的事件**从 250µs 降到 200ns，**匹配的事件**从 250µs 降到 250µs + 200ns（不变）。

### 内存

500 个常驻进程 × ~120 字节 = ~60KB。短期进程（编译 job）会累积 Entry，但 DashMap 不清理——Exited 标记的节点可定期清理。

## 实施顺序

1. **Phase 1**：进程树缓存（proc_cache 重写 + Fork/Exec/Exit 处理 + 启动快照）
2. **Phase 1.5**：`build_chain` 从 /proc 改为从进程树缓存读取（消除 /proc 读）
3. **Phase 2**：进程优先过滤器（ProcessEntry + 先查 PID 树再 chain）
4. **Phase 3**：CLI 接口
5. **Phase 4**：存储模型（ProcessStore + 旧配置迁移）
6. 测试 + 文档
