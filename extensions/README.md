# fsmon extensions

Example scripts showing how to consume fsmon's two JSONL data exits.

- `examples/read-jsonl.sh`       — Read JSONL files with jq/tail
- `examples/subscribe-socket.sh`  — Connect to Unix socket for real-time stream
- `examples/subscribe.py`         — Same, in Python (5 lines)
- `examples/forward-to-kafka.sh`  — Pipe socket stream into Kafka via kafkacat

All examples are minimal references. Adapt them to your stack.
