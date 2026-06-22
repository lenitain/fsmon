# 安全分析总结：文件路径解析与符号链接

## 分析概述
- **焦点区域**: 文件路径解析与符号链接
- **目标文件**: 
  - `src/common/filters.rs`
  - `src/common/monitor/live_path.rs`
  - `src/common/fid_parser.rs`
- **分析日期**: 2026-06-22
- **分析师**: MiMo安全扫描专家

## 发现统计
| 严重性 | 数量 | 占比 |
|--------|------|------|
| HIGH   | 2    | 40%  |
| MEDIUM | 3    | 60%  |
| LOW    | 0    | 0%   |
| **总计** | **5** | 100% |

## 核心风险链

三个漏洞形成完整的攻击链：

```
攻击者创建 symlink
        ↓
F-004: 无路径作用域验证
        ↓
F-001: resolve_recursion_check canonicalize 跟随符号链接 (filters.rs:25)
        ↓
F-002: mark_recursive_inner 进入符号链接目录 (fid_parser.rs:395)
        ↓
结果: fanotify 监控敏感系统目录
```

## 关键发现

### 1. [HIGH] resolve_recursion_check 跟随符号链接 (F-001)
**位置**: `src/common/filters.rs:25`
**问题**: `canonicalize()` 会解析所有符号链接，无敏感路径防护
**影响**: 可将监控重定向到 `/etc`、`/root`、`/proc` 等敏感目录

### 2. [HIGH] mark_recursive_inner 跟随目录符号链接 (F-002)
**位置**: `src/common/fid_parser.rs:395`
**问题**: `path.is_dir()` 跟随符号链接，无 `is_symlink()` 检查
**影响**: 递归标记会进入符号链接目标目录，扩大监控范围

### 3. [MEDIUM] pending 路径符号链接注入 (F-003)
**位置**: `src/common/monitor/dir_watcher.rs:265`
**问题**: `check_pending` 重新处理路径时，攻击者可注入符号链接
**影响**: 等待期间创建的符号链接会被跟随

### 4. [MEDIUM] 无路径作用域验证 (F-004)
**位置**: `src/common/monitor/live_path.rs:112`
**问题**: canonical 路径无范围限制
**影响**: 结合符号链接可监控任意目录

### 5. [MEDIUM] 递归无深度限制 (F-005)
**位置**: `src/common/fid_parser.rs:390`
**问题**: `mark_recursive_inner` 无递归深度参数
**影响**: 符号链接可导致监控范围爆炸式增长

## 最高风险漏洞

**F-002 (置信度 0.92)** 是最直接可利用的缺陷：

```rust
// fid_parser.rs:395 - 缺少 is_symlink() 检查
if path.is_dir() {  // 跟随符号链接！
    // 递归进入...
}
```

任何有权限在被监控目录中创建符号链接的用户，都能将监控范围扩展到系统任意目录树。

## 优先修复建议

### 高优先级
1. **F-002**: 在 `mark_recursive_inner` 中增加 `is_symlink()` 检查
2. **F-001**: 在 `resolve_recursion_check` 返回后增加敏感路径黑名单

### 中优先级
3. **F-003**: 在 `check_pending` 中检查符号链接
4. **F-004**: 增加路径白名单/黑名单机制
5. **F-005**: 增加递归深度限制参数

## 代码修复示例

### F-002 修复
```rust
fn mark_recursive_inner(fan_fd: &OwnedFd, safe_mask: u64, dir: &Path) -> Vec<PathBuf> {
    // ...
    for entry in entries.flatten() {
        let path = entry.path();
        // 新增：跳过符号链接
        if entry.file_type().map_or(false, |ft| ft.is_symlink()) {
            continue;
        }
        if path.is_dir() {
            // ... 原有逻辑
        }
    }
    discovered
}
```

### F-001 修复
```rust
const SENSITIVE_PATHS: &[&str] = &["/etc", "/root", "/proc", "/sys", "/dev", "/boot"];

pub fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = crate::common::config::guess_home();
    let expanded = crate::common::config::expand_tilde(path, &home);
    let resolved = expanded.canonicalize().unwrap_or(expanded);
    
    // 新增：检查敏感路径
    for sensitive in SENSITIVE_PATHS {
        if resolved.starts_with(sensitive) {
            eprintln!("[WARNING] Refusing to monitor sensitive path: {}", resolved.display());
            return expanded; // 返回未解析路径
        }
    }
    resolved
}
```

## 文件清单
- `SECURITY-SYMLINK-ANALYSIS-FINAL.json`: 详细漏洞报告（JSON格式）
- `SECURITY-SYMLINK-ANALYSIS.md`: 详细漏洞报告（Markdown格式）
- `SECURITY-SYMLINK-SUMMARY.md`: 本总结文件
