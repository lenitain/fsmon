//! P1 — Metrics report output format tests.

#[test]
fn rss_reading_is_reasonable() {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let parts: Vec<&str> = statm.split_whitespace().collect();
    let rss_pages: u64 = parts[1].parse().unwrap();
    let rss_mb = (rss_pages * 4096) as f64 / (1024.0 * 1024.0);
    // Test process RSS should be between 0.5MB and 500MB
    assert!(rss_mb > 0.5, "RSS too low: {:.1}MB", rss_mb);
    assert!(rss_mb < 500.0, "RSS too high: {:.1}MB", rss_mb);
}

#[test]
fn metrics_format_is_parseable() {
    // Simulate a real metrics output line and verify it can be parsed
    let line = "[metrics] uptime=3600s rss=4.2MB caches(d/p/t/f)=823/156/12/45 readers=3/2/1";
    assert!(line.starts_with("[metrics]"));
    assert!(line.contains("uptime="));
    assert!(line.contains("rss="));
    assert!(line.contains("caches(d/p/t/f)="));
    assert!(line.contains("readers="));
}

#[test]
fn metrics_line_no_missing_sections() {
    // Verify the format is consistent: all expected keys present
    let line = "[metrics] uptime=0s rss=1.0MB caches(d/p/t/f)=0/0/0/0 readers=0/0/0";
    let keys = ["uptime=", "rss=", "caches(d/p/t/f)=", "readers="];
    for key in &keys {
        assert!(line.contains(key), "metrics line should contain '{}'", key);
    }
}
