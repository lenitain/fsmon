use std::time::Duration;
use libsystemd::daemon;
use tokio::time::interval;

/// Watchdog manager for systemd integration.
/// Sends periodic WATCHDOG=1 notifications to systemd.
#[derive(Clone)]
pub struct Watchdog {
    interval: Duration,
    enabled: bool,
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

    /// Start watchdog heartbeat task.
    /// Returns JoinHandle that can be aborted on shutdown.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !self.enabled {
                return;
            }

            // Verify watchdog is supported by systemd
            let watchdog_timeout = daemon::watchdog_enabled(false);
            if watchdog_timeout.is_none() {
                eprintln!("[WARNING] systemd watchdog not enabled in service unit");
                return;
            }

            let mut ticker = interval(self.interval);
            // Skip first tick (immediate)
            ticker.tick().await;

            loop {
                ticker.tick().await;
                if let Err(e) = daemon::notify(false, &[daemon::NotifyState::Watchdog]) {
                    eprintln!("[ERROR] systemd watchdog notify failed: {}", e);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watchdog_disabled() {
        let wd = Watchdog::new(None);
        assert!(!wd.is_enabled());
    }

    #[test]
    fn test_watchdog_disabled_zero() {
        let wd = Watchdog::new(Some(0));
        assert!(!wd.is_enabled());
    }

    #[test]
    fn test_watchdog_enabled() {
        let wd = Watchdog::new(Some(30));
        assert!(wd.is_enabled());
        assert_eq!(wd.interval(), Duration::from_secs(30));
    }
}