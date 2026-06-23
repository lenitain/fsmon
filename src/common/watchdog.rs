use libsystemd::daemon;
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
        daemon::notify(false, &[daemon::NotifyState::Watchdog])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Send a notify state to systemd.
/// Used internally for both READY and WATCHDOG signals.
pub(crate) fn sd_notify(state: daemon::NotifyState) -> Result<(), String> {
    daemon::notify(false, &[state])
        .map(|_| ())
        .map_err(|e| e.to_string())
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
    fn test_libsystemd_notify_ready() {
        // Test that libsystemd notify function works.
        // In non-systemd environment, this returns an error — that's expected.
        let result = sd_notify(daemon::NotifyState::Ready);
        let _ = result; // don't care about result in test
    }

    #[test]
    fn test_libsystemd_notify_watchdog() {
        let result = sd_notify(daemon::NotifyState::Watchdog);
        let _ = result;
    }

    #[test]
    fn test_libsystemd_notify_status() {
        let result = sd_notify(daemon::NotifyState::Status("fsmon test".to_string()));
        let _ = result;
    }

    #[test]
    fn test_libsystemd_watchdog_enabled() {
        // In non-systemd environment, this returns None
        let _ = daemon::watchdog_enabled(false);
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
