//! Unified security checks for path validation.
//!
//! Provides `check_path_allowed()` to reject paths that could cause
//! self-referencing loops or security issues.

use std::path::Path;

/// Default blocked paths (hardcoded, cannot be removed via config).
const DEFAULT_BLOCKED: &[(&str, &str)] = &[
    ("/proc/self", "fsmon process (self-monitoring)"),
];

/// Check if a path is allowed for monitoring.
///
/// Rejects paths that match the default blacklist or user-configured blacklist.
/// Returns Ok(()) if allowed, Err with reason if blocked.
pub fn check_path_allowed(path: &Path, user_blocked: &[String]) -> Result<(), String> {
    let path_str = path.to_string_lossy();

    // Default blacklist
    for (blocked, reason) in DEFAULT_BLOCKED {
        if path_str.starts_with(blocked) {
            return Err(format!(
                "Path '{}' is blocked: {}",
                path.display(),
                reason
            ));
        }
    }

    // User-configured blacklist
    for blocked in user_blocked {
        if !blocked.is_empty() && path_str.starts_with(blocked.as_str()) {
            return Err(format!(
                "Path '{}' is blocked by user configuration",
                path.display()
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_allowed_path() {
        assert!(check_path_allowed(Path::new("/home/user/data"), &[]).is_ok());
    }

    #[test]
    fn test_blocked_proc_self() {
        assert!(check_path_allowed(Path::new("/proc/self/fd"), &[]).is_err());
    }

    #[test]
    fn test_user_blocked() {
        let blocked = vec!["/etc/secret".to_string()];
        assert!(check_path_allowed(Path::new("/etc/secret/key"), &blocked).is_err());
        assert!(check_path_allowed(Path::new("/home/user"), &blocked).is_ok());
    }

    #[test]
    fn test_empty_user_blocked_ignored() {
        let blocked = vec!["".to_string()];
        assert!(check_path_allowed(Path::new("/home/user"), &blocked).is_ok());
    }
}
