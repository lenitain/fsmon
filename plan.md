# Plan: Watchdog 集成

## Goal
为 fsmon 守护进程添加 systemd watchdog 支持，实现进程健康监控和自动重启。

## Steps
- [x] 1. 添加 libsystemd 依赖到 Cargo.toml
- [x] 2. 创建 watchdog 模块 (src/watchdog.rs)
- [x] 3. 添加配置选项 (WatchdogSec)
- [x] 4. 在 Monitor 中集成 watchdog 心跳
- [x] 5. 更新 systemd 服务文件生成
- [x] 6. 编写测试
- [x] 7. 构建验证

## Files
- Modify: Cargo.toml
- Create: src/watchdog.rs
- Modify: src/config.rs
- Modify: src/monitor/mod.rs
- Modify: src/bin/commands/init_cd.rs
- Modify: src/bin/fsmon.rs
- Modify: src/bin/commands/mod.rs
- Modify: src/monitor/tests.rs

## Rollback
git revert HEAD