use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use fanotify_fid::types::HandleKey;

/// Thread-safe directory handle cache with capacity + TTL eviction.
///
/// A plain `Mutex<HashMap>` with lazy expiry checks — replaces moka while
/// keeping the same semantics used by fsmon (bounded capacity, TTL since
/// insertion, no access-time refresh).
#[derive(Clone)]
pub struct DirCache {
    inner: Arc<Mutex<HashMap<HandleKey, (PathBuf, Instant)>>>,
    capacity: u64,
    ttl: Duration,
}

/// Adapter so the `DirCache` can plug into
/// `fanotify_fid`'s convergence resolver (`PathStore`).
pub struct DirCacheStore(pub DirCache);

impl fanotify_fid::types::PathStore for DirCacheStore {
    fn get(&self, key: &[u8]) -> Option<PathBuf> {
        self.0.get(key)
    }

    fn insert(&mut self, key: Vec<u8>, path: PathBuf) {
        self.0.insert(key, path);
    }
}

impl DirCache {
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            capacity: capacity.max(1),
            ttl,
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<PathBuf> {
        let mut map = self.inner.lock().unwrap();
        match map.get(key) {
            Some((path, at)) if at.elapsed() < self.ttl => Some(path.clone()),
            _ => {
                map.remove(key);
                None
            }
        }
    }

    pub fn insert(&self, key: Vec<u8>, path: PathBuf) {
        let mut map = self.inner.lock().unwrap();
        if map.len() >= self.capacity as usize {
            let now = Instant::now();
            map.retain(|_, (_, at)| now.duration_since(*at) < self.ttl);
        }
        if map.len() >= self.capacity as usize
            && let Some(oldest_key) = map.keys().next().cloned()
        {
            map.remove(&oldest_key);
        }
        map.insert(key, (path, Instant::now()));
    }

    pub fn entry_count(&self) -> u64 {
        self.inner.lock().unwrap().len() as u64
    }
}

/// Look up the file handle for a path, using [`fanotify_fid::handle::name_to_handle_at`].
///
/// Returns the handle key bytes matching the file_handle format in fanotify FID events.
fn path_to_handle_key(path: &Path) -> Option<HandleKey> {
    fanotify_fid::handle::name_to_handle_at(path).ok()
}

/// Add directory path handle key to cache
pub fn cache_dir_handle(cache: &DirCache, path: &Path) {
    if let Some(key) = path_to_handle_key(path) {
        cache.insert(key, path.to_path_buf());
    }
}

/// Recursively cache directory and all subdirectory handles
pub fn cache_recursive(cache: &DirCache, dir: &Path) {
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
