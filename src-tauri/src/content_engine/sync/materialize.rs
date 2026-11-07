// SPDX-License-Identifier: MIT
//! Sync engine — what remains of the per-destination materialization
//! primitives: the hash-guarded removal teardown uses
//! ([`verify_and_remove_dest`]). SW-XS-8 deleted the projection engine and
//! its write path (`materialize_pair` and the dest-vs-prev classifier).
//!
//! ## Skip-locally-modified semantics
//!
//! When the destination's current hash diverges from
//! `MaterializedEntry::last_materialized_hash`, the user has edited the
//! materialized file in the code repo. The projection materialize honors this
//! edit — it skips (`SkippedLocallyModified`) and leaves the entry untouched.
//!
//! ## Suppression
//!
//! The function opens a [`crate::content_engine::watcher::Suppression`] guard over `dest`
//! at entry and holds it across both the write-branch and the
//! delete-branch (see AC-8). The guard composes monotonically with
//! [`crate::content_engine::atomic_io::write_tmp_fsync_rename_suppressed`]'s own internal
//! suppression — both push `valid_until` forward, never backward.

use std::fs;
use std::io;
use std::path::Path;

use log::warn;

use super::super::atomic_io::hash_file;
use super::super::lockfile::MaterializedEntry;
use super::super::Error;

/// SS-FU-11 AC-1: per-stage timing accumulator (named buckets rather than the
/// wave's `other` remainder).
///
/// Nanoseconds (`u128`) to match `atomic_io::file_mtime_ns`'s unit and
/// avoid float drift when summing ~1800 sub-millisecond intervals. The
/// `write` bucket spans the full atomic write (tmp create + `write_all` +
/// `fsync` + chmod + rename); fsync is not split out here — the AC-9
/// before/after re-measure isolates its cost (see story Task 3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StageTimings {
    /// Time in the per-name layer probe.
    pub resolve_ns: u128,
    /// Time reading source bytes (`read_source_bytes`).
    pub read_ns: u128,
    /// Time hashing source bytes (`blake3::hash`).
    pub hash_ns: u128,
    /// Time in the atomic write (`clone_tmp_rename_suppressed_no_fsync` —
    /// per-file `fsync` is dropped from this hot path and replaced by a
    /// single end-of-wave parallel barrier accounted under
    /// `fsync_barrier_ns` at the `run_repo_reconcile` level).
    pub write_ns: u128,
    /// SR-0 (Q2): time in `classify_dest_against_prev` — the per-file dest
    /// `stat` + baseline comparison that runs before the read/hash on the
    /// skip path. Previously part of the `run_repo_reconcile` `other` remainder; now
    /// a named bucket so the 26 s `other` at N=40 is attributable. Additive
    /// instrumentation only (one `Instant` delta per classified name).
    pub classify_stat_ns: u128,
}

impl StageTimings {
    /// Fold another call's timings into this accumulator. Saturating add
    /// so an absurd outlier interval can never wrap the sum.
    pub fn accumulate(&mut self, other: &StageTimings) {
        self.resolve_ns = self.resolve_ns.saturating_add(other.resolve_ns);
        self.read_ns = self.read_ns.saturating_add(other.read_ns);
        self.hash_ns = self.hash_ns.saturating_add(other.hash_ns);
        self.write_ns = self.write_ns.saturating_add(other.write_ns);
        self.classify_stat_ns = self.classify_stat_ns.saturating_add(other.classify_stat_ns);
    }
}

/// Disposition of a single hash-guarded dest removal — the return of
/// [`verify_and_remove_dest`].
///
/// Two outcomes only: the file was removed (or was already gone — a
/// `NotFound` is folded into `Removed` since the post-condition is the
/// same), or it diverged from codesense's last write and was left
/// untouched. Callers that need richer logging key off `dest`/`prev`,
/// which they already hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoveDisposition {
    /// `dest` was unlinked (or was already absent). Safe to drop the
    /// lockfile entry.
    Removed,
    /// `dest` diverged from `prev`'s baseline (user edit, non-regular
    /// entry, raced write, or an inaccessible stat/hash) and was left in
    /// place. The lockfile entry is still dropped by the caller; the file
    /// becomes a codesense-orphan that `route_file_to_context` can re-adopt.
    SkippedLocallyModified,
}

/// Hash-guarded removal of one materialized `dest` against its recorded
/// `prev` baseline. The single source of truth for "is this file still
/// exactly what codesense last wrote, and may we delete it?" — shared by
/// the resolver-driven source-delete path ([`handle_source_delete`]) and
/// the whole-repo teardown path
/// ([`crate::content_engine::sync::teardown::teardown_repo`]).
///
/// Removes `dest` and returns [`RemoveDisposition::Removed`] when the file
/// is byte-identical to `prev.last_materialized_hash` (via the mtime fast
/// gate, falling back to a hash compare on mtime drift). Returns
/// [`RemoveDisposition::SkippedLocallyModified`] — leaving the file in
/// place — when it has been locally modified, replaced with a non-regular
/// entry, or otherwise diverged. Only a hard, non-`NotFound` I/O error
/// from a stat / hash / unlink propagates as `Err`.
///
/// **Trust boundary note** (iter-1 D6): codesense does not protect against
/// attacker-controlled dest-side symlinks (an intermediate path component
/// of `dest` being a symlink pointing outside the code repo). Consistent
/// with `.git/info/exclude`'s trust model — the code repo is the user's
/// working tree, owned and inspected by them.
pub(crate) fn verify_and_remove_dest(
    dest: &Path,
    prev: &MaterializedEntry,
) -> Result<RemoveDisposition, Error> {
    // Iter-2 BH2-MED-2 / BH2-HIGH-1 + Iter-3 BH3-HIGH-2: single
    // `symlink_metadata` snapshot drives every subsequent branch.
    // Replaces iter-1's triple-stat pattern (`symlink_metadata` +
    // `dest.exists()` + mtime_gate's internal `metadata`) which had
    // three TOCTOU windows AND mis-classified broken symlinks
    // (`Path::exists()` follows symlinks → false → fell into the
    // remove path → user's broken symlink silently destroyed).
    //
    // `symlink_metadata` does NOT follow symlinks, so non-regular files
    // (symlinks, dirs, FIFOs, broken symlinks) all classify as
    // `!file_type().is_file()`. mtime is extracted from the same
    // snapshot via `meta_mtime_ns` — no second stat.
    //
    // **Iter-3 BH3-HIGH-2:** if `meta_mtime_ns` returns `Err(InvalidData)`
    // for a pre-`UNIX_EPOCH` mtime (archive extraction without
    // timestamps, FAT32 restore, clock-skew rollback), fall through to
    // `hash_file` rather than aborting the whole call. Symmetric with the
    // write path's `u128::MAX` sentinel.
    let dest_locally_modified = match fs::symlink_metadata(dest) {
        // Non-regular entry at dest: user-placed (symlink, dir, FIFO,
        // broken-symlink-to-anywhere). Preserve.
        Ok(meta) if !meta.file_type().is_file() => true,
        // Regular file: mtime compare → hash if drifted (or if mtime
        // unreadable for an edge-case reason).
        Ok(meta) => match meta_mtime_ns(&meta) {
            Ok(dest_mtime) if dest_mtime == prev.last_materialized_mtime_ns => false,
            // Mtime drifted → force hash compare.
            Ok(_) => hash_file(dest)? != prev.last_materialized_hash,
            // Iter-4 EH4-LOW-1: surface the mtime-read failure in logs.
            // Iter-3 added `warn!` to the D1 refresh path's Err arms for
            // exactly this diagnostic symmetry; this site was missed.
            // The `InvalidData` arm (pre-UNIX_EPOCH from archive restore,
            // FAT32, clock-skew) AND any networked-FS Err here fall
            // through to hash_file — matches write path's "force
            // re-hash" disposition.
            Err(e) => {
                warn!(
                    target: "content_engine::sync",
                    "verify_and_remove_dest: meta_mtime_ns failed on {}: {} — falling through to hash compare",
                    dest.display(),
                    e
                );
                hash_file(dest)? != prev.last_materialized_hash
            }
        },
        // dest absent → fall through to remove (which tolerates NotFound).
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(Error::Io(e)),
    };

    if dest_locally_modified {
        // Leave the file in place; the caller drops the lockfile entry.
        return Ok(RemoveDisposition::SkippedLocallyModified);
    }

    // Iter-2 EH2-MED-3 + Iter-3 BH3-HIGH-1 / EH3-HIGH-1 / EH3-HIGH-2
    // + Iter-4 EH4-MED-1: re-stat just before `remove_file` to catch
    // a user write that landed in the gap between our hash compare
    // and here. Reduces the race window from ~function execution
    // time to microseconds.
    //
    // **Iter-3:** re-stat via `fs::symlink_metadata` (NOT `mtime_gate`
    // which follows symlinks) so file-type protection is symmetric
    // with the outer check — a race-placed broken symlink that
    // followed `metadata → NotFound` would otherwise slip through.
    //
    // **Iter-4 EH4-MED-1 (symmetric drift handling):** mirror the
    // outer's "mtime drift → hash compare" tolerance. Iter-3
    // accepted ONLY mtime equality as safe-to-remove, which meant
    // a user `touch` of a codesense-clean file followed by upstream
    // source-delete left the file orphaned in the user's tree even
    // though codesense itself wrote those exact bytes. The
    // hash-compare-on-drift here makes the re-stat consistent with
    // the outer classification: if mtime drifted but hash still
    // matches what we last wrote, the file is unchanged in bytes
    // and safe to remove. One extra hash read per touched-clean
    // file — symmetric with the outer's tolerance, cheap, removes
    // the orphan-on-touch user-trust surprise.
    //
    // Truly atomic delete-if-unchanged would need an open FD or
    // `unlinkat`; symmetric stat + hash is the pragmatic best-effort.
    let still_safe_to_remove = match fs::symlink_metadata(dest) {
        // Non-regular replacement (symlink/dir/FIFO planted between
        // the outer check and here) → preserve.
        Ok(meta) if !meta.file_type().is_file() => false,
        // Regular file: prefer mtime fast path, fall back to hash
        // compare on drift / unreadable mtime — symmetric with the
        // outer classification above.
        Ok(meta) => match meta_mtime_ns(&meta) {
            Ok(m) if m == prev.last_materialized_mtime_ns => true,
            // Mtime drifted OR un-readable. Hash to disambiguate
            // "user wrote in gap" (different bytes) from "user
            // touched without changing bytes" (same hash).
            Ok(_) | Err(_) => match hash_file(dest) {
                Ok(h) => h == prev.last_materialized_hash,
                // File raced out between symlink_metadata and
                // hash_file → `fs::remove_file` will tolerate
                // NotFound; safe to attempt.
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => true,
                // Any other hash error → preserve.
                Err(_) => false,
            },
        },
        // dest already gone — `fs::remove_file` will tolerate NotFound.
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        // Any other Err (permission denied, EIO) → preserve.
        Err(_) => false,
    };
    if !still_safe_to_remove {
        // User wrote / replaced / made dest inaccessible in the gap.
        return Ok(RemoveDisposition::SkippedLocallyModified);
    }

    // Safe to remove. Tolerate `NotFound` (race with concurrent
    // user-deletion or already-gone-from-prior-cleanup).
    if let Err(e) = fs::remove_file(dest) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(Error::Io(e));
        }
    }
    Ok(RemoveDisposition::Removed)
}

/// Extract nanoseconds-since-`UNIX_EPOCH` mtime from a `Metadata`
/// snapshot. Mirrors [`crate::content_engine::atomic_io::file_mtime_ns`] but works
/// against an already-fetched metadata struct to avoid a second `stat`
/// syscall (the consolidation iter-2 BH2-MED-2 calls for in
/// `handle_source_delete`).
///
/// Propagates the same `InvalidData` error as `file_mtime_ns` for mtimes
/// before `UNIX_EPOCH` (legitimate edge: archive extraction without
/// mtime metadata, clock-skew rollback) so the caller can choose to
/// treat as "force re-hash" the same way mtime_gate would.
fn meta_mtime_ns(meta: &fs::Metadata) -> Result<u128, Error> {
    let modified = meta.modified().map_err(Error::Io)?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("mtime before UNIX_EPOCH: {}", e),
            ))
        })?;
    Ok(duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SLW-RS-2 AC-8: size fast-gate closes the same-mtime window =====

    // -- fixture -------------------------------------------------------

    // ====== SS-FU-11 AC-1: per-stage timing instrumentation ==========

    /// AC-1: `StageTimings::accumulate` sums each bucket independently.
    #[test]
    fn stage_timings_accumulate_sums_each_bucket() {
        let mut acc = StageTimings::default();
        acc.accumulate(&StageTimings {
            resolve_ns: 1,
            read_ns: 2,
            hash_ns: 3,
            write_ns: 4,
            classify_stat_ns: 5,
        });
        acc.accumulate(&StageTimings {
            resolve_ns: 10,
            read_ns: 20,
            hash_ns: 30,
            write_ns: 40,
            classify_stat_ns: 50,
        });
        assert_eq!(acc.resolve_ns, 11);
        assert_eq!(acc.read_ns, 22);
        assert_eq!(acc.hash_ns, 33);
        assert_eq!(acc.write_ns, 44);
        assert_eq!(acc.classify_stat_ns, 55);
    }

    // ====== Task 4 unit tests ========================================

    // ====== Task 5: walkthrough matrix (AC-14) =======================

    // ====== Task 6: Suppression integration ==========================
}
