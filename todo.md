# Todo: Phase 2 高价值改进

- [x] 6. PathEntry → PathOptions 转换统一（impl TryFrom<&PathEntry> for PathOptions）
- [x] 7. chown 逻辑统一（chown_to_original_user 委托给 chown_to_user）
- [ ] 8. Monitor::run() 事件循环重构（提取 EventProcessor）
- [ ] 9. add_path 函数拆分（120行→3个聚焦方法）
- [ ] 10. Monitor::new() 参数简化（使用 MonitorConfig 结构体）
