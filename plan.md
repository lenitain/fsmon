# Plan: 结构性问题逐项修复

## Goal
按核热审查发现的结构性问题逐项修复，提升代码质量和可维护性。

## Steps

- [ ] 1. 提取 debug 日志宏（最简单，全局收益）
- [ ] 2. 统一 matching_opts_for_event() 匹配逻辑
- [ ] 3. 提取 CacheState 子结构体（减少 Monitor 字段数）
- [ ] 4. 提取 PathState 子结构体
- [ ] 5. 提取 FanotifyState 子结构体
- [ ] 6. 提取 process_event_batch() 子方法
- [ ] 7. to_jsonl_string_local() 改用 serde serializer
- [ ] 8. run() 事件循环提取 handler 方法
- [ ] 9. handle_socket_cmd() 错误分类用枚举

## Files
- Modify: src/monitor/mod.rs, src/monitor/events.rs, src/monitor/live_path.rs
- Modify: src/monitor/init.rs, src/monitor/reader.rs, src/monitor/filtering.rs
- Modify: src/monitor/socket_handler.rs, src/monitor/file_writer.rs
- Modify: src/lib.rs, src/filters.rs, src/config.rs

## Rollback
每个 step 完成后 cargo test + cargo clippy，失败则 revert 该 step。
