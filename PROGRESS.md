# fsmon 项目状态

## 🐛 待修复

无 — 当前无已知运行时 bug。

## 🧹 代码组织（待决策）

| 问题 | 行数 | 建议 |
|------|------|------|
| `src/monitor.rs` | 2249 | 拆出 `filters.rs` + `fid_parser.rs` |
| `src/bin/fsmon.rs` | 953 | 拆出 `commands/` 目录 |
| `src/lib.rs` | 966 | 拆出 `clean.rs` |

## 📄 文档不一致

| 位置 | 问题 |
|------|------|
| `README.md`/`README.zh-CN.md` | 源码结构图提到不存在的 `fid_parser.rs` |
| `README.md` 架构图 | 同上 |
| `docs/` 下 5 个 `.md` | 引用 `config.toml`、`--since`/`--until`、`generate` 等已废弃接口 |

## 🔧 技术债务

| 问题 | 说明 |
|------|------|
| `edition = "2024"` | 要求 Rust 1.85+，是否降级到 2021？ |
| 7 个 ignored 测试 | 依赖 fanotify/netlink 内核，CI 无法运行，需 mock |
| `docs/` 目录 | 5 个旧 spec，建议归档或删除 |

## ✅ fanotify-fid

干净规范，2891 行，0 警告，0 ignored 测试。可改进：
- `src/lib.rs` 971 行偏大
- `edition = "2024"` 同样偏新
- 缺少文档注释
