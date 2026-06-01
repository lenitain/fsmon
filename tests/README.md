# fsmon 测试集

## 测试文件

- `p1_cli.rs` — CLI 命令端到端测试（add/monitored/remove/query/changes/clean）
- `p1_monitor.rs` — 事件解析、序列化、EventType 完整性测试
- `p1_crash_recovery.rs` — 崩溃恢复与容错测试（DaemonLock / 原子写入 / 配置容错 / 日志截断处理）
- `p1_utils.rs` — 工具函数测试（parse_size / parse_size_filter / parse_time_filter）

## 运行测试

```bash
# 全部测试
cargo test

# 仅集成测试
cargo test --test '*'

# 单个测试文件
cargo test --test p1_cli

# 单个测试函数
cargo test --test p1_cli add_global_with_path

# 无警告编译
cargo test --no-run
```

## 测试分层

| 层级 | 内容 | 位置 |
|------|------|------|
| 单元测试 | 模块内部逻辑（monitor/events/filtering/fid_parser） | `src/**/*.rs` (`#[cfg(test)]`) |
| CLI 参数解析 | AddArgs/QueryArgs 等解析 | `src/bin/fsmon.rs` (`#[cfg(test)]`) |
| 集成测试 | CLI 端到端 / 崩溃恢复 / 工具函数 | `tests/*.rs` |

## CI

- `.github/workflows/ci.yml` — 每次 push/PR 自动运行: build + test + fmt + clippy
- `.github/workflows/bench.yml` — main 分支源码变更时: release build + binary size + smoke test
