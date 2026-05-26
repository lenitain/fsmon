# fsmon 自愈体系完善计划

## P1 小修小补（低代价，高收益）

### 建议 1: `procfs` 依赖的去留(finished)

`Cargo.toml` 里引入了 `procfs = "0.16"`，但 `proc_cache.rs` 和 `utils.rs` 中仍然手动解析 `/proc/{pid}/status`。`procfs::process::Process` 一行就能拿到 ppid / comm / tgid / starttime，比手动解析更可靠（处理了线程名中的空格、字段格式变化等边界）。

- **要么用 `procfs` 替代所有手动 `/proc` 解析**，减少代码量并提高健壮性
- **要么从 Cargo.toml 里删掉 `procfs`**，避免引入但不用增加编译时间

### 建议 2: 日志写入 flush + fsync 策略 (finished)

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

### 事件去重/合并 (`--coalesce`) ❌ 已废弃

原方案：用 moka Cache + 100ms 时间窗口将 CREATE+MODIFY+CLOSE_WRITE 合并为一条记录。

废弃理由：
- flush timer 方案有硬伤：`process_event_batch` 是同步调用，batch 间有任意长间隔，不配独立定时器任务则事件无限期滞留在缓存中
- SIGKILL 时整组事件丢失，比不 coalesce 更糟
- 双日志格式使下游工具（query/diff/changes）复杂度翻倍
- 用户直接用 `--type CLOSE_WRITE` 即可达到类似效果，零代码变更

### `fanotify_mark` 中消除不必要的堆分配(finished)

`path.as_ref().as_bytes().to_vec()` 每次都分配新 Vec。可用 `CString::new` 或 `OsStr::as_encoded_bytes()`（Rust 1.74+）避免。mark 操作不频繁，但作为 crates.io 公共 API，零开销是承诺。

### HandleCache 的脏数据问题 ❌ 不处理

已删除目录的 handle→path 映射在 dir_cache 中残留，直到 TTL 淘汰。

正确性不受影响：Phase 1 本地 `handle_map` + `resolve_file_handle` 兜底。

潜在修复（如确有必要）：
```rust
// process_event_batch 的 is_canonical_root 分支
if let Some(ref key) = raw.self_handle {
    self.dir_cache.invalidate(key);
}
```

不处理理由：脏 entry 约 5% cache 浪费（~600KB 量级），对 daemon 可忽略。改和不改用户感知不到差异。

——用户可以通过 CLI flag 自行控制：`--cache-dir-cap` 调容量、`--cache-dir-ttl` 调淘汰速度。

---

## P3 架构级（如果做大）

### Unix socket 实时事件订阅

当前事件只能写到 JSONL 文件供事后查询。无法实时消费。

方案：新增 `subscribe` socket endpoint，保持长连接，持续推送 JSONL 事件流。

```bash
# 外部进程（任意语言）：
echo 'subscribe' | nc -U /tmp/fsmon-1000.sock
# ← 持续收到 JSONL 行，直到连接断开
```

优势：
- 不引入 protobuf/Avro/Prometheus 等任何依赖
- 外部进程用任何语言写 adapter（~10 行 Python/Go/Rust）
- fsmon 只负责 JSONL 输出，格式转换是 adapter 的事
- 不 fork，不改 fsmon 代码

代价：
- 当前 socket 是 request-response 模式（加 cmd，收 resp，断连）
- 需要改成长连接 + subscriber 列表 + broadcast 推送
- 需要处理背压（慢 subscriber 是否 drop？）
- 需要 cleanup 断连 subscriber

优先级：低。等有真实需求（有人要在 fsmon 上接 Kafka）再动手。

### 日志格式 trait 化 ❌ 已废弃

原方案：`trait EventSink` + impl 切换输出格式。

废弃理由：
- fsmon 是二进制工具，trait 化后用户仍需 fork 才能加新 impl，没有解决任何实际问题
- 真正需要的不是 Rust 抽象，而是外部进程能消费的实时事件流（见上方 socket subscribe 方案）

### fanotify-fid 支持 io-uring

Linux 6.0+ 下 fanotify 可配合 io-uring 做异步事件读取。当前 `AsyncFd` 方案在 tokio 下完全正确，但极致低延迟场景（微秒级）io-uring 的 submission queue polling 更优。不急，但 crate README 的 "Future Work" 里提一句让后来者知道这条路被考虑过。
