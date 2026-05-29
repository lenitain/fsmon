"""
Canonical retry + dead-letter-queue helpers.

DO NOT import this file. Copy the functions you need into your bridge script.
No external dependencies (stdlib only).

Provides:
  - retry_with_backoff()   — call a function with exponential backoff
  - DeadLetterQueue        — append failed items to a JSONL file on disk
"""

import json
import logging
import os
import time
from collections.abc import Callable
from typing import Any, TypeVar

logger = logging.getLogger(__name__)

T = TypeVar("T")


def retry_with_backoff(
    fn: Callable[[], T],
    max_retries: int = 3,
    base_delay: float = 1.0,
    *,
    on_failure: Callable[[Exception, int], None] | None = None,
) -> T:
    """Call fn(), retrying on exception with exponential backoff.

    Args:
        fn: Zero-argument callable to retry.
        max_retries: Total attempts (1 initial + N retries).
        base_delay: Initial delay in seconds, doubles each retry.
        on_failure: Called with (exception, attempt_number) on each failure.

    Returns:
        Return value of fn() on success.

    Raises:
        The last exception if all retries are exhausted.
    """
    last_exc: Exception | None = None
    for attempt in range(max_retries):
        try:
            return fn()
        except Exception as exc:
            last_exc = exc
            if on_failure:
                on_failure(exc, attempt + 1)
            if attempt < max_retries - 1:
                delay = base_delay * (2 ** attempt)
                logger.warning(
                    "attempt %d/%d failed: %s, retrying in %.1fs...",
                    attempt + 1, max_retries, exc, delay,
                )
                time.sleep(delay)
    raise last_exc  # type: ignore[misc]


class DeadLetterQueue:
    """Append failed items to a JSONL dead-letter file.

    Usage:
        dlq = DeadLetterQueue("/tmp/fsmon-dlq")
        dlq.append({"event": ..., "error": "timeout"})

    The file is rotated daily: dlq-2026-05-29.jsonl
    """

    def __init__(self, directory: str, max_file_size_mb: int = 100):
        self.directory = directory
        self.max_file_size_mb = max_file_size_mb
        os.makedirs(directory, exist_ok=True)

    def append(self, item: dict[str, Any]) -> None:
        """Append one item as a JSON line to today's dead-letter file."""
        today = time.strftime("%Y-%m-%d")
        path = os.path.join(self.directory, f"dlq-{today}.jsonl")

        # Rotate if file exceeds limit
        try:
            if os.path.getsize(path) > self.max_file_size_mb * 1024 * 1024:
                rotated = f"{path}.{int(time.time())}"
                os.rename(path, rotated)
                logger.warning("dead-letter file rotated: %s", rotated)
        except FileNotFoundError:
            pass

        try:
            with open(path, "a") as f:
                f.write(json.dumps(item, default=str) + "\n")
        except OSError as exc:
            logger.error("failed to write dead-letter: %s", exc)
