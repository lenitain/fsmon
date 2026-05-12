use std::path::{Path, PathBuf};

use crate::utils::{SizeFilter, SizeOp};
use crate::{EventType, FileEvent};

// ---- PathOptions ----

#[derive(Clone)]
pub struct PathOptions {
    pub size_filter: Option<SizeFilter>,
    pub event_types: Option<Vec<EventType>>,
    pub recursive: bool,
    pub cmd: Option<String>,
}

/// Resolve a path for recursion check: expand tilde, then canonicalize if the path exists
/// (follows symlinks). Falls back to tilde-expanded path if can't canonicalize.
pub fn resolve_recursion_check(path: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expanded = crate::config::expand_tilde(path, &home);
    expanded.canonicalize().unwrap_or(expanded)
}

/// Find the PathOptions matching a given event path.
#[cfg(test)]
pub fn get_matching_path_options<'a>(
    paths: &[PathBuf],
    entries: &'a [(PathBuf, PathOptions)],
    canonical_paths: &[PathBuf],
    path: &Path,
) -> Option<&'a PathOptions> {
    for watched in paths {
        if let Some((_, opts)) = entries.iter().find(|(p, _)| p == watched) {
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
            && let Some((_, opts)) = entries.iter().find(|(p, _)| p == orig)
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

    true
}

/// Find the configured path that matches a given event path.
pub fn matching_path<'a>(
    paths: &'a [PathBuf],
    canonical_paths: &[PathBuf],
    path: &Path,
) -> Option<&'a PathBuf> {
    // Direct match first: find the configured PathBuf that matches this path
    for watched in paths {
        if watched == path {
            return Some(watched);
        }
    }
    // Recursive match: find watched path that is a prefix of event path
    for watched in paths {
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
#[cfg(test)]
pub fn is_path_in_scope(
    paths: &[PathBuf],
    entries: &[(PathBuf, PathOptions)],
    _canonical_paths: &[PathBuf],
    path: &Path,
) -> bool {
    for watched in paths {
        let recursive = entries
            .iter()
            .find(|(p, _)| p == watched)
            .map(|(_, o)| o.recursive)
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
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        }
    }

    #[test]
    fn test_should_output_no_opts() {
        assert!(should_output(None, &make_event("/tmp/x", EventType::Create, 1, 0)));
        assert!(should_output(None, &make_event("/tmp/y", EventType::Delete, 2, 999)));
    }

    #[test]
    fn test_should_output_event_type_filter() {
        let opts = PathOptions {
            size_filter: None,
            event_types: Some(vec![EventType::Create, EventType::Delete]),
            recursive: false,
            cmd: None,
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
            recursive: false,
            cmd: None,
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
            recursive: false,
            cmd: None,
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
            recursive: false,
            cmd: None,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 100)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 99)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/a", EventType::Create, 1, 101)));
    }

    #[test]
    fn test_should_output_combined_filters() {
        let opts = PathOptions {
            size_filter: Some(SizeFilter { op: SizeOp::Ge, bytes: 100 }),
            event_types: Some(vec![EventType::Create]),
            recursive: false,
            cmd: None,
        };
        assert!(should_output(Some(&opts), &make_event("/tmp/data", EventType::Create, 1, 200)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/data", EventType::Delete, 1, 200)));
        assert!(!should_output(Some(&opts), &make_event("/tmp/data", EventType::Create, 1, 50)));
    }

    // ---- matching_path ----

    fn make_entries() -> Vec<(PathBuf, PathOptions)> {
        vec![
            (
                PathBuf::from("/home/user/project"),
                PathOptions {
                    size_filter: None,
                    event_types: None,
                    recursive: true,
                    cmd: None,
                },
            ),
            (
                PathBuf::from("/var/log"),
                PathOptions {
                    size_filter: None,
                    event_types: None,
                    recursive: false,
                    cmd: None,
                },
            ),
        ]
    }

    #[test]
    fn test_matching_path_direct_match() {
        let paths = vec![PathBuf::from("/home/user/project"), PathBuf::from("/var/log")];
        let canonical = paths.clone();
        let result = matching_path(&paths, &canonical, Path::new("/home/user/project"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_recursive_prefix() {
        let paths = vec![PathBuf::from("/home/user/project"), PathBuf::from("/var/log")];
        let canonical = paths.clone();
        let result = matching_path(&paths, &canonical, Path::new("/home/user/project/src/main.rs"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_canonical_fallback() {
        let paths = vec![PathBuf::from("/home/user/project")];
        let canonical = vec![PathBuf::from("/real/project")];
        let result = matching_path(&paths, &canonical, Path::new("/real/project/src/main.rs"));
        assert_eq!(result, Some(&PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_matching_path_no_match() {
        let paths = vec![PathBuf::from("/home/user/project")];
        let canonical = paths.clone();
        let result = matching_path(&paths, &canonical, Path::new("/etc/passwd"));
        assert!(result.is_none());
    }

    #[test]
    fn test_matching_path_non_recursive_child_allowed() {
        let paths = vec![PathBuf::from("/var/log")];
        let canonical = paths.clone();
        let result = matching_path(&paths, &canonical, Path::new("/var/log/syslog"));
        assert_eq!(result, Some(&PathBuf::from("/var/log")));
    }

    // ---- is_path_in_scope ----

    #[test]
    fn test_is_path_in_scope_recursive() {
        let paths = vec![PathBuf::from("/tmp")];
        let entries = vec![
            (PathBuf::from("/tmp"), PathOptions {
                size_filter: None, event_types: None,
                recursive: true, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp")));
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp/sub")));
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp/sub/deep/file.txt")));
        assert!(!is_path_in_scope(&paths, &entries, &canonical, Path::new("/var/log")));
        assert!(!is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmpfile")));
    }

    #[test]
    fn test_is_path_in_scope_non_recursive() {
        let paths = vec![PathBuf::from("/tmp")];
        let entries = vec![
            (PathBuf::from("/tmp"), PathOptions {
                size_filter: None, event_types: None,
                recursive: false, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp")));
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp/file.txt")));
        assert!(!is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp/sub/file.txt")));
        assert!(!is_path_in_scope(&paths, &entries, &canonical, Path::new("/var/log")));
    }

    #[test]
    fn test_is_path_in_scope_multiple_paths() {
        let paths = vec![PathBuf::from("/tmp"), PathBuf::from("/var/log")];
        let entries = vec![
            (PathBuf::from("/tmp"), PathOptions {
                size_filter: None, event_types: None,
                recursive: true, cmd: None,
            }),
            (PathBuf::from("/var/log"), PathOptions {
                size_filter: None, event_types: None,
                recursive: true, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/tmp/file")));
        assert!(is_path_in_scope(&paths, &entries, &canonical, Path::new("/var/log/syslog")));
        assert!(!is_path_in_scope(&paths, &entries, &canonical, Path::new("/etc/passwd")));
    }

    // ---- get_matching_path_options ----

    #[test]
    fn test_get_matching_path_options_recursive() {
        let paths = vec![PathBuf::from("/home")];
        let entries = vec![
            (PathBuf::from("/home"), PathOptions {
                size_filter: None, event_types: None,
                recursive: true, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        let result = get_matching_path_options(&paths, &entries, &canonical, Path::new("/home/user/file.txt"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_non_recursive_child() {
        let paths = vec![PathBuf::from("/var/log")];
        let entries = vec![
            (PathBuf::from("/var/log"), PathOptions {
                size_filter: None, event_types: None,
                recursive: false, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        let result = get_matching_path_options(&paths, &entries, &canonical, Path::new("/var/log/messages"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_non_recursive_grandchild() {
        let paths = vec![PathBuf::from("/var/log")];
        let entries = vec![
            (PathBuf::from("/var/log"), PathOptions {
                size_filter: None, event_types: None,
                recursive: false, cmd: None,
            }),
        ];
        let canonical = paths.clone();
        let result = get_matching_path_options(&paths, &entries, &canonical, Path::new("/var/log/sub/messages"));
        assert!(result.is_none());
    }

    #[test]
    fn test_get_matching_path_options_canonical_fallback() {
        let paths = vec![PathBuf::from("/symlink_target")];
        let entries = vec![
            (PathBuf::from("/symlink_target"), PathOptions {
                size_filter: None, event_types: None,
                recursive: true, cmd: None,
            }),
        ];
        let canonical = vec![PathBuf::from("/real/path")];
        let result = get_matching_path_options(&paths, &entries, &canonical, Path::new("/real/path/sub"));
        assert!(result.is_some());
    }

    #[test]
    fn test_get_matching_path_options_no_match() {
        let paths = vec![PathBuf::from("/home")];
        let entries: Vec<(PathBuf, PathOptions)> = vec![];
        let canonical = vec![];
        let result = get_matching_path_options(&paths, &entries, &canonical, Path::new("/etc"));
        assert!(result.is_none());
    }

    // ---- resolve_recursion_check ----

    #[test]
    fn test_resolve_recursion_check_existing_path() {
        let result = resolve_recursion_check(Path::new("/tmp"));
        assert!(result.starts_with("/tmp"));
    }

    #[test]
    fn test_resolve_recursion_check_nonexistent_path() {
        let path = Path::new("/nonexistent_fsmon_test_dir_xyz123");
        let result = resolve_recursion_check(path);
        assert_eq!(result, path);
    }

    #[test]
    fn test_resolve_recursion_check_with_tilde() {
        let result = resolve_recursion_check(Path::new("~/some_random_dir_xyz789"));
        assert!(result.as_os_str().len() > 0);
    }
}
