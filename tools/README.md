# tools/ — 权限分离方案的验证探针

本目录是 `../PRIVILEGE-SEPARATION-PLAN.md` 里**每一个实测结论**的可复现证据。
所有探针都不依赖 fsmon 源码，各自独立可编译运行。

| 探针 | 证明了什么 | 需要 root？ |
|---|---|---|
| `captest2.c` | §1 能力矩阵：哪些 syscall 真正需要特权 | 否 |
| `captest4.c` | §2 非特权 fanotify 会把 `pid` 抹成 0；userns 无效 | 否 |
| `fsidmatrix.c` | §5.2 **决定性**：一个 group 装不下多个文件系统，内核返回 `EXDEV` | 否 |
| `fsidmix.c` | §5.2 同上，早期版本（含共享 group 事件投递测试） | 否 |
| `np-tier3.sh` | §1 端到端非特权运行：310/310 路径正确、0 次 tier-3、0 次归因 | 否 |
| `verify-fanotify-privsep.c` | §8 **支点假设**：特权随 fd 传递，`CapEff=0` 的读者拿到 root 事件的真实 pid | **是**（一次） |
| `fidtest/` | §5.6b **`fanotify-fid` 自身的通用缺陷**：RENAME / PIDFD / FS_ERROR 记录被静默丢弃 | 否 |

> `fidtest/` 与其他探针不同：它针对的是**通用库**而非 fsmon。
> 它依赖的是 **crates.io 上发布的** `fanotify-fid` —— 不是本地检出，也不是
> `path = "/home/<某人>/…"`（那样只有一台机器能构建）。`Cargo.lock` 锁定版本，
> 克隆本仓库即可直接复现，无需先构建 fanotify-fid。
>
> 它跟踪的是 **fsmon 自己解析到的那个版本**，所以它展示的就是 fsmon 实际拿到的行为。
> 要换成别的版本，改 `Cargo.lock` 而不是 `Cargo.toml`：`build.rs` 读的是**解析后的**
> 版本并据此选择编译哪条分支，所以
> `cargo update -p fanotify-fid --precise X.Y.Z` 就够了，源码一行不用动。
>
> | 锁定版本 | 展示什么 |
> |---|---|
> | `0.7.1`（当前） | **修复后**：RENAME / PIDFD / FS_ERROR 记录被解析，可通过访问器读到 |
> | `0.7.0` | **缺陷基线**：同样的记录被静默丢弃，公开 API 毫无提示 —— §5.6b 当初证明的就是这个 |
>
> 这些缺陷**不影响**本方案的实施（见 §5.6.1：fsmon 按构造避开了它们），
> 所以本目录的结论对本方案依然成立。

## 跑法

```bash
# ── 无需 root ──
gcc -O2 -o /tmp/captest2   captest2.c   && /tmp/captest2
gcc -O2 -o /tmp/captest4   captest4.c   && /tmp/captest4
gcc -O2 -o /tmp/fsidmatrix fsidmatrix.c && /tmp/fsidmatrix   # 最重要的一张表

# fanotify-fid 自身的通用缺陷 —— §5.6b
(cd fidtest && cargo run --quiet)                               # 修复后（0.7.1）
(cd fidtest && cargo update -p fanotify-fid --precise 0.7.0 \
             && cargo run --quiet)                              # 缺陷基线

# np-tier3.sh 需要一个把 check_root() 改成只告警的构建：
#   cp -r ~/.projects/fsmon /tmp/fsmon-np && cd /tmp/fsmon-np
#   # 把 src/common/monitor/init.rs 的 check_root() 改为打印告警而非 bail
#   cargo build --release
FSMON=/tmp/fsmon-np/target/release/fsmon ./np-tier3.sh

# ── 需要 root 一次 ──
gcc -O2 -o /tmp/verify-fanotify-privsep verify-fanotify-privsep.c
sudo /tmp/verify-fanotify-privsep 1000        # 期望输出 CONFIRMED
```

## 两个探针里的"自检"是有意为之

`verify-fanotify-privsep.c` 会单独打印**非特权写者是否真的写出了文件**。
这是必要的：该探针的第一版把测试目录建成 `root:root 0755`，非特权写者 `fopen`
静默失败 → **一个事件都没产生** → 读者空等 40 秒后报出**假阴性 `REFUTED`**。
那个假阴性足以让人错误地否掉整个设计方案。

**任何结论都必须先通过自检。** 同理，`fsidmatrix.c` 报告 `EXDEV` 时，
接受方是内核而不是某个可能失败的副作用——这类探针的信噪比要靠设计保证，不能靠运气。
