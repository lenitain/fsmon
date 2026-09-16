use std::collections::HashMap;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Number of lookups that missed. With `mount_fds = &[]` (plan §6 阶段 4)
    /// every miss means the fanotify-fid tier-3 `open_by_handle_at` fallback
    /// is a zero-syscall immediate failure, so this counter is the observable
    /// "path resolution degraded" signal.
    misses: Arc<AtomicU64>,
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
            misses: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<PathBuf> {
        let mut map = self.inner.lock().unwrap();
        match map.get(key) {
            Some((path, at)) if at.elapsed() < self.ttl => Some(path.clone()),
            _ => {
                map.remove(key);
                drop(map);
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Number of cache misses observed so far (tier-3 fallback attempts).
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
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

/// Cache `fd`'s handle as pointing at `path`, without touching the filesystem
/// by name.
///
/// Prefer this over [`cache_dir_handle`] whenever a descriptor is already open:
/// it cannot be raced by a concurrent rename, needs no path walk, and needs no
/// privileges.  A recursive marking walk holds exactly such a descriptor for
/// every directory it marks, which is what makes the cache complete instead of
/// best-effort.
pub fn cache_handle_from_fd(cache: &DirCache, fd: &impl AsFd, path: &Path) {
    if let Ok(key) = fanotify_fid::handle::handle_from_fd(fd) {
        cache.insert(key, path.to_path_buf());
    }
}
