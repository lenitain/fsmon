# fsmon Benchmark

性能与正确性测试套件。

## 目录结构

```
benchmark/
├── common.sh               # 共享配置与工具函数（从 fsmon.toml 读取路径）
├── events_run.sh           # 事件正确性测试入口
├── post_run.sh             # 后期处理性能测试入口
├── perf/
│   ├── stress.sh           # 事件压力 perf 采集（默认 5000）
│   ├── query.sh            # 查询性能 perf 采集（默认 5000）
│   ├── clean.sh            # 清理性能 perf 采集（默认 5000）
│   └── manual.sh           # 快速 perf 采集（手动用）
└── tests/
    ├── events/             # 事件捕获正确性测试
    │   ├── create.sh       # CREATE 事件 (100 文件)
    │   ├── modify.sh       # MODIFY 事件 (50 文件)
    │   ├── delete.sh       # DELETE 事件 (50 文件)
    │   ├── move.sh         # MOVE 事件 (30 文件)
    │   ├── recursive.sh    # 递归监控 (2 层子目录)
    │   └── stress.sh       # 压力测试 (5000 文件顺序修改)
    └── post/               # 后期处理性能测试
        ├── query.sh        # 查询性能 (100/1000/5000 事件 + jq 管道)
        └── clean.sh        # 清理性能 (按数量/时间/dry-run)
```

所有脚本通过 `source common.sh` 共享配置，路径从 `~/.config/fsmon/fsmon.toml` 读取，无硬编码。

## 前置条件

```bash
cargo build --release
fsmon init
```

## 测试流程

### 1. 功能验证

```bash
bash events_run.sh      # 6 个事件测试套件
bash post_run.sh        # 2 个后期处理套件
```

### 2. 性能采集

```bash
bash perf/stress.sh [count]     # 事件压力（默认 5000）
bash perf/query.sh [count]      # 查询性能（默认 5000）
bash perf/clean.sh [count]      # 清理性能（默认 5000）
```

查看报告：

```bash
sudo perf report -i /tmp/perf_stress.data
sudo perf report -i /tmp/perf_query.data
sudo perf report -i /tmp/perf_clean.data
```

快速手动采集（需 fsmon 已运行）：

```bash
bash perf/manual.sh
```

### 推荐顺序

```bash
bash events_run.sh && bash perf/stress.sh && bash perf/query.sh && bash perf/clean.sh
```

## 测试详情

### 事件测试 (`tests/events/`)

| 测试 | 内容 | 通过条件 |
|------|------|----------|
| create.sh | 创建 100 个文件 | 捕获 100 条 CREATE |
| modify.sh | 修改 50 个文件 | 捕获 50 条 MODIFY |
| delete.sh | 删除 50 个文件 | 捕获 50 条 DELETE |
| move.sh | 移动 30 个文件 | 捕获 30 条 MovedFrom + MovedTo |
| recursive.sh | 2 层子目录创建文件 | 3 个文件全部捕获 |
| stress.sh | 顺序修改 5000 个文件 | 捕获 5000 条 MODIFY |

### 后期处理测试 (`tests/post/`)

| 测试 | 内容 | 通过条件 |
|------|------|----------|
| query.sh | 查询 100/1000/5000 事件 + jq 管道 | 各查询 < 阈值 |
| clean.sh | 按数量/时间/dry-run 清理 | 各清理 < 阈值 |

## 设计原则

- **Daemon 生命周期**：每个脚本自行管理 fsmon 启停，不依赖外部状态
- **Perf 绑定**：`perf/*` 脚本内联测试逻辑，perf 全程绑定 fsmon PID 不变
- **路径配置**：`LOG_FILE` 从 `fsmon.toml` 的 `[logging].path` 读取，`BENCH_DIR` 统一为 `/tmp/fsmon_benchmark`
- **事件计数**：验证精确数量（`==`），丢事件即为 bug
