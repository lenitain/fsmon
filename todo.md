# max_depth 功能实现

## 需要修改的文件

1. [ ] src/common/cli.rs - 添加 max_depth 参数到 AddArgs
2. [ ] src/common/monitored.rs - 添加 max_depth 字段到 PathEntry
3. [ ] src/common/socket.rs - 添加 max_depth 字段到 SocketCmd::Add
4. [ ] src/bin/fsmon/commands/add.rs - 传递 max_depth 到 SocketCmd
5. [ ] src/common/monitor/dir_watcher.rs - 使用 max_depth 调用 mark_recursive_with_depth
6. [ ] src/common/monitor/init.rs - 使用 max_depth 调用 mark_recursive_with_depth
7. [ ] src/common/monitor/live_path.rs - 使用 max_depth 调用 mark_recursive_with_depth
8. [ ] 测试验证
