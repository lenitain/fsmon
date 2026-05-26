//! Lightweight metrics registry for fsmon daemon.
//!
//! Three-layer design:
//!   Counter layer — generic atomic counters (zero deps)
//!   Format layer  — format to Prometheus text (extensible)
//!   Transport     — socket command + optional TCP HTTP

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

// ── CounterVec ────────────────────────────────────────────────────────

/// Thread-safe counter with string labels (like Prometheus CounterVec).
/// Labels are interned lazily: first `inc()` call for a label set creates a counter.
#[derive(Clone)]
pub struct CounterVec {
    inner: Arc<RwLock<CounterVecInner>>,
    name: Arc<String>,
    help: Arc<String>,
}

struct CounterVecInner {
    counters: HashMap<Vec<String>, Arc<AtomicU64>>,
}

impl CounterVec {
    pub fn new(name: &str, help: &str) -> Self {
        Self {
            inner: Arc::new(RwLock::new(CounterVecInner {
                counters: HashMap::new(),
            })),
            name: Arc::new(name.to_string()),
            help: Arc::new(help.to_string()),
        }
    }

    /// Increment the counter for the given label values.
    /// Creates an entry if this label combination is seen for the first time.
    /// Read-dominant: only takes write lock on first occurrence.
    pub fn inc(&self, labels: &[&str]) {
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
        let mut result = Vec::new();
        if let Ok(map) = self.inner.read() {
            for (labels, counter) in &map.counters {
                result.push((labels.clone(), counter.load(Ordering::Relaxed)));
            }
        }
        result
    }

    /// Prometheus metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn help(&self) -> &str {
        &self.help
    }
}

// ── IntGauge ────────────────────────────────────────────────────────

/// Simple atomic gauge.
#[derive(Clone)]
pub struct IntGauge {
    value: Arc<AtomicI64>,
    name: Arc<String>,
    help: Arc<String>,
}

impl IntGauge {
    pub fn new(name: &str, help: &str) -> Self {
        Self {
            value: Arc::new(AtomicI64::new(0)),
            name: Arc::new(name.to_string()),
            help: Arc::new(help.to_string()),
        }
    }

    pub fn set(&self, val: i64) {
        self.value.store(val, Ordering::Relaxed);
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn help(&self) -> &str {
        &self.help
    }
}

// ── MetricsRegistry ──────────────────────────────────────────────────

/// All metrics registered by the daemon.
/// Cheap to clone (Arc-backed) — pass a clone to background tasks.
#[derive(Clone)]
pub struct MetricsRegistry {
    events_total: CounterVec,
    subscribers: IntGauge,
    monitored_paths: IntGauge,
    reader_groups: IntGauge,
    pending_paths: IntGauge,
    disk_buffer_events: IntGauge,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            events_total: CounterVec::new(
                "fsmon_events_total",
                "Total file system events processed by fsmon",
            ),
            subscribers: IntGauge::new(
                "fsmon_subscribers",
                "Current number of active subscribe connections",
            ),
            monitored_paths: IntGauge::new(
                "fsmon_monitored_paths",
                "Current number of monitored path entries",
            ),
            reader_groups: IntGauge::new(
                "fsmon_reader_groups",
                "Current number of fanotify fd groups",
            ),
            pending_paths: IntGauge::new(
                "fsmon_pending_paths",
                "Current number of paths pending creation",
            ),
            disk_buffer_events: IntGauge::new(
                "fsmon_disk_buffer_events",
                "Current number of buffered events (disk full)",
            ),
        }
    }

    /// Increment the events_total counter.
    pub fn inc_event(&self, event_type: &str, cmd: &str) {
        self.events_total.inc(&[event_type, cmd]);
    }

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

    pub fn set_reader_groups(&self, n: i64) {
        self.reader_groups.set(n);
    }

    pub fn set_pending_paths(&self, n: i64) {
        self.pending_paths.set(n);
    }

    pub fn set_disk_buffer_events(&self, n: i64) {
        self.disk_buffer_events.set(n);
    }

    // ── Format ──

    /// Format all metrics in Prometheus text format.
    /// https://prometheus.io/docs/instrumenting/exposition_formats/
    pub fn format_prometheus(&self) -> String {
        let mut out = String::new();

        // events_total (counter vec)
        let entries = self.events_total.gather();
        if !entries.is_empty() {
            push_help_type(&mut out, self.events_total.name(), self.events_total.help(), "counter");
            for (labels, value) in &entries {
                push_metric_line(&mut out, self.events_total.name(), &[("event_type", &labels[0]), ("cmd", &labels[1])], *value);
            }
        }

        // gauges
        push_gauge(&mut out, &self.subscribers);
        push_gauge(&mut out, &self.monitored_paths);
        push_gauge(&mut out, &self.reader_groups);
        push_gauge(&mut out, &self.pending_paths);
        push_gauge(&mut out, &self.disk_buffer_events);

        out
    }
}

fn push_help_type(out: &mut String, name: &str, help: &str, typ: &str) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(typ);
    out.push('\n');
}

fn push_metric_line(out: &mut String, name: &str, labels: &[(&str, &str)], value: u64) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_gauge(out: &mut String, g: &IntGauge) {
    push_help_type(out, g.name(), g.help(), "gauge");
    push_metric_line(out, g.name(), &[], g.get() as u64);
}

// ── Socket handler ───────────────────────────────────────────────────

/// Handle a `metrics` socket command: write Prometheus text and close.
pub async fn handle_metrics_socket(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    metrics: &MetricsRegistry,
) {
    use tokio::io::AsyncWriteExt;
    let text = metrics.format_prometheus();
    let _ = writer.write_all(text.as_bytes()).await;
}

// ── TCP HTTP server ──────────────────────────────────────────────────

/// Start a minimal TCP HTTP server serving `/metrics`.
/// Spawns a tokio task. Returns `Ok(true)` if bound successfully,
/// `Ok(false)` if binding failed (port in use).
pub async fn serve_metrics_tcp(
    addr: SocketAddr,
    metrics: MetricsRegistry,
) -> std::io::Result<bool> {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[WARNING] Cannot bind metrics TCP address {}: {}",
                addr, e
            );
            return Ok(false);
        }
    };

    eprintln!("[INFO] Metrics TCP endpoint: http://{}/metrics", addr);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut stream, _)) => {
                    let text = metrics.format_prometheus();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Type: text/plain; version=0.0.4\r\n\
                         Content-Length: {}\r\n\
                         Connection: close\r\n\
                         \r\n\
                         {}",
                        text.len(),
                        text,
                    );
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
                Err(e) => {
                    eprintln!("[WARNING] Metrics TCP accept error: {}", e);
                }
            }
        }
    });

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    /// Start metrics TCP server on a random port, return the bound address.
    async fn start_test_metrics_server(metrics: MetricsRegistry) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((mut stream, _)) => {
                        let text = metrics.format_prometheus();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\n\
                             Content-Type: text/plain; version=0.0.4\r\n\
                             Content-Length: {}\r\n\
                             Connection: close\r\n\
                             \r\n\
                             {}",
                            text.len(),
                            text,
                        );
                        tokio::spawn(async move {
                            use tokio::io::AsyncWriteExt;
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                    Err(_) => break,
                }
            }
        });
        addr
    }

    /// Read full HTTP response from a TCP stream.
    async fn read_http_response(stream: &mut TcpStream) -> (String, String) {
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap();
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        // Split headers and body
        if let Some(pos) = raw.find("\r\n\r\n") {
            let headers = raw[..pos].to_string();
            let body = raw[pos + 4..].to_string();
            (headers, body)
        } else {
            (raw, String::new())
        }
    }

    #[test]
    fn test_counter_vec_inc() {
        let cv = CounterVec::new("test_total", "Test counter");
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["CREATE", "nginx"]);
        cv.inc(&["MODIFY", "global"]);
        cv.inc(&["CREATE", "nginx"]);

        let entries = cv.gather();
        // Sort for deterministic assertion
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
        let cv = CounterVec::new("test_total", "Test counter");
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
        let g = IntGauge::new("test_gauge", "Test gauge");
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

    #[test]
    fn test_format_prometheus() {
        let r = MetricsRegistry::new();
        r.inc_event("CREATE", "nginx");
        r.inc_event("CREATE", "nginx");
        r.inc_event("MODIFY", "global");
        r.set_subscribers(3);
        r.set_monitored_paths(5);

        let text = r.format_prometheus();
        assert!(text.contains("fsmon_events_total{event_type=\"CREATE\",cmd=\"nginx\"} 2"));
        assert!(text.contains("fsmon_events_total{event_type=\"MODIFY\",cmd=\"global\"} 1"));
        assert!(text.contains("fsmon_subscribers 3"));
        assert!(text.contains("fsmon_monitored_paths 5"));
        assert!(text.contains("# HELP fsmon_subscribers"));
        assert!(text.contains("# TYPE fsmon_subscribers gauge"));
        assert!(text.contains("# HELP fsmon_events_total"));
        assert!(text.contains("# TYPE fsmon_events_total counter"));
    }

    #[test]
    fn test_format_prometheus_empty_counters() {
        let r = MetricsRegistry::new();
        // No events incremented
        let text = r.format_prometheus();
        // Should still have gauge lines
        assert!(text.contains("fsmon_subscribers 0"));
        assert!(!text.contains("fsmon_events_total{"));
    }

    // ── TCP HTTP /metrics integration tests ──

    #[tokio::test]
    async fn test_tcp_metrics_http_200_content_type() {
        // Verify HTTP response format: status line, Content-Type, valid body
        let r = MetricsRegistry::new();
        r.set_subscribers(7);
        r.set_monitored_paths(3);
        r.set_reader_groups(2);
        r.set_pending_paths(1);

        let addr = start_test_metrics_server(r).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (headers, body) = read_http_response(&mut stream).await;

        // HTTP status line
        assert!(headers.starts_with("HTTP/1.1 200 OK"), "Expected 200 OK, got: {}", headers.lines().next().unwrap_or(""));
        // Content-Type
        assert!(headers.contains("Content-Type: text/plain"), "Expected text/plain Content-Type");
        // Body is valid Prometheus text
        assert!(body.contains("fsmon_subscribers 7"));
        assert!(body.contains("fsmon_monitored_paths 3"));
        assert!(body.contains("fsmon_reader_groups 2"));
        assert!(body.contains("fsmon_pending_paths 1"));
        assert!(body.contains("# HELP fsmon_subscribers"));
        assert!(body.contains("# TYPE fsmon_subscribers gauge"));
    }

    #[tokio::test]
    async fn test_tcp_metrics_empty_registry() {
        // Empty registry: all gauges at zero, no counter lines
        let r = MetricsRegistry::new();
        let addr = start_test_metrics_server(r).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (headers, body) = read_http_response(&mut stream).await;

        assert!(headers.starts_with("HTTP/1.1 200 OK"));
        assert!(body.contains("fsmon_subscribers 0"));
        assert!(body.contains("fsmon_monitored_paths 0"));
        assert!(!body.contains("fsmon_events_total{"));
    }

    #[tokio::test]
    async fn test_tcp_metrics_with_events() {
        // Events should appear as labeled counter lines
        let r = MetricsRegistry::new();
        r.inc_event("CREATE", "nginx");
        r.inc_event("CREATE", "nginx");
        r.inc_event("DELETE", "global");
        r.inc_event("MODIFY", "global");
        r.inc_event("MODIFY", "global");
        r.inc_event("MODIFY", "global");

        let addr = start_test_metrics_server(r).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (_, body) = read_http_response(&mut stream).await;

        assert!(body.contains("fsmon_events_total{event_type=\"CREATE\",cmd=\"nginx\"} 2"));
        assert!(body.contains("fsmon_events_total{event_type=\"DELETE\",cmd=\"global\"} 1"));
        assert!(body.contains("fsmon_events_total{event_type=\"MODIFY\",cmd=\"global\"} 3"));
        // HELP/TYPE for the counter
        assert!(body.contains("# HELP fsmon_events_total"));
        assert!(body.contains("# TYPE fsmon_events_total counter"));
    }

    #[tokio::test]
    async fn test_tcp_metrics_connection_close_header() {
        // Prometheus expects Connection: close for one-shot scrape
        let r = MetricsRegistry::new();
        let addr = start_test_metrics_server(r).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (headers, _) = read_http_response(&mut stream).await;

        assert!(headers.contains("Connection: close"),
            "Prometheus scrape requires Connection: close, got headers: {}", headers);
    }

    #[tokio::test]
    async fn test_tcp_metrics_content_length_matches_body() {
        // Content-Length must match actual body length for HTTP compliance
        let r = MetricsRegistry::new();
        r.set_subscribers(42);
        r.set_monitored_paths(99);

        let addr = start_test_metrics_server(r).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (headers, body) = read_http_response(&mut stream).await;

        // Extract Content-Length value
        let cl = headers
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|s| s.trim().parse::<usize>().ok())
            .expect("Content-Length header missing or invalid");

        assert_eq!(cl, body.len(),
            "Content-Length ({}) must match body length ({})", cl, body.len());
    }
}
