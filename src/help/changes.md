Show the most recent event per path — a deduplicated summary of file changes.

Output is JSONL (same format as `query`), but with duplicate paths collapsed:
only the latest event for each unique path is shown, sorted by time descending.

This is the easiest way to answer "what files changed since last deploy?"

USAGE:
  fsmon changes [CMD] [OPTIONS]

ARGS:
  <CMD>   Cmd group to query (positional). Omit to query all cmd groups.

Options:
  -p, --path        Path prefix filter(s). Repeatable.
  -t, --time        Time filter with operator (repeatable).
                    >1h  — events newer than 1 hour ago
                    <2026-05-01 — events before a date
                    Combine both for a range: -t '>1h' -t '<now'

Examples:
  fsmon changes                        Latest event per path across all cmd groups
  fsmon changes _global                Latest event per path in global log
  fsmon changes nginx -t '>1h'        Latest nginx file changes in last hour
  fsmon changes -p /etc -t '>24h'     What changed in /etc since yesterday
  fsmon changes -t '>2026-05-25 08:00'  What changed since last deploy
  fsmon changes | wc -l               Count of changed files
