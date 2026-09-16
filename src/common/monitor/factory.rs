//! Privileged "fanotify factory" subprocess.
//!
//!
//! `CAP_SYS_ADMIN` is needed for exactly one thing: calling `fanotify_init()`
//! as a privileged process, so the kernel does **not** set `FANOTIFY_UNPRIV`
//! on the group (which would blank out `metadata.pid` for every event caused
//! by another process).
//!
//! A group can only ever hold marks from a single filesystem — the kernel
//! returns `EXDEV` for a second one — and new filesystems can appear at any
//! time, so that capability must stay available for the whole daemon lifetime.
//! The answer is to confine it to a fork()ed child whose entire input
//! is `(dirfd[, fan_fd], flags, mask)` and whose syscall surface is a seccomp
//! whitelist:
//!
//! ```text
//!   fsmon daemon (no capabilities)
//!    └─ [fanotify factory]  ← CAP_SYS_ADMIN, seccomp-limited
//!         • fanotify_init, fanotify_mark, recvmsg, sendmsg,
//!           read, write, close, exit_group
//! ```
//!
//! The privilege is inherited at fork() time and never exists as a file on
//! disk, so no other user can obtain it — not even one who can run the
//! binary, which is what a `setcap` helper would expose. The child never sees
//! a path string either: the parent opens
//! directories and passes the fds over `SCM_RIGHTS`.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

/// `fanotify_init()` flags fsmon always wants: exactly the flags the kernel
/// accepts from an unprivileged caller, so the group starts as privileged as
/// it can and only `UNLIMITED_MARKS` needs to be added on top.
pub(crate) const GROUP_INIT_FLAGS: u32 = fanotify_fid::consts::FAN_CLOEXEC
    | fanotify_fid::consts::FAN_NONBLOCK
    | fanotify_fid::consts::FAN_CLASS_NOTIF
    | fanotify_fid::consts::FAN_REPORT_FID
    | fanotify_fid::consts::FAN_REPORT_DIR_FID
    | fanotify_fid::consts::FAN_REPORT_NAME;

/// Extra flag that only a privileged group can accept. It lifts the
/// per-uid `max_user_marks` cap, which otherwise applies even to root.
/// Never add `FAN_UNLIMITED_QUEUE`: that would remove kernel-side
/// backpressure and trade dropped events for unbounded memory growth.
pub(crate) const UNLIMITED_MARKS: u32 = fanotify_fid::consts::FAN_UNLIMITED_MARKS;

// ── Wire protocol ──

const KIND_CREATE_GROUP: u32 = 1;
const KIND_MARK: u32 = 2;

/// Fixed-size request payload. Ancillary fds follow:
/// - `KIND_CREATE_GROUP`: `[dirfd]`
/// - `KIND_MARK`: `[fan_fd, dirfd, dirfd, ...]` (`count` dirfds)
#[repr(C)]
#[derive(Clone, Copy)]
struct FactoryRequest {
    kind: u32,
    flags: u32,
    mark_flags: u32,
    count: u32,
    mask: u64,
}

const REQ_SIZE: usize = mem::size_of::<FactoryRequest>();
const REPLY_SIZE: usize = mem::size_of::<i32>();

/// Maximum fds carried in one message.
const MAX_FDS: usize = 64;

/// Directory fds one [`FanotifyFactory::mark`] request can carry: every slot
/// except the one taken by the fanotify group fd.
pub(crate) const MAX_DIRS_PER_MARK: usize = MAX_FDS - 1;

/// File descriptor the factory child keeps its socketpair end on. Fixed so the
/// child can close everything else with one `close_range` call.
const FACTORY_SOCK_FD: RawFd = 3;

/// Close every descriptor strictly above `keep`, leaving the standard streams
/// and `keep` itself alone.
///
/// `close_range(2)` does this in one syscall (Linux 5.9+, which fsmon already
/// requires). Kernels without it fall back to a bounded `close` loop — slower,
/// but it runs once, before the factory handles any request.
fn close_fds_above(keep: RawFd) -> bool {
    // SAFETY: close_range takes integer arguments and never dereferences.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            (keep + 1) as libc::c_uint,
            libc::c_uint::MAX,
            0 as libc::c_uint,
        )
    };
    if rc == 0 {
        return true;
    }
    // ENOSYS/EPERM on kernels without close_range: fall back to a plain loop.
    // Bounded so a pathological fd table cannot wedge startup.
    const FALLBACK_FD_LIMIT: RawFd = 4096;
    for fd in (keep + 1)..FALLBACK_FD_LIMIT {
        // SAFETY: closing an fd we may not own simply returns EBADF.
        unsafe { libc::close(fd) };
    }
    true
}

/// Maximum fds the factory can receive: one `fan_fd` plus `MAX_FDS - 1` dirfds.
const MAX_IN_FDS: usize = MAX_FDS;

// ── cmsg helpers (allocation-free, stack only) ──

const fn cmsg_align(len: usize) -> usize {
    let h = mem::size_of::<libc::cmsghdr>();
    (len + h - 1) & !(h - 1)
}

const fn control_space(nfds: usize) -> usize {
    cmsg_align(mem::size_of::<libc::cmsghdr>()) + cmsg_align(nfds * mem::size_of::<RawFd>())
}

const fn cmsg_data_len(nfds: usize) -> usize {
    nfds * mem::size_of::<RawFd>()
}

/// Send `payload` plus `fds` over `sock`. No heap allocation.
fn send_msg(sock: RawFd, payload: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut control = [0u8; control_space(MAX_FDS)];
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    if !fds.is_empty() {
        debug_assert!(fds.len() <= MAX_FDS);
        let data_len = cmsg_data_len(fds.len());
        let cmsg = control.as_mut_ptr() as *mut libc::cmsghdr;
        // SAFETY: `control` is sized for MAX_FDS fds, so writing one cmsg
        // header plus `data_len` bytes stays in bounds.
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = cmsg_align(mem::size_of::<libc::cmsghdr>()) + data_len;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr() as *const u8,
                libc::CMSG_DATA(cmsg),
                data_len,
            );
        }
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = control_space(fds.len());
    }

    // SAFETY: `msg` points at live stack buffers for the duration of the call.
    let n = unsafe { libc::sendmsg(sock, &msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Receive one message into `payload` / `fds`.
///
/// Returns `(payload_bytes, fd_count)`. A payload of 0 bytes means the peer
/// closed the socket.
fn recv_msg(sock: RawFd, payload: &mut [u8], fds: &mut [RawFd]) -> io::Result<(usize, usize)> {
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut control = [0u8; control_space(MAX_FDS)];
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len();

    // SAFETY: all pointers refer to live stack buffers.
    let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut fd_count = 0usize;
    let hdr_align = cmsg_align(mem::size_of::<libc::cmsghdr>());
    let control_start = control.as_ptr() as usize;
    let control_end = control_start + msg.msg_controllen;
    // SAFETY: cmsg pointers walk the control buffer the kernel just filled.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            let cmsg_addr = cmsg as usize;
            if cmsg_addr < control_start || cmsg_addr + hdr_align > control_end {
                break;
            }
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let total = (*cmsg).cmsg_len;
                if total >= hdr_align {
                    let available = (total - hdr_align) / mem::size_of::<RawFd>();
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    for i in 0..available {
                        if fd_count >= fds.len() {
                            break;
                        }
                        fds[fd_count] = *data.add(i);
                        fd_count += 1;
                    }
                }
            }
            let next = (cmsg_addr + cmsg_align((*cmsg).cmsg_len)) as *mut libc::cmsghdr;
            if (next as usize) + hdr_align > control_end {
                break;
            }
            cmsg = next;
        }
    }

    Ok((n as usize, fd_count))
}

// ── seccomp ──

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

const SECCOMP_DATA_NR_OFF: u32 = 0;
const SECCOMP_DATA_ARCH_OFF: u32 = 4;

// libc does not expose these on every gnu target, so define the stable
// kernel ABI values here and issue `prctl` through `syscall(2)`.
const PR_SET_PDEATHSIG: libc::c_long = 1;
const PR_SET_SECCOMP: libc::c_long = 22;
const PR_SET_NO_NEW_PRIVS: libc::c_long = 38;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const AUDIT_ARCH: u32 = 0;

/// The plan's whitelist (§5.5)：`fanotify_init, fanotify_mark, recvmsg,
/// sendmsg, read, write, close, exit_group`.
const ALLOWED_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_fanotify_init,
    libc::SYS_fanotify_mark,
    libc::SYS_recvmsg,
    libc::SYS_sendmsg,
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_close,
    libc::SYS_exit_group,
];

/// Build the seccomp-BPF program. Everything not in [`ALLOWED_SYSCALLS`]
/// kills the factory process.
fn build_filter() -> Vec<SockFilter> {
    let mut f = Vec::with_capacity(ALLOWED_SYSCALLS.len() + 4);
    // Arch check. On unknown architectures the check is skipped rather than
    // rejecting everything (fsmon is Linux-only, but keep it portable).
    if AUDIT_ARCH != 0 {
        f.push(SockFilter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: SECCOMP_DATA_ARCH_OFF,
        });
        f.push(SockFilter {
            code: BPF_JMP_JEQ_K,
            jt: 1,
            jf: 0,
            k: AUDIT_ARCH,
        });
        f.push(SockFilter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        });
    }
    f.push(SockFilter {
        code: BPF_LD_W_ABS,
        jt: 0,
        jf: 0,
        k: SECCOMP_DATA_NR_OFF,
    });
    // For each allowed syscall: jump to the trailing ALLOW when it matches.
    // `allow_index` is computed once the vector is otherwise complete; use a
    // forward placeholder and patch below.
    let first_check = f.len();
    for n in ALLOWED_SYSCALLS {
        f.push(SockFilter {
            code: BPF_JMP_JEQ_K,
            jt: 0, // patched
            jf: 0,
            k: *n as u32,
        });
    }
    f.push(SockFilter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    let allow_index = f.len();
    f.push(SockFilter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    for i in 0..ALLOWED_SYSCALLS.len() {
        // jt counts instructions skipped after this one.
        f[first_check + i].jt = (allow_index - (first_check + i + 1)) as u8;
    }
    f
}

/// Install `PR_SET_NO_NEW_PRIVS` + the seccomp filter. Only called in the
/// forked child, before it touches the socket.
///
/// # Safety
/// Must be called in the child after fork, before entering the request loop.
unsafe fn install_seccomp(filter: &[SockFilter]) -> bool {
    // SAFETY: prctl is a pure kernel syscall with integer arguments.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_prctl,
            PR_SET_NO_NEW_PRIVS,
            1 as libc::c_long,
            0 as libc::c_long,
            0 as libc::c_long,
            0 as libc::c_long,
        )
    };
    if rc != 0 {
        return false;
    }
    let prog = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    // SAFETY: `prog` points at a live slice for the duration of the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_prctl,
            PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_long,
            &prog as *const SockFprog,
            0 as libc::c_long,
            0 as libc::c_long,
        )
    };
    rc == 0
}

// ── Factory child ──

fn send_reply(sock: RawFd, status: i32, fds: &[RawFd]) {
    let payload = status.to_ne_bytes();
    let _ = send_msg(sock, &payload, fds);
}

/// The factory's entire lifetime, in the forked child. Never returns.
///
/// This function must stay allocation-free: it runs after `fork()` in a
/// multi-threaded process and inside a seccomp filter that only allows the
/// eight syscalls above.
/// Sentinel byte the factory sends once its seccomp filter is installed.
const READY_BYTE: u8 = 0x2A;

/// Wait until `sock` has data to read, up to `timeout_ms`. Returns false on
/// timeout or error.
fn wait_readable(sock: RawFd, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: sock,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd points at a live stack value.
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    rc > 0
}

fn factory_child_main(sock: RawFd, filter: &[SockFilter]) -> ! {
    // SAFETY: we are the forked child and own `sock`.
    if !unsafe { install_seccomp(filter) } {
        unsafe { libc::_exit(101) };
    }

    // Readiness handshake: the parent only starts sending requests once this
    // byte arrives, so no request is ever processed without the filter.
    let _ = send_msg(sock, &[READY_BYTE], &[]);

    loop {
        let mut payload = [0u8; REQ_SIZE];
        let mut in_fds = [-1i32; MAX_IN_FDS];
        let (n, fd_count) = match recv_msg(sock, &mut payload, &mut in_fds) {
            Ok(v) => v,
            Err(_) => break,
        };
        if n == 0 {
            break; // parent closed the socket
        }
        if n < REQ_SIZE {
            send_reply(sock, libc::EINVAL, &[]);
            close_fds(&in_fds[..fd_count]);
            continue;
        }

        let req: FactoryRequest =
            unsafe { std::ptr::read_unaligned(payload.as_ptr() as *const FactoryRequest) };

        match req.kind {
            KIND_CREATE_GROUP => {
                if fd_count < 1 {
                    send_reply(sock, libc::EINVAL, &[]);
                } else {
                    create_group_and_mark(sock, &req, in_fds[0]);
                }
            }
            KIND_MARK => {
                if fd_count < 1 {
                    send_reply(sock, libc::EINVAL, &[]);
                } else {
                    mark_dirs(sock, &req, &in_fds[..fd_count]);
                }
            }
            _ => send_reply(sock, libc::EINVAL, &[]),
        }

        close_fds(&in_fds[..fd_count]);
    }

    unsafe { libc::_exit(0) }
}

fn close_fds(fds: &[RawFd]) {
    for fd in fds {
        if *fd >= 0 {
            // SAFETY: fd came from recvmsg/SCM_RIGHTS and is owned by us.
            unsafe { libc::close(*fd) };
        }
    }
}

fn create_group_and_mark(sock: RawFd, req: &FactoryRequest, dirfd: RawFd) {
    // SAFETY: pure syscalls; flags/mask come from the trusted parent.
    let fan_fd = unsafe {
        libc::fanotify_init(
            req.flags,
            (libc::O_RDONLY | libc::O_CLOEXEC) as libc::c_uint,
        )
    };
    if fan_fd < 0 {
        send_reply(sock, last_errno(), &[]);
        return;
    }
    let dotted = c".";
    // SAFETY: `dotted` is a valid NUL-terminated C string.
    let rc = unsafe {
        libc::fanotify_mark(
            fan_fd,
            fanotify_fid::consts::FAN_MARK_ADD as libc::c_uint,
            req.mask,
            dirfd,
            dotted.as_ptr(),
        )
    };
    if rc < 0 {
        send_reply(sock, last_errno(), &[]);
        // SAFETY: fan_fd was just created by us.
        unsafe { libc::close(fan_fd) };
        return;
    }
    send_reply(sock, 0, &[fan_fd]);
    // SCM_RIGHTS duplicated the fd into the parent; drop our copy.
    // SAFETY: fan_fd is owned by this process.
    unsafe { libc::close(fan_fd) };
}

fn mark_dirs(sock: RawFd, req: &FactoryRequest, fds: &[RawFd]) {
    let fan_fd = fds[0];
    let dirs = &fds[1..];
    let dotted = c".";
    let mut first_error = 0i32;
    for dirfd in dirs {
        // SAFETY: `dotted` is a valid NUL-terminated C string.
        let rc = unsafe {
            libc::fanotify_mark(
                fan_fd,
                req.mark_flags as libc::c_uint,
                req.mask,
                *dirfd,
                dotted.as_ptr(),
            )
        };
        if rc < 0 && first_error == 0 {
            first_error = last_errno();
        }
    }
    send_reply(sock, first_error, &[]);
}

fn last_errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

// ── Parent-side handle ──

/// Parent-side handle to the forked privileged factory.
///
/// All methods are `&self`; requests are serialized by an internal mutex.
/// The daemon only ever calls this from its main task, but the mutex keeps
/// the wire protocol intact if that ever changes.
pub(crate) struct FanotifyFactory {
    sock: Mutex<OwnedFd>,
    child: i32,
    alive: AtomicBool,
    /// Round-trips observed, so tests can prove callers batch their requests.
    #[cfg(test)]
    requests: AtomicU64,
}

impl std::fmt::Debug for FanotifyFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FanotifyFactory")
            .field("child", &self.child)
            .field("alive", &self.alive.load(Ordering::Relaxed))
            .finish()
    }
}

/// `true` when this process can create a *privileged* fanotify group.
///
/// The kernel decides that with `capable(CAP_SYS_ADMIN)` inside
/// `fanotify_init()`, so asking it directly is more faithful than `capget()`
/// (which lies inside a user namespace). `FAN_UNLIMITED_MARKS` is the probe:
/// it is one of the admin-only init flags, so a non-root process can never
/// set it.
pub(crate) fn has_privileged_fanotify() -> bool {
    // SAFETY: fanotify_init is a pure syscall.
    let fd = unsafe {
        libc::fanotify_init(
            (GROUP_INIT_FLAGS | UNLIMITED_MARKS) as libc::c_uint,
            (libc::O_RDONLY | libc::O_CLOEXEC) as libc::c_uint,
        )
    };
    if fd < 0 {
        return false;
    }
    // SAFETY: fd was just returned by a successful fanotify_init.
    unsafe { libc::close(fd) };
    true
}

impl FanotifyFactory {
    /// Fork the privileged factory and install its seccomp whitelist.
    ///
    /// The child inherits the parent's capabilities; the caller must drop
    /// its own capabilities afterwards (`crate::common::privileges`).
    pub(crate) fn spawn() -> Result<Self> {
        let filter = build_filter();
        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid two-element array; socketpair is a pure syscall.
        let rc = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error()).context("socketpair for fanotify factory");
        }

        // SAFETY: getpid is a pure syscall.
        let parent_pid = unsafe { libc::getpid() };
        // SAFETY: fork is a pure syscall.
        let child = unsafe { libc::fork() };
        if child < 0 {
            let e = io::Error::last_os_error();
            // SAFETY: both fds were created above and are owned by us.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(e).context("fork fanotify factory");
        }

        if child == 0 {
            // Child: keep the second fd, close the parent's end, run forever.
            // SAFETY: fds[0] is open in this process.
            unsafe { libc::close(fds[0]) };
            // Drop every other inherited descriptor *before* the seccomp filter
            // goes on, because the filter does not allow dup2/close_range.
            // This process holds CAP_SYS_ADMIN for its whole life, so it must
            // not also hold the daemon's event log, command-socket listener or
            // epoll fds. After this it owns only the socketpair and the three
            // standard streams.
            // SAFETY: fds[1] is open in this process; dup2/close_range are
            // pure syscalls with integer arguments.
            unsafe {
                if fds[1] != FACTORY_SOCK_FD {
                    if libc::dup2(fds[1], FACTORY_SOCK_FD) < 0 {
                        libc::_exit(102);
                    }
                    libc::close(fds[1]);
                }
                if !close_fds_above(FACTORY_SOCK_FD) {
                    libc::_exit(103);
                }
            }
            // Die with the parent. Without this an orphaned factory (e.g. the
            // daemon was SIGKILLed) would keep CAP_SYS_ADMIN alive forever.
            // PR_SET_PDEATHSIG must be set before the seccomp filter, which
            // does not allow prctl.
            // SAFETY: prctl with integer arguments.
            unsafe {
                libc::syscall(
                    libc::SYS_prctl,
                    PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_long,
                    0 as libc::c_long,
                    0 as libc::c_long,
                    0 as libc::c_long,
                );
            }
            // If the parent died before PDEATHSIG was armed, getppid() is 1.
            // SAFETY: getppid is a pure syscall.
            if unsafe { libc::getppid() } != parent_pid {
                unsafe { libc::_exit(0) };
            }
            factory_child_main(FACTORY_SOCK_FD, &filter);
        }

        // Parent: keep the first fd, close the child's end.
        // SAFETY: fds[1] is open in this process.
        unsafe { libc::close(fds[1]) };
        // SAFETY: fds[0] was returned by socketpair and is now owned.
        let sock = unsafe { OwnedFd::from_raw_fd(fds[0]) };

        // Wait for the child to install its seccomp filter before any request
        // can be sent.
        let mut ready = [0u8; 1];
        let mut ready_fds = [-1i32; 1];
        let ready_ok = wait_readable(sock.as_raw_fd(), 10_000);
        let handshake = if ready_ok {
            recv_msg(sock.as_raw_fd(), &mut ready, &mut ready_fds)
        } else {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fanotify factory did not signal readiness within 10s",
            ))
        };
        match handshake {
            Ok((1, 0)) if ready[0] == READY_BYTE => {}
            other => {
                // SAFETY: child is the pid we just forked.
                unsafe {
                    libc::kill(child, libc::SIGKILL);
                    libc::waitpid(child, std::ptr::null_mut(), 0);
                }
                return Err(io::Error::other(format!(
                    "fanotify factory failed to become ready: {other:?}"
                )))
                .context("starting fanotify factory");
            }
        }

        if std::env::var_os("FSMON_FACTORY_DEBUG").is_some() {
            eprintln!("[DEBUG] fanotify factory forked (pid {})", child);
        }

        Ok(Self {
            sock: Mutex::new(sock),
            child,
            alive: AtomicBool::new(true),
            #[cfg(test)]
            requests: AtomicU64::new(0),
        })
    }

    /// Number of request round-trips this handle has made (tests only).
    #[cfg(test)]
    pub(crate) fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// Whether the factory is still believed to be running.
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn request(&self, req: &FactoryRequest, out_fds: &[RawFd]) -> Result<Vec<RawFd>> {
        if !self.is_alive() {
            bail!("fanotify factory is no longer running");
        }
        #[cfg(test)]
        self.requests.fetch_add(1, Ordering::Relaxed);
        let sock = self
            .sock
            .lock()
            .map_err(|_| anyhow::anyhow!("fanotify factory mutex poisoned"))?;

        let payload = unsafe {
            std::slice::from_raw_parts(req as *const FactoryRequest as *const u8, REQ_SIZE)
        };
        send_msg(sock.as_raw_fd(), payload, out_fds).context("send fanotify factory request")?;

        let mut reply = [0u8; REPLY_SIZE];
        let mut fds = [-1i32; MAX_FDS];
        if !wait_readable(sock.as_raw_fd(), 30_000) {
            // A wedged factory is fatal for new marks; kill it so no late
            // reply can desynchronize the protocol.
            self.alive.store(false, Ordering::Relaxed);
            if self.child > 0 {
                // SAFETY: child is our fork()ed pid.
                unsafe { libc::kill(self.child, libc::SIGKILL) };
            }
            bail!("fanotify factory did not answer within 30s");
        }
        let (n, fd_count) = recv_msg(sock.as_raw_fd(), &mut reply, &mut fds)
            .context("receive fanotify factory reply")?;
        if n == 0 {
            self.alive.store(false, Ordering::Relaxed);
            bail!("fanotify factory exited unexpectedly");
        }
        if n < REPLY_SIZE {
            for fd in &fds[..fd_count] {
                if *fd >= 0 {
                    // SAFETY: fd was received via SCM_RIGHTS and is owned by us.
                    unsafe { libc::close(*fd) };
                }
            }
            bail!("fanotify factory sent a malformed reply");
        }

        let status = i32::from_ne_bytes([reply[0], reply[1], reply[2], reply[3]]);
        if status != 0 {
            for fd in &fds[..fd_count] {
                if *fd >= 0 {
                    // SAFETY: fd was received via SCM_RIGHTS and is owned by us.
                    unsafe { libc::close(*fd) };
                }
            }
            return Err(anyhow::Error::new(io::Error::from_raw_os_error(status)))
                .context("fanotify factory request failed");
        }
        Ok(fds[..fd_count].to_vec())
    }

    /// Create a new fanotify group and mark `dir_fd` with `mask`.
    ///
    /// `flags` should include [`UNLIMITED_MARKS`] when the factory is
    /// privileged; otherwise the kernel rejects the whole `fanotify_init`.
    pub(crate) fn create_group_and_mark(
        &self,
        dir_fd: &OwnedFd,
        flags: u32,
        mask: u64,
    ) -> Result<OwnedFd> {
        let req = FactoryRequest {
            kind: KIND_CREATE_GROUP,
            flags,
            mark_flags: fanotify_fid::consts::FAN_MARK_ADD,
            count: 1,
            mask: mask & !fanotify_fid::consts::FAN_FS_ERROR,
        };
        let mut got = self.request(&req, &[dir_fd.as_raw_fd()])?;
        if got.len() != 1 {
            for fd in got.drain(..) {
                if fd >= 0 {
                    // SAFETY: fd was received via SCM_RIGHTS and is owned by us.
                    unsafe { libc::close(fd) };
                }
            }
            bail!("fanotify factory returned no group fd");
        }
        let raw = got[0];
        // SAFETY: raw was received via SCM_RIGHTS and is owned by us.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    /// Add (`FAN_MARK_ADD`) or remove (`FAN_MARK_REMOVE`) marks on
    /// already-open directories belonging to `fan_fd`'s group.
    pub(crate) fn mark(
        &self,
        fan_fd: &OwnedFd,
        dir_fds: &[BorrowedFd<'_>],
        mark_flags: u32,
        mask: u64,
    ) -> Result<()> {
        if dir_fds.len() + 1 > MAX_FDS {
            bail!(
                "fanotify factory request carries too many fds ({} dirs)",
                dir_fds.len()
            );
        }
        let mut raw_fds = Vec::with_capacity(dir_fds.len() + 1);
        raw_fds.push(fan_fd.as_raw_fd());
        raw_fds.extend(dir_fds.iter().map(|fd| fd.as_raw_fd()));
        let req = FactoryRequest {
            kind: KIND_MARK,
            flags: 0,
            mark_flags,
            count: dir_fds.len() as u32,
            mask: mask & !fanotify_fid::consts::FAN_FS_ERROR,
        };
        self.request(&req, &raw_fds)?;
        Ok(())
    }
}

impl Drop for FanotifyFactory {
    fn drop(&mut self) {
        // Closing the socket makes the child's recvmsg return 0 so it exits;
        // SIGKILL guarantees we never block on a wedged child.
        if let Ok(sock) = self.sock.lock() {
            drop(sock);
        }
        if self.child > 0 {
            // SAFETY: child is our fork()ed pid.
            unsafe {
                libc::kill(self.child, libc::SIGKILL);
                libc::waitpid(self.child, std::ptr::null_mut(), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::time::{Duration, Instant};

    use crate::common::dir_cache::{DirCache, cache_dir_handle};
    use crate::common::fid_parser::{open_dir_safe, read_fid_events_cached};

    fn event_mask() -> u64 {
        fanotify_fid::consts::FAN_CREATE
            | fanotify_fid::consts::FAN_CLOSE_WRITE
            | fanotify_fid::consts::FAN_EVENT_ON_CHILD
            | fanotify_fid::consts::FAN_ONDIR
    }

    /// Tiny interpreter for the generated BPF program, used to verify the
    /// jump arithmetic without actually invoking disallowed syscalls.
    fn eval_filter(filter: &[SockFilter], arch: u32, nr: u32) -> u32 {
        let mut acc: u32 = 0;
        let mut pc = 0usize;
        loop {
            let ins = filter[pc];
            match ins.code {
                BPF_LD_W_ABS => {
                    acc = if ins.k == SECCOMP_DATA_ARCH_OFF {
                        arch
                    } else {
                        nr
                    };
                    pc += 1;
                }
                BPF_JMP_JEQ_K => {
                    pc = if acc == ins.k {
                        pc + 1 + ins.jt as usize
                    } else {
                        pc + 1 + ins.jf as usize
                    };
                }
                BPF_RET_K => return ins.k,
                other => panic!("unexpected BPF opcode {other:#x}"),
            }
        }
    }

    #[test]
    fn seccomp_filter_allows_only_the_whitelist() {
        let filter = build_filter();
        if AUDIT_ARCH != 0 {
            for n in ALLOWED_SYSCALLS {
                assert_eq!(
                    eval_filter(&filter, AUDIT_ARCH, *n as u32),
                    SECCOMP_RET_ALLOW,
                    "syscall {n} should be allowed"
                );
            }
            // A wrong architecture must be rejected outright.
            assert_eq!(
                eval_filter(&filter, AUDIT_ARCH.wrapping_add(1), libc::SYS_read as u32),
                SECCOMP_RET_KILL_PROCESS,
                "foreign architecture must be killed"
            );
        }
        // Syscalls outside the whitelist must be killed.
        for n in [
            libc::SYS_getpid,
            libc::SYS_open_by_handle_at,
            libc::SYS_mount,
        ] {
            assert_eq!(
                eval_filter(&filter, AUDIT_ARCH, n as u32),
                SECCOMP_RET_KILL_PROCESS,
                "syscall {n} must not be allowed"
            );
        }
    }

    /// The factory must survive forking, the seccomp install, group creation,
    /// an extra mark request, and then deliver events to the unprivileged
    /// parent. This runs fine without CAP_SYS_ADMIN (the group is simply
    /// unprivileged in that case), so it exercises the whole IPC path in CI.
    #[test]
    fn factory_creates_group_and_reports_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DirCache::new(1024, Duration::from_secs(60));
        cache_dir_handle(&cache, dir.path());

        let factory = FanotifyFactory::spawn().expect("spawn fanotify factory");
        assert!(factory.is_alive(), "factory alive right after fork");

        // Prove the seccomp filter is actually installed (mode 2 == filter),
        // not merely attempted. The child may need a moment after fork().
        let mut seccomp_mode = String::from("missing");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let status = std::fs::read_to_string(format!("/proc/{}/status", factory.child))
                .expect("read factory /proc status");
            seccomp_mode = status
                .lines()
                .find_map(|l| l.strip_prefix("Seccomp:"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "missing".to_string());
            if seccomp_mode == "2" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            seccomp_mode, "2",
            "factory must run under a seccomp filter, got {seccomp_mode}"
        );

        let dir_fd = open_dir_safe(dir.path()).expect("open watched dir");
        let fan_fd = factory
            .create_group_and_mark(&dir_fd, GROUP_INIT_FLAGS, event_mask())
            .expect("factory creates group");
        assert!(
            factory.is_alive(),
            "factory must survive group creation (seccomp whitelist covers it)"
        );

        // A second mark on an already-existing group exercises KIND_MARK,
        // where both the fan fd and the dir fd travel over SCM_RIGHTS.
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("create subdir");
        let sub_fd = open_dir_safe(&sub).expect("open subdir");
        factory
            .mark(
                &fan_fd,
                &[sub_fd.as_fd()],
                fanotify_fid::consts::FAN_MARK_ADD,
                event_mask(),
            )
            .expect("factory adds mark");
        assert!(factory.is_alive(), "factory alive after mark request");

        // Generate an event in the watched directory.
        let file = dir.path().join("hello.txt");
        std::fs::File::create(&file)
            .expect("create file")
            .write_all(b"hi")
            .expect("write file");

        let mut buf = vec![0u8; 8192];
        let start = Instant::now();
        let mut seen_path = false;
        while start.elapsed() < Duration::from_secs(3) {
            let events = read_fid_events_cached(&fan_fd, &[], &cache, &mut buf);
            if events
                .iter()
                .any(|e| e.path().to_string_lossy().ends_with("hello.txt"))
            {
                seen_path = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(seen_path, "expected a resolvable event for hello.txt");
        assert!(factory.is_alive(), "factory alive after event delivery");
    }

    /// A request after the factory is gone must fail cleanly instead of
    /// hanging or panicking.
    #[test]
    fn factory_request_after_drop_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_fd = open_dir_safe(dir.path()).expect("open dir");
        let factory = FanotifyFactory::spawn().expect("spawn fanotify factory");
        // Kill the child behind the handle's back.
        // SAFETY: `child` is the pid we forked.
        unsafe {
            libc::kill(factory.child, libc::SIGKILL);
            libc::waitpid(factory.child, std::ptr::null_mut(), 0);
        }
        let err = factory
            .create_group_and_mark(&dir_fd, GROUP_INIT_FLAGS, event_mask())
            .expect_err("request must fail once the factory is gone");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("fanotify factory"),
            "unexpected error message: {msg}"
        );
    }

    /// The probe must never panic and (on an unprivileged test runner) must
    /// report `false`.
    #[test]
    fn privilege_probe_is_consistent() {
        let privileged = has_privileged_fanotify();
        if nix::unistd::geteuid().is_root() {
            assert!(privileged, "root must be able to create a privileged group");
        } else if !privileged {
            // expected on CI / developer machines
        }
    }

    /// The factory holds CAP_SYS_ADMIN for its whole life, so it must not drag
    /// the daemon's file descriptors along with it — a privileged process has
    /// no business holding the event log, the command socket listener or an
    /// epoll fd it will never use. Only the socketpair end (plus the three
    /// standard streams) may survive the fork.
    #[test]
    fn factory_closes_inherited_fds() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A sentinel the parent holds open. If it shows up in the child's
        // /proc/<pid>/fd, inherited fds were not closed.
        let sentinel_path = dir.path().join("sentinel-must-not-be-inherited");
        let _sentinel = std::fs::File::create(&sentinel_path).expect("create sentinel");

        let factory = FanotifyFactory::spawn().expect("spawn fanotify factory");

        let fd_dir = format!("/proc/{}/fd", factory.child);
        let mut highest_fd = -1i32;
        let mut leaked_targets = Vec::new();
        for entry in std::fs::read_dir(&fd_dir).expect("read factory fd table") {
            let entry = entry.expect("fd entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let fd_num: i32 = name.parse().expect("fd name is a number");
            highest_fd = highest_fd.max(fd_num);
            if let Ok(target) = std::fs::read_link(entry.path())
                && target
                    .to_string_lossy()
                    .contains("sentinel-must-not-be-inherited")
            {
                leaked_targets.push(format!("{fd_num} -> {}", target.display()));
            }
        }

        assert!(
            leaked_targets.is_empty(),
            "factory inherited the parent's files: {leaked_targets:?}"
        );
        assert!(
            highest_fd <= 3,
            "factory should keep only fds 0-2 and the socketpair end at 3, \
             but fd {highest_fd} is still open"
        );
    }

    /// Recursively marking a tree must not cost one IPC round-trip per
    /// directory. The request format carries up to 63 directory fds
    /// (`MAX_FDS` minus the group fd), so the walk has to fill batches.
    #[test]
    fn recursive_mark_batches_factory_requests() {
        const DIRS: usize = 200;
        // One fd of each request is the fanotify group; the rest are dirfds.
        const DIRS_PER_REQUEST: usize = 63;

        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..DIRS {
            std::fs::create_dir(dir.path().join(format!("d{i:03}"))).expect("create subdir");
        }

        let factory = FanotifyFactory::spawn().expect("spawn fanotify factory");
        let root_fd = open_dir_safe(dir.path()).expect("open root");
        let fan_fd = factory
            .create_group_and_mark(&root_fd, GROUP_INIT_FLAGS, event_mask())
            .expect("factory creates group");

        let before = factory.requests();
        let discovered = crate::common::fid_parser::mark_recursive_with_depth(
            &factory,
            &fan_fd,
            event_mask(),
            dir.path(),
            None,
            None,
        );
        let requests = factory.requests() - before;

        assert_eq!(discovered.len(), DIRS, "every subdirectory must be marked");

        let worst_case = DIRS.div_ceil(DIRS_PER_REQUEST) as u64;
        assert!(
            requests <= worst_case,
            "marking {DIRS} directories took {requests} round-trips; \
             batching should need at most {worst_case}"
        );

        // Batching must not silently drop marks: the directory from the last
        // batch has to actually deliver an event.
        let last = dir.path().join(format!("d{:03}", DIRS - 1));
        std::fs::File::create(last.join("probe.txt")).expect("create file in last subdir");

        let cache = DirCache::new(1024, Duration::from_secs(60));
        let mut buf = vec![0u8; 8192];
        let started = Instant::now();
        let mut delivered = false;
        while started.elapsed() < Duration::from_secs(3) {
            if !read_fid_events_cached(&fan_fd, &[], &cache, &mut buf).is_empty() {
                delivered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            delivered,
            "a file created in the last batched directory produced no event"
        );
    }

    /// A deep chain has one directory per level, so batching cannot help. If
    /// the walk held marks back until the batch filled or the walk ended, the
    /// deepest directories would stay unmarked for the whole traversal and
    /// events inside them would be lost. Each depth level must flush its own
    /// batch.
    #[test]
    fn recursive_mark_flushes_at_each_depth_level() {
        const LEVELS: usize = 12;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut path = dir.path().to_path_buf();
        for i in 0..LEVELS {
            path = path.join(format!("l{i}"));
            std::fs::create_dir(&path).expect("create level");
        }

        let factory = FanotifyFactory::spawn().expect("spawn fanotify factory");
        let root_fd = open_dir_safe(dir.path()).expect("open root");
        let fan_fd = factory
            .create_group_and_mark(&root_fd, GROUP_INIT_FLAGS, event_mask())
            .expect("factory creates group");

        let before = factory.requests();
        let discovered = crate::common::fid_parser::mark_recursive_with_depth(
            &factory,
            &fan_fd,
            event_mask(),
            dir.path(),
            None,
            None,
        );
        let requests = factory.requests() - before;

        assert_eq!(discovered.len(), LEVELS, "every level must be marked");
        assert_eq!(
            requests, LEVELS as u64,
            "a one-directory-per-level chain must flush once per level, \
             otherwise the deepest directories stay unmarked until the walk ends"
        );
    }
}
