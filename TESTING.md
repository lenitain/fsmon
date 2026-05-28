# fsmon Output Channel Testing Guide

## Prerequisites

```bash
# Build
cd fsmon && cargo build --release

# Start daemon (all tests below depend on this)
sudo ./target/release/fsmon daemon --debug &

# Add a test path
fsmon add _global --path /tmp/fsmon-test -r

# Create working directory
mkdir -p /tmp/fsmon-test
```

Cleanup after testing:
```bash
sudo kill %1
rm -rf /tmp/fsmon-test
```

---

## (1) File Output — JSONL Logs

**Already testable.**

```bash
# Trigger events
echo "hello" > /tmp/fsmon-test/a.txt
rm /tmp/fsmon-test/a.txt

# Verify log file exists
ls ~/.local/state/fsmon/_global_log.jsonl

# Verify event content
cat ~/.local/state/fsmon/_global_log.jsonl | tail -2 | jq .
```

Extended tests:

```bash
# --log-path enables file output
sudo kill %1
sudo ./target/release/fsmon daemon --log-path ~/.local/state/fsmon &
echo "test" > /tmp/fsmon-test/b.txt
cat ~/.local/state/fsmon/_global_log.jsonl | wc -l  # line count unchanged

# --local-time uses local timezone
sudo kill %1
sudo ./target/release/fsmon daemon --local-time &
echo "test" > /tmp/fsmon-test/c.txt
cat ~/.local/state/fsmon/_global_log.jsonl | tail -1 | jq .time  # should show +08:00 not Z
```

---

## (2) Push Output — Socket Subscribe

### Basic: receive event stream

```bash
# Terminal 1: start daemon (skip if already running)
sudo ./target/release/fsmon daemon &

# Terminal 2: subscribe to all events
echo 'cmd = "subscribe"' | nc -U /tmp/fsmon-$(id -u).sock

# Terminal 3: trigger events
echo "subscribe test" > /tmp/fsmon-test/sub.txt
rm /tmp/fsmon-test/sub.txt
```

**Expected:** Terminal 2 receives `ok = true` first, then continuous JSONL event stream.

### Filter by event type

```bash
echo 'cmd = "subscribe"
types = ["CREATE"]' | nc -U /tmp/fsmon-$(id -u).sock

# In another terminal:
echo "a" > /tmp/fsmon-test/mod.txt      # → should receive CREATE
echo "mod" >> /tmp/fsmon-test/mod.txt   # → should NOT receive MODIFY
rm /tmp/fsmon-test/mod.txt              # → should NOT receive DELETE
```

### Multiple subscribers

```bash
# Open 3 terminals, each subscribing — all should receive the same events
# Terminal A:
echo 'cmd = "subscribe"' | nc -U /tmp/fsmon-$(id -u).sock > /tmp/sub-a.log &
# Terminal B:
echo 'cmd = "subscribe"' | nc -U /tmp/fsmon-$(id -u).sock > /tmp/sub-b.log &
# Terminal C: trigger event
echo "multi" > /tmp/fsmon-test/multi.txt
# Compare outputs
diff <(tail -3 /tmp/sub-a.log) <(tail -3 /tmp/sub-b.log)
```

### Lag test (slow subscriber)

```bash
# Rapidly generate 5000+ events
for i in $(seq 1 5000); do echo "$i" > /tmp/fsmon-test/$i.txt; done
# Expected: slow subscribers receive warning: "subscriber too slow, dropped N events"
```

### Disconnect test

```bash
# Ctrl+C a subscriber → daemon should not crash
# Reconnect → should receive events normally
```

---

## (3) Pull Socket — `metrics` Command

### Basic test

```bash
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock
```

**Expected:** Prometheus text output containing `fsmon_events_total`, `fsmon_subscribers`, etc. Connection closes immediately after response.

### Counter increment test

```bash
# 1. Record current value
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_events_total | grep CREATE

# 2. Trigger 10 CREATE events
for i in $(seq 1 10); do echo "m$i" > /tmp/fsmon-test/m$i.txt; done
sleep 1

# 3. Pull again → should increase by 10
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_events_total | grep CREATE
```

### Gauge test

```bash
# subscribers
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_subscribers
# Expected: 0

# Open a subscribe connection
echo 'cmd = "subscribe"' | nc -U /tmp/fsmon-$(id -u).sock > /dev/null &
sleep 1
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_subscribers
# Expected: 1

# Close subscribe
kill %%
sleep 1
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_subscribers
# Expected: 0

# monitored_paths
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock | grep fsmon_monitored_paths
# Expected: >= 1 (because /tmp/fsmon-test was added)
```

### No daemon running

```bash
sudo kill %1
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock
# Expected: nc error "Connection refused" or "No such file"
```

---

## (4) Pull TCP — HTTP `/metrics`

### Basic test

```bash
# Start daemon with TCP metrics enabled
sudo ./target/release/fsmon daemon --metrics-listen 127.0.0.1:9845 &
sleep 1

# Pull with curl
curl -s http://127.0.0.1:9845/metrics
```

**Expected:** Same Prometheus text as socket `metrics` command.

### Compare with socket output

```bash
diff <(echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock) <(curl -s http://127.0.0.1:9845/metrics)
# Expected: identical output
```

### Verify after triggering events

```bash
echo "tcp-test" > /tmp/fsmon-test/tcp.txt
sleep 1
curl -s http://127.0.0.1:9845/metrics | grep fsmon_events_total | grep CREATE
# Should include the new CREATE count
```

### Port conflict test

```bash
# Bind the port with another process first
python3 -m http.server 9845 --bind 127.0.0.1 &
sleep 1

sudo ./target/release/fsmon daemon --metrics-listen 127.0.0.1:9845 &
# Expected: daemon starts successfully, but prints WARNING: Cannot bind metrics TCP address
# Socket metrics command should still work
echo 'cmd = "metrics"' | nc -U /tmp/fsmon-$(id -u).sock
```

### TCP not enabled

```bash
sudo ./target/release/fsmon daemon &
# No --metrics-listen flag
curl -s http://127.0.0.1:9845/metrics
# Expected: curl error "Connection refused"
```

---

## (5) Command Response — Socket TOML

**Already testable.** add / remove / list / health all use socket TOML protocol.

### Additional checks

```bash
# health command
echo 'cmd = "health"' | nc -U /tmp/fsmon-$(id -u).sock
# Expected: TOML response with uptime_secs, monitored_paths, reader_groups, readers

# list command
echo 'cmd = "list"' | nc -U /tmp/fsmon-$(id -u).sock
# Expected: TOML response with paths array

# invalid command
echo 'cmd = "invalid"' | nc -U /tmp/fsmon-$(id -u).sock
# Expected: error = "Unknown command: invalid"
```

---

## One-Liner Smoke Test

```bash
#!/bin/bash
# Quick check: all output channels reachable
set -e
SOCK=/tmp/fsmon-$(id -u).sock
echo "file"  > /tmp/fsmon-test/smoke.txt

echo "1. File:"
tail -1 ~/.local/state/fsmon/_global_log.jsonl | jq -r '"[\(.event_type)] \(.path)"'

echo "2. Push subscribe:"
timeout 2 bash -c "echo 'cmd = \"subscribe\"' | nc -U $SOCK" | head -3 || true

echo "3. Pull socket metrics:"
echo 'cmd = "metrics"' | nc -U $SOCK | head -3

echo "4. Pull TCP metrics:"
curl -s --connect-timeout 1 http://127.0.0.1:9845/metrics | head -3 || echo "(TCP not enabled — OK)"

echo "5. Command response:"
echo 'cmd = "health"' | nc -U $SOCK | head -1

echo "=== All channels OK ==="
```
