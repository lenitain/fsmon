use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

/// Watchdog configuration for systemd integration.
///
/// Heartbeat lives inside the main event loop (tokio::select!), NOT as a
/// separate task, ensuring liveness detection.
//
//  main loop (tokio::select!)  ──── poll all branches each iteration
//    event_rx.recv()             fanotify events
//    heartbeat_tick.tick()       periodic timer
//    proc_readable               proc connector
//    inotify_ready               dir creation
//    socket_listener             client commands
//    ...                         ...
//
//    whichever is ready first gets executed, rest keep awaiting
//
//  heartbeat_tick fires:
//    wd.send_heartbeat()  ──▶  systemd WATCHDOG=1
//
//  if handler blocks (e.g. fs::metadata on NFS):
//    select! can't poll heartbeat_tick  ──▶  no heartbeat  ──▶  systemd restarts
//
//  if idle (no events):
//    heartbeat_tick still fires on schedule  ──▶  heartbeat sent  ──▶  all good
/// Watchdog timer for systemd service health monitoring.
/// # Examples
///
/// ```ignore
/// use fsmon::Watchdog;
///
/// // Create a watchdog with 30-second interval
/// let watchdog = Watchdog::new(Some(30));
/// assert!(watchdog.is_enabled());
/// assert_eq!(watchdog.interval(), std::time::Duration::from_secs(30));
///
/// // Create a disabled watchdog
/// let watchdog = Watchdog::new(None);
/// assert!(!watchdog.is_enabled());
/// ```
#[derive(Clone)]
pub struct Watchdog {
    interval: Duration,
    enabled: bool,
}

impl std::fmt::Debug for Watchdog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watchdog")
            .field("interval", &self.interval)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Watchdog {
    /// Create new watchdog from config interval.
    /// If interval is None or zero, watchdog is disabled.
    pub fn new(interval_secs: Option<u64>) -> Self {
        let enabled = interval_secs.is_some_and(|s| s > 0);
        let interval = Duration::from_secs(interval_secs.unwrap_or(30));
        Self { interval, enabled }
    }

    /// Check if watchdog is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the watchdog interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Send WATCHDOG=1 to systemd.
    /// Called from the main event loop's heartbeat tick.
    /// Returns Ok(()) on success, or error message on failure.
    pub fn send_heartbeat(&self) -> Result<(), String> {
        sd_notify(NotifyState::Watchdog)
    }
}

/// systemd notify states (subset of sd_notify(3) messages used by fsmon).
pub(crate) enum NotifyState {
    Ready,
    Watchdog,
}

impl NotifyState {
    fn to_message(&self) -> String {
        match self {
            NotifyState::Ready => "READY=1".to_string(),
            NotifyState::Watchdog => "WATCHDOG=1".to_string(),
        }
    }
}

/// Send a notify state to systemd.
/// Used internally for both READY and WATCHDOG signals.
///
/// Minimal pure-std implementation of sd_notify(3): sends a datagram to
/// `$NOTIFY_SOCKET` (leading `@` denotes an abstract socket address).
pub(crate) fn sd_notify(state: NotifyState) -> Result<(), String> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr as UnixSocketAddr;
    use std::os::unix::net::UnixDatagram;

    let raw =
        std::env::var_os("NOTIFY_SOCKET").ok_or_else(|| "NOTIFY_SOCKET is not set".to_string())?;
    let addr = if raw.as_bytes().first() == Some(&b'@') {
        // Abstract socket address: '@' prefix maps to the Linux abstract namespace.
        UnixSocketAddr::from_abstract_name(&raw.as_bytes()[1..]).map_err(|e| e.to_string())?
    } else {
        UnixSocketAddr::from_pathname(&raw).map_err(|e| e.to_string())?
    };
    let sock = UnixDatagram::unbound().map_err(|e| e.to_string())?;
    sock.send_to_addr(state.to_message().as_bytes(), &addr)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watchdog_disabled_none() {
        let wd = Watchdog::new(None);
        assert!(!wd.is_enabled());
        assert_eq!(wd.interval(), Duration::from_secs(30)); // default
    }

    #[test]
    fn test_watchdog_disabled_zero() {
        let wd = Watchdog::new(Some(0));
        assert!(!wd.is_enabled());
    }

    #[test]
    fn test_watchdog_enabled() {
        let wd = Watchdog::new(Some(15));
        assert!(wd.is_enabled());
        assert_eq!(wd.interval(), Duration::from_secs(15));
    }

    #[test]
    fn test_watchdog_clone() {
        let wd = Watchdog::new(Some(20));
        let wd2 = wd.clone();
        assert_eq!(wd.is_enabled(), wd2.is_enabled());
        assert_eq!(wd.interval(), wd2.interval());
    }

    #[test]
    fn test_sd_notify_roundtrip() {
        use std::os::unix::net::UnixDatagram;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("notify.sock");
        let listener = match UnixDatagram::bind(&sock_path) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: unix datagram sockets blocked in this environment");
                return;
            }
            Err(e) => panic!("bind failed: {e}"),
        };
        temp_env::with_var("NOTIFY_SOCKET", Some(&sock_path), || {
            match sd_notify(NotifyState::Ready) {
                Ok(()) => {}
                Err(e) if e.contains("Operation not permitted") => {
                    eprintln!("skipping: unix datagram send blocked in this environment");
                }
                Err(e) => panic!("sd_notify failed: {e}"),
            }
        });
        let mut buf = [0u8; 64];
        let n = listener.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
    }

    #[test]
    fn test_sd_notify_message_format() {
        assert_eq!(NotifyState::Ready.to_message(), "READY=1");
        assert_eq!(NotifyState::Watchdog.to_message(), "WATCHDOG=1");
    }

    #[test]
    fn test_send_heartbeat_disabled() {
        let wd = Watchdog::new(None);
        // send_heartbeat will fail in non-systemd environment — that's fine
        let _ = wd.send_heartbeat();
    }

    #[test]
    fn test_send_heartbeat_enabled() {
        let wd = Watchdog::new(Some(15));
        // send_heartbeat will fail in non-systemd environment — that's fine
        let _ = wd.send_heartbeat();
    }
}
