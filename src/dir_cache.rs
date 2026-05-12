use moka::sync::Cache;
use std::fs;
use std::path::{Path, PathBuf};

pub use fanotify_fid::types::HandleKey;

/// Look up the file handle for a path, using [`fanotify_fid::handle::name_to_handle_at`].
///
/// Returns the handle key bytes matching the file_handle format in fanotify FID events.
pub fn path_to_handle_key(path: &Path) -> Option<HandleKey> {
    fanotify_fid::handle::name_to_handle_at(path).ok()
}

/// Add directory path handle key to cache
pub fn cache_dir_handle(cache: &Cache<HandleKey, PathBuf>, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// Recursively cache directory and all subdirectory handles
pub fn cache_recursive(cache: &Cache<HandleKey, PathBuf>, dir: &Path) {
    cache_dir_handle(cache, dir);
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            cache_recursive(cache, &path);
        }
    }
}
