"""
Canonical JSON stats line output.

DO NOT import this file. Copy the function into your bridge script.
Outputs a single-line JSON object to stderr for monitoring consumption.

Usage:
    stats = StatsReporter()
    stats.inc("events")
    stats.inc("errors")
    stats.flush()  # or let the periodic timer do it
"""

import json
import logging
import sys
import time
from typing import Any

logger = logging.getLogger(__name__)


class StatsReporter:
    """Track counters and periodically emit JSON stats line to stderr."""

    def __init__(self, interval_secs: float = 30.0):
        self.interval_secs = interval_secs
        self.counters: dict[str, int] = {}
        self.start_time = time.time()
        self.last_flush = time.time()

    def inc(self, name: str, delta: int = 1) -> None:
        """Increment a named counter."""
        self.counters[name] = self.counters.get(name, 0) + delta

    def set(self, name: str, value: int) -> None:
        """Set a gauge value."""
        self.counters[name] = value

    def maybe_flush(self) -> None:
        """Flush if the interval has elapsed since last flush."""
        now = time.time()
        if now - self.last_flush >= self.interval_secs:
            self.flush()

    def flush(self) -> None:
        """Emit current stats as a JSON line to stderr."""
        now = time.time()
        stats: dict[str, Any] = {
            "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now)),
            "uptime_secs": int(now - self.start_time),
        }
        stats.update(self.counters)
        print(json.dumps(stats), file=sys.stderr, flush=True)
        self.last_flush = now
