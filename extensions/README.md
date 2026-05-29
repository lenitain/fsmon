# fsmon extensions

Minimal examples for fsmon's two JSONL data exits.

## ① JSONL file — persistent on-disk events

- `examples/read-jsonl.sh` — jq queries, recent events
- `examples/read-jsonl.py` — same in Python

## ② Unix socket — zero-disk real-time stream

Subscribe protocol: send TOML command → receive TOML OK → stream JSONL.

- `examples/subscribe.sh` — socat + jq (python fallback)
- `examples/subscribe.py` — socket programming in 25 lines

Adapt them to your downstream of choice.
