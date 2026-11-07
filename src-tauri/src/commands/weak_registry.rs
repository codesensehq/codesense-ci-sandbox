// SPDX-License-Identifier: MIT
//! RF-perf-8: a self-pruning `Arc<V>` registry keyed by `String`.
//!
//! Replaces the insert-only `Mutex<HashMap<String, Arc<T>>>` anti-pattern that
//! two backend registries shared — [`crate::commands::git::SyncLockManager`] and
//! [`crate::commands::claude_session::SessionWatcherState`] — both of which grew
//! `O(distinct keys ever seen this process)` because nothing ever removed an
//! entry.
//!
//! Values are held as [`Weak`], so an entry evaporates once the last outstanding
//! `Arc<V>` clone is dropped — bounding growth to `O(live keys)` with **no
//! teardown wiring** and **no in-flight-prune risk**: a live caller holds an
//! `Arc` clone, which keeps its `Weak` upgradable, so it is never swept.
//!
//! Dead-weak sweeping happens **only on the create/miss path**
//! ([`get_or_create`](WeakArcRegistry::get_or_create) /
//! [`swap`](WeakArcRegistry::swap)) — the steady-state hit path is O(1) and never
//! scans, so hot callers never pay an O(n) walk on the UI/render thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// A self-pruning registry of `Arc<V>` keyed by `String`. See the module docs for
/// the bounding + concurrency invariants.
pub struct WeakArcRegistry<V> {
    map: Mutex<HashMap<String, Weak<V>>>,
}

impl<V> Default for WeakArcRegistry<V> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<V> WeakArcRegistry<V> {
    /// Return the live `Arc` for `key` if one is still outstanding; otherwise build
    /// it via `make`, store a `Weak`, and return the strong `Arc`. Concurrent
    /// callers for the same live key share one `Arc` (serialization preserved).
    ///
    /// The dead-weak sweep runs only on a miss (the sole growth path), never on the
    /// steady-state hit — so hot callers stay O(1).
    ///
    /// **Contract for lock-like values:** the returned `Arc`'s liveness *is* the
    /// eviction guard — an entry can only self-evict once its last `Arc` clone is
    /// dropped. A caller relying on this registry for mutual exclusion must hold the
    /// returned `Arc` for the whole critical section; dropping it early lets the
    /// entry evict and a racing caller mint a *different* value for the same key.
    pub fn get_or_create(&self, key: &str, make: impl FnOnce() -> Arc<V>) -> Arc<V> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(key).and_then(Weak::upgrade) {
            return existing; // hot path — O(1), no sweep
        }
        // Miss (new key OR the previous value's last Arc was dropped). This is the
        // ONLY growth path, so amortize the dead-weak sweep here — never on the hit
        // path.
        map.retain(|_, w| w.strong_count() > 0);
        let arc = make();
        map.insert(key.to_string(), Arc::downgrade(&arc));
        arc
    }

    /// Install a freshly-made `Arc` at `key`, returning `(new, previous_live)` so the
    /// caller can tear down the outgoing value. ONE lock acquisition — no
    /// check-then-act window (AC-4). Sweeps dead weaks on the same pass.
    ///
    /// Unlike [`get_or_create`](WeakArcRegistry::get_or_create), `swap` always
    /// installs a fresh value even when a live one exists — the caller wants to
    /// replace (and typically retire) the previous entry.
    pub fn swap(&self, key: &str, make: impl FnOnce() -> Arc<V>) -> (Arc<V>, Option<Arc<V>>) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let previous = map.get(key).and_then(Weak::upgrade);
        map.retain(|_, w| w.strong_count() > 0);
        let arc = make();
        map.insert(key.to_string(), Arc::downgrade(&arc));
        (arc, previous)
    }

    /// The live `Arc` for `key`, or `None` if the last clone was already dropped
    /// (nothing to signal — the owner has torn down). Never sweeps.
    pub fn get(&self, key: &str) -> Option<Arc<V>> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .and_then(Weak::upgrade)
    }

    /// Occupied-slot count for the RF-perf-5 memory gate. `try_lock`-or-skip
    /// (`None` on contention) so instrumentation never blocks a caller — mirrors
    /// [`crate::content_engine::sync::WaveCoordinator::wave_target_locks_len`].
    ///
    /// Counts every map slot, *including* dead-but-not-yet-swept weaks (each still
    /// costs a key + `Weak` until the next miss sweeps it) — i.e. the true resident
    /// footprint, not the live-entry count. Over a churn workload it returns toward
    /// the live set after each create/`swap` rather than climbing monotonically,
    /// which is exactly the bound this story proves.
    pub fn len(&self) -> Option<usize> {
        self.map.try_lock().ok().map(|m| m.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trivial value type — the registry is generic, so the value's shape is
    // irrelevant to the bounding logic under test.
    fn make_val(n: u32) -> impl FnOnce() -> Arc<u32> {
        move || Arc::new(n)
    }

    #[test]
    fn get_or_create_returns_same_arc_while_held() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        let a = reg.get_or_create("k", make_val(1));
        // Second call for the same key while `a` is still held returns the SAME Arc
        // (serialization identity preserved — AC-3). `make` returns 2, but it must
        // never run because the live entry short-circuits.
        let b = reg.get_or_create("k", make_val(2));
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(*b, 1);
        assert_eq!(reg.len(), Some(1));
    }

    #[test]
    fn entry_self_evicts_when_last_arc_dropped() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        // Create N entries, dropping each Arc immediately → all weaks go dead.
        for i in 0..10 {
            let _ = reg.get_or_create(&format!("k{i}"), make_val(i));
        }
        // Dead weaks linger until the next miss triggers the sweep (bounded, not
        // leaked). One more create sweeps them all: the map settles at the single
        // live entry — this is the O(ever-seen) → O(live) bound (AC-2).
        let live = reg.get_or_create("live", make_val(99));
        assert_eq!(reg.len(), Some(1));
        assert_eq!(*live, 99);
    }

    #[test]
    fn get_or_create_recreates_after_last_arc_dropped() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        {
            let a = reg.get_or_create("k", make_val(1));
            assert_eq!(*a, 1);
        } // `a` dropped → weak dead
          // A fresh get_or_create for the same key must build a NEW value, not hand
          // back the dead one.
        let b = reg.get_or_create("k", make_val(2));
        assert_eq!(*b, 2);
    }

    #[test]
    fn swap_returns_previous_and_installs_fresh() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        let (first, prev0) = reg.swap("k", make_val(1));
        assert!(prev0.is_none());
        // While `first` is held, swap installs a DISTINCT fresh Arc and returns the
        // previous live one (AC-4 replace semantics — used by the cancels map).
        let (second, prev1) = reg.swap("k", make_val(2));
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(*second, 2);
        let prev1 = prev1.expect("previous live entry returned");
        assert!(Arc::ptr_eq(&prev1, &first));
        // Only the newest is registered.
        assert_eq!(reg.len(), Some(1));
    }

    #[test]
    fn swap_settles_at_one_after_prior_dropped() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        let (a, _) = reg.swap("k", make_val(1));
        drop(a);
        let (_b, prev) = reg.swap("k", make_val(2));
        // The prior was already dropped, so nothing to retire, and the sweep keeps
        // the map at the single live entry.
        assert!(prev.is_none());
        assert_eq!(reg.len(), Some(1));
    }

    #[test]
    fn get_returns_none_after_owner_dropped() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        {
            let _a = reg.get_or_create("k", make_val(1));
            assert!(reg.get("k").is_some());
        }
        // Owner dropped → get is a safe no-op-yielding None (cancel-after-teardown).
        assert!(reg.get("k").is_none());
        assert!(reg.get("never-seen").is_none());
    }

    #[test]
    fn len_starts_zero_and_reads_are_side_effect_free() {
        let reg: WeakArcRegistry<u32> = WeakArcRegistry::default();
        assert_eq!(reg.len(), Some(0));
        // A second read is identical — len() never mutates the map.
        assert_eq!(reg.len(), Some(0));
    }
}
