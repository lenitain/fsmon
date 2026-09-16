#!/bin/bash
# End-to-end regression checks for the three daemon-hardening fixes.
#
# These exercise the real binary, so they catch integration breakage that the
# unit tests in src/common/monitor/factory.rs cannot. Run from anywhere:
#
#     tools/check-daemon-hardening.sh [path-to-fsmon]
#
# Checks:
#   1. missing CAP_SYS_ADMIN exits 2 (RestartPreventExitStatus), not 1
#   2. the factory inherits no descriptors beyond the std streams + its socket
#   3. the factory dies with the daemon on SIGKILL (no orphan holding the cap)
#   4. recursive marking still delivers events (batching must not drop marks)
set -u

FSMON="${1:-$(dirname "$0")/../target/release/fsmon}"
if [ ! -x "$FSMON" ]; then
    echo "error: fsmon binary not found at $FSMON (build with: cargo build --release)" >&2
    exit 2
fi

WORK="$HOME/.cache/fsmon-verify"
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" XDG_RUNTIME_DIR="$WORK/run"
rm -rf "$WORK"; mkdir -p "$HOME" "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
"$FSMON" init >/dev/null 2>&1

fail=0
pass() { echo "  PASS  $1"; }
bad()  { echo "  FAIL  $1"; fail=$((fail + 1)); }

echo "=== 1. missing CAP_SYS_ADMIN must exit 2 ==="
if [ "$(id -u)" -eq 0 ]; then
    echo "  SKIP  running as root (the daemon legitimately has the capability)"
else
    "$FSMON" daemon >"$WORK/refuse.log" 2>&1
    code=$?
    if [ "$code" -eq 2 ]; then
        pass "exit code 2 (systemd will not restart it)"
    else
        bad "exit code $code, expected 2 — systemd would retry StartLimitBurst times"
    fi
    if grep -q "CAP_SYS_ADMIN" "$WORK/refuse.log"; then
        pass "error names the missing capability"
    else
        bad "error does not mention CAP_SYS_ADMIN"
    fi
fi

echo
echo "=== 2/3. factory descriptor hygiene and lifetime ==="
W="$WORK/watch"; mkdir -p "$W"
FSMON_ALLOW_UNPRIVILEGED=1 "$FSMON" daemon >"$WORK/daemon.log" 2>&1 &
D=$!
sleep 2
FACTORY="$(pgrep -P "$D" | head -1)"
if [ -z "${FACTORY:-}" ]; then
    bad "no factory child found under the daemon (pid $D)"
else
    # Highest fd the factory holds. Only 0-2 (std streams) and 3 (socketpair)
    # may survive; anything else is a leaked descriptor in a CAP_SYS_ADMIN
    # process.
    highest=0
    for f in /proc/"$FACTORY"/fd/*; do
        n="$(basename "$f")"
        [ "$n" -gt "$highest" ] 2>/dev/null && highest="$n"
    done
    if [ "$highest" -le 3 ]; then
        pass "factory holds only fds <= 3 (highest: $highest)"
    else
        bad "factory still holds fd $highest — inherited descriptors were not closed"
    fi
    if [ "$(grep -oP 'Seccomp:\s*\K\d+' /proc/"$FACTORY"/status)" = "2" ]; then
        pass "factory runs under a seccomp filter"
    else
        bad "factory has no seccomp filter installed"
    fi
fi

"$FSMON" add _global --path "$W" -r >/dev/null 2>&1
sleep 1
mkdir -p "$W/sub/deep"
echo x > "$W/sub/deep/late.txt"
echo y > "$W/top.txt"
sleep 2

echo
echo "=== 4. events still captured after batching ==="
events="$("$FSMON" query _global 2>/dev/null | grep -c '^{')"
paths="$("$FSMON" query _global 2>/dev/null | grep -c '"path":"'"$W"'')"
if [ "$events" -gt 0 ] && [ "$paths" -gt 0 ]; then
    pass "$events events, $paths with a resolved path"
else
    bad "no events captured (events=$events paths=$paths)"
fi

if [ -n "${FACTORY:-}" ]; then
    kill -9 "$D" 2>/dev/null
    sleep 1
    if kill -0 "$FACTORY" 2>/dev/null; then
        bad "factory survived SIGKILL of the daemon — it would hold CAP_SYS_ADMIN forever"
        kill -9 "$FACTORY" 2>/dev/null
    else
        pass "factory died with the daemon"
    fi
else
    kill -9 "$D" 2>/dev/null
fi

echo
if [ "$fail" -eq 0 ]; then
    echo "all checks passed"
else
    echo "$fail check(s) failed"
fi
exit "$fail"
