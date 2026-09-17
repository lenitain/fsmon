use std::collections::HashMap;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use fanotify_fid::Fsid;

/// A directory handle together with the filesystem that issued it.
///
/// Handles are only meaningful **within** their filesystem: two filesystems can
/// hand out the same bytes for different directories, so a key without the fsid
/// would let one filesystem's handle answer for another's.
pub type HandleKey = (Fsid, Vec<u8>);

/// One filesystem's handle→path entries.
type FsMap = HashMap<Vec<u8>, (PathBuf, Instant)>;

/// Thread-safe directory handle cache with capacity + TTL eviction.
///
/// A plain `Mutex<HashMap>` with lazy expiry checks — replaces moka while
/// keeping the same semantics used by fsmon (bounded capacity, TTL since
/// insertion, no access-time refresh).
///
/// Two levels, keyed by fsid first, for the same reason
/// [`fanotify_fid::HandleCache`] is: a single map over `(Fsid, Vec<u8>)` cannot
/// be looked up with a borrowed handle without building the owned key first, so
/// every hit would cost an allocation and a copy. Splitting the levels keeps
/// `Vec<u8>: Borrow<[u8]>` doing the work, and a hit stays allocation-free.
#[derive(Clone)]
pub struct DirCache {
    inner: Arc<Mutex<HashMap<Fsid, FsMap>>>,
    capacity: u64,
    ttl: Duration,
    /// Number of lookups that missed. With no mount descriptors the resolver's
    /// `open_by_handle_at` fallback is a zero-syscall immediate failure, so this
    /// counter is the observable "path resolution degraded" signal.
    misses: Arc<AtomicU64>,
}

/// Adapter so the `DirCache` can plug into `fanotify_fid`'s explicit resolver
/// (`PathStore`, `PathMemo`).
///
/// It exists because the cache predates the resolver and has a policy the crate's
/// default store does not: a capacity and a TTL.  Both traits take `&self` — the
/// cache is already shared through an `Arc<Mutex<..>>`, so it has the interior
/// mutability a store that records what it learns is required to own — and the
/// read hands the path to the resolver's closure while the guard is held, so a
/// hit copies nothing.
pub struct DirCacheStore(pub DirCache);

impl fanotify_fid::PathStore for DirCacheStore {
    fn with_path<R>(&self, fsid: Fsid, handle: &[u8], f: impl FnOnce(&Path) -> R) -> Option<R> {
        self.0.with_path(fsid, handle, f)
    }
}

impl fanotify_fid::PathMemo for DirCacheStore {
    fn remember(&self, fsid: Fsid, handle: &[u8], path: &Path) {
        self.0.insert((fsid, handle.to_vec()), path.to_path_buf());
    }

    fn forget(&self, fsid: Fsid, handle: &[u8]) {
        self.0.forget(fsid, handle);
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

    pub fn get(&self, fsid: Fsid, handle: &[u8]) -> Option<PathBuf> {
        self.with_path(fsid, handle, Path::to_path_buf)
    }

    /// Hand the known path to `f` **borrowed**, while the entry's guard is held.
    ///
    /// The form the resolver reads through: a hit copies nothing, and the guard
    /// lives exactly as long as the closure.  It is the same lookup as
    /// [`get`](Self::get), which is this with `Path::to_path_buf` — so a caller
    /// that does not need to keep the path should never call that one.
    pub fn with_path<R>(&self, fsid: Fsid, handle: &[u8], f: impl FnOnce(&Path) -> R) -> Option<R> {
        let mut map = self.inner.lock().unwrap();
        // The key borrows for the lookup, which is what `Vec<u8>: Borrow<[u8]>`
        // buys at the inner level; only a *write* builds an owned one.
        let hit = match map.get_mut(&fsid).and_then(|fs| fs.get(handle)) {
            Some((path, at)) if at.elapsed() < self.ttl => Some(f(path)),
            // Expired: drop it so the miss is a miss in fact as well as in the
            // count below.
            Some(_) => {
                if let Some(fs) = map.get_mut(&fsid) {
                    fs.remove(handle);
                }
                None
            }
            None => None,
        };
        if hit.is_none() {
            drop(map);
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    /// Lookups that could not answer — an entry absent or past its TTL — which on
    /// the resolve path is one per event dropped because its path was unknown.
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn insert(&self, key: HandleKey, path: PathBuf) {
        let (fsid, handle) = key;
        let mut map = self.inner.lock().unwrap();
        // Eviction is over the whole cache, not per filesystem: the bound is
        // about fsmon's memory, and one busy filesystem filling it is exactly
        // the case the bound exists for.
        let total: usize = map.values().map(HashMap::len).sum();
        if total >= self.capacity as usize {
            let now = Instant::now();
            let ttl = self.ttl;
            map.retain(|_, fs| {
                fs.retain(|_, (_, at)| now.duration_since(*at) < ttl);
                !fs.is_empty()
            });
            // Still full: drop one entry from the first filesystem that has any.
            if map.values().map(HashMap::len).sum::<usize>() >= self.capacity as usize
                && let Some(fs) = map.values_mut().find(|fs| !fs.is_empty())
                && let Some(oldest) = fs.keys().next().cloned()
            {
                fs.remove(&oldest);
            }
        }
        map.entry(fsid)
            .or_default()
            .insert(handle, (path, Instant::now()));
    }

    /// Drop what is known about one handle, so the next lookup asks the
    /// filesystem again.
    pub fn forget(&self, fsid: Fsid, handle: &[u8]) {
        let mut map = self.inner.lock().unwrap();
        if let Some(fs) = map.get_mut(&fsid) {
            fs.remove(handle);
        }
    }

    pub fn entry_count(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(HashMap::len)
            .sum::<usize>() as u64
    }
}

/// Look up the file handle for a path, using [`fanotify_fid::handle::name_to_handle_at`].
///
/// Returns the handle bytes matching the file_handle format in fanotify FID
/// events, paired with the fsid of the filesystem that issued them.
fn path_to_handle_key(path: &Path) -> Option<HandleKey> {
    let fsid = fanotify_fid::handle::fsid_of_path(path).ok()?;
    let handle = fanotify_fid::handle::name_to_handle_at(path).ok()?;
    Some((fsid, handle))
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
    let Ok(fsid) = fanotify_fid::handle::fsid_of_fd(fd) else {
        return;
    };
    if let Ok(handle) = fanotify_fid::handle::handle_from_fd(fd) {
        cache.insert((fsid, handle), path.to_path_buf());
    }
}

#[cfg(test)]
#[path = "dir_cache/tests.rs"]
mod tests;
