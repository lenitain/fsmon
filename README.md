# fsmon

Real-time Linux filesystem change monitoring with process attribution.

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

🌍 **Language**: [English](./README.md) | [简体中文](./README.zh-CN.md)

## Overview

**fsmon** is a real-time Linux filesystem change monitor powered by fanotify. It watches files and directories, captures every event (create, modify, delete, move, attribute change, etc.), and attributes each change back to the process that caused it — including the PID, command name, user, parent PID, thread group ID, and optional full process ancestry chain.

### Why fsmon?

Unlike standard file monitoring tools that only report which file changed, **fsmon** adds **process attribution** — it identifies which process caused each change. This makes it easier to debug unexpected file modifications in multi-process environments. For system administrators and developers who need to track down the source of filesystem changes, fsmon provides deeper insights that traditional tools cannot offer.

## Usage

```
Usage: fsmon [OPTIONS] <COMMAND>

Commands:
  daemon, d       Start the fsmon daemon
  add, a          Add a path to the monitoring list
  remove, r       Remove paths from the monitoring list
  monitored, m    List monitored paths
  query, q        Query historical events
  clean, cl       Clean log files
  changes, ch     Show most recent event per path
  init, i         Create config file
  cd              Open subshell in directory
  health, h       Query daemon health status

Options:
  -h, --help      Print help
  -V, --version   Print version
```

### Quick start

```bash
# Install
cargo install fsmon

# Start daemon (requires root for fanotify)
sudo fsmon daemon

# In another terminal, add a path to monitor
fsmon add _global --path /var/www -r

# Query events
fsmon query _global | jq 'select(.cmd == "nginx")'
```

## Building from Source

Requires Rust toolchain (tested with `rustc 1.78.0`).

```bash
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo build --release
```
