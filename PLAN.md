# Plan: fsmon 代码质量改进

## Goal
根据CODE_REVIEW.md中的审计报告，系统性地改进代码质量，消除重复、拆分大文件、重构上帝对象。

## 优先级
1. 推定阻塞项（🔴）
2. 高价值改进（🟡）
3. 美化但低价值（🟢）

## Steps

### Phase 1: 推定阻塞项（必须完成）

- [x] 1. **TimeFilter 方法提取**（最大的赢面）
  - 在 `utils.rs` 中为 `TimeFilter` 添加 `matches()`、`is_lower_bound()`、`is_upper_bound()` 方法
  - 更新 `query.rs` 和 `clean.rs` 中的重复 match 块
  - 删除约30行重复代码

- [x] 2. **PID 状态读取去重**
  - 比较 `utils.rs:read_proc_status_fields` 和 `proc_cache.rs:read_proc_info`
  - 删除 `proc_cache.rs` 中的重复实现，复用 `utils.rs` 版本
  - 消除15行重复代码

- [x] 3. **拆分 query.rs（1078行）**
  - 创建 `src/query/` 模块目录
  - 将核心逻辑提取到 `src/query/core.rs`
  - 将测试移到 `src/query/tests.rs`
  - 目标：每个文件<500行

- [x] 4. **拆分 clean.rs（1014行）**
  - 将 `clean_single_log` 拆分为：
    - 纯过滤函数（时间过滤）
    - 大小裁剪器
    - 输出格式化器
  - 创建 `src/clean/` 模块目录

- [x] 5. **开始拆分 Monitor 上帝对象**
  - 创建 `src/monitor/fanotify_mgr.rs`（FanotifyManager）
  - 创建 `src/monitor/inotify_mgr.rs`（InotifyManager）
  - 创建 `src/monitor/event_bus_mod.rs`（EventBus）
  - 创建 `src/monitor/socket_server_mod.rs`（SocketServer）
  - 创建 `src/monitor/proc_mgr.rs`（ProcManager）
  - 创建 `src/monitor/metrics_collect.rs`（MetricsCollector）

### Phase 2: 高价值改进

- [ ] 6. **PathEntry → PathOptions 转换统一**
  - 为 `PathEntry` 实现 `impl From<&PathEntry> for PathOptions`
  - 更新 `commands/mod.rs`、`monitor/events.rs`、`monitor/live_path.rs` 中的转换逻辑
  - 消除~40行重复代码

- [ ] 7. **chown 逻辑统一**
  - 统一 `config.rs:chown_to_original_user` 和 `fid_parser.rs:chown_to_user`
  - 让 `chown_to_original_user` 委托给 `chown_to_user`

- [ ] 8. **Monitor::run() 事件循环重构**
  - 创建 `EventProcessor` 结构体
  - 将事件处理逻辑提取到独立方法
  - 简化 `tokio::select!` 循环

- [ ] 9. **add_path 函数拆分**
  - 将120行的 `add_path` 拆分为3个聚焦方法：
    - `add_existing_path`
    - `add_nonexistent_path`
    - `add_monitored_path`

- [ ] 10. **Monitor::new() 参数简化**
  - 创建 `MonitorConfig` 结构体
  - 使用 builder 模式或配置结构体

### Phase 3: 美化但低价值

- [ ] 11. **PathEntry 领域专用类型**
  - 创建 `StoredEntry`、`SocketEntry`、`MonitorEntry`
  - 替换 `PathEntry` 的多职使用

- [ ] 12. **EventSender/EventReceiver 简化**
  - 使用统一的 `EventChannel` 结构体
  - 简化 `channel.rs`

- [ ] 13. **Monitor 测试方法移动**
  - 将 `#[cfg(test)]` 方法移到单独的 impl 块
  - 改善代码组织

- [ ] 14. **to_jsonl_string_local 改进**
  - 使用结构化序列化替代手动字符串替换
  - 提高健壮性

## Files to Modify

### Phase 1
- `src/utils.rs` - 添加 TimeFilter 方法
- `src/query.rs` - 使用 TimeFilter 方法，然后拆分
- `src/clean.rs` - 使用 TimeFilter 方法，然后拆分
- `src/proc_cache.rs` - 删除重复的 read_proc_info
- `src/monitor/mod.rs` - 开始拆分 Monitor

### Phase 2
- `src/monitored.rs` - 添加 From 实现
- `src/commands/mod.rs` - 使用统一转换
- `src/monitor/events.rs` - 使用统一转换
- `src/monitor/live_path.rs` - 使用统一转换
- `src/config.rs` - 统一 chown 逻辑
- `src/fid_parser.rs` - 统一 chown 逻辑
- `src/monitor/mod.rs` - 重构 run() 和 add_path

### Phase 3
- `src/monitored.rs` - 创建领域专用类型
- `src/monitor/channel.rs` - 简化通道
- `src/monitor/mod.rs` - 移动测试方法
- `src/utils.rs` - 改进 JSON 序列化

## Rollback

每个 Phase 完成后：
1. 运行完整测试套件：`cargo test`
2. 验证功能：`cargo build`
3. 如果出现问题，使用 `git revert` 回滚到上一个稳定状态

## Current Status

- [ ] Phase 1: 推定阻塞项
- [ ] Phase 2: 高价值改进  
- [ ] Phase 3: 美化但低价值

## Next Steps

开始 Phase 1，步骤1：TimeFilter 方法提取。