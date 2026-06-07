# fsmon Benchmark

Performance and correctness test suite for fsmon.

## Structure

```
benchmark/
├── events_run.sh           # Event capture tests entry
├── post_run.sh             # Post-processing tests entry (query / clean)
├── perf_fsmon.sh           # perf sampling helper
└── tests/
    ├── events/             # Event reception tests
    │   ├── create.sh       # CREATE events (100 files)
    │   ├── modify.sh       # MODIFY events (50 files)
    │   ├── delete.sh       # DELETE events (50 files)
    │   ├── move.sh         # MOVE events (30 files)
    │   ├── recursive.sh    # Recursive monitoring (2-level subdirs)
    │   └── stress.sh       # Stress test (1000 files + 10 parallel modifiers)
    └── post/               # Post-processing tests
        ├── query.sh        # Query performance (100/1000/5000 events + jq pipeline)
        └── clean.sh        # Clean performance (by count/time/dry-run)
```

Each test script is self-contained with its own setup, assertions, and cleanup.

## Prerequisites

```bash
# Build fsmon
cargo build --release

# Initialize config
fsmon init

# Install systemd service (optional)
sudo fsmon init --service
```

## Running Tests

### Prerequisites

```bash
# Start daemon (requires sudo for fanotify)
sudo fsmon daemon &
```

### Run all event tests

```bash
bash events_run.sh
```

### Run all post-processing tests

```bash
bash post_run.sh
```

### Step 2: Profile performance

Use perf to analyze where fsmon spends CPU time during event capture:

```bash
# Terminal 1: start perf recording on fsmon daemon
sudo perf record -g -a -p $(pgrep -x fsmon) -o /tmp/perf_events.data &

# Terminal 2: run event tests
bash events_run.sh

# Terminal 1: stop perf (Ctrl+C), then analyze
sudo perf report -i /tmp/perf_events.data
```

Profile post-processing separately:

```bash
sudo perf record -g -a -p $(pgrep -x fsmon) -o /tmp/perf_post.data &
bash post_run.sh
# Ctrl+C, then:
sudo perf report -i /tmp/perf_post.data
```

Or use the helper script for quick single-run profiling:

```bash
bash perf_fsmon.sh
```

### Profile stress test

To collect perf data during the stress test:

```bash
bash perf_stress.sh [stress_count]   # default: 5000

# View report
sudo perf report -i /tmp/perf_stress.data
```

## Test Details

### Event Tests (`tests/events/`)

| Test | What it does | Pass criteria |
|------|-------------|---------------|
| create.sh | Creates 100 files, queries CREATE events | Exactly 100 events |
| modify.sh | Creates 50 files then appends to each | Exactly 50 MODIFY events |
| delete.sh | Creates 50 files then deletes them | Exactly 50 DELETE events |
| move.sh | Moves 30 files to a subdirectory | Exactly 30 MovedTo events (or warn if inotify limitation) |
| recursive.sh | Creates files in 2-level nested dirs | All 3 files captured |
| stress.sh | Creates 1000 files in 21ms, then 10 parallel processes modify 100 files each | Exactly 1000 CREATE + 1000 MODIFY |

### Post-Processing Tests (`tests/post/`)

| Test | What it does | Pass criteria |
|------|-------------|---------------|
| query.sh | Queries 100/1000/5000 events, measures jq pipeline | All queries < threshold |
| clean.sh | Cleans by count, time filter, and dry-run | All cleans < threshold |

## Design Notes

- **Test isolation**: Each event test uses a unique directory (`/tmp/fsmon_create`, `/tmp/fsmon_modify`, etc.) to avoid event cross-contamination between tests.
- **Move events**: `MovedTo` is captured when the destination directory is being monitored. This is expected inotify behavior — events are only reported for watched directories.
- **Recursive monitoring**: Covers directories that exist when `fsmon add` is called. New subdirectories created after monitoring starts require re-registration.
- **Event counts**: Tests verify exact counts (`==`). If concurrent operations lose events, that's a bug the test should catch.
- **No sudo in tests**: Test scripts do not use sudo. The daemon must be started manually before running tests.
