# fsmon 代码质量审查

## 死代码
- [x] `filters.rs:make_entries()` — 已删除
- [x] `filters.rs:is_path_in_scope()` — 已移除未使用的 `_canonical_paths` 参数

## #[allow(dead_code)]
- [x] `common.rs:DaemonLock.listener` — 合法（RAII 语义，保持 socket 绑定）
- [x] `monitor.rs:MetricsReport` — 已移除不必要的 allow
- [x] `filters.rs:make_entries()` — 已删除

## unwrap (非测试代码)
- [x] `file_writer.rs:115` — 安全（key 刚 insert 过）

## 下划线变量
- [x] `_canonical_paths` — 已删除未使用参数
- [x] `_monitored_path` — 模式匹配中未使用的值，合法
- [x] `_exited1`, `_exited2`, `_exited` — 已移除未使用的绑定

## 性能问题
- [x] `get_rss_mb()` — 已用 `.nth(1)` 替代 Vec 分配
- [x] `handle_canonical_root_deleted` — 已优化 `path.to_path_buf()` 调用
- [x] `handle_proc_events` — events 为空时返回 Vec::new()，已有优化 ✓
- [x] `matching_opts_for_event` — 克隆 PathBuf/PathOptions（借用安全需要，无法避免）

## clippy pedantic
- [x] 已运行 cargo clippy，无警告
