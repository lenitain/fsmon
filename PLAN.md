# 拆分 monitor.rs — 完成

## 结果
- 原始 3153 行 → 9 个文件，最大文件 35KB
- 零警告编译，192 测试全部通过

## 文件结构
```
src/monitor/
├── mod.rs              (35541B) Monitor struct + new() + run()
├── channel.rs           (1219B)  EventSender + EventReceiver
├── file_writer.rs       (9247B)  FileLogWriter + notify_sd_ready
├── reader.rs            (8166B)  ReaderState + spawn/restart reader + dup_fd/open_dir  
├── events.rs            (9383B)  process_event_batch + build_file_event + matching
├── live_path.rs        (21922B)  add_path + remove_path + inotify + check_pending
├── socket_handler.rs   (11593B)  handle_socket_cmd + subscribe + health + reload_config + subscriber_task
├── filtering.rs         (2391B)  should_output + matching_path + is_path_in_scope + opts_for_path
└── tests.rs            (18387B)  #[cfg(test)] — 20 个测试
```
