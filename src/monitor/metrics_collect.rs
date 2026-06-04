use std::time::Instant;

use crate::metrics::MetricsRegistry;

/// Manages metrics collection and reporting.
pub struct MetricsCollector {
    /// Daemon start time, set in run() for uptime calculation.
    pub started_at: Instant,
    /// Metrics report interval. None = disabled.
    pub metrics_interval: Option<std::time::Duration>,
    /// Atomic metrics counters (thread-safe, cloneable).
    pub metrics: MetricsRegistry,
}

impl MetricsCollector {
    pub fn new(metrics_interval: Option<u64>) -> Self {
        Self {
            started_at: Instant::now(),
            metrics_interval: metrics_interval
                .map(|secs| std::time::Duration::from_secs(secs.max(1))),
            metrics: MetricsRegistry::new(metrics_interval.is_some()),
        }
    }

    /// Get the uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Check if metrics reporting is enabled.
    pub fn is_enabled(&self) -> bool {
        self.metrics_interval.is_some()
    }

    /// Get the metrics registry.
    pub fn metrics(&self) -> &MetricsRegistry {
        &self.metrics
    }

    /// Get a mutable reference to the metrics registry.
    pub fn metrics_mut(&mut self) -> &mut MetricsRegistry {
        &mut self.metrics
    }

    /// Get the metrics interval duration.
    pub fn interval(&self) -> Option<std::time::Duration> {
        self.metrics_interval
    }
}
