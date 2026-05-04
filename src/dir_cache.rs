use dashmap::DashMap;
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::fid_parser::HandleKey;

/// Get handle key for a path via name_to_handle_at
/// Returns bytes matching the file_handle format in fanotify FID events
pub fn path_to_handle_key(path: &Path) -> Option<HandleKey> {
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut mount_id: libc::c_int = 0;
    let mut buf = vec![0u8; 128];

    let capacity = (buf.len() - 8) as u32;
    buf[0..4].copy_from_slice(&capacity.to_ne_bytes());

    let ret = unsafe {
        libc::name_to_handle_at(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            buf.as_mut_ptr() as *mut libc::file_handle,
            &mut mount_id,
            0,
        )
    };

    if ret != 0 {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
    Some(HandleKey::from_slice(&buf[0..8 + handle_bytes]))
}

/// Add directory path handle key to cache
pub fn cache_dir_handle(cache: &DashMap<HandleKey, PathBuf>, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// Recursively cache directory and all subdirectory handles
pub fn cache_recursive(cache: &DashMap<HandleKey, PathBuf>, dir: &Path) {
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
