# fsmon

Real-time Linux filesystem change monitoring with process attribution.

[![Crates.io](https://img.shields.io/crates/v/fsmon)](https://crates.io/crates/fsmon)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/lenitain/fsmon/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/fsmon/actions/workflows/ci.yml)

🌍 **Language**: [English](./README.md) | [简体中文](./README.zh-CN.md)

## Overview

**fsmon** is a real-time Linux filesystem change monitor powered by fanotify. It watches files and directories, captures every event (create, modify, delete, move, attribute change, etc.), and attributes each change back to the process that caused it — including the PID, command name, user, parent PID, thread group ID, and optional full process ancestry chain.

Process tracking is **event-driven** : the kernel's cn_proc event stream maintains the topology with no polling and no recursive `/proc` scanning; per-CPU message sequences quantify lost events. A one-shot `/proc` scan serves only as the bootstrap baseline.

### Why fsmon?

Unlike standard file monitoring tools that only report which file changed, **fsmon** adds **process attribution** — it identifies which process caused each change. This makes it easier to debug unexpected file modifications in multi-process environments. For system administrators and developers who need to track down the source of filesystem changes, fsmon provides deeper insights that traditional tools cannot offer.

This crate is Linux-only and will fail to compile on other platforms.

## Usage

```
Lightweight high-performance file change tracking tool

Usage: fsmon <COMMAND>

Commands:
  daemon     Run the fsmon daemon (needs CAP_SYS_ADMIN for pid attribution) [alias: d]
  add        Add a path to the monitoring list [alias: a]
  remove     Remove one or more paths from the monitoring list [alias: r]
  monitored  List all monitored paths with their configuration [alias: m]
  query      Query historical file change events from log files [alias: q]
  clean      Clean historical log files, retain by time or size [alias: cl]
  changes    Show the most recent event per path (deduplicated changes) [alias: ch]
  init       Create the config file (directories created on first use) [alias: i]
  cd         Open a subshell in the monitored path or log directory
  health     Query daemon health status [alias: h]
  help       Print this message or the help of the given subcommand(s)

Options:
  -v, --version  Print version
  -h, --help     Print help (see more with '--help')
```

Man pages and shell completion scripts (bash, fish, zsh, nushell) can be generated with:
```
fsmon init -c
```

Use `fsmon --help` or `man fsmon` for detailed documentation.

### Quick start

```bash
# Install
cargo install fsmon

# Install the hardened systemd service: runs as your user, grants only
# CAP_SYS_ADMIN, and filters the syscall surface
sudo fsmon init --service
sudo systemctl enable --now fsmon

# Or run it manually for debugging
sudo fsmon daemon

# In another terminal, add a path to monitor
fsmon add _global --path /var/www -r

# Query events
fsmon query _global | jq 'select(.cmd == "nginx")'
```

## Privileges

fsmon needs **`CAP_SYS_ADMIN` for exactly one thing**: creating a *privileged*
fanotify group. The kernel marks a group created without that capability as
`FANOTIFY_UNPRIV`, and then blanks `metadata.pid` for every event caused by
another process (`fs/notify/fanotify/fanotify_user.c`) — process attribution,
fsmon's entire point, silently becomes `pid: 0`.

Nothing else needs privilege: marking, reading events, FID→path resolution,
process tracking and logging all work unprivileged.

The daemon is built so that this capability is confined:

- At startup it forks a tiny **fanotify factory** subprocess that inherits
  `CAP_SYS_ADMIN`. Its whole input is `(dirfd[, fan_fd], flags, mask)` — no
  path strings, no JSON, no event stream — and it runs under a seccomp
  whitelist of eight syscalls (`fanotify_init`, `fanotify_mark`, `recvmsg`,
  `sendmsg`, `read`, `write`, `close`, `exit_group`).
- The main daemon then drops **all** capabilities (`CapEff=0`,
  `PR_SET_NO_NEW_PRIVS`). The privileged fanotify groups keep reporting real
  pids because the kernel stores `FANOTIFY_UNPRIV` on the *group* object.
- The privilege never exists as a file on disk, so no other local user can
  obtain it (unlike a `setcap` helper binary).

`fsmon init --service` writes a unit that runs the daemon as your user with
`AmbientCapabilities=CAP_SYS_ADMIN`, `CapabilityBoundingSet=CAP_SYS_ADMIN`,
`NoNewPrivileges=yes`, `SystemCallFilter=@system-service fanotify_init
fanotify_mark`, and write access limited to the store, log and runtime
directories. Note that `fanotify_init`/`fanotify_mark` are **not** part of
systemd's `@system-service` set, so they must be listed explicitly.

fsmon deliberately does **not** request `CAP_DAC_READ_SEARCH`. It would only
serve the `open_by_handle_at` path-resolution fallback, which in practice is
never reached (directory handles are pre-cached via the unprivileged
`name_to_handle_at`), and delegating it would create a real file-read oracle.
Instead the resolver runs with its syscall fallback turned off, so the fallback
fails with zero syscalls; `fsmon health` exposes the miss count as
`dir_cache_misses`.

If you start the daemon without `CAP_SYS_ADMIN` it refuses to run rather than
silently recording `pid: 0`. Set `FSMON_ALLOW_UNPRIVILEGED=1` to accept the
degraded mode deliberately; `fsmon health` then reports `"unprivileged": true`.

## Building from Source

Requires Rust toolchain (tested with `rustc 1.78.0`).

```bash
git clone https://github.com/lenitain/fsmon.git
cd fsmon
cargo build --release
```

## Known Limitations

### `comm` for short-lived processes (kernel limitation)

fsmon tracks processes via the kernel's cn_proc connector (fork/exec/exit events). The kernel only emits a `COMM` event when a process renames itself via `prctl(PR_SET_NAME)` — **never on `exec`** (`proc_comm_connector` is only called from the `PR_SET_NAME` branch in `kernel/sys.c`). This is kernel design, not a bug in fsmon or its dependencies.

Consequences for short-lived processes (spawn → exec → exit in <1 ms, e.g. `touch`):

- File events are **never missed**; pid/tgid/ppid/chain are complete.
- The `comm`/`cmd` fields of the recorded event are **empty** — the process is usually already gone by the time the event is processed, so `/proc` cannot be read either.
- A `cmd=` filter group does not match such processes.

Long-lived processes are unaffected: their comm is captured by the bootstrap `/proc` scan and refreshed via rename events.
