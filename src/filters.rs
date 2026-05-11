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
