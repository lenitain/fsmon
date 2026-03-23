//! Proc Connector 进程缓存
//!
//! 通过 Linux netlink proc connector 监听进程 exec 事件，
//! 在进程执行时立即缓存 PID → (cmd, user) 映射。
//! 解决 fanotify 事件中短命进程（touch, rm, mv 等）已退出导致
//! /proc/{pid} 不可读的问题。

use dashmap::DashMap;
use std::sync::Arc;

// ---- Netlink / Proc Connector 常量 ----

const NETLINK_CONNECTOR: libc::c_int = 11;
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;
const PROC_EVENT_EXEC: u32 = 0x00000002;

const NLMSG_HDR_SIZE: usize = std::mem::size_of::<libc::nlmsghdr>();
/// cn_msg header: cb_id(8) + seq(4) + ack(4) + len(2) + flags(2)
const CN_MSG_HDR_SIZE: usize = 20;

// ---- 公开类型 ----

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub cmd: String,
    pub user: String,
}

pub type ProcCache = Arc<DashMap<u32, ProcInfo>>;

/// 启动 proc connector 监听线程，返回共享缓存。
/// 在进程 exec() 时立即读取 /proc/{pid} 信息并缓存，
/// 确保短命进程的信息在 fanotify 事件处理时可用。
pub fn start_proc_listener() -> ProcCache {
    let cache: ProcCache = Arc::new(DashMap::new());
    let cache_clone = cache.clone();

    std::thread::Builder::new()
        .name("proc-connector".into())
        .spawn(move || {
            if let Err(e) = run_listener(cache_clone) {
                eprintln!("proc connector listener failed: {}", e);
            }
        })
        .ok();

    cache
}

// ---- 内部实现 ----

fn run_listener(cache: ProcCache) -> anyhow::Result<()> {
    let sock = unsafe {
        libc::socket(
            libc::PF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_CONNECTOR,
        )
    };
    if sock < 0 {
        anyhow::bail!(
            "socket(NETLINK_CONNECTOR): {}",
            std::io::Error::last_os_error()
        );
    }

    // 确保 sock 在任何退出路径都关闭
    let _guard = SockGuard(sock);

    // 绑定到 proc connector 组
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
    addr.nl_pid = std::process::id();
    addr.nl_groups = CN_IDX_PROC;

    if unsafe {
        libc::bind(
            sock,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    } < 0
    {
        anyhow::bail!(
            "bind(NETLINK_CONNECTOR): {}",
            std::io::Error::last_os_error()
        );
    }

    // 订阅进程事件
    send_subscribe(sock)?;

    // 接收循环
    let mut buf = vec![0u8; 4096];
    loop {
        let n = unsafe {
            libc::recv(sock, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
        };

        if n <= 0 {
            // socket 关闭或错误 → 退出
            break;
        }

        handle_message(&buf[..n as usize], &cache);
    }

    Ok(())
}

/// 发送 PROC_CN_MCAST_LISTEN 订阅消息
fn send_subscribe(sock: libc::c_int) -> anyhow::Result<()> {
    let payload_len = CN_MSG_HDR_SIZE + 4; // cn_msg + u32 op
    let total_len = NLMSG_HDR_SIZE + payload_len;
    let mut msg = vec![0u8; total_len];

    // nlmsghdr
    msg[0..4].copy_from_slice(&(total_len as u32).to_ne_bytes()); // nlmsg_len
    msg[4..6].copy_from_slice(&(libc::NLMSG_DONE as u16).to_ne_bytes()); // nlmsg_type
    msg[6..8].copy_from_slice(&0u16.to_ne_bytes()); // nlmsg_flags
    msg[8..12].copy_from_slice(&0u32.to_ne_bytes()); // nlmsg_seq
    msg[12..16].copy_from_slice(&std::process::id().to_ne_bytes()); // nlmsg_pid

    // cn_msg
    let cn = NLMSG_HDR_SIZE;
    msg[cn..cn + 4].copy_from_slice(&CN_IDX_PROC.to_ne_bytes()); // cb_id.idx
    msg[cn + 4..cn + 8].copy_from_slice(&CN_VAL_PROC.to_ne_bytes()); // cb_id.val
    msg[cn + 8..cn + 12].copy_from_slice(&0u32.to_ne_bytes()); // seq
    msg[cn + 12..cn + 16].copy_from_slice(&0u32.to_ne_bytes()); // ack
    msg[cn + 16..cn + 18].copy_from_slice(&4u16.to_ne_bytes()); // len = sizeof(u32)
    msg[cn + 18..cn + 20].copy_from_slice(&0u16.to_ne_bytes()); // flags

    // payload: PROC_CN_MCAST_LISTEN
    let data = cn + CN_MSG_HDR_SIZE;
    msg[data..data + 4].copy_from_slice(&PROC_CN_MCAST_LISTEN.to_ne_bytes());

    let ret = unsafe {
        libc::send(sock, msg.as_ptr() as *const libc::c_void, msg.len(), 0)
    };
    if ret < 0 {
        anyhow::bail!(
            "send(PROC_CN_MCAST_LISTEN): {}",
            std::io::Error::last_os_error()
        );
    }

    Ok(())
}

/// 解析 netlink 消息，提取 EXEC 事件中的进程信息
fn handle_message(buf: &[u8], cache: &ProcCache) {
    // 最小长度：nlmsghdr + cn_msg header + proc_event header (what + cpu + timestamp)
    let min_len = NLMSG_HDR_SIZE + CN_MSG_HDR_SIZE + 16;
    if buf.len() < min_len {
        return;
    }

    // proc_event 起始偏移
    let ev_off = NLMSG_HDR_SIZE + CN_MSG_HDR_SIZE;

    // proc_event.what
    let what = u32::from_ne_bytes(
        buf[ev_off..ev_off + 4].try_into().unwrap_or([0; 4]),
    );

    if what == PROC_EVENT_EXEC {
        // exec event_data: { process_pid: i32, process_tgid: i32 }
        // 位于 proc_event 偏移 16（跳过 what(4) + cpu(4) + timestamp_ns(8)）
        let data_off = ev_off + 16;
        if data_off + 8 > buf.len() {
            return;
        }

        let pid = u32::from_ne_bytes(
            buf[data_off..data_off + 4].try_into().unwrap_or([0; 4]),
        );

        // 进程刚 exec()，/proc/{pid} 此时一定存在
        let cmd = std::fs::read_to_string(format!("/proc/{}/comm", pid))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let user = read_proc_uid(pid).unwrap_or_else(|| "unknown".to_string());

        cache.insert(pid, ProcInfo { cmd, user });
    }
}

fn read_proc_uid(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let uid: u32 = status
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    uid_to_username(uid)
}

fn uid_to_username(uid: u32) -> Option<String> {
    let uid_str = uid.to_string();
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for entry in passwd.lines() {
        let parts: Vec<&str> = entry.split(':').collect();
        if parts.len() >= 3 && parts[2] == uid_str {
            return Some(parts[0].to_string());
        }
    }
    Some(format!("uid:{}", uid_str))
}

/// RAII guard 确保 socket 关闭
struct SockGuard(libc::c_int);
impl Drop for SockGuard {
    fn drop(&mut self) {
        unsafe { libc::close(self.0); }
    }
}
