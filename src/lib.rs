//! fsmon - 轻量级高性能文件系统监控工具
//!
//! # 概述
//!
//! fsmon 是一个基于 fanotify 的 Linux 文件系统监控工具，提供以下功能：
//!
//! - **实时监控**：监控文件系统的创建、修改、删除、移动等事件
//! - **事件过滤**：支持按路径、事件类型、文件大小等进行过滤
//! - **日志记录**：将事件记录到 JSONL 格式的日志文件中
//! - **历史查询**：支持查询历史事件记录
//! - **守护进程**：支持作为 systemd 服务运行
//!
//! # 使用示例
//!
//! ```ignore
//! let config = MonitorConfig {
//!     paths_and_options: vec![(PathBuf::from("/home/user"), PathOptions::default())],
//!     log_dir: Some(PathBuf::from("/var/log/fsmon")),
//!     debug: true,
//!     ..Default::default()
//! };
//!
//! // let monitor = Monitor::new(config)?;
//! ```
//!
//! # 模块结构
//!
//! - [`common`] - 核心功能模块，包含所有主要类型和逻辑
//!
//! # 平台支持
//!
//! 目前仅支持 Linux 系统（需要 fanotify 支持）。

pub mod common;
