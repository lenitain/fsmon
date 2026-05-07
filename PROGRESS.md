### `ALL_EVENT_MASK` 包含 `FAN_FS_ERROR` ⏸️ 搁置，等 fanotify-rs 支持

**文件**: `src/monitor.rs` + `src/fid_parser.rs`

`FAN_FS_ERROR` (0x0000_8000) 需要 Linux 5.16+。`fanotify-rs 0.3.1` 不导出此常量,
在 `fid_parser.rs` 手动定义。在旧内核上 `fanotify_mark` 会返回 EINVAL
(mask 含未知 bit),导致 `all_events: true` 的路径静默监控失败。

**处理**:
- 删除 `fid_parser.rs` 中手动定义的 `FAN_FS_ERROR` 常量
- 从 `EVENT_BITS` 和 `ALL_EVENT_MASK` 中移除该 bit
- 保留 `EventType::FsError` 枚举变体以保持向前兼容
- 等 `fanotify-rs` 后续版本原生导出该常量后再加回来

