# fsmon 自愈体系完善计划

## P1 小修小补（低代价，高收益）

### 建议 1: `procfs` 依赖的去留(finished)

`Cargo.toml` 里引入了 `procfs = "0.16"`，但 `proc_cache.rs` 和 `utils.rs` 中仍然手动解析 `/proc/{pid}/status`。`procfs::process::Process` 一行就能拿到 ppid / comm / tgid / starttime，比手动解析更可靠（处理了线程名中的空格、字段格式变化等边界）。

- **要么用 `procfs` 替代所有手动 `/proc` 解析**，减少代码量并提高健壮性
- **要么从 Cargo.toml 里删掉 `procfs`**，避免引入但不用增加编译时间

### 建议 2: 日志写入 flush + fsync 策略

当前写入走 `BufWriter`，不是每次 flush。如果 daemon 被 `kill -9`（非 SIGTERM），最后几秒的事件可能丢失。在"用 fsmon 找到是谁删了文件"的场景下，威胁模型需要覆盖"监控工具自身被干掉"。

- 加 `--sync-interval N`（默认 5s），每 N 秒对日志文件做一次 `fdatasync`
- SIGTERM 接收时做最后一次 sync 后再退出
- 代价：几十毫秒磁盘 IO / 每 5s — 可忽略

### 建议 3: `fsmon diff` 命令(finished)

运维中高频场景："上次部署以后哪些文件被改了？" `fsmon query` 只能按时间和路径过滤，需要用户用 `jq` 写复杂的去重聚合脚本。

```bash
# 新命令：按 path 去重，取最后一次更改
fsmon diff _global --since '2026-05-25 08:00' --until 'now'
```

实现上就是 `query` 的结果按 path dedup 取 `max(time)`，但有了独立子命令用户心智负担小很多。新增功能里 ROI 最高的一个。

---

## P2 中型改进（需要投入，但收益明显）

### 事件去重/合并 (`--coalesce`)

单个操作（如 `echo "hello" > file.txt`）产生 3 个 fanotify 事件：CREATE → MODIFY → CLOSE_WRITE。用 `(pid, path)` 做 key，固定时间窗口（~100ms）内合并为一个带事件列表 + 持续时间的记录。

- 可用 `moka::Cache` 做（项目已有依赖），在 `process_event_batch` 末尾加 flush timer
- 高频写入场景下日志量可减少 60-70%
- 作为**可选**功能（`--coalesce`），默认不开启，保持兼容

### `fanotify_mark` 中消除不必要的堆分配

`path.as_ref().as_bytes().to_vec()` 每次都分配新 Vec。可用 `CString::new` 或 `OsStr::as_encoded_bytes()`（Rust 1.74+）避免。mark 操作不频繁，但作为 crates.io 公共 API，零开销是承诺。

### HandleCache 的脏数据问题

moka TTL 淘汰被删除目录的 handle entry 但无通知。Phase 1 本地 `handle_map` 会走 `resolve_file_handle` 回退，不影响正确性，但浪费空间。未来可考虑把 HandleCache trait 化，让用户自由选择后端。

---

## P3 架构级（如果做大）

### 日志格式 trait 化

`FileEvent` 硬编码 JSONL 序列化。如果团队用 protobuf / Avro 做日志管道，就得 fork 项目。

```rust
pub trait EventSink {
    fn write(&mut self, event: &FileEvent) -> Result<()>;
}
```

~20 行抽象，让"接 Kafka / S3 / protobuf 输出"成为可能，且不影响默认 JSONL 路径。不急着做，但架构上预留位置。

### fanotify-fid 支持 io-uring

Linux 6.0+ 下 fanotify 可配合 io-uring 做异步事件读取。当前 `AsyncFd` 方案在 tokio 下完全正确，但极致低延迟场景（微秒级）io-uring 的 submission queue polling 更优。不急，但 crate README 的 "Future Work" 里提一句让后来者知道这条路被考虑过。
