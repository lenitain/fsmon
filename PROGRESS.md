# fsmon 项目进度

## 缓存策略全景分析与评价

> 分析基于 fsmon v0.3.0，2026-05-13

---

### 一、缓存总览

fsmon 共有 **5 个缓存系统** + **1 个守护进程锁**，分布在 5 个模块中，覆盖路径解析、进程信息、文件大小、用户映射和进程树追踪。

| # | 缓存 | 位置 | 类型 | 容量 | 淘汰策略 |
|---|------|------|------|------|----------|
| 1 | 目录 Handle 缓存 | `dir_cache.rs` / `fid_parser.rs` | `moka::sync::Cache<HandleKey, PathBuf>` | 100,000 | W-TinyLFU + TTL 1h |
| 2 | 进程信息缓存 | `proc_cache.rs` + `monitor.rs` | `Cache<u32, ProcInfo>` | 65,536 | W-TinyLFU + TTL 10min |
| 3 | 进程树缓存 | `proc_cache.rs` | `Cache<u32, PidNode>` | 65,536 | W-TinyLFU + TTL 10min |
| 4 | 文件大小缓存 | `monitor.rs` | `LruCache<PathBuf, u64>` | 10,000 | LRU |
| 5 | ~~PID 信息缓存（第二层）~~ ✅ 已移除 | — | — | — | — |
| 6 | UID→用户名映射 | `utils.rs` | `OnceLock<HashMap<u32, String>>` | 全部 /etc/passwd | 永不刷新（静态单例） |
| 7 | 守护进程锁 | `lib.rs` | `flock` 文件锁 | 1（排他锁） | 进程退出后内核自动释放 |

---

### 二、逐项详细分析

#### 1. 目录 Handle 缓存（`moka::sync::Cache<HandleKey, PathBuf>`）

**位置**：`dir_cache.rs` + `fid_parser.rs` 中的 `read_fid_events_cached`

**用途**：fanotify 上报的事件只包含"文件句柄"而非路径名，此缓存将句柄反查为完整路径。

**写入时机**：
- 启动时：对已监控目录及其子目录递归预填充（`cache_recursive`）
- 运行时：每收到一个事件，若路径已成功解析则插入缓存（`read_fid_events_cached` 的第二遍处理）
- 运行时 `add_path` 时对新目录预缓存

**读取时机**：每次 fanotify 事件到达时，在 `read_fid_events_cached` 中最多进行 **10 轮** 尝试：
1. 用 `dfid_name_handle` 查缓存 → 得到目录路径
2. 若缓存未命中，fallback 到 `resolve_file_handle`（内核级 `open_by_handle_at`）
3. 将成功解析的路径写回缓存
4. 若一轮中无任何进展则提前退出

**评价**：

✅ **优点**：
- **多轮渐进式解析**：对嵌套删除场景（父目录先被删除→子目录 handle 失效），通过缓存更新的渐进传播，最多重试 10 轮，设计合理
- **moka 并发安全**：内部 shard-level locking，被多个 tokio reader task 共享读写，无全局锁争用
- **绕过第三方库限制**：fanotify-fid 的 `resolve_with_cache` 需要拷贝整个 `HashMap`，本实现直接使用 moka 引用，避免了每次事件拷贝的开销
- **容量上限 100,000 + W-TinyLFU 淘汰**：超限自动淘汰低频条目，内存可控
- **TTL 1 小时**：目录被删除/重命名后，旧条目最迟 1 小时后自动过期

⚠️ **已解决（对比 v0.3.0）**：
- ~~无上限增长~~ → ✅ 容量 100,000，W-TinyLFU 淘汰
- ~~无失效机制~~ → ✅ TTL 1h 自动过期
- ~~无 TTL~~ → ✅ 已添加

**实现细节**：
- 使用 `moka::sync::Cache::builder()` 构建，内置 W-TinyLFU 淘汰策略
- 常量 `DIR_CACHE_CAP = 100_000` 和 `DIR_CACHE_TTL_SECS = 3600` 在 `fid_parser.rs` 中定义
- `Cache` 本身是 `Arc` 语义，直接 `clone()` 即可跨 task 共享，无需额外的 `Arc` 包装

---

#### 2. 进程信息缓存（`Cache<u32, ProcInfo>`）

**位置**：`proc_cache.rs`，定义 `ProcCache = Cache<u32, ProcInfo>`

**用途**：将 PID 映射到 `{cmd, user, ppid, tgid}`，避免每次事件都读 `/proc`。

**写入时机**：
- 无预填充
- 运行时：proc connector 收到 `Exec` 事件时，读取 `/proc/{pid}/comm` 和 `/proc/{pid}/status` 后插入

**读取时机**：`build_file_event_for_opts` → `utils::get_process_info_by_pid`（先查此缓存，未命中再读 `/proc` 或 `pid_cache`）

**评价**：

✅ **优点**：
- **事件驱动填充**：通过 proc connector（Netlink）实时捕获 exec 事件，对短生命周期进程也能捕捉到
- **`/proc` 读取有重试机制**：`retry()` 函数 3 次尝试 + 500µs 间隔，容错性好
- **容量上限 65,536 + TTL 10min**：W-TinyLFU 自动淘汰低频/已退出进程的条目

⚠️ **已解决（对比 v0.3.0）**：
- ~~无上限增长~~ → ✅ 容量 65,536，W-TinyLFU 淘汰
- ~~Exit 事件不清理~~ → ✅ TTL 10min 自动过期，不再积累僵尸条目
- ~~两套 PID 缓存不一致~~ → ✅ pid_cache（LruCache 4096）已移除，统一走 proc_cache（moka 65536 + TTL 10min）

---

#### 3. 进程树缓存（`Cache<u32, PidNode>`）

**位置**：`proc_cache.rs`，定义 `PidTree = Cache<u32, PidNode>`

**用途**：维护 PID→{ppid, cmd} 的进程父子关系树，用于：
- `is_descendant()`：判断事件 PID 是否是指定命令的子孙进程（cmd 过滤的核心实现）
- `build_chain()`：构建进程调用链字符串（如 `"102|touch|root;101|sh|root;..."`）

**写入时机**：
- **启动快照**：`snapshot_process_tree()` 扫描 `/proc/*/status` 全量填充
- **Fork 事件**：插入子节点（cmd 为空）
- **Exec 事件**：更新 cmd

**评价**：

✅ **优点**：
- **启动快照**：确保进程树在启动时刻就有一个完整基线，避免窗口期
- **Fork+Exec 双事件处理**：解决 fork 后不知 cmd 的问题，两步组装
- **`build_chain` 的 `/proc` fallback**：树中查不到的 PID 能回退读 `/proc/{pid}/status`，鲁棒性好
- **容量上限 65,536 + TTL 10min**：W-TinyLFU 自动淘汰，已退出进程的节点不再长期驻留

⚠️ **问题**：
- ~~无上限增长~~ → ✅ 容量 65,536，W-TinyLFU 淘汰
- **无循环检测**：`is_descendant` 只检测 `current == ppid` 的自循环，但更复杂的循环（A→B→C→A）会导致无限循环。实际 Linux 进程树是 DAG，但有潜在风险
- **PID 重用风险**：Linux PID 可被回收后重新分配。moka TTL 10min 过期 + Fork/Exec 事件覆盖 → 基本可忽略

---

#### 4. 文件大小缓存（`LruCache<PathBuf, u64>`）

**位置**：`monitor.rs`，`Monitor.file_size_cache`

**容量**：`FILE_SIZE_CACHE_CAP = 10,000`（定义在 `fid_parser.rs`）

**用途**：缓存文件的已知大小，避免对每个事件都调用 `stat()`。

**写入时机**：
- `Create` / `Modify` / `CloseWrite`：执行 `fs::metadata()` 获取大小后缓存
- 其他事件类型：不会刷新缓存（读旧值）

**失效时机**：
- `Delete` / `DeleteSelf` / `MovedFrom`：从缓存中移除（`pop`）
- LRU 淘汰：超出 10,000 条时淘汰最久未访问的条目

**评价**：

✅ **优点**：
- **LRU 淘汰**：容量有限，防止内存无限增长
- **惰性写入 + 显式失效**：创建/修改时写，删除时移除，语义正确
- **`pop` on delete**：事件发生后才 pop，此时文件已不存在无法 stat，利用缓存获取最后已知大小——正确且高效

⚠️ **问题**：
- **容量 10,000 条是否足够？**：对监控整个文件系统的场景，可能被快速填满。但考虑到只有有事件的路径才入缓存，多数路径不会被缓存，目前足够
- **LruCache 非线程安全**：内部用 RefCell，但当前只有一个事件循环线程在消费，所以没问题。如果将来多消费者需加锁或换用并发 LRU

---

#### 5. ~~PID 信息缓存（第二层，`LruCache<u32, ProcInfo>`）~~ ✅ 已移除

**位置**：~~`monitor.rs`，`Monitor.pid_cache`~~

**状态**：`pid_cache` 已在 v0.3.1 中移除。原因是 proc_cache（moka 65536 + TTL 10min）已足够快且 bounded，不再需要第二层 LruCache。移除后消除了与 proc_cache 的数据不一致风险。

---

#### 6. UID→用户名映射（`OnceLock<HashMap<u32, String>>`）

**位置**：`utils.rs`，`uid_passwd_map()`

**用途**：避免对每个进程调 getpwuid / 反复读 `/etc/passwd`。

**实现**：`OnceLock` + 一次性解析 `/etc/passwd`

**评价**：

✅ **优点**：
- **静态单例**：首次使用时初始化一次，后续零开销
- **线程安全**：`OnceLock` 保证只初始化一次
- **内存紧凑**：一个 HashMap 存全部用户，通常 <100KB

⚠️ **问题**：
- **不会刷新**：运行中新增/删除/改名用户不会反映到缓存中。对 fsmon 的日志记录场景影响不大（历史记录的 user 字段是描述性的），但仍需注意

---

#### 7. 守护进程锁（`flock` 文件锁）

**位置**：`lib.rs`，`DaemonLock`

**用途**：确保同一用户下只有一个 fsmon 守护进程实例。

**实现**：`fs2::FileExt::try_lock_exclusive()` + PID 写入 `/tmp/fsmon-<UID>.lock`

**评价**：

✅ **优点**：
- **内核级排他锁**：进程崩溃后内核自动释放锁，无锁定残留
- **友好的冲突提示**：读取已有 PID 显示具体信息
- **多用户友好**：锁文件路径包含 UID

⚠️ **问题**：无。此实现简洁且正确。

---

### 三、总结

#### 整体架构：8/10

缓存分层清晰，路径解析、进程信息、文件大小、用户映射各司其职，关注点分离。moka 作为并发缓存被多个 tokio reader task 共享。事件驱动 + 惰性填充的结合方式合理。

#### 核心问题

| 问题 | 严重程度 | 说明 |
|------|----------|------|
| 问题 | 严重程度 | 说明 |
|------|----------|------|
| 进程树无循环保护 | **低** | 极端情况下 `is_descendant` 可能无限循环 |
| 缺少缓存指标/监控 | **低** | 没有缓存命中率、容量等可观测指标 |

✅ **已解决**：
- `dir_cache` 无淘汰机制、无 TTL（moka Cache + 容量 100k + TTL 1h）
- `proc_cache` / `pid_tree` 无淘汰机制、Exit 不清理（moka Cache + 各 65536 + TTL 10min）
- `pid_cache` 与 `proc_cache` 不一致（移除 pid_cache LruCache，统一走 proc_cache moka）

#### 改进方向

✅ **已完成**：
1. **`dir_cache` DashMap → moka Cache**：容量上限 100,000 + W-TinyLFU 淘汰 + TTL 1h
2. **`proc_cache` / `pid_tree` DashMap → moka Cache**：各 65,536 + TTL 10min + W-TinyLFU 淘汰
3. **移除全部 DashMap 依赖**：3 个缓存统一为 moka
4. **统一两套 PID 缓存**：移除 pid_cache（LruCache 4096），仅保留 proc_cache（moka 65536），彻底消除不一致

📋 **待处理**：
5. **进程树循环检测**：给 `is_descendant` 加 visited set 防止极端循环
6. **增加 PID 重用检测**：利用 proc connector 提供的进程启动时间戳来区分新旧进程
7. **增加缓存指标导出**：moka 提供 `entry_count()` 等统计接口，可暴露给 debug 输出
