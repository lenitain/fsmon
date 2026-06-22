# 安全分析报告：进程信息伪造与 PID 复用

**分析日期**: 2026-06-22
**焦点区域**: 进程信息伪造与 PID 复用
**目标文件**:
- `src/common/proc_cache.rs`
- `src/common/utils.rs`
- `src/common/monitor/events.rs`
**依赖模块**: `proc-tree` crate (v0.3.0)

---

## 漏洞汇总

| # | 严重性 | 类别 | 文件:行 | 标题 |
|---|--------|------|---------|------|
| 1 | HIGH | PID复用竞态 | utils.rs:90 | TOCTOU竞态窗口 |
| 2 | HIGH | 进程信息伪造 | utils.rs:91 | start_time_ns=0绕过检测 |
| 3 | MEDIUM | netlink截断 | proc_cache.rs:71 | 截断消息继续处理 |
| 4 | MEDIUM | 事件丢失 | proc_cache.rs:68 | overrun导致进程树不同步 |
| 5 | MEDIUM | 进程树污染 | events.rs:138 | 依赖过时存储数据 |
| 6 | MEDIUM | 竞态条件 | events.rs:208 | patch_pending_events竞态 |
| 7 | MEDIUM | TOCTOU | utils.rs:97 | /proc回退读取竞态 |
| 8 | LOW | 信息泄露 | utils.rs:79 | 跨用户进程信息泄露 |

---

## 详细分析

### 漏洞 1：PID 复用检测 TOCTOU 竞态窗口 [HIGH]

**位置**: `src/common/utils.rs:90`

**代码路径**:
```
get_process_info_by_pid()
  → store.get_process(pid)          // 第86行：读取缓存
  → read_proc_start_time_ns(pid)    // 第90行：读取当前start_time
  → 比较两者                         // 第91行：判断是否PID复用
```

**问题**: 缓存读取与当前值读取之间存在时间窗口。PID可能在此窗口内被复用。

**利用难度**: 中等——需要精确控制进程退出和fork时机。

---

### 漏洞 2：start_time_ns=0 绕过 PID 复用检测 [HIGH]

**位置**: `src/common/utils.rs:91`

**关键代码**:
```rust
if cached_start == current_start || current_start == 0 {
    return info.clone();  // 返回可能过时的缓存信息
}
```

**问题链**:
1. Fork事件插入进程时 `start_time_ns: 0`（proc-tree ops.rs:173）
2. `read_proc_start_time_ns` 在进程退出后返回 0
3. 条件 `0 == 0 || 0 == 0` 为 true，返回过时缓存

**攻击场景**:
```
T0: fork(A, PID=X) → proc connector Fork事件 → 缓存{PID=X, start_time=0}
T1: A退出
T2: fork(B, PID=X) → B复用PID
T3: B触发fanotify事件
T4: get_process_info_by_pid(X) → cached_start=0, current_start=0 → 返回A的缓存
```

---

### 漏洞 3：netlink 截断消息继续处理 [MEDIUM]

**位置**: `src/common/proc_cache.rs:71`

**问题**: `Truncated` 错误后继续迭代 `NetlinkMessageIter`。如果迭代器未正确重定位，后续消息可能被错误解析。

**影响**: 虚假的 Fork/Exec/Exit 事件被注入进程树。

---

### 漏洞 4：proc connector overrun 事件丢失 [MEDIUM]

**位置**: `src/common/proc_cache.rs:68`

**问题**: Overrun 后仅打印警告，不触发进程树重建。丢失的事件导致进程树与真实状态不同步。

**影响**: 基于进程树的过滤规则（`is_descendant`）可能失效。

---

### 漏洞 5：进程树遍历依赖过时数据 [MEDIUM]

**位置**: `src/common/monitor/events.rs:138`

**问题**: `matches_process_tree` → `is_descendant` → `walk_ancestors` 遍历 ppid 链。TTL 过期（600秒）或事件丢失可导致 ppid 关系过时。

---

### 漏洞 6：patch_pending_events 竞态 [MEDIUM]

**位置**: `src/common/monitor/events.rs:208`

**问题**: 不验证 PID 有效性（不检查 start_time_ns），直接使用 store 中可能过时的信息更新事件。

---

### 漏洞 7：/proc 回退读取 TOCTOU [MEDIUM]

**位置**: `src/common/utils.rs:97-105`

**问题**: `retry` 函数在两次重试之间 sleep 500µs。`parse_proc_entry` 在此窗口内可能读到混合的新旧进程信息。

---

### 漏洞 8：跨用户进程信息泄露 [LOW]

**位置**: `src/common/utils.rs:79`

**问题**: 默认 `hidepid=0` 配置下，任何用户可读取所有进程的 cmdline（可能包含密码、API 密钥）。

---

## 修复优先级

### P0（立即修复）
1. **漏洞 2**: 当 `current_start == 0` 时不返回缓存，回退到 /proc 直接读取
2. **漏洞 1**: 使用原子操作确保 PID+start_time_ns 读取一致性

### P1（短期修复）
3. **漏洞 3-4**: Truncated/Overrun 后停止处理或触发进程树重建
4. **漏洞 6**: patch 时验证 start_time_ns

### P2（中期修复）
5. **漏洞 5**: 添加 TTL 时效性标记
6. **漏洞 7**: parse_proc_entry 后二次验证

### P3（低优先级）
7. **漏洞 8**: 多用户环境设置 hidepid=2
