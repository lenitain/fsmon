# fsmon crate extraction plan

## Status
完成度 0/2。代码改到位后，PROGRESS.md 和每次新建的 todo.md/plan.md 删除。

## 目标
从 fsmon 中拆出 2 个独立轻量 crate，发布后可单独被其他项目引用。

---

## 1. `human-size` crate

**来源**: `src/utils.rs` 中的 `parse_size` / `parse_size_filter` / `format_size` / `SizeFilter` / `SizeOp`

**外部依赖**: 无（纯 std）

**步骤**:

### 1.1 新建目录
```
fsmon/
├── crates/
│   └── human-size/
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
```

### 1.2 Cargo.toml 内容
```toml
[package]
name = "human-size"
version = "0.1.0"
edition = "2024"
license = "MIT"
description = "Human-readable size string parsing and formatting (e.g., \">=1GB\", \"<500KB\")"
```

### 1.3 从 utils.rs 提取到 lib.rs
- `SizeOp` enum (Gt, Ge, Lt, Le, Eq) — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`
- `SizeFilter` struct (op: SizeOp, bytes: i64) — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`
- `parse_size(size_str: &str) -> Result<i64>` — 解析 `"1GB"`, `"500KB"`, `"1024"`
- `format_size(bytes: i64) -> String` — 反格式化 `1024` → `"1KB"`
- `parse_size_filter(s: &str) -> Result<SizeFilter>` — 解析 `">=1GB"`, `"<500KB"`, `"=0"`

**需要改动的原始代码**: `src/utils.rs` 中 SizeOp/SizeFilter/parse_size/parse_size_filter/format_size 的定义和实现。保留位置加 `#[path = "..."]` 或改为 re-export。

### 1.4 更新 fsmon 的 Cargo.toml
```toml
[dependencies]
human-size = { path = "crates/human-size" }
```

### 1.5 更新 fsmon 的 src/utils.rs
```rust
// 原来: pub use/use 内部定义的 SizeOp, SizeFilter, parse_size 等
// 改为: pub use human_size::{SizeOp, SizeFilter, parse_size, parse_size_filter, format_size};
```

### 1.6 在新 crate 里写测试
从现有 tests 中迁移：
- `test_parse_size_basic`
- `test_parse_size_filter_*`
- `test_format_size_*`
- `test_parse_size_invalid`
- `test_parse_size_filter_no_operator_error`

### 1.7 编译验证
```
cd crates/human-size && cargo test
cd ../.. && cargo test  # fsmon 全套
```

---

## 2. `human-time` crate

**来源**: `src/utils.rs` 中的 `parse_time` / `parse_time_filter` / `TimeFilter`

**外部依赖**: `chrono = { version = "0.4", features = ["serde"] }`

**步骤**:

### 2.1 新建目录
```
fsmon/
├── crates/
│   └── human-time/
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
```

### 2.2 Cargo.toml 内容
```toml
[package]
name = "human-time"
version = "0.1.0"
edition = "2024"
license = "MIT"
description = "Human-readable time duration string parsing and filtering (e.g., \">=7d\", \"<2h\")"

[dependencies]
chrono = { version = "0.4", features = ["serde"] }
```

### 2.3 从 utils.rs 提取到 lib.rs
- `TimeFilter` struct (op: SizeOp, time: DateTime<Utc>) — 用的是 `SizeOp`！
  - 注意：这里依赖 SizeOp。有两种方案：
    **A**: 让 `human-time` 自己定义 `TimeOp`（Gt/Ge/Lt/Le/Eq）— 和 SizeOp 长一样但独立
    **B**: 让 `human-time` 依赖 `human-size` — 但 `human-size` 是纯零依赖，加上去也行
  → **推荐 A**：自己定义 TimeOp，保持零依赖（仅 chrono）。SizeOp vs TimeOp 概念不同，未来可能演化方向不同。
- `parse_time(s: &str) -> Result<DateTime<Utc>>` — 解析 `"7d"`, `"2h"`, `"2024-01-01"`, `"30m"`
- `parse_time_filter(s: &str) -> Result<TimeFilter>` — 解析 `">=7d"`, `"<2h"`
- `TimeOp` enum (Gt, Ge, Lt, Le, Eq) — 注意 fsmon 原文用的是 SizeOp

**同样需要**: 更新 `src/utils.rs` 中的 import，和 fsmon 中各模块的引用（query.rs, clean.rs 等用了 `SizeOp` 来匹配 time filter）。

### 2.4 更新 fsmon
- `Cargo.toml`: 加 `human-time = { path = "crates/human-time" }`
- `src/utils.rs`: `pub use human_time::{TimeFilter, parse_time, parse_time_filter, TimeOp};`
- `src/lib.rs`: `pub use utils::{TimeFilter, TimeOp, ...}`（对外 API 不能变）
- `src/query.rs`: 改 `use crate::utils::{TimeFilter, SizeOp, ...}` → 其中 SizeOp 是给 time filter 用的，需改为 `TimeOp`
- `src/clean.rs`: 同样，检查 `SizeOp` 用于 time filter 的地方 → 改为 `TimeOp`

### 2.5 测试迁移
- `test_parse_time_filter_*`
- `test_parse_time_*`

---

---

## 注意事项

### 修改顺序
1. `human-size`（零依赖，优先拆）
2. `human-time`（依赖 chrono，需决定 TimeOp 方案）

### 每次拆完后都做
```
cd crates/<name> && cargo test
cd ../.. && cargo test   # fsmon 仍全部通过
cargo clippy             # 无新 warning
```

### 发布后需要做
- 在 Cargo.toml 中把 `path` 依赖改为 crates.io 版本
- 更新 README 说明
