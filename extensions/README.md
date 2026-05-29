# fsmon extensions

Example scripts for fsmon's two JSONL data exits.

## ① JSONL file — persistent on-disk events

- `examples/read-jsonl.sh` — jq queries, real-time tail

## ② Unix socket — zero-disk real-time stream

- `examples/subscribe-socket.sh` — connect with nc, pipe to jq
- `examples/subscribe.py` — same in Python (5 lines)

All examples are minimal. Adapt them to your downstream of choice.
