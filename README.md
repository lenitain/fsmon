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
Note: If installed via 'cargo install', copy to system path for sudo compatibility:
  sudo cp ~/.cargo/bin/fsmon /usr/local/bin/

Config:  ~/.config/fsmon/fsmon.toml (created by 'fsmon init')
Monitor: ~/.local/share/fsmon/monitored.jsonl
Logs:    ~/.local/state/fsmon/
Socket:  /run/user/<UID>/fsmon/daemon.sock

Usage: fsmon <COMMAND>

Commands:
  daemon     Run the fsmon daemon (requires sudo for fanotify) [aliases: d]
  add        Add a path to the monitoring list [aliases: a]
  remove     Remove one or more paths from the monitoring list [aliases: r]
  monitored  List all monitored paths with their configuration [aliases: m]
  query      Query historical file change events from log files [aliases: q]
  clean      Clean historical log files, retain by time or size [aliases: cl]
  changes    Show the most recent event per path (deduplicated changes) [aliases: ch]
  init       Create the config file (directories created on first use) [aliases: i]
  cd         Open a subshell in the monitored path or log directory
  health     Query daemon health status [aliases: h]
  help       Print this message or the help of the given subcommand(s)

Options:
  -v, --version  Print version
  -h, --help     Print help
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
