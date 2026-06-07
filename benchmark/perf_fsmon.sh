echo 0 >/proc/sys/kernel/kptr_restrict
perf record -g -p $(pgrep -x fsmon)
