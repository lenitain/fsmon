//! Proc Connector Process Cache
//!
//! Listens to process exec events via Linux netlink proc connector,
//! caches PID -> (cmd, user) mapping immediately when process executes.
//! Solves the problem where short-lived processes (touch, rm, mv etc.)
//! cause /proc/{pid} to be unreadable when fanotify events arrive.

use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::utils::uid_to_username;

// ---- Netlink / Proc Connector Constants ----

const NETLINK_CONNECTOR: libc::c_int = 11;
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;
const PROC_EVENT_EXEC: u32 = 0x00000002;
const NETLINK_RECV_BUF_SIZE: usize = 4096;

const NLMSG_HDR_SIZE: usize = std::mem::size_of::<libc::nlmsghdr>();
/// cn_msg header: cb_id(8) + seq(4) + ack(4) + len(2) + flags(2)
const CN_MSG_HDR_SIZE: usize = 20;

// ---- Public Types ----

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub cmd: String,
    pub user: String,
}

pub type ProcCache = Arc<DashMap<u32, ProcInfo>>;

/// Start proc connector listener thread, returns shared cache and a readiness flag.
/// The flag is set to `true` once netlink subscription succeeds, so callers can
/// avoid a fixed sleep and instead poll the flag with a timeout.
pub fn start_proc_listener() -> (ProcCache, Arc<AtomicBool>) {
    let cache: ProcCache = Arc::new(DashMap::new());
    let cache_clone = cache.clone();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();

    std::thread::Builder::new()
        .name("proc-connector".into())
        .spawn(move || {
            if let Err(e) = run_listener(cache_clone, ready_clone) {
                eprintln!("proc connector listener failed: {}", e);
            }
        })
        .ok();

    (cache, ready)
}

// ---- Internal Implementation ----

fn run_listener(cache: ProcCache, ready: Arc<AtomicBool>) -> anyhow::Result<()> {
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

    // Ensure socket is closed on any exit path
    let _guard = SockGuard(sock);

    // Bind to proc connector group
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

    // Subscribe to process events
    send_subscribe(sock)?;

    // Signal readiness: subscription complete, safe to process fanotify events
    ready.store(true, Ordering::Release);

    // Receive loop
    let mut buf = vec![0u8; NETLINK_RECV_BUF_SIZE];
    loop {
        let n = unsafe { libc::recv(sock, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };

        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // Other errors -> exit
            break;
        }
        if n == 0 {
            // Socket closed
            break;
        }

        handle_message(&buf[..n as usize], &cache);
    }

    Ok(())
}

/// Send PROC_CN_MCAST_LISTEN subscription message
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

    let ret = unsafe { libc::send(sock, msg.as_ptr() as *const libc::c_void, msg.len(), 0) };
    if ret < 0 {
        anyhow::bail!(
            "send(PROC_CN_MCAST_LISTEN): {}",
            std::io::Error::last_os_error()
        );
    }

    Ok(())
}

/// Parse netlink message, extract process info from EXEC events
fn handle_message(buf: &[u8], cache: &ProcCache) {
    // Minimum length: nlmsghdr + cn_msg header + proc_event header (what + cpu + timestamp)
    let min_len = NLMSG_HDR_SIZE + CN_MSG_HDR_SIZE + 16;
    if buf.len() < min_len {
        return;
    }

    // proc_event offset
    let ev_off = NLMSG_HDR_SIZE + CN_MSG_HDR_SIZE;

    // proc_event.what
    let what = u32::from_ne_bytes(buf[ev_off..ev_off + 4].try_into().unwrap_or([0; 4]));

    if what == PROC_EVENT_EXEC {
        // exec event_data: { process_pid: i32, process_tgid: i32 }
        // At proc_event offset 16 (skip what(4) + cpu(4) + timestamp_ns(8))
        let data_off = ev_off + 16;
        if data_off + 8 > buf.len() {
            return;
        }

        let pid = u32::from_ne_bytes(buf[data_off..data_off + 4].try_into().unwrap_or([0; 4]));

        // Process just exec()'d, /proc/{pid} must exist
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

/// RAII guard to ensure socket is closed
struct SockGuard(libc::c_int);
impl Drop for SockGuard {
    fn drop(&mut self) {
        let _ = nix::unistd::close(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_cache_insert_and_get() {
        let cache: ProcCache = Arc::new(DashMap::new());
        cache.insert(
            12345,
            ProcInfo {
                cmd: "test_process".to_string(),
                user: "testuser".to_string(),
            },
        );

        let info = cache.get(&12345);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.cmd, "test_process");
        assert_eq!(info.user, "testuser");
    }

    #[test]
    fn test_proc_cache_missing_pid() {
        let cache: ProcCache = Arc::new(DashMap::new());
        assert!(cache.get(&99999).is_none());
    }

    #[test]
    fn test_proc_cache_overwrite() {
        let cache: ProcCache = Arc::new(DashMap::new());
        cache.insert(
            1,
            ProcInfo {
                cmd: "old".into(),
                user: "a".into(),
            },
        );
        cache.insert(
            1,
            ProcInfo {
                cmd: "new".into(),
                user: "b".into(),
            },
        );

        let info = cache.get(&1).unwrap();
        assert_eq!(info.cmd, "new");
        assert_eq!(info.user, "b");
    }

    #[test]
    fn test_proc_cache_concurrent_access() {
        use std::thread;

        let cache: ProcCache = Arc::new(DashMap::new());
        let mut handles = vec![];

        for i in 0..10 {
            let cache_clone = cache.clone();
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    let pid = (i * 100 + j) as u32;
                    cache_clone.insert(
                        pid,
                        ProcInfo {
                            cmd: format!("proc_{}", pid),
                            user: "test".into(),
                        },
                    );
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(cache.len(), 1000);
    }

    // ---- Integration tests (require sudo) ----

    #[test]
    #[ignore]
    fn test_netlink_socket_create() {
        let sock = unsafe {
            libc::socket(
                libc::PF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                NETLINK_CONNECTOR,
            )
        };
        assert!(
            sock >= 0,
            "Should be able to create NETLINK_CONNECTOR socket with root"
        );
        unsafe {
            libc::close(sock);
        }
    }

    #[test]
    #[ignore]
    fn test_netlink_bind() {
        let sock = unsafe {
            libc::socket(
                libc::PF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                NETLINK_CONNECTOR,
            )
        };
        assert!(sock >= 0);

        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        addr.nl_pid = std::process::id();
        addr.nl_groups = CN_IDX_PROC;

        let ret = unsafe {
            libc::bind(
                sock,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        assert_eq!(ret, 0, "Should be able to bind to proc connector with root");

        unsafe {
            libc::close(sock);
        }
    }

    #[test]
    #[ignore]
    fn test_proc_listener_receives_events() {
        let (cache, _ready) = start_proc_listener();

        // Spawn a short-lived process that will trigger PROC_EVENT_EXEC
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Create a subprocess
        let mut child = std::process::Command::new("echo")
            .arg("test")
            .spawn()
            .unwrap();
        child.wait().unwrap();

        // Wait for event to be cached
        std::thread::sleep(std::time::Duration::from_millis(200));

        // The proc connector should have captured the exec event for our process
        // Note: due to timing, this might not always capture the exact pid,
        // but it should have captured some events
        assert!(
            !cache.is_empty(),
            "Proc cache should have received some events"
        );
    }
}
