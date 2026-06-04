# fsmon RPC 设计改进计划

基于 niri-ipc 的优秀设计，总结对 fsmon 有帮助的改进点。

## 1. 类型安全的请求/响应系统

### 当前 fsmon 的问题
```rust
// 字符串匹配，运行时才发现错误
match cmd.cmd.as_str() {
    "add" => ...,
    "remove" => ...,
    _ => SocketResp::err(format!("Unknown command: {}", cmd.cmd)),
}
```

### niri 的做法
```rust
// 编译时保证类型安全
pub enum Request {
    Version,
    Outputs,
    Workspaces,
    Windows,
    Action(Action),
    EventStream,
    // ...
}

pub type Reply = Result<Response, String>;

pub enum Response {
    Handled,
    Version(String),
    Outputs(HashMap<String, Output>),
    // ...
}
```

### 改进建议
```rust
// fsmon 新设计
#[derive(Serialize, Deserialize)]
pub enum SocketCmd {
    Add { path: PathBuf, recursive: bool, types: Option<Vec<String>>, ... },
    Remove { path: PathBuf, track_cmd: Option<String> },
    List,
    Health,
    Subscribe { track_cmd: Option<String>, types: Option<Vec<String>>, ... },
}

pub type SocketReply = Result<SocketResponse, SocketError>;

pub enum SocketResponse {
    Ok,
    Paths(Vec<PathEntry>),
    Health(HealthInfo),
}

pub enum SocketError {
    Permanent(String),
    Transient(String),
}
```

## 2. 清晰的协议语义 ✅ 已完成

### 当前实现（已满足）
- 每个请求**恰好**得到一个响应（普通命令）
- 事件流通过 `Subscribe` 命令启动（先 OK 后流式）
- 每个连接处理一个命令（隐式并发限制）
- 已添加文档注释说明协议语义

```rust
/// # Protocol semantics
/// - Each command is processed independently
/// - Non-subscribe: send JSON → receive JSON → connection closes
/// - Subscribe: send JSON → receive OK → stream JSONL events
pub fn send_cmd(socket_path: &Path, cmd: &SocketCmd) -> Result<SocketResponse, SocketError> {
    // ...
}
```

## 3. 完整的事件系统

### niri 的做法
```rust
pub enum Event {
    WorkspacesChanged { workspaces: Vec<Workspace> },
    WindowOpenedOrChanged { window: Window },
    WindowClosed { id: u64 },
    // 每个事件携带完整状态或增量信息
}
```

### 改进建议
```rust
pub enum FsmonEvent {
    // 全量状态事件
    PathsChanged { paths: Vec<PathEntry> },
    
    // 增量事件
    FileEvent(FileEvent),
    
    // 状态变更事件
    HealthChanged { info: HealthInfo },
    
    // 错误事件
    Error { message: String, kind: ErrorKind },
}
```

## 4. 客户端辅助库

### niri 的做法
```rust
pub struct Socket {
    stream: BufReader<UnixStream>,
}

impl Socket {
    pub fn connect() -> io::Result<Self> { ... }
    pub fn send(&mut self, request: Request) -> io::Result<Reply> { ... }
    pub fn read_events(self) -> impl FnMut() -> io::Result<Event> { ... }
}
```

### 改进建议
```rust
// fsmon 客户端辅助
pub struct FsmonClient {
    socket_path: PathBuf,
}

impl FsmonClient {
    pub fn connect() -> Result<Self> { ... }
    pub fn add(&mut self, path: &Path, cmd: &str, opts: AddOpts) -> Result<()> { ... }
    pub fn remove(&mut self, path: &Path, cmd: Option<&str>) -> Result<()> { ... }
    pub fn list(&mut self) -> Result<Vec<PathEntry>> { ... }
    pub fn health(&mut self) -> Result<HealthInfo> { ... }
    pub fn subscribe(&mut self) -> Result<EventStream> { ... }
}

pub struct EventStream { ... }

impl Iterator for EventStream {
    type Item = FsmonEvent;
    fn next(&mut self) -> Option<Self::Item> { ... }
}
```

## 5. 错误分类与处理

### niri 的做法
- 错误是简单的 `String`
- 客户端可以解析错误信息

### fsmon 已有优势
```rust
pub enum ErrorKind {
    Permanent,  // 重启也不会修复
    Transient,  // 运行时问题
}
```

### 改进建议
保留 `ErrorKind`，但可以扩展错误类型：
```rust
pub enum SocketErrorKind {
    Permanent(PermanentError),
    Transient(TransientError),
}

pub enum PermanentError {
    PathConflict { path: PathBuf, conflicting: PathBuf },
    InvalidConfig(String),
    LogDirectoryConflict { path: PathBuf },
}

pub enum TransientError {
    DaemonNotRunning,
    SocketConnectionFailed(String),
    FanotifyError(String),
}
```

## 6. 版本兼容性策略(这个暂时不考虑)

### niri 的做法
```rust
/// This crate follows the niri version. It is **not** API-stable in terms of the Rust semver.
/// Use an exact version requirement to avoid breaking changes:
/// [dependencies]
/// niri-ipc = "=26.4.0"
```

### 改进建议
在 fsmon-ipc crate 中明确版本策略：
```rust
/// fsmon-ipc follows fsmon's version.
/// For stability, use exact version: fsmon-ipc = "=0.4.3"
```

## 7. JSON Schema 支持

### niri 的做法
```rust
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Output { ... }
```

### 改进建议
为 fsmon-ipc 添加 `json-schema` feature：
```rust
[features]
json-schema = ["schemars"]

[dependencies]
schemars = { version = "0.8", optional = true }
```

## 实施计划

### Phase 1: 类型安全的 Socket 协议 ✅ 已完成
- [x] 定义 `SocketCmd` 枚举（替代字符串命令）
- [x] 定义 `SocketResponse` 枚举
- [x] 定义 `SocketError` 枚举（Permanent/Transient）
- [x] 协议格式从 TOML 升级为 JSON
- [x] 更新 daemon 端（socket_handler.rs, mod.rs）
- [x] 更新 CLI 端（add.rs, remove.rs, health.rs）
- [x] 更新 extensions 示例（subscribe.sh, subscribe.py）
- [x] 更新所有测试用例

### Phase 2: 定义 FsmonEvent 事件枚举
- [ ] 定义 `FsmonEvent` 枚举
- [ ] 更新事件广播使用 `FsmonEvent`
- [ ] 保持向后兼容（可选）

### Phase 3: 实现客户端库
- [ ] 实现 `FsmonClient` 结构
- [ ] 实现 `EventStream` 迭代器
- [ ] 添加文档和示例

### Phase 4: JSON Schema 支持
- [ ] 添加 `json-schema` feature
- [ ] 使用 schemars 派生 JsonSchema

## 参考资源

- niri-ipc: `/home/pilot/.cloned/niri/niri-ipc/src/lib.rs`
- niri socket helper: `/home/pilot/.cloned/niri/niri-ipc/src/socket.rs`
- 当前 fsmon socket: `/home/pilot/.projects/fsmon/src/socket.rs`
