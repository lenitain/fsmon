# fsmon 实时事件订阅功能

## 目标

给 fsmon daemon 新增 `subscribe` socket endpoint，外部进程可通过 Unix socket 实时接收 JSONL 事件流，不必轮询日志文件。

```bash
# 外部进程（任意语言）：拿到实时事件
echo 'subscribe' | nc -U /tmp/fsmon-1000.sock
# ← 持续收到 JSONL，一行一个 FileEvent
```

下游用 ~10 行 Python/Go/Rust adapter 就可把事件接 Kafka/S3/Prometheus/Webhook。

---

## 架构

```
fanotify reader tasks
        │
        │ (mpsc channel, 不变)
        ▼
   main loop ──► process_event_batch ──► write_jsonl (文件, 不变)
                        │
                        │ (broadcast channel, 新增)
                        ▼
              ┌─────────────────────┐
              │  ↓          ↓      │
            sub_1      sub_2    sub_3   (各自独立的 tokio task)
              │          │        │
              ▼          ▼        ▼
           socket     socket   socket  (长连接, JSONL 流)
```

- 主循环的事件处理流程 **不变** — JSONL 文件写入照旧
- 每处理一个 `FileEvent`，同时向 broadcast channel 发送一份
- 每个 subscribe 连接对应一个 tokio task，从 broadcast receiver 读事件，写 JSONL 到 socket
- 订阅者断开 → task 退出 → receiver 自动 drop
- daemon 关闭 → broadcast sender drop → 所有 receiver 收到 Closed → 所有 task 自动退出

---

## 协议

### 连接与认证（复用现有 socket）

```
TCP-like 流程:

1. Client 连接 /tmp/fsmon-1000.sock
2. Client 发送 TOML subscribe 命令（结尾加空行，同现有协议）
3. Server 返回 TOML SocketResp (ok = true 或 error)
4. 如 ok，Server 持续发送 JSONL（一行一个 FileEvent）
5. Client 断连 → 连接关闭；Server 关闭 → 连接关闭
```

subscribe 命令示例：
```toml
cmd = "subscribe"
track_cmd = "myapp"    # 可选：只订阅某个 cmd group，不填 = 全部
types = ["CLOSE_WRITE", "DELETE"]   # 可选：只订阅某些事件类型，不填 = 全部
```

复用 `SocketCmd` 的现有字段 `track_cmd` 和 `types`，不需新增字段。

### SocketResp（与现有命令兼容）

subscribe 成功后也返回 `SocketResp`：
```toml
ok = true
```
之后开始 JSONL 流。

subscribe 失败（如 daemon 未初始化 broadcast channel）：
```toml
ok = false
error = "subscriptions disabled"
error_kind = "Transient"
```

---

## 实现步骤

### 步骤 1：Monitor 结构体新增 broadcast sender

**文件**：`src/monitor.rs`

```rust
pub struct Monitor {
    // ... 现有字段不变 ...
    
    /// Broadcast channel for subscribe connections.
    /// Created in run() when socket listener is active.
    event_broadcast_tx: Option<tokio::sync::broadcast::Sender<FileEvent>>,
}
```

### 步骤 2：Monitor::new() 初始化 broadcast

默认 capacity：4096（允许 process_event_batch 连续产生数千事件而不丢订阅者）。

可加 CLI flag `--subscribe-buf N` 方便极端场景调大。

```rust
// Monitor::new()
let broadcast_cap = 4096;
let (event_broadcast_tx, _) = tokio::sync::broadcast::channel(broadcast_cap);
self.event_broadcast_tx = Some(event_broadcast_tx);
```

> 注意：broadcast channel 的 sender 即使在无 receiver 时也正常工作（`send` 不阻塞，返回 `Err`）

### 步骤 3：process_event_batch 推送事件到 broadcast

在现有 `write_event_for_opts` 之后加一行：

```rust
// 现有：写 JSONL 文件
if let Err(e) = self.write_event_for_opts(&event, opts) {
    eprintln!("[ERROR] Failed to write event: {}", e);
}

// 新增：推送给订阅者（忽略无订阅者的错误）
if let Some(ref tx) = self.event_broadcast_tx {
    let _ = tx.send(event.clone());
}
```

`let _` 丢弃 `SendError`。当无 subscriber 时 `send` 返回 `Err(FileEvent)`，忽略即可。

### 步骤 4：SocketCmd 支持 subscribe 命令

**无需修改 SocketCmd 结构体**。只需在 socket handler 里识别 `cmd == "subscribe"`。

### 步骤 5：subscribe 处理逻辑（main loop）

**文件**：`src/monitor.rs`，`run()` 的 `accept_result` 分支

当前逻辑：
```rust
accept_result => {
    Ok((mut writer, cmd_str)) => {
        let resp = ...;                    // 解析命令，调用 handle_socket_cmd
        writer.write_all(resp).await;      // 写入 TOML 响应
        // ← 函数结束，writer drop，连接关闭
    }
}
```

改为：
```rust
accept_result => {
    Ok((writer, cmd_str)) => {
        let cmd = match toml::from_str::<SocketCmd>(&cmd_str) {
            Ok(c) => c,
            Err(e) => {
                let resp = SocketResp::err(format!("Invalid command: {e}"));
                let _ = tokio_toml_resp(writer, &resp).await;
                continue;
            }
        };
        
        if cmd.cmd == "subscribe" {
            self.handle_subscribe(writer, &cmd);
        } else {
            let resp = self.handle_socket_cmd(cmd);
            let _ = tokio_toml_resp(writer, &resp).await;
        }
    }
}
```

### 步骤 6：handle_subscribe 实现

```rust
fn handle_subscribe(
    &self,
    writer: tokio::net::unix::OwnedWriteHalf,
    cmd: &SocketCmd,
) {
    let tx = match self.event_broadcast_tx.as_ref() {
        Some(tx) => tx,
        None => {
            let resp = SocketResp::permanent_err("subscriptions disabled");
            // 需要在 async context 里写响应，这里 spawn 一个小 task
            tokio::spawn(write_resp_and_close(writer, resp));
            return;
        }
    };

    let rx = tx.subscribe();
    let track_cmd = cmd.track_cmd.clone();
    let types: Option<Vec<EventType>> = cmd.types.as_ref().map(|v| {
        v.iter().filter_map(|t| t.parse().ok()).collect()
    });

    tokio::spawn(subscriber_task(writer, rx, track_cmd, types));
}
```

### 步骤 7：subscriber_task（核心流逻辑）

```rust
async fn subscriber_task(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut rx: tokio::sync::broadcast::Receiver<FileEvent>,
    track_cmd: Option<String>,
    type_filter: Option<Vec<EventType>>,
) {
    use tokio::io::AsyncWriteExt;

    // 1. 发送初始 ok 响应（TOML）
    let resp = SocketResp::ok();
    let resp_str = toml::to_string(&resp).unwrap_or_default();
    if writer.write_all(format!("{}\n", resp_str).as_bytes()).await.is_err() {
        return;
    }

    // 2. 流式发送事件
    loop {
        match rx.recv().await {
            Ok(event) => {
                // 可选过滤：按 cmd group
                if let Some(ref wanted) = track_cmd {
                    if event.chain.is_empty() || !chains_contain(&event.chain, wanted) {
                        continue;
                    }
                }
                // 可选过滤：按事件类型
                if let Some(ref allowed) = type_filter {
                    if !allowed.contains(&event.event_type) {
                        continue;
                    }
                }

                let line = event.to_jsonl_string() + "\n";
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break; // 订阅者断连
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // 订阅者太慢，丢了 n 条事件 — 发一条警告
                let warn = format!(
                    r#"{{"warning":"subscriber too slow, dropped {} events","path":""}}"# + "\n",
                    n
                );
                let _ = writer.write_all(warn.as_bytes()).await;
                // 继续接收，看看能不能追上来
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break; // daemon 关闭了
            }
        }
    }
    // writer drop → 连接关闭
}
```

辅助函数（cmd group 匹配）：
```rust
fn chains_contain(chain: &str, cmd_name: &str) -> bool {
    chain.split(" → ").any(|s| s.trim() == cmd_name)
}
```

### 步骤 8：CLI 订阅命令（可选，便利性）

新增 `fsmon subscribe` 子命令：

```bash
fsmon subscribe                    # 订阅全部事件
fsmon subscribe --cmd myapp         # 订阅 myapp cmd group
fsmon subscribe --types CLOSE_WRITE,DELETE  # 仅这些类型
```

**文件**：`src/bin/commands/subscribe.rs` + 注册到 `mod.rs` + `fsmon.rs`

这步是让用户不必手写 `nc`。可选——可以先不加，直接文档里教 `nc`。

---

## 背压策略

| 场景 | 行为 |
|------|------|
| 订阅者处理能力 ≥ 事件产生速度 | 正常 |
| 订阅者慢 | broadcast channel buffer 满后**丢弃最老的事件**，receiver 收到 `Lagged(n)`，发一条 JSON 警告到订阅者流 |
| 订阅者断连 | task 的 `write_all` 失败 → break → task 退出 |
| 无订阅者 | `tx.send()` 返回 `Err` → 忽略，不影响正常写 JSONL 文件 |

不做阻塞 backpressure——不能因为一个慢订阅者影响 daemon 的核心事件记录。

---

## 文件变更列表

| 文件 | 变更 |
|------|------|
| `src/monitor.rs` | 加 `event_broadcast_tx` 字段、初始化、`process_event_batch` 推送、main loop 中 handle subscribe、subscriber_task |
| `src/socket.rs` | 无需变更（SocketCmd 字段已够用） |
| `src/lib.rs` | 无变更（FileEvent 已由 serde 可序列化） |
| `src/bin/commands/subscribe.rs` | **可选**：CLI 便利子命令 |
| `src/bin/commands/mod.rs` | **可选**：注册 subscribe 子命令 |
| `Cargo.toml` | 无新依赖（只用了 tokio::sync::broadcast） |
| `plan.md` | 本文件 |

---

## 测试计划

1. **单元测试**：`subscriber_task` 独立可测试——给它一个 broadcast channel + 内存 writer，验证：
   - 正常事件流输出
   - cmd/type 过滤
   - Lagged 场景（填充 buffer 使其溢出）
   - Closed 场景（drop sender）

2. **集成测试**：用 `fsmon subscribe` 连接真实 daemon，发几个事件（`echo hello > /tmp/test`），验证收到 JSONL 行。

---

## 不做的事（明确界限）

- ❌ 不做 protobuf/Avro/Kafka 输出 — 那是外部 adapter 的职责
- ❌ 不做 daemon 侧复杂过滤 DSL — 只支持 cmd group + event types
- ❌ 不做多路复用（一个连接一个订阅） — 如果真需要多 cmd group 在一条连接上，以后加 `subscribe_cmds: [String]`
