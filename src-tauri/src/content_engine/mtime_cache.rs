// SPDX-License-Identifier: MIT
//! Generic path + file-stamp-keyed cache primitive.
//!
//! The "cache a per-file derived value, invalidate it when the file changes"
//! pattern recurs across the codebase — `install::frontmatter_cache` (path →
//! parsed frontmatter) and `commands::pipeline_format` (path → parsed pipeline
//! fields) each hand-roll it. This module factors the mechanism out once so a
//! third payload (here: a source file's blake3 content hash) reuses it instead of
//! copying it a third time. Folding the two existing hand-rolled caches onto this
//! primitive — and adding versioned disk persistence, which no current payload
//! needs — is a tracked follow-up, not done here.
//!
//! **Bounded by construction.** The map key is the **path**; the file stamp lives
//! *inside* the entry — so re-inserting a changed file overwrites its single slot
//! (editing a file N times leaves one entry, never N). Deleted/renamed files are
//! swept by [`MtimeKeyedCache::retain_live`] against the live set.
//!
//! **Stamp = mtime AND size.** A hit requires both to match the file's current
//! stamp, so an edit that changes size is caught even when a coarse-resolution
//! mtime clock didn't tick — belt-and-braces over mtime alone. A same-mtime AND
//! same-size content swap is not distinguished; this is the same mtime-invalidation
//! tradeoff `frontmatter_cache` already ships. Because the cached hash may come
//! from an earlier wave (even a different worktree), the divergence window is wider
//! than a single read→clone gap — but it stays *self-healing*: the dest is always a
//! live clone of the current bytes (output is byte-identical), only the recorded
//! baseline can lag, and the next full forward wave re-stamps it. For `content/`
//! sources this needs an external mtime-preserving mutation to hit at all (engine
//! writes always advance mtime). `file_mtime_and_size` follows symlinks, so a
//! symlink relinked to a same-stamp target shares that window.
//!
//! **No lock across I/O.** Callers hold the shared lock only for the map get/
//! insert; the file read/hash happens off-lock (see the `story_linker` call site),
//! so a shared cache never stalls the async runtime while a worker reads a file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// Identity stamp for a source file: mtime (ns since UNIX_EPOCH) **and** byte
/// size, both from a single `metadata` read
/// ([`super::atomic_io::file_mtime_and_size`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub mtime_ns: u128,
    pub size: u64,
}

impl FileStamp {
    pub fn new(mtime_ns: u128, size: u64) -> Self {
        Self { mtime_ns, size }
    }
}

/// One cached value plus the file stamp that keyed it. Private — callers use the
/// map API, never the entry directly.
#[derive(Debug, Clone)]
struct Entry<V> {
    stamp: FileStamp,
    value: V,
}

/// A path-keyed, stamp-invalidated cache of `V`.
#[derive(Debug, Clone, Default)]
pub struct MtimeKeyedCache<V> {
    /// path → (stamp, value). Keyed by path (NOT `(path, stamp)`), so a changed
    /// file replaces its own slot instead of accreting a new one.
    entries: HashMap<PathBuf, Entry<V>>,
}

impl<V: Clone> MtimeKeyedCache<V> {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Number of cached files (== distinct live paths after pruning).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The cached value for `path`, but only if its stored stamp matches `stamp`
    /// (the file's current mtime+size). A stale entry (either field moved) reads
    /// as a miss so the caller recomputes.
    pub fn get(&self, path: &Path, stamp: FileStamp) -> Option<&V> {
        self.entries
            .get(path)
            .filter(|e| e.stamp == stamp)
            .map(|e| &e.value)
    }

    /// Insert/overwrite the value for `path`. Because the key is the path, this
    /// replaces any prior entry for the same file (keeping the cache bounded).
    pub fn insert(&mut self, path: PathBuf, stamp: FileStamp, value: V) {
        self.entries.insert(path, Entry { stamp, value });
    }

    /// Drop every entry whose path fails `is_live` — the deleted/renamed sweep
    /// that keeps the cache bounded to the current file set. The predicate lets the
    /// caller test membership against a borrowed set (no per-path `PathBuf` clone
    /// on the hot path). Run on a full walk only — a narrowed/targeted set would
    /// wrongly evict siblings.
    pub fn retain_live(&mut self, is_live: impl Fn(&Path) -> bool) {
        self.entries.retain(|p, _| is_live(p));
    }

    /// Drop a single path's entry. Returns the removed value if present.
    pub fn remove(&mut self, path: &Path) -> Option<V> {
        self.entries.remove(path).map(|e| e.value)
    }
}

/// A cache shared across concurrent workers. `Mutex` (not `RwLock`) because every
/// access reads-then-maybe-writes; the critical sections are tiny map ops — never
/// file I/O (the caller reads off-lock).
pub type SharedMtimeCache<V> = Arc<Mutex<MtimeKeyedCache<V>>>;

/// Wrap a cache for sharing.
pub fn shared<V>(cache: MtimeKeyedCache<V>) -> SharedMtimeCache<V> {
    Arc::new(Mutex::new(cache))
}

/// Lock a shared cache, recovering from poisoning. A cache is best-effort: a
/// prior panic while the lock was held must degrade to a usable (at worst cold)
/// cache, NOT cascade-panic every subsequent worker. The poisoned inner value is
/// still a valid map — worst case a stale entry that its own stamp check
/// invalidates.
pub fn lock_recover<V>(cache: &SharedMtimeCache<V>) -> MutexGuard<'_, MtimeKeyedCache<V>> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

// ===================== source-hash cache (first payload) =====================
//
// The projection materialize loop needs each source file's blake3 hash as the
// lockfile baseline, but the clonefile write block-shares `source_path` and never
// reads the bytes — so, uncached, every worktree create re-reads + re-hashes the
// same ~7k identical sources. `source_path` is the shared context-repo path (the
// same physical source every worktree projects from), NOT a per-worktree copy —
// which is what makes a cross-worktree cache keyed by that path sound.

/// A source file's blake3 content hash.
pub type SharedSourceHashCache = SharedMtimeCache<[u8; 32]>;

/// Process-wide registry of source-hash caches, keyed by `(org_root,
/// projection_key)`. Session-scoped and in-memory: the win is cross-worktree
/// reuse *within* a session (opening N worktrees hashes each source once, not N
/// times); the stamp key self-invalidates on edit, and a full walk prunes deleted
/// paths via [`MtimeKeyedCache::retain_live`]. Scoped per projection so a prune
/// touches exactly one projection's live set. Bounded by the number of distinct
/// `(org, projection)` pairs touched in a session (a handful), not by file count.
fn source_hash_registry() -> &'static Mutex<HashMap<(PathBuf, String), SharedSourceHashCache>> {
    static REG: OnceLock<Mutex<HashMap<(PathBuf, String), SharedSourceHashCache>>> =
        OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The shared source-hash cache for one projection under one org root, created on
/// first use.
pub fn source_hash_cache_for(org_root: &Path, projection_key: &str) -> SharedSourceHashCache {
    source_hash_registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry((org_root.to_path_buf(), projection_key.to_string()))
        .or_insert_with(|| shared(MtimeKeyedCache::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn stamp(mtime_ns: u128, size: u64) -> FileStamp {
        FileStamp::new(mtime_ns, size)
    }

    #[test]
    fn miss_then_hit_serves_cached_value() {
        let mut c: MtimeKeyedCache<u64> = MtimeKeyedCache::new();
        assert_eq!(c.get(&p("/x"), stamp(100, 5)), None);
        c.insert(p("/x"), stamp(100, 5), 42);
        assert_eq!(c.get(&p("/x"), stamp(100, 5)), Some(&42));
    }

    #[test]
    fn mtime_change_is_a_miss() {
        let mut c: MtimeKeyedCache<u64> = MtimeKeyedCache::new();
        c.insert(p("/x"), stamp(100, 5), 42);
        assert_eq!(c.get(&p("/x"), stamp(200, 5)), None);
    }

    #[test]
    fn size_change_is_a_miss_even_at_same_mtime() {
        // The size half of the stamp catches a same-mtime edit that changed size —
        // the coarse-clock hazard the size-gate exists for.
        let mut c: MtimeKeyedCache<u64> = MtimeKeyedCache::new();
        c.insert(p("/x"), stamp(100, 5), 42);
        assert_eq!(c.get(&p("/x"), stamp(100, 6)), None);
    }

    #[test]
    fn edits_do_not_grow_cache_one_entry_per_path() {
        let mut c: MtimeKeyedCache<u64> = MtimeKeyedCache::new();
        for i in 0..1000u128 {
            c.insert(p("/x"), stamp(i, i as u64), i as u64);
        }
        assert_eq!(c.len(), 1, "path-keyed overwrite: one entry after N edits");
        assert_eq!(c.get(&p("/x"), stamp(999, 999)), Some(&999));
        assert_eq!(
            c.get(&p("/x"), stamp(0, 0)),
            None,
            "old stamp no longer valid"
        );
    }

    #[test]
    fn retain_live_prunes_deleted_paths() {
        let mut c: MtimeKeyedCache<u64> = MtimeKeyedCache::new();
        c.insert(p("/a"), stamp(1, 1), 1);
        c.insert(p("/b"), stamp(1, 1), 2);
        c.insert(p("/c"), stamp(1, 1), 3);
        let live: HashSet<PathBuf> = [p("/a"), p("/c")].into_iter().collect();
        c.retain_live(|path| live.contains(path));
        assert_eq!(c.len(), 2);
        assert!(c.get(&p("/a"), stamp(1, 1)).is_some());
        assert!(
            c.get(&p("/b"), stamp(1, 1)).is_none(),
            "deleted path pruned"
        );
        assert!(c.get(&p("/c"), stamp(1, 1)).is_some());
    }

    #[test]
    fn lock_recover_degrades_through_a_poisoned_lock() {
        let cache = shared(MtimeKeyedCache::<u64>::new());
        lock_recover(&cache).insert(p("/x"), stamp(1, 1), 7);
        // Poison the mutex from a panicking thread while it holds the guard.
        let c2 = cache.clone();
        let _ = std::thread::spawn(move || {
            let _g = c2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(cache.lock().is_err(), "mutex is poisoned");
        // lock_recover still yields the map (prior value intact) instead of panicking.
        assert_eq!(lock_recover(&cache).get(&p("/x"), stamp(1, 1)), Some(&7));
    }

    #[test]
    fn source_hash_cache_for_returns_same_handle_per_key() {
        let root = p("/org");
        let a = source_hash_cache_for(&root, "story");
        let b = source_hash_cache_for(&root, "story");
        lock_recover(&a).insert(p("/s"), stamp(1, 1), [9u8; 32]);
        assert_eq!(
            lock_recover(&b).get(&p("/s"), stamp(1, 1)),
            Some(&[9u8; 32]),
            "same (org, projection) key shares one cache"
        );
        let other = source_hash_cache_for(&root, "docs");
        assert!(
            lock_recover(&other).get(&p("/s"), stamp(1, 1)).is_none(),
            "a different projection is a separate cache"
        );
    }
}
