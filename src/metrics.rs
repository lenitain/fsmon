//! Lightweight metrics registry for fsmon daemon.
//!
//! Provides generic atomic counters (CounterVec) and gauges (IntGauge)
//! for tracking runtime stats. Used by the periodic metrics report
//! printed to stderr via --metrics-interval.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

// ── CounterVec ────────────────────────────────────────────────────────

/// Thread-safe counter with string labels (like Prometheus CounterVec).
/// Labels are interned lazily: first `inc()` call for a label set creates a counter.
/// When `enabled` is false, all operations are no-ops (zero overhead).
#[derive(Clone)]
pub struct CounterVec {
    enabled: bool,
    inner: Arc<RwLock<CounterVecInner>>,
}

struct CounterVecInner {
    counters: HashMap<Vec<String>, Arc<AtomicU64>>,
}

impl Default for CounterVec {
    fn default() -> Self {
        Self::new(false)
    }
}

impl CounterVec {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            inner: Arc::new(RwLock::new(CounterVecInner {
                counters: HashMap::new(),
            })),
        }
    }

    /// Increment the counter for the given label values.
    /// Creates an entry if this label combination is seen for the first time.
    /// Read-dominant: only takes write lock on first occurrence.
    #[inline]
    pub fn inc(&self, labels: &[&str]) {
        if !self.enabled {
            return;
        }
        let label_key: Vec<String> = labels.iter().map(|s| s.to_string()).collect();

        // Fast path: try read lock first
        if let Ok(map) = self.inner.read()
            && let Some(counter) = map.counters.get(&label_key)
        {
            counter.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Slow path: insert new counter under write lock
        if let Ok(mut map) = self.inner.write() {
            let counter = map
                .counters
                .entry(label_key)
                .or_insert_with(|| Arc::new(AtomicU64::new(0)));
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot all label sets and their current values.
    pub fn gather(&self) -> Vec<(Vec<String>, u64)> {
        if !self.enabled {
            return Vec::new();
        }
        let mut result = Vec::new();
        if let Ok(map) = self.inner.read() {
            for (labels, counter) in &map.counters {
                result.push((labels.clone(), counter.load(Ordering::Relaxed)));
            }
        }
        result
    }
}

// ── IntGauge ────────────────────────────────────────────────────────

/// Simple atomic gauge. When `enabled` is false, all operations are no-ops.
#[derive(Clone)]
pub struct IntGauge {
    enabled: bool,
    value: Arc<AtomicI64>,
}

impl Default for IntGauge {
    fn default() -> Self {
        Self::new(false)
    }
}

impl IntGauge {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            value: Arc::new(AtomicI64::new(0)),
        }
    }

    #[inline]
    pub fn set(&self, val: i64) {
        if self.enabled {
            self.value.store(val, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn inc(&self) {
        if self.enabled {
            self.value.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn dec(&self) {
        if self.enabled {
            self.value.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub fn get(&self) -> i64 {
        if self.enabled {
            self.value.load(Ordering::Relaxed)
        } else {
            0
        }
    }
}

// ── MetricsRegistry ──────────────────────────────────────────────────

/// All metrics registered by the daemon.
/// Cheap to clone (Arc-backed) — pass a clone to background tasks.
#[derive(Clone)]
pub struct MetricsRegistry {
    subscribers: IntGauge,
    monitored_paths: IntGauge,
    reader_groups: IntGauge,
    pending_paths: IntGauge,
    disk_buffer_events: IntGauge,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new(false)
    }
}

impl MetricsRegistry {
    pub fn new(enabled: bool) -> Self {
        Self {
            subscribers: IntGauge::new(enabled),
            monitored_paths: IntGauge::new(enabled),
            reader_groups: IntGauge::new(enabled),
            pending_paths: IntGauge::new(enabled),
            disk_buffer_events: IntGauge::new(enabled),
        }
    }

    /// Returns true if metrics collection is enabled.
    pub fn is_enabled(&self) -> bool {
        self.subscribers.enabled
    }

    /// Increment the events_total counter.


    // ── Gauge accessors ──

    pub fn set_subscribers(&self, n: i64) {
        self.subscribers.set(n);
    }
    pub fn inc_subscribers(&self) {
        self.subscribers.inc();
    }
    pub fn dec_subscribers(&self) {
        self.subscribers.dec();
    }
    pub fn subscribers(&self) -> i64 {
        self.subscribers.get()
    }

    pub fn set_monitored_paths(&self, n: i64) {
        self.monitored_paths.set(n);
    }
    pub fn monitored_paths(&self) -> i64 {
        self.monitored_paths.get()
    }

    pub fn set_reader_groups(&self, n: i64) {
        self.reader_groups.set(n);
    }
    pub fn reader_groups(&self) -> i64 {
        self.reader_groups.get()
    }

    pub fn set_pending_paths(&self, n: i64) {
        self.pending_paths.set(n);
    }
    pub fn pending_paths(&self) -> i64 {
        self.pending_paths.get()
    }

    pub fn set_disk_buffer_events(&self, n: i64) {
        self.disk_buffer_events.set(n);
    }
    pub fn disk_buffer_events(&self) -> i64 {
        self.disk_buffer_events.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_vec_inc() {
        let cv = CounterVec::new(true);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["MODIFY", "global"]);
        cv.inc(&["CREATE", "nginx"]);

        let entries = cv.gather();
        let find = |et: &str, cmd: &str| -> Option<u64> {
            entries
                .iter()
                .find(|(l, _)| l[0] == et && l[1] == cmd)
                .map(|(_, v)| *v)
        };
        assert_eq!(find("CREATE", "nginx"), Some(3));
        assert_eq!(find("MODIFY", "global"), Some(1));
        assert_eq!(find("DELETE", "nginx"), None);
    }

    #[test]
    fn test_counter_vec_concurrent() {
        use std::thread;
        let cv = CounterVec::new(true);
        let cv2 = cv.clone();
        let h = thread::spawn(move || {
            for _ in 0..1000 {
                cv2.inc(&["CREATE", "nginx"]);
            }
        });
        for _ in 0..1000 {
            cv.inc(&["CREATE", "nginx"]);
        }
        h.join().unwrap();
        let entries = cv.gather();
        let val = entries
            .iter()
            .find(|(l, _)| l[0] == "CREATE" && l[1] == "nginx")
            .map(|(_, v)| *v);
        assert_eq!(val, Some(2000));
    }

    #[test]
    fn test_int_gauge() {
        let g = IntGauge::new(true);
        assert_eq!(g.get(), 0);
        g.inc();
        assert_eq!(g.get(), 1);
        g.inc();
        g.inc();
        assert_eq!(g.get(), 3);
        g.dec();
        assert_eq!(g.get(), 2);
        g.set(42);
        assert_eq!(g.get(), 42);
    }
}
