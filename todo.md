# Todo: Monitor 上帝对象拆分

- [x] 1. 创建 FanotifyManager 结构体（fs_groups, dir_cache, path_to_group）
- [x] 2. 创建 InotifyManager 结构体（inotify, _inotify_watches, pending_paths）
- [x] 3. 创建 EventBus 结构体（event_tx, event_stream_tx）
- [x] 4. 创建 SocketServer 结构体（socket_listener）
- [x] 5. 创建 ProcManager 结构体（proc_cache, pid_tree）
- [x] 6. 创建 Metrics 结构体（metrics, started_at）
- [x] 7. 重构 Monitor 结构体，使用新的子组件
- [x] 8. 测试编译通过
