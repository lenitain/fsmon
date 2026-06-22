# 安全漏洞分析：进程信息伪造与PID复用

## 概述
本分析聚焦于fsmon中进程信息读取、PID复用检测、netlink消息解析和进程树构建的安全问题。

## 发现总结

### 高危漏洞 (2个)

#### 1. PID复用检测存在TOCTOU竞态窗口
- **文件**: `src/common/utils.rs:90`
- **类别**: PID复用竞态条件
- **严重性**: HIGH
- **置信度**: 0.9

**描述**:  
在`get_process_info_by_pid`函数中，PID复用检测通过比较缓存的`start_time_ns`与当前读取的`start_time_ns`实现。但读取当前`start_time_ns`（第90行）与后续比较（第91行）之间存在时间窗口，PID可能已被复用。

**攻击场景**:  
攻击者可监控进程退出，在检查后、使用前快速创建新进程复用同一PID。新进程继承原进程的缓存信息（cmd、user等），导致进程信息伪造。

**修复建议**:  
使用原子操作或锁机制确保PID与`start_time_ns`的读取是原子的。或在获取进程信息后立即验证PID未被复用。

#### 2. read_proc_start_time_ns返回0时绕过PID复用检测
- **文件**: `src/common/utils.rs:91`
- **类别**: 进程信息伪造
- **严重性**: HIGH
- **置信度**: 0.85

**描述**:  
当`read_proc_start_time_ns`返回0（表示读取失败）时，条件`current_start == 0`为真，函数直接返回缓存信息。攻击者可能通过权限限制或文件系统错误使`/proc/{pid}/stat`不可读，从而绕过PID复用检测。

**攻击场景**:  
攻击者通过挂载特殊文件系统或修改`/proc`权限，使`read_proc_start_time_ns`返回0。此时系统会返回过时的缓存进程信息，可能包含错误的cmd、user等。

**修复建议**:  
当`current_start == 0`时应视为异常情况，返回unknown或采取其他保守策略，而不是返回可能过时的缓存信息。

### 中危漏洞 (4个)

#### 3. 截断的netlink消息继续处理可能导致数据不一致
- **文件**: `src/common/proc_cache.rs:71`
- **类别**: netlink消息截断
- **严重性**: MEDIUM
- **置信度**: 0.8

**描述**:  
当proc connector返回`Truncated`错误时（第71行），代码仅打印警告并继续处理后续消息。截断的消息可能包含不完整的事件数据，导致进程树状态不一致。

**攻击场景**:  
攻击者通过向netlink socket发送精心构造的截断消息，可能导致进程事件被部分解析，造成进程树状态错误。

**修复建议**:  
对于截断的消息，应丢弃该消息并记录错误，而不是继续处理。考虑实现消息完整性验证。

#### 4. proc connector overrun导致事件丢失
- **文件**: `src/common/proc_cache.rs:68`
- **类别**: 事件丢失
- **严重性**: MEDIUM
- **置信度**: 0.75

**描述**:  
当proc connector返回`Overrun`错误时（第68行），代码仅打印警告并继续。overrun意味着事件队列溢出，部分事件已丢失，可能导致进程树与真实状态不同步。

**攻击场景**:  
攻击者通过快速创建/退出大量进程，使proc connector事件队列溢出。丢失的事件可能导致进程树不完整，影响安全决策。

**修复建议**:  
实现事件丢失检测和恢复机制。在overrun发生后，考虑重建进程树或标记状态为不可信。

#### 5. 进程树遍历依赖可能过时的存储数据
- **文件**: `src/common/monitor/events.rs:138`
- **类别**: 进程树污染
- **严重性**: MEDIUM
- **置信度**: 0.7

**描述**:  
`matches_process_tree`函数（第138行）调用`proc_tree::is_descendant`，该函数遍历进程树。如果进程存储中的数据过时（例如由于事件丢失或延迟），遍历结果可能不准确。

**攻击场景**:  
攻击者利用事件丢失窗口，使进程树存储过时。然后利用过时的进程树关系绕过基于进程树的过滤规则。

**修复建议**:  
为进程存储条目添加时效性标记，过期的条目不应参与进程树遍历。考虑实现定期进程树刷新机制。

#### 6. patch_pending_events与进程状态变化之间的竞态
- **文件**: `src/common/monitor/events.rs:208`
- **类别**: 竞态条件
- **严重性**: MEDIUM
- **置信度**: 0.7

**描述**:  
`patch_pending_events`函数（第208行）在事件循环后期尝试解析'unknown'字段。但进程可能在第一次获取信息后、第二次解析前退出或PID被复用。

**攻击场景**:  
攻击者在事件处理窗口期快速替换进程，导致`patch_pending_events`使用错误的进程信息更新事件。

**修复建议**:  
在`patch_pending_events`中再次验证PID的有效性，确保进程信息的一致性。

### 低危漏洞 (1个)

#### 7. 进程信息读取可能泄露跨用户信息
- **文件**: `src/common/utils.rs:63`
- **类别**: 信息泄露
- **严重性**: LOW
- **置信度**: 0.6

**描述**:  
`get_process_info_by_pid`函数读取`/proc/{pid}/cmdline`和`/proc/{pid}/status`等文件。虽然Linux默认限制跨用户读取，但某些配置下可能允许，导致进程信息泄露。

**攻击场景**:  
在配置不当的系统上，攻击者可能读取其他用户的进程信息，包括命令行参数（可能包含敏感数据）。

**修复建议**:  
确保进程信息读取遵循最小权限原则。考虑在读取前检查进程属主权限。

## 代码行号参考

### proc_cache.rs
- 第68行: `Err(proc_connector::Error::Overrun) => {`
- 第71行: `Err(proc_connector::Error::Truncated) => {`

### utils.rs
- 第63行: `pub fn get_process_info_by_pid(`
- 第90行: `let current_start = read_proc_start_time_ns(pid);`
- 第91行: `if cached_start == current_start || current_start == 0 {`

### events.rs
- 第138行: `fn matches_process_tree(&self, cmd: Option<&str>, event_pid: u32) -> bool {`
- 第208行: `pub(crate) fn patch_pending_events(&self, pending: &mut [PendingEvent]) {`

## 总结
主要风险集中在PID复用检测的竞态条件和进程信息伪造。攻击者可能利用这些漏洞绕过进程过滤、注入虚假进程信息或导致进程树状态不一致。建议优先修复高危漏洞，特别是TOCTOU竞态条件和`start_time_ns`返回0时的处理逻辑。

## 补充分析：proc-connector crate消息解析

### 缓冲区边界检查
proc-connector crate在消息解析中实现了适当的边界检查：
- `NetlinkMessageIter::next()`检查剩余空间是否小于`SIZE_NLMSGHDR`
- `parse_netlink_message()`检查`payload.len() < SIZE_NLMSGHDR`
- `parse_cn_msg()`检查`buf.len() < SIZE_CN_MSG`
- `parse_proc_event()`检查`buf.len() < PROC_EVENT_HEADER_SIZE`

### 潜在问题
1. **整数溢出风险**: `nlmsg_len`作为`u32`读取后转换为`usize`，在32位系统上可能溢出。但后续检查`nlmsg_len > payload.len()`应能捕获此情况。

2. **Panic风险**: `read_u32`和`read_u16`使用`try_into().unwrap()`，如果缓冲区太小会panic。但这应该被前面的边界检查所防止。

3. **未知事件处理**: 对于未知事件类型，代码返回`ProcEvent::Unknown`并包含原始数据。这可能导致信息泄露，但风险较低。

### 结论
proc-connector crate的消息解析在边界检查方面是合理的，没有发现明显的缓冲区溢出或内存安全漏洞。主要安全风险仍然集中在fsmon代码中的PID复用检测和进程信息伪造问题上。
