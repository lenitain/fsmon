//! `DirCache` semantics: what the key means, and what eviction is allowed to
//! throw away.
//!
//! The key is the interesting part.  Before fanotify-fid 0.8 the cache was
//! keyed on a bare handle byte string, which is only correct while one
//! filesystem is in play: a handle is issued **by a filesystem**, so two of
//! them can hand out the same bytes for different directories.  These tests
//! pin the isolation, because getting it wrong does not fail loudly — it
//! resolves a rename to the wrong path.

use super::*;
use std::time::Duration;

/// Two filesystems, neither of them the one `/tmp` is on, so a stray real
/// filesystem id cannot make a test pass by accident.
const FS_A: Fsid = (0x0000_5eed, 0x0000_0001);
const FS_B: Fsid = (0x0000_5eed, 0x0000_0002);

/// The same handle bytes, as two different filesystems would report them.
const SHARED_HANDLE: &[u8] = &[0x01, 0x02, 0x03, 0x04];

fn cache() -> DirCache {
    DirCache::new(100, Duration::from_secs(3600))
}

/// The reason the key carries an fsid at all: identical bytes on two
/// filesystems must not answer for each other.
#[test]
fn identical_handles_on_different_filesystems_stay_apart() {
    let c = cache();
    c.insert((FS_A, SHARED_HANDLE.to_vec()), PathBuf::from("/a/dir"));
    c.insert((FS_B, SHARED_HANDLE.to_vec()), PathBuf::from("/b/dir"));

    assert_eq!(
        c.get(FS_A, SHARED_HANDLE).as_deref(),
        Some(Path::new("/a/dir")),
        "filesystem A must get its own path back"
    );
    assert_eq!(
        c.get(FS_B, SHARED_HANDLE).as_deref(),
        Some(Path::new("/b/dir")),
        "filesystem B must get its own path back, not A's"
    );
    assert_eq!(
        c.entry_count(),
        2,
        "both entries are distinct: one key per filesystem"
    );
    assert_eq!(c.misses(), 0, "both lookups hit");
}

/// An unknown fsid is a miss even when the handle bytes are known elsewhere —
/// guessing here is exactly how a path from the wrong filesystem gets handed
/// out.
#[test]
fn an_unknown_filesystem_is_a_miss_not_a_guess() {
    let c = cache();
    c.insert((FS_A, SHARED_HANDLE.to_vec()), PathBuf::from("/a/dir"));

    assert_eq!(c.get(FS_B, SHARED_HANDLE), None);
    assert_eq!(
        c.misses(),
        1,
        "the miss must be countable: it is the degraded-resolution signal"
    );
}

/// `forget` is scoped to one filesystem, so invalidating a handle on A leaves
/// B's entry for the same bytes alone.
#[test]
fn forget_removes_only_the_named_filesystem() {
    let c = cache();
    c.insert((FS_A, SHARED_HANDLE.to_vec()), PathBuf::from("/a/dir"));
    c.insert((FS_B, SHARED_HANDLE.to_vec()), PathBuf::from("/b/dir"));

    c.forget(FS_A, SHARED_HANDLE);

    assert_eq!(c.get(FS_A, SHARED_HANDLE), None);
    assert_eq!(
        c.get(FS_B, SHARED_HANDLE).as_deref(),
        Some(Path::new("/b/dir")),
        "the other filesystem's entry is not collateral damage"
    );
}

/// An expired entry is dropped, counted as a miss, and cannot come back.
#[test]
fn an_expired_entry_is_a_miss() {
    let c = DirCache::new(100, Duration::from_millis(0));
    c.insert((FS_A, SHARED_HANDLE.to_vec()), PathBuf::from("/a/dir"));

    assert_eq!(c.get(FS_A, SHARED_HANDLE), None);
    assert_eq!(c.misses(), 1);
    assert_eq!(c.entry_count(), 0, "the dead entry is removed, not kept");
}

/// Capacity is a property of the cache, not of one filesystem: a filesystem
/// filling it must not be able to grow it past the bound.
#[test]
fn capacity_bounds_the_whole_cache_across_filesystems() {
    let c = DirCache::new(4, Duration::from_secs(3600));
    for i in 0..8u8 {
        let fsid = if i % 2 == 0 { FS_A } else { FS_B };
        c.insert((fsid, vec![i]), PathBuf::from(format!("/dir/{i}")));
    }

    assert!(
        c.entry_count() <= 4,
        "the bound holds across filesystems, got {}",
        c.entry_count()
    );
}

/// The adapter the resolver uses must read and write the same entries as the
/// cache's own API — otherwise a path learned through `PathStore` would be
/// invisible to the rename lookup that needs it.
#[test]
fn the_path_store_adapter_shares_the_cache() {
    let c = cache();
    let store = DirCacheStore(c.clone());
    fanotify_fid::PathMemo::remember(&store, FS_A, SHARED_HANDLE, Path::new("/a/from-store"));

    assert_eq!(
        c.get(FS_A, SHARED_HANDLE).as_deref(),
        Some(Path::new("/a/from-store")),
        "an entry inserted through the trait is visible on the cache"
    );
    assert_eq!(
        fanotify_fid::PathStore::with_path(&store, FS_A, SHARED_HANDLE, |p| p.to_path_buf())
            .as_deref(),
        Some(Path::new("/a/from-store")),
        "and readable back through the trait"
    );
    assert_eq!(
        fanotify_fid::PathStore::with_path(&store, FS_B, SHARED_HANDLE, |p| p.to_path_buf()),
        None,
        "still isolated by filesystem through the adapter"
    );
}
