# fsmon 项目状态

## 🐛 待修复

无 — 当前无已知运行时 bug。

## 🧹 代码组织

| 等级 | 问题 | 状态 |
|------|------|------|
| ✅ | `src/monitor.rs` 拆出 `filters.rs` + `fid_parser.rs` | `monitor.rs` 从 2249 行降至 ~1660 行 |
| ⏳ | `src/bin/fsmon.rs` 拆出 `commands/` 目录 | 待完成 |
| ⏳ | `src/lib.rs` 拆出 `clean.rs` | 待完成 |

当前行数：
- `src/monitor.rs`: ~1660
- `src/filters.rs`: ~160
- `src/fid_parser.rs`: ~240

## 📄 文档不一致

| 位置 | 问题 |
|------|------|
| `README.md`/`README.zh-CN.md` | 源码结构图现在有 `fid_parser.rs` 了，但需确认位置 |
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
