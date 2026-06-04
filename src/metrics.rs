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
    use std::thread;

    // ── CounterVec: enabled ──────────────────────────────────────────────

    #[test]
    fn counter_vec_inc_and_gather() {
        let cv = CounterVec::new(true);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["MODIFY", "global"]);
        cv.inc(&["CREATE", "nginx"]);

        let entries = cv.gather();
        assert_eq!(entries.len(), 2);

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
    fn counter_vec_gather_empty() {
        let cv = CounterVec::new(true);
        assert!(cv.gather().is_empty());
    }

    #[test]
    fn counter_vec_single_label() {
        let cv = CounterVec::new(true);
        cv.inc(&["total"]);
        cv.inc(&["total"]);
        let entries = cv.gather();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, 2);
    }

    #[test]
    fn counter_vec_many_labels() {
        let cv = CounterVec::new(true);
        cv.inc(&["a", "b", "c", "d"]);
        let entries = cv.gather();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn counter_vec_concurrent_inc() {
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
    fn counter_vec_concurrent_different_labels() {
        let cv = CounterVec::new(true);
        let mut handles = vec![];
        for i in 0..10 {
            let cv = cv.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    cv.inc(&[&format!("type{}", i), "cmd"]);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let entries = cv.gather();
        assert_eq!(entries.len(), 10);
        let total: u64 = entries.iter().map(|(_, v)| v).sum();
        assert_eq!(total, 1000);
    }

    // ── CounterVec: disabled ─────────────────────────────────────────────

    #[test]
    fn counter_vec_disabled_inc_noop() {
        let cv = CounterVec::new(false);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["CREATE", "nginx"]);
        assert!(cv.gather().is_empty());
    }

    #[test]
    fn counter_vec_disabled_gather_empty() {
        let cv = CounterVec::new(false);
        assert!(cv.gather().is_empty());
    }

    // ── IntGauge: enabled ────────────────────────────────────────────────

    #[test]
    fn int_gauge_default_zero() {
        let g = IntGauge::new(true);
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn int_gauge_set() {
        let g = IntGauge::new(true);
        g.set(42);
        assert_eq!(g.get(), 42);
        g.set(0);
        assert_eq!(g.get(), 0);
        g.set(-10);
        assert_eq!(g.get(), -10);
    }

    #[test]
    fn int_gauge_inc() {
        let g = IntGauge::new(true);
        g.inc();
        assert_eq!(g.get(), 1);
        g.inc();
        g.inc();
        assert_eq!(g.get(), 3);
    }

    #[test]
    fn int_gauge_dec() {
        let g = IntGauge::new(true);
        g.dec();
        assert_eq!(g.get(), -1);
        g.dec();
        assert_eq!(g.get(), -2);
    }

    #[test]
    fn int_gauge_inc_dec_combined() {
        let g = IntGauge::new(true);
        g.inc();
        g.inc();
        g.inc();
        g.dec();
        assert_eq!(g.get(), 2);
        g.set(10);
        g.dec();
        assert_eq!(g.get(), 9);
    }

    #[test]
    fn int_gauge_concurrent_inc() {
        let g = IntGauge::new(true);
        let g2 = g.clone();
        let h = thread::spawn(move || {
            for _ in 0..1000 {
                g2.inc();
            }
        });
        for _ in 0..1000 {
            g.inc();
        }
        h.join().unwrap();
        assert_eq!(g.get(), 2000);
    }

    #[test]
    fn int_gauge_concurrent_mixed() {
        let g = IntGauge::new(true);
        let g2 = g.clone();
        let g3 = g.clone();
        let h1 = thread::spawn(move || {
            for _ in 0..1000 {
                g2.inc();
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..500 {
                g3.dec();
            }
        });
        for _ in 0..1000 {
            g.inc();
        }
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(g.get(), 1500);
    }

    // ── IntGauge: disabled ───────────────────────────────────────────────

    #[test]
    fn int_gauge_disabled_set_noop() {
        let g = IntGauge::new(false);
        g.set(42);
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn int_gauge_disabled_inc_noop() {
        let g = IntGauge::new(false);
        g.inc();
        g.inc();
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn int_gauge_disabled_dec_noop() {
        let g = IntGauge::new(false);
        g.dec();
        g.dec();
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn int_gauge_disabled_get_always_zero() {
        let g = IntGauge::new(false);
        assert_eq!(g.get(), 0);
    }

    // ── MetricsRegistry: enabled ─────────────────────────────────────────

    #[test]
    fn registry_enabled_is_enabled() {
        let r = MetricsRegistry::new(true);
        assert!(r.is_enabled());
    }

    #[test]
    fn registry_disabled_is_not_enabled() {
        let r = MetricsRegistry::new(false);
        assert!(!r.is_enabled());
    }

    #[test]
    fn registry_default_not_enabled() {
        let r = MetricsRegistry::default();
        assert!(!r.is_enabled());
    }

    #[test]
    fn registry_subscribers() {
        let r = MetricsRegistry::new(true);
        assert_eq!(r.subscribers(), 0);
        r.inc_subscribers();
        assert_eq!(r.subscribers(), 1);
        r.inc_subscribers();
        assert_eq!(r.subscribers(), 2);
        r.dec_subscribers();
        assert_eq!(r.subscribers(), 1);
        r.set_subscribers(10);
        assert_eq!(r.subscribers(), 10);
    }

    #[test]
    fn registry_monitored_paths() {
        let r = MetricsRegistry::new(true);
        assert_eq!(r.monitored_paths(), 0);
        r.set_monitored_paths(5);
        assert_eq!(r.monitored_paths(), 5);
    }

    #[test]
    fn registry_reader_groups() {
        let r = MetricsRegistry::new(true);
        assert_eq!(r.reader_groups(), 0);
        r.set_reader_groups(3);
        assert_eq!(r.reader_groups(), 3);
    }

    #[test]
    fn registry_pending_paths() {
        let r = MetricsRegistry::new(true);
        assert_eq!(r.pending_paths(), 0);
        r.set_pending_paths(2);
        assert_eq!(r.pending_paths(), 2);
    }

    #[test]
    fn registry_disk_buffer_events() {
        let r = MetricsRegistry::new(true);
        assert_eq!(r.disk_buffer_events(), 0);
        r.set_disk_buffer_events(100);
        assert_eq!(r.disk_buffer_events(), 100);
    }

    #[test]
    fn registry_all_gauges_independent() {
        let r = MetricsRegistry::new(true);
        r.set_subscribers(1);
        r.set_monitored_paths(2);
        r.set_reader_groups(3);
        r.set_pending_paths(4);
        r.set_disk_buffer_events(5);
        assert_eq!(r.subscribers(), 1);
        assert_eq!(r.monitored_paths(), 2);
        assert_eq!(r.reader_groups(), 3);
        assert_eq!(r.pending_paths(), 4);
        assert_eq!(r.disk_buffer_events(), 5);
    }

    // ── MetricsRegistry: disabled ────────────────────────────────────────

    #[test]
    fn registry_disabled_subscribers_always_zero() {
        let r = MetricsRegistry::new(false);
        r.inc_subscribers();
        r.set_subscribers(10);
        assert_eq!(r.subscribers(), 0);
    }

    #[test]
    fn registry_disabled_monitored_paths_always_zero() {
        let r = MetricsRegistry::new(false);
        r.set_monitored_paths(10);
        assert_eq!(r.monitored_paths(), 0);
    }

    #[test]
    fn registry_disabled_reader_groups_always_zero() {
        let r = MetricsRegistry::new(false);
        r.set_reader_groups(10);
        assert_eq!(r.reader_groups(), 0);
    }

    #[test]
    fn registry_disabled_pending_paths_always_zero() {
        let r = MetricsRegistry::new(false);
        r.set_pending_paths(10);
        assert_eq!(r.pending_paths(), 0);
    }

    #[test]
    fn registry_disabled_disk_buffer_always_zero() {
        let r = MetricsRegistry::new(false);
        r.set_disk_buffer_events(100);
        assert_eq!(r.disk_buffer_events(), 0);
    }

    // ── MetricsRegistry: clone shares state ──────────────────────────────

    #[test]
    fn registry_clone_shares_state() {
        let r1 = MetricsRegistry::new(true);
        let r2 = r1.clone();
        r1.set_subscribers(5);
        assert_eq!(r2.subscribers(), 5);
        r2.inc_subscribers();
        assert_eq!(r1.subscribers(), 6);
    }

    #[test]
    fn registry_clone_disabled_stays_disabled() {
        let r1 = MetricsRegistry::new(false);
        let r2 = r1.clone();
        r1.set_monitored_paths(10);
        assert_eq!(r2.monitored_paths(), 0);
    }

    // ── MetricsRegistry: concurrent ──────────────────────────────────────

    #[test]
    fn registry_concurrent_subscribers() {
        let r = MetricsRegistry::new(true);
        let r2 = r.clone();
        let h = thread::spawn(move || {
            for _ in 0..1000 {
                r2.inc_subscribers();
            }
        });
        for _ in 0..1000 {
            r.inc_subscribers();
        }
        h.join().unwrap();
        assert_eq!(r.subscribers(), 2000);
    }

    #[test]
    fn registry_concurrent_mixed_gauges() {
        let r = MetricsRegistry::new(true);
        let r2 = r.clone();
        let r3 = r.clone();
        let h1 = thread::spawn(move || {
            for _ in 0..1000 {
                r2.inc_subscribers();
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..500 {
                r3.set_monitored_paths(r3.monitored_paths() + 1);
            }
        });
        for _ in 0..1000 {
            r.inc_subscribers();
        }
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(r.subscribers(), 2000);
        assert_eq!(r.monitored_paths(), 500);
    }
}
