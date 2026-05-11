# fsmon → chezmoi-style 重构 ✅ 完成

| 任务 | 状态 |
|------|------|
| config.rs - 配置文件名 `config.toml` → `fsmon.toml` | ✅ |
| config.rs - 新增 `init_dirs()` 创建 log/managed/config 目录 | ✅ |
| help.rs - 移除 Generate，增加 Init 和 Cd | ✅ |
| bin/fsmon.rs - 替换 Generate→Init，增加 Cd 命令 | ✅ |
| README.md 更新 | ✅ |
| README.zh-CN.md 更新 | ✅ |
| TESTING.md 更新 | ✅ |
| `cargo test` 全部通过 (120/120) | ✅ |
