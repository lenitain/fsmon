# Plan: 结构性问题逐项修复

## Goal
按核热审查发现的结构性问题逐项修复，提升代码质量和可维护性。

## Steps

- [x] 1. 提取 debug 日志宏（最简单，全局收益）
- [x] 2. 统一 matching_opts_for_event() 匹配逻辑
- [x] 3. 用 SlotMap 替代 Vec+index 管理 FsGroup（消除索引修复逻辑）
- [ ] 4. 提取 process_event_batch() 子方法
- [ ] 5. run() 事件循环提取 handler 方法

## Files
- Modify: src/monitor/mod.rs, src/monitor/events.rs, src/monitor/live_path.rs
- Modify: src/monitor/init.rs, src/monitor/reader.rs, src/monitor/filtering.rs
- Modify: src/monitor/socket_handler.rs, src/monitor/file_writer.rs
- Modify: src/lib.rs, src/filters.rs, src/config.rs

## Rollback
每个 step 完成后 cargo test + cargo clippy，失败则 revert 该 step。
