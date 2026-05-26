//! fsmon 订阅示例 — 通过 Unix socket 接收实时事件。
//!
//! 与 daemon 内置 FileLogWriter 逻辑等价，作为一个独立工具运行。
//!
//! 用法：
//!   cargo run --example fsmon-subscriber-example
//!   或编译后直接运行：
//!   rustc extensions/fsmon-subscriber-example.rs -o fsmon-subscriber
//!   ./fsmon-subscriber
//!
//! 需要 Cargo.toml 依赖：
//!   tokio = { version = "1", features = ["full"] }
//!   serde_json = "1.0"
//!   serde = { version = "1.0", features = ["derive"] }

use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::interval;

/// 简单的 FileEvent 结构（仅反序列化需要的字段）
#[derive(serde::Deserialize)]
struct FileEvent {
    event_type: String,
    path: PathBuf,
    pid: u32,
    cmd: String,
    chain: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/fsmon-1000.sock".to_string());

    let log_dir = std::env::args()
        .nth(2)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            format!("{}/.local/state/fsmon", home)
        });

    let log_dir = PathBuf::from(&log_dir);
    tokio::fs::create_dir_all(&log_dir).await?;

    // 连接 daemon
    let stream = UnixStream::connect(&socket_path).await?;
    println!("[subscriber] Connected to {}", socket_path);

    // 发送 subscribe 命令
    let sub_cmd = b"cmd = \"subscribe\"\n\n";
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(sub_cmd).await?;
    // drop writer so reader can see EOF when server closes
    // (not needed for subscribe since server keeps writing)

    // 读取 TOML ok 响应
    let mut buf_reader = BufReader::new(reader_half);
    let mut first_line = String::new();
    buf_reader.read_line(&mut first_line).await?;
    if !first_line.contains("ok = true") {
        eprintln!("[ERROR] Subscribe failed: {}", first_line.trim());
        return Ok(());
    }
    println!("[subscriber] Subscribe OK, listening... (Ctrl+C to stop)");

    // 可选：定期 fdatasync（每 5 秒）
    let sync_log_dir = log_dir.clone();
    tokio::spawn(async move {
        let mut timer = interval(Duration::from_secs(5));
        loop {
            timer.tick().await;
            // 简单 sync：不做脏页追踪，全部 fdatasync
            if let Ok(mut dir) = tokio::fs::read_dir(&sync_log_dir).await {
                while let Ok(Some(entry)) = dir.next_entry().await {
                    if entry.path().extension().map_or(false, |e| e == "jsonl") {
                        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(entry.path()) {
                            let _ = f.sync_data();
                        }
                    }
                }
            }
        }
    });

    // 逐行读取事件并写入文件
    let mut line = String::new();
    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break; // daemon 关闭
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // 跳过警告行
        if line.contains(r#""warning""#) {
            eprintln!("[WARNING] {}", line);
            continue;
        }

        // 解析事件
        let event: FileEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[ERROR] Failed to parse event: {}", e);
                continue;
            }
        };

        // 从 chain 推断 cmd group
        let cmd_name = if event.chain.contains(" → ") {
            event.chain.split(" → ").last().unwrap_or("_global").trim()
        } else {
            "_global"
        };

        // 写入 {cmd}_log.jsonl
        let log_path = log_dir.join(format!("{}_log.jsonl", cmd_name));
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await?;
        file.write_all(format!("{}\n", line).as_bytes()).await?;

        if log_path.to_string_lossy().contains("test") {
            println!("[{}] {} ({})", event.event_type, event.path.display(), cmd_name);
        }
    }

    println!("[subscriber] Daemon disconnected, exiting.");
    Ok(())
}
