# 安全扫描总结：进程信息伪造与 PID 复用

**扫描日期**: 2026-06-22
**目标**: fsmon 进程信息获取与 PID 复用检测机制

## 发现统计

| 严重性 | 数量 |
|--------|------|
| HIGH   | 2    |
| MEDIUM | 5    |
| LOW    | 1    |
| **总计** | **8** |

## 核心风险

**最高风险**：PID 复用检测的 `current_start == 0` 绕过（漏洞 #2）

Fork 事件插入进程时 `start_time_ns=0`，进程退出后 `read_proc_start_time_ns` 也返回 0。两者相等导致过时缓存被信任。攻击者可利用此逻辑缺陷伪造进程信息、绕过进程树过滤。

**攻击链**:
```
fork → Fork事件(缓存start_time=0) → exit → fork(复用PID) → 触发事件
→ get_process_info_by_pid → 0==0 → 返回过时缓存 → 安全策略被绕过
```

## 关键代码路径

```
proc_cache.rs:42  handle_proc_events()     ← netlink消息解析入口
  ↓
utils.rs:79       get_process_info_by_pid() ← PID复用检测（漏洞1+2）
  ↓
events.rs:138     matches_process_tree()   ← 进程树过滤（漏洞5）
  ↓
events.rs:208     patch_pending_events()   ← 二次解析（漏洞6）
```

## 修复建议

1. **P0**: `current_start == 0` 时不信任缓存，回退到 /proc 读取
2. **P0**: Fork 事件处理时尝试填充 start_time_ns
3. **P1**: Overrun/Truncated 后触发进程树 snapshot 重建
4. **P1**: patch_pending_events 增加 start_time_ns 验证

## 详细报告

- [JSON 格式](SECURITY-PROC-ANALYSIS-FINAL.json)
- [Markdown 格式](SECURITY-PROC-ANALYSIS.md)
