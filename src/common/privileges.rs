//! Privilege handling for the daemon (PRIVILEGE-SEPARATION-PLAN.md §6 阶段 3).
//!
//! After the fanotify factory has been forked and the initial groups have
//! been created, the main daemon process calls [`drop_privileges`]. From
//! that point on it holds no capabilities at all (`CapEff = 0`), mirroring
//! gsr's auditable main process.
//!
//! The privileged groups keep reporting real pids because the kernel stores
//! `FANOTIFY_UNPRIV` on the *group* object, not on the process (plan §3).

use anyhow::{Context, Result, bail};
use std::io;

/// A startup failure that no restart can fix — a missing capability, for
/// instance.
///
/// `fsmon daemon` maps this to **exit code 2**, which the generated systemd
/// unit lists in `RestartPreventExitStatus`, so systemd reports the failure
/// instead of retrying it `StartLimitBurst` times. Any future permanent
/// startup failure should use this type for the same reason.
#[derive(Debug, Clone)]
pub struct PermanentStartupError {
    message: String,
}

impl PermanentStartupError {
    /// Wrap `message` as a permanent startup failure (exit code 2).
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PermanentStartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PermanentStartupError {}

/// `_LINUX_CAPABILITY_VERSION_3` — 64-bit capability masks (two u32 words).
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

// prctl options (stable kernel ABI; not exposed by libc on every target).
const PR_CAPBSET_DROP: libc::c_long = 24;
const PR_SET_NO_NEW_PRIVS: libc::c_long = 38;

/// Highest capability number on current kernels (`CAP_CHECKPOINT_RESTORE` = 40).
const CAP_LAST_CAP: u32 = 40;

/// `CAP_SETPCAP` must be dropped last, otherwise the remaining
/// `PR_CAPBSET_DROP` calls lose the capability that authorizes them.
const CAP_SETPCAP: u32 = 8;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Clear effective, permitted and inheritable capability sets.
///
/// Dropping capabilities from one's own sets never requires a capability,
/// so this always succeeds for a well-formed process.
pub(crate) fn clear_all_capabilities() -> io::Result<()> {
    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let data = [CapData::default(); 2];
    // SAFETY: capset is a pure syscall; both pointers are valid for the call.
    let rc = unsafe { libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Best-effort bounding-set reduction. When the process no longer has
/// `CAP_SETPCAP` (e.g. systemd already restricted the bounding set) every
/// drop fails harmlessly; systemd's `CapabilityBoundingSet` is the real
/// guarantee there.
fn drop_bounding_set() {
    for cap in (0..=CAP_LAST_CAP).filter(|c| *c != CAP_SETPCAP) {
        // SAFETY: prctl with integer arguments.
        unsafe {
            libc::syscall(
                libc::SYS_prctl,
                PR_CAPBSET_DROP,
                cap as libc::c_long,
                0 as libc::c_long,
                0 as libc::c_long,
                0 as libc::c_long,
            );
        }
    }
    // SAFETY: as above; dropping CAP_SETPCAP last keeps earlier drops working.
    unsafe {
        libc::syscall(
            libc::SYS_prctl,
            PR_CAPBSET_DROP,
            CAP_SETPCAP as libc::c_long,
            0 as libc::c_long,
            0 as libc::c_long,
            0 as libc::c_long,
        );
    }
}

/// Drop to the service user (when started as root), clear every capability
/// and set `PR_SET_NO_NEW_PRIVS`.
///
/// Under the hardened systemd unit the process already runs as the target
/// user with only `CAP_SYS_ADMIN`, so the uid/gid half is a no-op and only
/// the capability drop happens.
pub(crate) fn drop_privileges() -> Result<()> {
    let euid = nix::unistd::geteuid();
    let (uid, gid) = crate::common::config::resolve_uid_gid();

    if euid.is_root() && uid != 0 {
        // Do this while we still hold CAP_SETPCAP.
        drop_bounding_set();

        // Clear supplementary groups first, then gid, then uid. Use the libc
        // wrappers (via nix) so glibc propagates the change to every thread.
        if let Err(e) = nix::unistd::setgroups(&[]) {
            eprintln!("[WARNING] setgroups([]) failed: {e}");
        }
        nix::unistd::setgid(nix::unistd::Gid::from_raw(gid))
            .with_context(|| format!("setgid({gid}) while dropping privileges"))?;
        nix::unistd::setuid(nix::unistd::Uid::from_raw(uid))
            .with_context(|| format!("setuid({uid}) while dropping privileges"))?;
    } else {
        // Not root (or already the target uid): still shed the bounding set
        // if we happen to have CAP_SETPCAP.
        drop_bounding_set();
    }

    clear_all_capabilities().context("clearing capability sets after fanotify setup")?;

    // SAFETY: prctl with integer arguments.
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
        bail!("PR_SET_NO_NEW_PRIVS failed: {}", io::Error::last_os_error());
    }

    Ok(())
}
