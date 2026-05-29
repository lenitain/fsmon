# fsmon extensions

Minimal examples for fsmon's two JSONL data exits.

## ① JSONL file — persistent on-disk events

- `examples/read-jsonl.sh` — jq queries, real-time tail
- `examples/read-jsonl.py` — same in Python

## ② Unix socket — zero-disk real-time stream

- `examples/subscribe.sh` — connect with nc, pipe to jq
- `examples/subscribe.py` — same in Python (5 lines)

Adapt them to your downstream of choice.
