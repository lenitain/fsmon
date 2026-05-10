use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::utils::{SizeFilter, SizeOp};
use crate::{EventType, FileEvent};

// ---- PathOptions ----

#[derive(Clone)]
pub struct PathOptions {
    pub size_filter: Option<SizeFilter>,
    pub event_types: Option<Vec<EventType>>,
    pub exclude_regex: Option<regex::Regex>,
    pub exclude_invert: bool,
    pub exclude_cmd_regex: Option<regex::Regex>,
    pub exclude_cmd_invert: bool,
    pub recursive: bool,
}

/// Resolve a path for recursion check: expand tilde, then canonicalize if the path exists
/// (follows symlinks). Falls back to tilde-expanded path if can't canonicalize.
pub fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = crate::config::expand_tilde(path, &home);
    expanded.canonicalize().unwrap_or(expanded)
}

/// Build a combined regex from a list of patterns.
pub fn build_exclude_regex(patterns: Option<&[String]>, _label: &str) -> Result<(Option<Regex>, bool)> {
    let Some(patterns) = patterns else { return Ok((None, false)); };
    if patterns.is_empty() { return Ok((None, false)); }
    let invert = patterns[0].starts_with('!');
    let parts: Vec<String> = patterns.iter().map(|p| {
        p.strip_prefix('!').unwrap_or(p).to_string()
    }).collect();
    let regex = Regex::new(&parts.join("|"))
        .with_context(|| format!("invalid {} pattern", _label))?;
    Ok((Some(regex), invert))
}

/// Find the PathOptions matching a given event path.
pub fn get_matching_path_options<'a>(
    paths: &[PathBuf],
    path_options: &'a HashMap<PathBuf, PathOptions>,
    canonical_paths: &[PathBuf],
    path: &Path,
) -> Option<&'a PathOptions> {
    for watched in paths {
        if let Some(opts) = path_options.get(watched) {
            if opts.recursive {
                if path.starts_with(watched) {
                    return Some(opts);
                }
            } else if path == watched.as_path() || path.parent() == Some(watched.as_path()) {
                return Some(opts);
            }
        }
    }
    // Fallback: match against canonical paths (handles symlinks/bind-mounts)
    for (i, canonical) in canonical_paths.iter().enumerate() {
        if let Some(orig) = paths.get(i)
            && let Some(opts) = path_options.get(orig)
        {
            if opts.recursive {
                if path.starts_with(canonical) {
                    return Some(opts);
                }
            } else if path == canonical.as_path() || path.parent() == Some(canonical.as_path())
            {
                return Some(opts);
            }
        }
    }
    None
}

/// Check whether an event should be output, given matching path options.
pub fn should_output(
    opts: Option<&PathOptions>,
    event: &FileEvent,
) -> bool {
    let opts = match opts {
        Some(o) => o,
        None => return true,
    };

    if let Some(ref types) = opts.event_types
        && !types.contains(&event.event_type)
    {
        return false;
    }

    if let Some(ref filter) = opts.size_filter {
        let passes = match filter.op {
            SizeOp::Gt => event.file_size > filter.bytes as u64,
            SizeOp::Ge => event.file_size >= filter.bytes as u64,
            SizeOp::Lt => event.file_size < filter.bytes as u64,
            SizeOp::Le => event.file_size <= filter.bytes as u64,
            SizeOp::Eq => event.file_size == filter.bytes as u64,
        };
        if !passes { return false; }
    }

    if let Some(ref regex) = opts.exclude_regex {
        let matched = regex.is_match(&event.path.to_string_lossy());
        if opts.exclude_invert {
            if !matched { return false; }
        } else if matched {
            return false;
        }
    }

    if let Some(ref regex) = opts.exclude_cmd_regex {
        let matched = regex.is_match(&event.cmd);
        if opts.exclude_cmd_invert {
            if !matched { return false; }
        } else if matched {
            return false;
        }
    }

    true
}

/// Find the configured path that matches a given event path.
pub fn matching_path<'a>(
    paths: &'a [PathBuf],
    path_options: &'a HashMap<PathBuf, PathOptions>,
    canonical_paths: &[PathBuf],
    path: &Path,
) -> Option<&'a PathBuf> {
    // Direct match first: find the configured PathBuf that matches this path
    for watched in paths {
        if watched == path && path_options.contains_key(watched) {
            return Some(watched);
        }
    }
    // Recursive match: find watched path that is a prefix of event path
    for watched in path_options.keys() {
        if path.starts_with(watched) {
            return Some(watched);
        }
    }
    // Fallback: match against canonical paths (handles symlinks/bind-mounts)
    for (i, canonical) in canonical_paths.iter().enumerate() {
        if (path == canonical.as_path() || path.starts_with(canonical))
            && let Some(orig) = paths.get(i)
        {
            return Some(orig);
        }
    }
    None
}

/// Check if path is within monitoring scope.
pub fn is_path_in_scope(
    paths: &[PathBuf],
    path_options: &HashMap<PathBuf, PathOptions>,
    _canonical_paths: &[PathBuf],
    path: &Path,
) -> bool {
    for (i, watched) in paths.iter().enumerate() {
        let recursive = paths
            .get(i)
            .and_then(|p| path_options.get(p))
            .map(|o| o.recursive)
            .unwrap_or(false);
        if recursive {
            if path.starts_with(watched) {
                return true;
            }
        } else if path == watched.as_path() || path.parent() == Some(watched.as_path()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventType, FileEvent};
    use chrono::Utc;
    use std::path::{Path, PathBuf};

    // ---- build_exclude_regex ----

    #[test]
    fn test_build_exclude_regex_none() {
        let (re, inv) = build_exclude_regex(None, "exclude").unwrap();
        assert!(re.is_none());
        assert!(!inv);
    }

    #[test]
    fn test_build_exclude_regex_empty() {
        let (re, inv) = build_exclude_regex(Some(&[]), "exclude").unwrap();
        assert!(re.is_none());
        assert!(!inv);
    }

    #[test]
    fn test_build_exclude_regex_single_pattern() {
        let patterns = vec![".*\\.tmp$".to_string()];
        let (re, inv) = build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("foo.tmp"));
        assert!(!re.as_ref().unwrap().is_match("foo.txt"));
    }

    #[test]
    fn test_build_exclude_regex_multiple_patterns() {
        let patterns = vec![".*\\.tmp$".to_string(), ".*\\.log$".to_string()];
        let (re, inv) = build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("foo.tmp"));
        assert!(re.as_ref().unwrap().is_match("bar.log"));
        assert!(!re.as_ref().unwrap().is_match("foo.txt"));
    }

    #[test]
    fn test_build_exclude_regex_invert() {
        let patterns = vec!["!.*\\.py$".to_string()];
        let (re, inv) = build_exclude_regex(Some(&patterns), "exclude").unwrap();
        assert!(re.is_some());
        assert!(inv);
        assert!(re.as_ref().unwrap().is_match("foo.py"));
        assert!(!re.as_ref().unwrap().is_match("foo.tmp"));
    }

    #[test]
    fn test_build_exclude_regex_cmd_wildcard() {
        let patterns = vec!["nginx.*".to_string()];
        let (re, inv) = build_exclude_regex(Some(&patterns), "--exclude-cmd").unwrap();
        assert!(re.is_some());
        assert!(!inv);
        assert!(re.as_ref().unwrap().is_match("nginx"));
        assert!(re.as_ref().unwrap().is_match("nginx-worker"));
        assert!(!re.as_ref().unwrap().is_match("apache"));
    }

    // ---- should_output ----

    fn make_event(path: &str, event_type: EventType, pid: u32, size: u64) -> FileEvent {
        FileEvent {
            time: Utc::now(),
            event_type,
            path: PathBuf::from(path),
            pid,
            cmd: "test".to_string(),
            user: "root".to_string(),
            file_size: size,
            monitored_path: PathBuf::from("/watched"),
        }
    }

    fn make_event_cmd(path: &str, event_type: EventType, pid: u32, size: u64, cmd: &str) -> FileEvent {
        FileEvent {
            time: Utc::now(),
            event_type,
            path: PathBuf::from(path),
            pid,
            cmd: cmd.to_string(),
            user: "root".to_string(),
            file_size: size,
            monitored_path: PathBuf::from("/watched"),
        }
    }

    #[test]
    fn test_should_output_no_opts() {
        // None means "no filter" -> should always output
        assert!(should_output(None, &make_event("/tmp/x", EventType::Create, 1, 0)));
        assert!(should_output(None, &make_event("/tmp/y", EventType::Delete, 2, 999)));
    }

    #[test]
    fn test_should_output_event_type_filter() {
        let opts = PathOptions {
            size_filter: None,
            event_types: Some(vec![EventType::Create, EventType::Delete]),
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 0)));
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Delete, 1, 0)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Modify, 1, 0)));
    }

    #[test]
    fn test_should_output_size_filter_ge() {
        let opts = PathOptions {
            size_filter: Some(SizeFilter { op: SizeOp::Ge, bytes: 1000 }),
            event_types: None,
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 2000)));
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 1000)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 500)));
    }

    #[test]
    fn test_should_output_size_filter_lt() {
        let opts = PathOptions {
            size_filter: Some(SizeFilter { op: SizeOp::Lt, bytes: 100 }),
            event_types: None,
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 50)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 100)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 200)));
    }

    #[test]
    fn test_should_output_size_filter_eq() {
        let opts = PathOptions {
            size_filter: Some(SizeFilter { op: SizeOp::Eq, bytes: 100 }),
            event_types: None,
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 100)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 99)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 101)));
    }

    #[test]
    fn test_should_output_exclude_regex() {
        let opts = PathOptions {
            size_filter: None,
            event_types: None,
            exclude_regex: Some(Regex::new(".*\\.tmp$").unwrap()),
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(!should_output(Some(&opts), &make_event("/tmp/a.tmp", EventType::Create, 1, 0)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/b.tmp", EventType::Delete, 1, 0)));
        assert!(should_output(Some(&opts), &make_event("/tmp/a.txt", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_regex_invert() {
        // Inverted: only matching paths pass through
        let opts = PathOptions {
            size_filter: None,
            event_types: None,
            exclude_regex: Some(Regex::new(".*\\.py$").unwrap()),
            exclude_invert: true,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/main.py", EventType::Create, 1, 0)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/main.rs", EventType::Create, 1, 0)));
    }

    #[test]
    fn test_should_output_exclude_cmd() {
        let opts = PathOptions {
            size_filter: None,
            event_types: None,
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: Some(Regex::new("rsync|apt").unwrap()),
            exclude_cmd_invert: false,
            recursive: false,
        };
        assert!(!should_output(Some(&opts), &make_event_cmd("/tmp/a", EventType::Create, 1, 0, "rsync")));
        assert!(!should_output(Some(&opts), &make_event_cmd("/tmp/a", EventType::Create, 2, 0, "apt")));
        assert!(should_output(Some(&opts), &make_event_cmd("/tmp/a", EventType::Create, 3, 0, "nginx")));
    }

    #[test]
    fn test_should_output_exclude_cmd_invert() {
        let opts = PathOptions {
            size_filter: None,
            event_types: None,
            exclude_regex: None,
            exclude_invert: false,
            exclude_cmd_regex: Some(Regex::new("nginx").unwrap()),
            exclude_cmd_invert: true,
            recursive: false,
        };
        assert!(should_output(Some(&opts), &make_event_cmd("/tmp/a", EventType::Create, 1, 0, "nginx")));
        assert!(!should_output(Some(&opts), &make_event_cmd("/tmp/a", EventType::Create, 2, 0, "rsync")));
    }

    #[test]
    fn test_should_output_combined_filters() {
        let opts = PathOptions {
            size_filter: Some(SizeFilter { op: SizeOp::Ge, bytes: 100 }),
            event_types: Some(vec![EventType::Create]),
            exclude_regex: Some(Regex::new(".*\\.log$").unwrap()),
            exclude_invert: false,
            exclude_cmd_regex: None,
            exclude_cmd_invert: false,
            recursive: false,
        };
        // Size >= 100, type=Create, not .log -> passes
        assert!(should_output(Some(&opts), &make_event("/tmp/data", EventType::Create, 1, 200)));
        // Wrong type
        assert!(!should_output(Some(&opts), &make_event("/tmp/data", EventType::Delete, 1, 200)));
        // Size too small
        assert!(!should_output(Some(&opts), &make_event("/tmp/data", EventType::Create, 1, 50)));
        // Excluded by regex
        assert!(!should_output(Some(&opts), &make_event("/tmp/app.log", EventType::Create, 1, 200)));
    }

    // ---- matching_path ----

    fn make_path_options() -> HashMap<PathBuf, PathOptions> {
        let mut map = HashMap::new();
        map.insert(
            PathBuf::from("/home/user/project"),
            PathOptions {
                size_filter: None,
                event_types: None,
                exclude_regex: None,
                exclude_invert: false,
                exclude_cmd_regex: None,
                exclude_cmd_invert: false,
                recursive: true,
            },
        );
        map.insert(
            PathBuf::from("/var/log"),
            PathOptions {
                size_filter: None,
                event_types: None,
                exclude_regex: None,
                exclude_invert: false,
                exclude_cmd_regex: None,
                exclude_cmd_invert: false,
                recursive: false,
            },
        );
        map
    }

    #[test]
    fn test_matching_path_direct_match() {
        let paths = vec![PathBuf::from("/home/user/project"), PathBuf::from("/var/log")];
        let opts = make_path_options();
        let canonical = paths.clone();

        let result = matching_path(&paths, &opts, &canonical, Path::new("/home/user/project"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_recursive_prefix() {
        let paths = vec![PathBuf::from("/home/user/project"), PathBuf::from("/var/log")];
        let opts = make_path_options();
        let canonical = paths.clone();

        let result = matching_path(&paths, &opts, &canonical, Path::new("/home/user/project/src/main.rs"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_canonical_fallback() {
        let paths = vec![PathBuf::from("/home/user/project")];
        let opts = make_path_options();
        // Canonical paths differ from configured paths
        let canonical = vec![PathBuf::from("/real/project")];

        // Path matches canonical but not configured
        let result = matching_path(&paths, &opts, &canonical, Path::new("/real/project/src/main.rs"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_no_match() {
        let paths = vec![PathBuf::from("/home/user/project")];
        let opts = make_path_options();
        let canonical = paths.clone();

        let result = matching_path(&paths, &opts, &canonical, Path::new("/etc/passwd"));
        assert!(result.is_none());
    }

    #[test]
    fn test_matching_path_non_recursive_child_allowed() {
        let paths = vec![PathBuf::from("/var/log")];
        let opts = make_path_options();
        let canonical = paths.clone();

        // Non-recursive path: direct child is allowed
        let result = matching_path(&paths, &opts, &canonical, Path::new("/var/log/syslog"));
        assert_eq!(result, Some(&PathBuf::from("/var/log")));
    }

    // ---- is_path_in_scope ----

    #[test]
    fn test_is_path_in_scope_recursive() {
        let paths = vec![PathBuf::from("/tmp")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/tmp"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: true,
        });
        let canonical = paths.clone();

        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp")));
        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp/sub")));
        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp/sub/deep/file.txt")));
        assert!(!is_path_in_scope(&paths, &opts, &canonical, Path::new("/var/log")));
        assert!(!is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmpfile")));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let paths = vec![PathBuf::from("/tmp")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/tmp"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: false,
        });
        let canonical = paths.clone();

        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp")));
        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp/file.txt")));
        assert!(!is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp/sub/file.txt")));
        assert!(!is_path_in_scope(&paths, &opts, &canonical, Path::new("/var/log")));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let paths = vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/tmp"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: true,
        });
        opts.insert(PathBuf::from("/var/log"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: true,
        });
        let canonical = paths.clone();

        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/tmp/file")));
        assert!(is_path_in_scope(&paths, &opts, &canonical, Path::new("/var/log/syslog")));
        assert!(!is_path_in_scope(&paths, &opts, &canonical, Path::new("/etc/passwd")));
    }

    // ---- get_matching_path_options ----

    #[test]
    fn test_get_matching_path_options_recursive() {
        let paths = vec![PathBuf::from("/home")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/home"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: true,
        });
        let canonical = paths.clone();

        let result = get_matching_path_options(&paths, &opts, &canonical, Path::new("/home/user/file.txt"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_non_recursive_child() {
        let paths = vec![PathBuf::from("/var/log")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/var/log"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: false,
        });
        let canonical = paths.clone();

        // Direct child of non-recursive path should match
        let result = get_matching_path_options(&paths, &opts, &canonical, Path::new("/var/log/messages"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_non_recursive_grandchild() {
        let paths = vec![PathBuf::from("/var/log")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/var/log"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: false,
        });
        let canonical = paths.clone();

        // Grandchild of non-recursive path should NOT match
        let result = get_matching_path_options(&paths, &opts, &canonical, Path::new("/var/log/sub/messages"));
        assert!(result.is_none());
    }

    #[test]
    fn test_get_matching_path_options_canonical_fallback() {
        let paths = vec![PathBuf::from("/symlink_target")];
        let mut opts = HashMap::new();
        opts.insert(PathBuf::from("/symlink_target"), PathOptions {
            size_filter: None, event_types: None,
            exclude_regex: None, exclude_invert: false,
            exclude_cmd_regex: None, exclude_cmd_invert: false,
            recursive: true,
        });
        let canonical = vec![PathBuf::from("/real/path")];

        let result = get_matching_path_options(&paths, &opts, &canonical, Path::new("/real/path/sub"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_no_match() {
        let paths = vec![PathBuf::from("/home")];
        let opts = HashMap::new();
        let canonical = vec![];

        let result = get_matching_path_options(&paths, &opts, &canonical, Path::new("/etc"));
        assert!(result.is_none());
    }

    // ---- resolve_recursion_check ----

    #[test]
    fn test_resolve_recursion_check_existing_path() {
        let result = resolve_recursion_check(Path::new("/tmp"));
        // /tmp should exist and canonicalize to itself on Linux
        assert!(result.starts_with("/tmp"));
    }

    #[test]
    fn test_resolve_recursion_check_nonexistent_path() {
        let path = Path::new("/nonexistent_fsmon_test_dir_xyz123");
        let result = resolve_recursion_check(path);
        // Should return the original path since it can't canonicalize
        assert_eq!(result, path);
    }

    #[test]
    fn test_resolve_recursion_check_with_tilde() {
        // Just verify it doesn't panic with tilde prefix
        let result = resolve_recursion_check(Path::new("~/some_random_dir_xyz789"));
        assert!(result.as_os_str().len() > 0);
    }
}
