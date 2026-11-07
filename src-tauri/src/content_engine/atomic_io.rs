// SPDX-License-Identifier: MIT
//! Atomic file I/O primitives used by the sync engine.
//!
//! The sync engine writes are expected to be observed at most once in
//! their final state — never half-written. `write_tmp_fsync_rename`
//! delivers that guarantee on POSIX by writing to a per-call unique
//! `<dest>.tmp.<pid>.<counter>` path, flushing the file's contents to
//! disk, then atomically renaming over `dest`. The unique suffix makes
//! concurrent writers to the same `dest` race-safe at the tmp layer —
//! the only contention is the final rename, which the kernel serializes.
//!
//! [`hash_file`] streams the content through blake3 (chosen for
//! throughput; see story D-4). [`file_mtime_ns`] and [`mtime_gate`] are
//! cheap drift-detection helpers for the lockfile / hash-skip path.
//! Both **follow symlinks** (target mtime is returned, not the link's
//! own mtime) — callers that need link-side drift detection should use
//! `std::fs::symlink_metadata` directly.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};
use std::time::UNIX_EPOCH;

use log::debug;
use rand::Rng;

use super::dispatcher::SuppressionHandle;
use super::watcher::Suppression;
use super::Error;

// A bounded stripe set makes the compare/write boundary common to all core
// byte writers, clones, removals and renames without an unbounded lock registry.
fn mutation_stripe(path: &Path) -> usize {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    // Filename aliases (case and Unicode normalization) must share a lock even
    // after atomic replacement changes the file inode. Directory identity is
    // stable across those replacements and resolves all existing parent aliases.
    #[cfg(unix)]
    if let Ok(metadata) = parent.metadata() {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino()).hash(&mut hash);
        return hash.finish() as usize % 64;
    }
    // Missing parents and platforms without a directory identity conservatively
    // share one stripe; never infer filesystem case sensitivity from a path.
    0_u8.hash(&mut hash);
    hash.finish() as usize % 64
}

fn mutation_lock(stripe: usize) -> MutexGuard<'static, ()> {
    static LOCKS: OnceLock<[Mutex<()>; 64]> = OnceLock::new();
    LOCKS.get_or_init(|| std::array::from_fn(|_| Mutex::new(())))[stripe]
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn tree_mutation_gate() -> &'static RwLock<()> {
    static GATE: RwLock<()> = RwLock::new(());
    &GATE
}

struct MutationGate {
    _read: Option<std::sync::RwLockReadGuard<'static, ()>>,
    _write: Option<std::sync::RwLockWriteGuard<'static, ()>>,
}

fn path_mutation_gate(path: &Path) -> MutationGate {
    let read = tree_mutation_gate()
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    if path.parent().unwrap_or(Path::new(".")).is_dir() {
        MutationGate {
            _read: Some(read),
            _write: None,
        }
    } else {
        // Creating a parent changes the directory identity used for stripe
        // ownership. Exclude other writers until that transition is complete.
        drop(read);
        MutationGate {
            _read: None,
            _write: Some(
                tree_mutation_gate()
                    .write()
                    .unwrap_or_else(|poison| poison.into_inner()),
            ),
        }
    }
}

/// Recursive source deletion serializes with all child byte writers. Ordinary
/// per-file mutations retain parallelism; only structural tree changes exclude them.
pub fn remove_tree(path: &Path) -> Result<(), Error> {
    let _tree = tree_mutation_gate()
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.map_err(Error::Io),
    }
}

/// Compare and replace under the same serialization as ordinary content writers.
/// `None` requires absence. External filesystem editors remain optimistic;
/// this is deliberately not advertised as a filesystem transaction.
pub fn write_tmp_fsync_rename_expected(
    dest: &Path,
    content: &[u8],
    expected: Option<[u8; 32]>,
) -> Result<bool, Error> {
    write_tmp_fsync_rename_expected_mode(dest, content, expected, None)
}

pub fn write_tmp_fsync_rename_expected_mode(
    dest: &Path,
    content: &[u8],
    expected: Option<[u8; 32]>,
    mode: Option<u32>,
) -> Result<bool, Error> {
    write_tmp_fsync_rename_expected_authorized(dest, content, expected, mode, &|| Ok(()))
}

/// Recheck captured authority after waiting for writer locks, immediately before
/// any directory, temporary-file or destination mutation.
pub fn write_tmp_fsync_rename_expected_authorized(
    dest: &Path,
    content: &[u8],
    expected: Option<[u8; 32]>,
    mode: Option<u32>,
    validate: &dyn Fn() -> Result<(), Error>,
) -> Result<bool, Error> {
    let _tree = path_mutation_gate(dest);
    let _lock = mutation_lock(mutation_stripe(dest));
    validate()?;
    let metadata = fs::symlink_metadata(dest);
    let mut permissions = metadata.as_ref().ok().map(|meta| meta.permissions());
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        permissions = Some(fs::Permissions::from_mode(mode));
    }
    let matches = match metadata {
        Ok(meta) if meta.is_file() && meta.len() <= 4 * 1024 * 1024 => {
            expected.is_some_and(|expected| hash_file(dest).is_ok_and(|actual| actual == expected))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => expected.is_none(),
        Err(error) => return Err(Error::Io(error)),
        _ => false,
    };
    if !matches {
        return Ok(false);
    }
    validate()?;
    write_to_tmp_then_rename_with_permissions(
        dest,
        &pick_tmp_path(dest),
        content,
        true,
        permissions,
        validate,
    )?;
    fsync_path(dest.parent().unwrap_or(Path::new(".")))?;
    Ok(true)
}

/// SW-XS-8: remove `dest` only while it still holds `expected`, under the
/// same serialization as the compare-and-replace writers (so the hash check
/// and the delete are one step for every in-process writer). `Ok(false)`:
/// it holds something else (or is not a regular file); `Ok(true)` when it
/// was removed or was already gone.
pub fn remove_expected(dest: &Path, expected: [u8; 32]) -> Result<bool, Error> {
    let _tree = path_mutation_gate(dest);
    let _lock = mutation_lock(mutation_stripe(dest));
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.is_file() => {
            if hash_file(dest)? != expected {
                return Ok(false);
            }
        }
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(Error::Io(error)),
    }
    match fs::remove_file(dest) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Atomically write `content` to `dest` via a unique
/// `<dest>.tmp.<pid>.<counter>` file → fsync → rename.
///
/// Creates `dest`'s parent dir if absent (`fs::create_dir_all`). On any
/// failure after the temp file is created, makes a best-effort attempt
/// to remove it; cleanup failures are logged at `debug!` but do not
/// shadow the original error. `dest` is left untouched if the rename
/// fails.
///
/// # Symlinks
///
/// If `dest` is an existing symlink, the final `rename` replaces the
/// symlink entry with the new file — the link's target is not modified.
/// Callers that intend to overwrite a symlinked-to file (rather than
/// the link itself) must resolve the link first.
///
/// # Concurrency and watchers
///
/// Each call uses a unique tmp path, so two concurrent writers to the
/// same `dest` do not clobber each other's in-flight content.
/// **However, a watcher attached to `dest`'s parent directory will
/// observe the tmp-file create + remove events.** Callers that hold a
/// [`crate::content_engine::Suppression`] only for `dest` will still
/// dispatch handler events for the tmp file — use
/// [`write_tmp_fsync_rename_suppressed`] when the destination is inside
/// a watched directory.
pub fn write_tmp_fsync_rename(dest: &Path, content: &[u8]) -> Result<(), Error> {
    let tmp = pick_tmp_path(dest);
    write_to_tmp_then_rename(dest, &tmp, content, /* fsync */ true)
}

/// [`write_tmp_fsync_rename`] that keeps an existing regular file's permission bits:
/// the mode is set on the temporary file before the rename, so the destination never
/// appears (to `git status`, a watcher, or after a crash) with the default mode.
pub fn write_tmp_fsync_rename_keep_mode(dest: &Path, content: &[u8]) -> Result<(), Error> {
    let _tree = path_mutation_gate(dest);
    let _lock = mutation_lock(mutation_stripe(dest));
    let permissions = fs::symlink_metadata(dest)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.permissions());
    write_to_tmp_then_rename_with_permissions(
        dest,
        &pick_tmp_path(dest),
        content,
        true,
        permissions,
        &|| Ok(()),
    )
}

/// Atomic tmp+rename write **without** the per-file `fsync`.
///
/// Same atomicity contract as [`write_tmp_fsync_rename`] — `dest` is either
/// the new bytes or untouched, never partial. The difference is purely
/// durability: the bytes hit the kernel page cache, not necessarily the
/// disk platter, by the time this returns. Callers using this MUST pair
/// it with an out-of-band durability barrier (e.g. a batched `fsync` pass
/// over the touched paths) before any *dependent durable write* (such as
/// a lockfile that points at this content).
///
/// Intended for high-throughput batch writers where the per-file `fsync`
/// dominates wall time (~4 ms each on APFS at the time of writing) and a
/// single end-of-batch barrier suffices for the ordering invariant the
/// caller relies on. The wave materialization loop is the canonical user.
pub fn write_tmp_rename_no_fsync(dest: &Path, content: &[u8]) -> Result<(), Error> {
    let tmp = pick_tmp_path(dest);
    write_to_tmp_then_rename(dest, &tmp, content, /* fsync */ false)
}

/// Open `path` and call `sync_all` to durably persist its content.
///
/// Companion to [`write_tmp_rename_no_fsync`]: a batch writer issues many
/// no-fsync writes, then walks the touched paths through this helper
/// (typically in parallel across worker threads) before its dependent
/// durable write. Failure here is best-effort to the *caller* — propagate
/// the error and let the caller decide whether to warn-and-continue (the
/// wave's preferred posture) or abort.
///
/// On Apple targets `File::sync_all` uses `fcntl(F_FULLFSYNC)`, including
/// here. It can wait for device-cache durability. The app-state hot path
/// explicitly uses plain `libc::fsync` instead. Directory durability and
/// file-content durability are different guarantees.
pub fn fsync_path(path: &Path) -> Result<(), Error> {
    let file = fs::File::open(path).map_err(Error::Io)?;
    file.sync_all().map_err(Error::Io)
}

/// Default fsync-barrier fan-out. Sized to **disk** concurrency, not core
/// count — the barrier's threads block on the disk, not the CPU. SS-FU-19
/// AC-3's `CS_FSYNC_PARALLELISM` sweep (20×1800 cold, macOS/APFS) found the
/// disk saturates around 32 concurrent fsyncs: 8→32 cut cold wall ~33%
/// (48.5s→32.3s) while 64/128 *regressed* (contention). 32 is the measured
/// sweet spot; `CS_FSYNC_PARALLELISM` overrides it as a per-disk escape hatch.
pub const FSYNC_BARRIER_FANOUT: usize = 32;

/// Result of a batched [`fsync_barrier`].
///
/// `unit_failed` is set **only** when the barrier failed as a *whole* — a panic
/// escaped the fan-out (e.g. a worker thread aborted). It is the caller's
/// fail-closed trigger (skip the dependent lockfile write).
///
/// `failed_paths` counts individual [`fsync_path`] errors (EIO/ENOSPC/ENOENT).
/// These are **best-effort by design** — they do NOT flip `unit_failed` and do
/// NOT gate the lockfile, matching the resolver wave's long-standing posture
/// (SS-FU-19). Durability of the no-fsync path does not rest on catching these:
/// the materialized tree is **derived, re-clonable state**, so a rarely-unflushed
/// file is recovered by re-materialization, not by this barrier. `failed_paths`
/// exists so those errors are **observable** (logged + returned) rather than
/// silently swallowed — a persistently non-zero count signals a failing disk.
#[derive(Debug, Clone, Copy)]
pub struct BarrierOutcome {
    /// Whole-barrier failure (a panic escaped the fan-out) — the fail-closed trigger.
    pub unit_failed: bool,
    /// Count of per-path `fsync` failures (best-effort; does not gate the lockfile).
    pub failed_paths: usize,
    pub elapsed_ns: u128,
}

/// Parallel fsync barrier: fan every path in `dests` across worker threads and
/// [`fsync_path`] each one before the caller's dependent durable write. This is
/// the shared mechanism behind SS-FU-19's resolver wave and SLW-CO-4's
/// projection site — one implementation, two callers, so the fan-out constant
/// and `CS_FSYNC_PARALLELISM` knob never drift.
///
/// **Threading:** synchronous and blocking. It blocks for the slowest worker's
/// chunk (multi-second on a fresh ~1800-file wave), so a caller already on an
/// async runtime worker MUST wrap it in `spawn_blocking` (the resolver wave
/// does); a caller already on a blocking thread (the projection materialize,
/// wrapped in `spawn_blocking`/`run_worktree_projection_blocking` upstream)
/// calls it directly.
///
/// **Semantics:** no-op returning success on empty input. Per-path failure
/// warns, is counted into `failed_paths`, and continues (best-effort — it does
/// NOT gate the caller's lockfile; see [`BarrierOutcome`]). A panic escaping the
/// fan-out is caught and reported as `unit_failed = true` so the caller can
/// fail-closed (skip its lockfile write) instead of unwinding through its locks.
pub fn fsync_barrier(dests: &[PathBuf]) -> BarrierOutcome {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let start = std::time::Instant::now();
    if dests.is_empty() {
        return BarrierOutcome {
            unit_failed: false,
            failed_paths: 0,
            elapsed_ns: 0,
        };
    }

    // Test-only seam (SLW-CO-4 AC-4): force a whole-barrier failure so the
    // caller's fail-closed lockfile gate is exercisable without a real panic
    // (per-path fsync is warn-and-continue and never panics, so unit failure
    // is otherwise unreachable from a test).
    #[cfg(test)]
    if force_barrier_unit_fail_enabled() {
        return BarrierOutcome {
            unit_failed: true,
            failed_paths: 0,
            elapsed_ns: start.elapsed().as_nanos(),
        };
    }

    let fsync_cap = std::env::var("CS_FSYNC_PARALLELISM")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(FSYNC_BARRIER_FANOUT);
    let parallelism = fsync_cap.min(dests.len()).max(1);
    let chunk_size = dests.len().div_ceil(parallelism);
    let failed = AtomicUsize::new(0);
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::thread::scope(|s| {
            for chunk in dests.chunks(chunk_size) {
                let failed = &failed;
                s.spawn(move || {
                    for path in chunk {
                        if let Err(e) = fsync_path(path) {
                            failed.fetch_add(1, Ordering::Relaxed);
                            log::warn!(
                                target: "content_engine::atomic_io",
                                "fsync_path failed for {}: {} — continuing (best-effort barrier)",
                                path.display(),
                                e
                            );
                        }
                    }
                });
            }
        });
    }));
    let failed_paths = failed.load(Ordering::Relaxed);
    if failed_paths > 0 {
        // Surface the swallowed per-path failures (they don't gate the lockfile —
        // derived state is recovered by re-materialization — but a persistently
        // non-zero count is a failing-disk signal worth seeing in logs).
        log::warn!(
            target: "content_engine::atomic_io",
            "fsync_barrier: {}/{} dests failed to flush (best-effort; content is derived/re-clonable, recovered by re-materialization — but a sustained count signals a failing disk)",
            failed_paths,
            dests.len()
        );
    }
    BarrierOutcome {
        unit_failed: ran.is_err(),
        failed_paths,
        elapsed_ns: start.elapsed().as_nanos(),
    }
}

#[cfg(test)]
thread_local! {
    /// SLW-CO-4 AC-4 test seam — when set on the calling thread, the next
    /// [`fsync_barrier`] on that thread reports `unit_failed`.
    static FORCE_BARRIER_UNIT_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn force_barrier_unit_fail_enabled() -> bool {
    FORCE_BARRIER_UNIT_FAIL.with(|c| c.get())
}

/// SLW-CO-4 AC-4 test seam: force the next same-thread [`fsync_barrier`] to
/// report a whole-barrier failure (or clear the flag). Test-only.
#[cfg(test)]
pub(crate) fn set_force_barrier_unit_fail(on: bool) {
    FORCE_BARRIER_UNIT_FAIL.with(|c| c.set(on));
}

/// Same as [`write_tmp_fsync_rename`] but holds a
/// [`Suppression`](crate::content_engine::Suppression) guard covering
/// both `dest` and the unique tmp path for the duration of the write,
/// so the watcher does not see the codesense-driven events.
///
/// The dispatcher's suppression is *timestamped*: the guard sets a
/// `valid_until` on each path, and `Drop` is a no-op (the time-based
/// expiry takes over). This is what makes the helper correct against
/// FSEvents/inotify delivery latency — events queued by the kernel
/// during the write but delivered after the call returns are still
/// suppressed, as long as they arrive within the dispatcher's
/// `suppression_grace_ms` window.
///
/// Use this whenever `dest` is inside a watched directory — i.e. the
/// SS.4b materialization path. The raw [`write_tmp_fsync_rename`]
/// remains the right choice for one-off writes outside any watched
/// tree (e.g. `git_exclude`'s write to `.git/info/exclude`).
///
/// # Known limitations
///
/// - If `fs::create_dir_all(dest.parent())` materializes intermediate
///   directories that didn't previously exist, the watcher will see
///   create events for those new directories. The helper does **not**
///   register them in suppression because their paths aren't knowable
///   until the call begins. SS.4b's materialization layer should
///   either pre-create the directory tree under its own suppression
///   guard or accept the directory-create event noise.
/// - On a panic inside the write (allocator failure, signal-induced
///   unwind), Rust drops the guard before the tmp-cleanup branch runs.
///   Because suppression is timestamped (not refcounted), the tmp file
///   stays suppressed for `suppression_grace_ms` after the panic — long
///   enough for any pending watcher event to be dropped. The tmp file
///   itself may linger on disk; a follow-up cleanup pass is the
///   consumer's responsibility.
pub fn write_tmp_fsync_rename_suppressed(
    dest: &Path,
    content: &[u8],
    handle: &SuppressionHandle,
) -> Result<(), Error> {
    let tmp = pick_tmp_path(dest);
    // Suppression is now timestamped — `Suppression::new` extends the
    // window and `Drop` is a no-op (see watcher::Suppression). Binding
    // to `_guard` is purely stylistic; the suppression entry expires on
    // its own timestamp.
    let _guard = Suppression::new(handle, [dest.to_path_buf(), tmp.clone()]);
    write_to_tmp_then_rename(dest, &tmp, content, /* fsync */ true)
}

/// SW-GS-20 — write via the watcher-suppressed atomic writer when a suppression
/// handle is present (the running app, so CP-4's reverse watcher does not echo
/// the write as a phantom edit), else the plain atomic writer (headless/test,
/// no watcher registered). Centralizes the git-sync working-tree write policy
/// shared by `pull_ff`, `gs_publish`, and `gs_absorb_pull` (SW-GS-16 absorb
/// write-back) — one place decides suppressed-vs-plain.
pub fn write_atomic_suppressed(
    handle: Option<&SuppressionHandle>,
    dest: &Path,
    content: &[u8],
) -> Result<(), Error> {
    match handle {
        Some(h) => write_tmp_fsync_rename_suppressed(dest, content, h),
        None => write_tmp_fsync_rename(dest, content),
    }
}

/// SLW-CO-3: copy-on-write counterpart to the byte writer
/// [`write_tmp_fsync_rename_suppressed`], skipping the per-file `fsync`.
///
/// Takes a source **path** (not bytes) and block-shares it into the tmp
/// sibling via `cow::clone_or_copy` instead of writing the bytes, then
/// renames — the **same** crash-atomic `tmp → rename` structure and the
/// **same** `[dest, tmp]` suppression window as the byte writer, so the
/// reverse watcher never echoes the write (AC-5). On a CoW-capable volume
/// the write is near-zero time and ~zero disk-until-write; off-volume or on
/// an unsupported FS `clone_or_copy` degrades transparently to a byte copy
/// (no caller branching).
///
/// The caller still reads + hashes the source itself: the baseline hash
/// stays `blake3(source)` and the clone is byte-identical, so **no dest
/// re-hash** is needed (SLW-CO-1 / SLW-CO-3 AC-3).
///
/// **fsync policy — no per-file barrier.** This skips the `fsync` and relies
/// on the caller to drive the durability barrier out-of-band (via
/// [`fsync_path`]) after a batch — the resolver/install forward path already
/// does. At ~4 ms per `fsync` on APFS, batching the flush is the right shape
/// for a hot loop writing many files where one end-of-batch barrier satisfies
/// the caller's ordering invariant. (SLW-LC-2 removed the unused fsync-bearing
/// clone sibling; SLW-LC-3 removed the now-callerless no-fsync *byte* writer —
/// this CoW writer is the sole surviving forward-write primitive of the pair.)
pub fn clone_tmp_rename_suppressed_no_fsync(
    dest: &Path,
    source: &Path,
    handle: &SuppressionHandle,
) -> Result<(), Error> {
    let tmp = pick_tmp_path(dest);
    let _guard = Suppression::new(handle, [dest.to_path_buf(), tmp.clone()]);
    clone_to_tmp_then_rename(dest, &tmp, source)
}

/// Remove `path`, idempotently. A `NotFound` is treated as success — the
/// post-condition (the path is gone) already holds. Other I/O errors
/// propagate. Files only; directory removal is the caller's concern (the
/// structural-op command enumerates a directory's content files and removes
/// each one through here).
fn remove_inner(path: &Path) -> Result<(), Error> {
    let _tree = tree_mutation_gate()
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    let _lock = mutation_lock(mutation_stripe(path));
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Rename `from` → `to`, creating `to`'s parent dir if absent. Propagates
/// I/O errors (incl. `from` `NotFound` — a rename of a vanished source is a
/// real failure, unlike an idempotent delete).
fn rename_inner(from: &Path, to: &Path) -> Result<(), Error> {
    let _tree = tree_mutation_gate()
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    let first = mutation_stripe(from);
    let second = mutation_stripe(to);
    let _first = mutation_lock(first.min(second));
    let _second = (first != second).then(|| mutation_lock(first.max(second)));
    ensure_parent_dir(to)?;
    fs::rename(from, to)?;
    Ok(())
}

/// Remove `path` (no watcher suppression). The unsuppressed counterpart to
/// [`remove_suppressed`], for headless/test contexts where no `WatcherState`
/// is registered (mirrors how [`write_tmp_fsync_rename`] pairs with
/// [`write_tmp_fsync_rename_suppressed`]). Idempotent on `NotFound`.
pub fn remove(path: &Path) -> Result<(), Error> {
    remove_inner(path)
}

/// Rename `from` → `to` (no watcher suppression). Unsuppressed counterpart to
/// [`rename_suppressed`]; for headless/test contexts.
pub fn rename(from: &Path, to: &Path) -> Result<(), Error> {
    rename_inner(from, to)
}

/// Remove `path`, holding a [`Suppression`](crate::content_engine::Suppression)
/// guard over `path` for the dispatcher's grace window so the watcher does not
/// observe the codesense-driven removal.
///
/// Idempotent on `NotFound` (see [`remove`]). Same timestamped-suppression
/// semantics as [`write_tmp_fsync_rename_suppressed`]: the kernel-delayed
/// delete event is dropped as long as it arrives within `suppression_grace_ms`.
/// Use whenever `path` is inside a watched tree (the structural-op fan-out).
pub fn remove_suppressed(path: &Path, handle: &SuppressionHandle) -> Result<(), Error> {
    let _guard = Suppression::new(handle, [path.to_path_buf()]);
    remove_inner(path)
}

/// SW-GS-20 — remove via the watcher-suppressed remover when a suppression handle
/// is present (running app), else the plain remover (headless/test). The delete
/// analog of [`write_atomic_suppressed`], for applying an INCOMING deletion to the
/// working tree during a non-destructive pull. Idempotent on `NotFound`.
pub fn remove_atomic_suppressed(
    handle: Option<&SuppressionHandle>,
    path: &Path,
) -> Result<(), Error> {
    match handle {
        Some(h) => remove_suppressed(path, h),
        None => remove(path),
    }
}

/// Rename `from` → `to`, suppressing the watcher for **both** paths over the
/// grace window.
///
/// A rename surfaces to the watcher as a delete-of-`from` plus a create-of-`to`
/// (macOS FSEvents delivers both; see SW-SC-11), so both must be suppressed —
/// the same two-path shape [`write_tmp_fsync_rename_suppressed`] uses for
/// `[dest, tmp]`. Creates `to`'s parent dir if absent; propagates I/O errors.
pub fn rename_suppressed(from: &Path, to: &Path, handle: &SuppressionHandle) -> Result<(), Error> {
    let _guard = Suppression::new(handle, [from.to_path_buf(), to.to_path_buf()]);
    rename_inner(from, to)
}

/// SW-XS-7: block-share `src` into the fresh path `tmp` (APFS `clonefile`,
/// Linux reflink), falling back to a byte copy off-volume or where the file
/// system cannot share blocks. Bytes only: hashing, mode, fsync and the
/// rename stay with the caller. `tmp` must not exist; its parent must.
pub(crate) fn clone_or_copy(src: &Path, tmp: &Path) -> io::Result<()> {
    cow::clone_or_copy(src, tmp)
}

/// Streaming blake3 hash of `path`'s contents.
///
/// Streams via `io::copy` into `blake3::Hasher` so memory usage is
/// O(BUFSIZE), not O(file size).
pub fn hash_file(path: &Path) -> Result<[u8; 32], Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}

/// Nanoseconds-since-`UNIX_EPOCH` from an already-read [`fs::Metadata`] — the
/// shared core of [`file_mtime_ns`] / [`file_mtime_and_size`], and usable by a
/// caller that already holds a `Metadata` (e.g. one fused with a type check) to
/// avoid a second `stat`. `u128` so callers compare equality without float drift.
pub fn mtime_ns_from_metadata(metadata: &fs::Metadata) -> Result<u128, Error> {
    let modified = metadata.modified()?;
    let duration = modified.duration_since(UNIX_EPOCH).map_err(|e| {
        Error::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("mtime before UNIX_EPOCH: {}", e),
        ))
    })?;
    Ok(duration.as_nanos())
}

/// Nanosecond modification time, measured from `UNIX_EPOCH`.
///
/// Propagates I/O errors verbatim (`NotFound` becomes [`Error::Io`] with
/// `kind == NotFound`).
///
/// **Follows symlinks** — returns the target's mtime, not the link's.
/// Use `std::fs::symlink_metadata` directly if you need the link-side
/// timestamp (e.g. detecting that a symlink was relinked to a different
/// target without the target itself changing).
pub fn file_mtime_ns(path: &Path) -> Result<u128, Error> {
    mtime_ns_from_metadata(&fs::metadata(path)?)
}

/// SLW-RS-2 AC-8: mtime (ns since UNIX_EPOCH) **and** byte length from a
/// **single** `metadata` read — so a write site records both baseline stats
/// without a second `stat` syscall (the size pairs with the mtime fast-gate:
/// same mtime + different size ⇒ modified without hashing). Same symlink-follow
/// and `before UNIX_EPOCH` semantics as [`file_mtime_ns`]; `NotFound` propagates
/// as [`Error::Io`].
pub fn file_mtime_and_size(path: &Path) -> Result<(u128, u64), Error> {
    let metadata = fs::metadata(path)?;
    Ok((mtime_ns_from_metadata(&metadata)?, metadata.len()))
}

/// Returns `true` when the current mtime differs from `known_mtime_ns`.
///
/// Propagates I/O errors to the caller — does **not** silently treat a
/// missing file as drift, since downstream consumers (hash-skip, lockfile
/// reconciliation) need to distinguish "stale entry" from "file gone."
/// Follows symlinks — see [`file_mtime_ns`].
pub fn mtime_gate(path: &Path, known_mtime_ns: u128) -> Result<bool, Error> {
    let current = file_mtime_ns(path)?;
    Ok(current != known_mtime_ns)
}

/// Create `path`'s parent directory (and ancestors) if it has a non-empty
/// parent. Shared by the tmp→rename writers and [`rename_inner`], which all
/// need the dest's parent to exist before the final rename lands. A dest with
/// no parent (or an empty one) is a no-op.
fn ensure_parent_dir(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Best-effort tmp cleanup after a failed tmp→rename write, returning the
/// original write `err` (as [`Error::Io`]) for the caller to propagate.
///
/// A `NotFound` is expected and stays silent: a failed atomic clone/rename
/// may never have created the tmp (`clonefile` is atomic; the CO-2 byte-copy
/// fallback self-cleans), so the remove is often a no-op — but a failed
/// fsync/rename *after* the tmp already landed DOES leave one, which is where
/// this cleanup is load-bearing. Any non-`NotFound` cleanup failure is logged,
/// never propagated: the real write error is what the caller must see.
fn discard_tmp_after_failure(tmp: &Path, err: io::Error) -> Error {
    if let Err(cleanup_err) = fs::remove_file(tmp) {
        if cleanup_err.kind() != io::ErrorKind::NotFound {
            debug!(
                target: "content_engine::atomic_io",
                "tmp cleanup failed for {}: {}",
                tmp.display(),
                cleanup_err
            );
        }
    }
    Error::Io(err)
}

/// App-state durability: flush to the filesystem without Apple's device-cache
/// full-sync. Stage before taking shared mutation locks so a stalled flush
/// cannot stop unrelated writers on the same stripe.
pub fn write_tmp_plain_fsync_rename(dest: &Path, content: &[u8]) -> Result<(), Error> {
    let tmp = pick_tmp_path(dest);
    stage_tmp(dest, &tmp, content, |file| {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            loop {
                // The descriptor is live and owned by this call.
                if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                    return Ok(());
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
        #[cfg(not(unix))]
        file.sync_all()
    })?;
    commit_tmp(dest, &tmp)
}

#[cfg(test)]
thread_local! {
    static SYNC_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

fn stage_tmp(
    dest: &Path,
    tmp: &Path,
    content: &[u8],
    sync: impl FnOnce(&fs::File) -> io::Result<()>,
) -> Result<(), Error> {
    crate::context_repo::codesense_docs::refuse_io_write_at(dest)?;
    {
        let _tree = path_mutation_gate(dest);
        ensure_parent_dir(dest)?;
    }
    stage_tmp_file(tmp, content, sync)
}

fn stage_tmp_file(
    tmp: &Path,
    content: &[u8],
    sync: impl FnOnce(&fs::File) -> io::Result<()>,
) -> Result<(), Error> {
    let result: io::Result<()> = (|| {
        let mut file = fs::File::create(tmp)?;
        file.write_all(content)?;
        #[cfg(test)]
        SYNC_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
        sync(&file)
    })();
    result.map_err(|error| discard_tmp_after_failure(tmp, error))
}

fn write_to_tmp_then_rename(
    dest: &Path,
    tmp: &Path,
    content: &[u8],
    fsync: bool,
) -> Result<(), Error> {
    // Shared content writes retain structural and path serialization through
    // staging, sync and rename. A rename may never carry this tmp path away
    // while a writer still targets its original parent.
    crate::context_repo::codesense_docs::refuse_io_write_at(dest)?;
    let _tree = path_mutation_gate(dest);
    ensure_parent_dir(dest)?;
    let _lock = mutation_lock(mutation_stripe(dest));
    stage_tmp_file(
        tmp,
        content,
        |file| {
            if fsync {
                file.sync_all()
            } else {
                Ok(())
            }
        },
    )?;
    fs::rename(tmp, dest).map_err(|error| discard_tmp_after_failure(tmp, error))
}

fn commit_tmp(dest: &Path, tmp: &Path) -> Result<(), Error> {
    let _tree = path_mutation_gate(dest);
    let _lock = mutation_lock(mutation_stripe(dest));
    fs::rename(tmp, dest).map_err(|error| discard_tmp_after_failure(tmp, error))
}

fn write_to_tmp_then_rename_with_permissions(
    dest: &Path,
    tmp: &Path,
    content: &[u8],
    fsync: bool,
    permissions: Option<fs::Permissions>,
    validate: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    // SW-OC-16: the bundled CodeSense docs are read-only (the one rule, at the writer).
    crate::context_repo::codesense_docs::refuse_io_write_at(dest)?;
    ensure_parent_dir(dest)?;

    let result: io::Result<()> = (|| {
        let mut file = fs::File::create(tmp)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)?;
        if fsync {
            file.sync_all()?;
        }
        drop(file);
        validate()
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        fs::rename(tmp, dest)
    })();

    if let Err(err) = result {
        return Err(discard_tmp_after_failure(tmp, err));
    }

    Ok(())
}

/// SLW-CO-3: clone twin of [`write_to_tmp_then_rename`]. Block-shares
/// `source` into `tmp` via [`cow::clone_or_copy`] (transparent byte-copy
/// fallback off-volume / on an unsupported FS), then renames it onto
/// `dest`. Identical crash-atomic contract and tmp-cleanup-on-error branch
/// as the byte writer — **only** the "how bytes land" step differs (clone
/// instead of `File::create` + `write_all`).
///
/// No per-file fsync: its sole remaining caller
/// ([`clone_tmp_rename_suppressed_no_fsync`]) batches an out-of-band
/// durability barrier ([`fsync_barrier`]) at the wave level. SLW-LC-2 removed the
/// fsync-bearing clone variant, whose tmp-reopen `sync_all` was the only
/// reason this helper ever took an `fsync` flag; with that gone the flag
/// was always `false`, so both were dropped.
fn clone_to_tmp_then_rename(dest: &Path, tmp: &Path, source: &Path) -> Result<(), Error> {
    let _tree = path_mutation_gate(dest);
    let _lock = mutation_lock(mutation_stripe(dest));
    // SW-OC-16: the bundled CodeSense docs are read-only (the one rule, at the writer).
    crate::context_repo::codesense_docs::refuse_io_write_at(dest)?;
    ensure_parent_dir(dest)?;

    let result: io::Result<()> = (|| {
        cow::clone_or_copy(source, tmp)?;
        fs::rename(tmp, dest)
    })();

    if let Err(err) = result {
        return Err(discard_tmp_after_failure(tmp, err));
    }

    Ok(())
}

fn pick_tmp_path(dest: &Path) -> PathBuf {
    // Caller invariant: `dest` is a file path, not a directory with a
    // trailing path separator. A trailing separator would put the tmp
    // file *inside* `dest` (interpreted as a directory) rather than
    // beside it, and the final rename would target a directory.
    debug_assert!(
        dest.file_name().is_some(),
        "write_tmp_fsync_rename requires dest to have a file name (no trailing separator); got {:?}",
        dest
    );
    // Random suffix per call eliminates the module-level static counter
    // (which would have violated AC-9's "no module-level statics" rule)
    // and removes the cross-process collision window that a (pid,
    // counter) scheme had inside shared PID namespaces (containers).
    let suffix: u64 = rand::thread_rng().gen();
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(format!(".tmp.{:016x}", suffix));
    PathBuf::from(tmp)
}

// ============================================================================
// SLW-CO-2: copy-on-write clone primitive + transparent byte-copy fallback
// ----------------------------------------------------------------------------
// `cow::clone_or_copy` block-shares a source into a fresh tmp sibling via the
// platform CoW primitive (APFS `clonefile` / Linux `reflink`), degrading
// silently to a byte copy off-volume or on filesystems that can't share
// blocks. It lands bytes ONLY — hashing, fsync, and the tmp→rename dance stay
// with the caller. SLW-CO-3 wired it into both forward write sites via the
// `clone_tmp_*_suppressed` entry points → `clone_to_tmp_then_rename`.
// ============================================================================
mod cow {
    use std::fs;
    use std::io;
    use std::path::Path;

    /// Block-sharing clone of `src` into `tmp`, with a transparent byte-copy
    /// fallback when the volume / filesystem can't share blocks.
    ///
    /// On success `tmp` shares `src`'s on-disk blocks copy-on-write: near-zero
    /// time and ~zero extra disk until a block is modified. The clone is
    /// byte-identical to the source, so a caller that already hashed the source
    /// need not re-hash the result (SLW-CO-1 picked hash-the-source).
    ///
    /// # Contract
    /// - `tmp` MUST NOT already exist — `clonefile` / `FICLONE` fail `EEXIST`
    ///   otherwise. `pick_tmp_path` mints a fresh random sibling, the intended tmp.
    /// - `tmp`'s parent directory MUST exist (as `clonefile` / `fs::copy` require).
    ///   The SLW-CO-3 entry points create it, mirroring `write_to_tmp_then_rename`.
    ///
    /// # Fallback (silent by design)
    /// A cross-volume dest (`EXDEV` — the load-bearing off-volume-worktree case,
    /// SLW-CO-1) or an unsupported filesystem (`ENOTSUP` / `EOPNOTSUPP` / `ENOTTY`,
    /// or the no-CoW-primitive platform arm) degrades to `fs::copy` — no error
    /// surfaced, no branching required in callers. Any *other* I/O error
    /// (permissions, `ENOSPC`, missing source) propagates.
    pub(super) fn clone_or_copy(src: &Path, tmp: &Path) -> io::Result<()> {
        finish_clone_or_copy(src, tmp, platform_clone(src, tmp))
    }

    /// The fallback decision, split from the syscall so tests can drive every
    /// branch deterministically — inject an `EXDEV` failure without needing a
    /// second volume, a real error to prove propagation, etc.
    pub(super) fn finish_clone_or_copy(
        src: &Path,
        tmp: &Path,
        clone_result: io::Result<()>,
    ) -> io::Result<()> {
        match clone_result {
            Ok(()) => Ok(()),
            Err(e) if is_clone_unsupported(&e) => {
                // Off-volume / unsupported FS → byte copy into the same tmp.
                // A failed CLONE leaves no tmp behind (`clonefile` is atomic;
                // `reflink-copy` removes its O_EXCL dest via RAII). The byte
                // COPY has no such guarantee — a mid-stream failure (ENOSPC,
                // EIO) can leave a partial tmp — so clean it up on error,
                // mirroring `write_to_tmp_then_rename`, to keep the "no tmp on
                // failure" contract true on every path.
                if let Err(copy_err) = fs::copy(src, tmp) {
                    let _ = fs::remove_file(tmp);
                    return Err(copy_err);
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// True when a clone error means "this volume / filesystem can't block-share,
    /// fall back to a byte copy" rather than "a real I/O failure to propagate."
    ///
    /// Off-volume worktrees return `EXDEV` (the measured, load-bearing case,
    /// SLW-CO-1); a same-device unsupported FS returns `ENOTSUP` / `EOPNOTSUPP`.
    /// Those three are common to `clonefile` and `FICLONE`, so they match on any
    /// Unix. **Linux `FICLONE` additionally** signals unsupported as `ENOTTY`
    /// (older kernels / an fs whose ioctl handler rejects it) or — per
    /// `man 2 ioctl_ficlone`, for a whole-file clone of a **regular file** —
    /// `EINVAL` ("filesystem does not support reflinking"; several overlay /
    /// FUSE / network filesystems report it this way). `ENOTTY`/`EINVAL` are
    /// **not** `clonefile` errnos, so they are scoped to Linux — on macOS an
    /// `EINVAL` from `clonefile` means a genuine bad argument and must propagate,
    /// not be masked as a silent copy. The no-CoW-primitive platform arm signals
    /// `ErrorKind::Unsupported`.
    ///
    /// Genuine failures (`EACCES`, `ENOSPC`, `ENOENT`, `EPERM`, …) are NOT in the
    /// set — they propagate rather than being masked as a silent copy.
    pub(super) fn is_clone_unsupported(err: &io::Error) -> bool {
        if err.kind() == io::ErrorKind::Unsupported {
            return true;
        }
        // Errno inspection is Unix-only, mirroring the EXDEV shape at
        // `commands/content_engine.rs:2056-2062` (keeps non-Unix builds green).
        #[cfg(unix)]
        {
            if let Some(code) = err.raw_os_error() {
                // Cross-volume + same-device-unsupported: clonefile AND FICLONE.
                if code == libc::EXDEV || code == libc::ENOTSUP || code == libc::EOPNOTSUPP {
                    return true;
                }
                // Linux FICLONE-only "unsupported" signals — see doc above.
                #[cfg(target_os = "linux")]
                if code == libc::ENOTTY || code == libc::EINVAL {
                    return true;
                }
            }
        }
        false
    }

    /// Attempt the platform copy-on-write clone of `src` → `tmp`, returning the
    /// raw syscall / crate result. The fallback decision lives in
    /// `finish_clone_or_copy`.
    ///
    /// macOS: `libc::clonefile` on the existing `libc` dep (no new crate; the
    /// path SLW-CO-1 measured). `flags = 0` (no `CLONE_NOFOLLOW`) — content
    /// sources are regular files.
    #[cfg(target_os = "macos")]
    pub(super) fn platform_clone(src: &Path, tmp: &Path) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let src_c = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let tmp_c = std::ffi::CString::new(tmp.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let rc = unsafe { libc::clonefile(src_c.as_ptr(), tmp_c.as_ptr(), 0) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Linux CoW clone via `reflink-copy` — encapsulates btrfs / XFS / bcachefs
    /// `FICLONE` detection. Strict `reflink` (NOT `reflink_or_copy`) so the
    /// unsupported-FS case funnels into the single `finish_clone_or_copy`
    /// fallback rather than the crate's, keeping one fallback path.
    #[cfg(target_os = "linux")]
    pub(super) fn platform_clone(src: &Path, tmp: &Path) -> io::Result<()> {
        reflink_copy::reflink(src, tmp)
    }

    /// Platforms without a wired CoW primitive (Windows, other BSDs): signal
    /// `Unsupported` so `clone_or_copy` always takes the byte-copy fallback.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn platform_clone(_src: &Path, _tmp: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn cp3_stalled_app_state_sync_does_not_hold_the_shared_mutation_stripe() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let other = dir.path().join("other.json");
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            SYNC_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                })));
            });
            write_tmp_plain_fsync_rename(&path, b"state")
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (written, observed) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            let result = write_tmp_rename_no_fsync(&other, b"independent");
            written.send(result).unwrap();
        });
        let result = observed.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        contender.join().unwrap();
        result
            .expect("another file in the same directory waited for the stalled sync")
            .unwrap();
    }

    #[test]
    fn cp3_file_rename_waits_for_staged_save_and_moves_saved_bytes() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.md");
        let renamed = dir.path().join("moved.md");
        fs::write(&path, b"before").unwrap();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            SYNC_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                })))
            });
            write_tmp_fsync_rename(&writer_path, b"saved")
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (renamed_tx, renamed_rx) = mpsc::channel();
        let from = path.clone();
        let to = renamed.clone();
        let rename_operation =
            std::thread::spawn(move || renamed_tx.send(rename(&from, &to)).unwrap());
        assert!(renamed_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        renamed_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        rename_operation.join().unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(renamed).unwrap(), b"saved");
    }

    #[test]
    fn cp3_parent_rename_waits_for_staged_save_and_keeps_tmp_in_place() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let moved = dir.path().join("moved");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("file.md");
        fs::write(&path, b"before").unwrap();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            SYNC_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                })))
            });
            write_tmp_fsync_rename(&writer_path, b"saved")
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (renamed_tx, renamed_rx) = mpsc::channel();
        let from = parent.clone();
        let to = moved.clone();
        let rename_operation =
            std::thread::spawn(move || renamed_tx.send(rename(&from, &to)).unwrap());
        assert!(renamed_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        renamed_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        rename_operation.join().unwrap();
        assert_eq!(fs::read(moved.join("file.md")).unwrap(), b"saved");
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
    }

    #[test]
    fn selected_authority_revoked_while_waiting_for_writer_locks_cannot_commit() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        for tree_wait in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("original");
            fs::write(&path, b"original").unwrap();
            let revoked = Arc::new(AtomicBool::new(false));
            let stripe = (!tree_wait).then(|| mutation_lock(mutation_stripe(&path)));
            let tree = tree_wait.then(|| tree_mutation_gate().write().unwrap());
            let (started_tx, started_rx) = mpsc::channel();
            let (validated_tx, validated_rx) = mpsc::channel();
            let worker_path = path.clone();
            let worker_revoked = revoked.clone();
            let worker = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                write_tmp_fsync_rename_expected_authorized(
                    &worker_path,
                    b"forbidden",
                    Some(*blake3::hash(b"original").as_bytes()),
                    None,
                    &|| {
                        validated_tx.send(()).unwrap();
                        if worker_revoked.load(Ordering::SeqCst) {
                            Err(Error::Io(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "retired session",
                            )))
                        } else {
                            Ok(())
                        }
                    },
                )
            });
            started_rx.recv().unwrap();
            assert!(
                validated_rx
                    .recv_timeout(std::time::Duration::from_millis(30))
                    .is_err(),
                "validator must run after the held writer lock"
            );
            revoked.store(true, Ordering::SeqCst);
            drop(stripe);
            drop(tree);
            assert!(worker.join().unwrap().is_err());
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
            assert!(write_tmp_fsync_rename_expected_authorized(
                &path,
                b"authorized continuation",
                Some(*blake3::hash(b"original").as_bytes()),
                None,
                &|| Ok(())
            )
            .unwrap());
            assert_eq!(fs::read(&path).unwrap(), b"authorized continuation");
        }
    }

    #[test]
    fn selected_authority_rechecked_before_atomic_rename() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("original");
        fs::write(&path, b"original").unwrap();
        let calls = std::cell::Cell::new(0);
        let result = write_tmp_fsync_rename_expected_authorized(
            &path,
            b"forbidden",
            Some(*blake3::hash(b"original").as_bytes()),
            None,
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 3 {
                    Err(Error::Io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "retired before rename",
                    )))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 3);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            1,
            "temporary bytes removed on refused commit"
        );
    }

    #[test]
    fn selected_filename_aliases_share_writer_stripe_across_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("É.md");
        let lower = temp.path().join("e\u{301}.md");
        fs::write(&upper, b"original").unwrap();
        assert_eq!(mutation_stripe(&upper), mutation_stripe(&lower));
        write_tmp_fsync_rename(&upper, b"replacement").unwrap();
        assert_eq!(mutation_stripe(&upper), mutation_stripe(&lower));
    }
    use super::cow::clone_or_copy;
    // The remaining CoW helpers are exercised only by `#[cfg(unix)]` tests below,
    // so gate the import too — keeps a non-Unix test build warning-free.
    #[cfg(unix)]
    use super::cow::{finish_clone_or_copy, is_clone_unsupported, platform_clone};
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::content_engine::dispatcher::Dispatcher;

    // ====== no-fsync write variant + path fsync =====================

    /// The no-fsync helper round-trips bytes to `dest`. Atomicity comes
    /// from tmp+rename, not from `fsync`, so dropping the flush must not
    /// change observable correctness.
    #[test]
    fn write_tmp_rename_no_fsync_round_trips_bytes() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("nofsync.txt");
        write_tmp_rename_no_fsync(&dest, b"payload").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
    }

    /// Atomicity invariant: `dest` is either absent or byte-complete,
    /// never partial. We can't crash mid-write in a unit test, but we
    /// can force the rename to fail (dest is a non-empty directory) and
    /// assert that a pre-existing sentinel file inside is untouched.
    /// Pins that tmp+rename is the atomicity primitive, not fsync.
    #[test]
    fn write_tmp_rename_no_fsync_leaves_dest_untouched_on_rename_failure() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("blocked");
        fs::create_dir(&dest).unwrap();
        let sentinel = dest.join("sentinel.txt");
        fs::write(&sentinel, b"sentinel-bytes").unwrap();

        let result = write_tmp_rename_no_fsync(&dest, b"would-overwrite");
        assert!(
            result.is_err(),
            "expected rename onto non-empty dir to fail"
        );
        assert!(dest.is_dir(), "dest must remain a directory");
        assert_eq!(
            fs::read(&sentinel).unwrap(),
            b"sentinel-bytes",
            "pre-existing content under dest must be byte-identical after failed no-fsync write"
        );

        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "tmp files must be cleaned up after a failed no-fsync write: {:?}",
            leftover.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    /// `fsync_path` is the durability primitive paired with the
    /// no-fsync write helpers. Succeeds on a regular file and surfaces
    /// `NotFound` cleanly for a missing path so a warn-and-continue
    /// caller has something concrete to inspect.
    #[test]
    fn fsync_path_succeeds_on_regular_file_and_surfaces_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, b"x").unwrap();
        fsync_path(&path).expect("fsync of a regular file must succeed");

        let missing = dir.path().join("does-not-exist");
        let err = fsync_path(&missing).expect_err("missing file must error");
        match err {
            Error::Io(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    // ---- SLW-CO-4: fsync_barrier ----------------------------------------

    #[test]
    fn fsync_barrier_empty_is_noop_success() {
        let outcome = fsync_barrier(&[]);
        assert!(!outcome.unit_failed, "empty barrier must not unit-fail");
        assert_eq!(outcome.elapsed_ns, 0, "empty barrier does no work");
    }

    #[test]
    fn fsync_barrier_flushes_all_written_paths() {
        let dir = TempDir::new().unwrap();
        // 40 files against the default fan-out (32) splits work across scoped
        // threads (chunk_size = ceil(40/32) = 2), exercising the fan-out
        // without mutating the process-global `CS_FSYNC_PARALLELISM`.
        let dests: Vec<PathBuf> = (0..40)
            .map(|i| {
                let p = dir.path().join(format!("f{i}.txt"));
                fs::write(&p, format!("content-{i}").as_bytes()).unwrap();
                p
            })
            .collect();
        let outcome = fsync_barrier(&dests);
        assert!(!outcome.unit_failed, "flushing real files must succeed");
        assert_eq!(outcome.failed_paths, 0, "all real files flush cleanly");
        // Content is intact after the barrier (fsync must not mutate bytes).
        for (i, p) in dests.iter().enumerate() {
            assert_eq!(fs::read(p).unwrap(), format!("content-{i}").into_bytes());
        }
    }

    #[test]
    fn fsync_barrier_missing_path_is_best_effort_not_unit_failure() {
        let dir = TempDir::new().unwrap();
        let present = dir.path().join("present.txt");
        fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("gone.txt");
        // A per-path fsync failure warns and continues; it does NOT flip
        // `unit_failed` (the whole barrier still ran) — but it IS counted and
        // returned so the swallowed error is observable.
        let outcome = fsync_barrier(&[present, missing]);
        assert!(
            !outcome.unit_failed,
            "a single missing path is best-effort, not a whole-barrier failure"
        );
        assert_eq!(
            outcome.failed_paths, 1,
            "the one missing path is counted (observable, not silently swallowed)"
        );
    }

    #[test]
    fn fsync_barrier_force_seam_reports_unit_failure() {
        // The AC-4 test seam is what makes the caller's fail-closed lockfile
        // gate exercisable (per-path fsync never panics, so unit failure is
        // otherwise unreachable from a test).
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("f.txt");
        fs::write(&p, b"x").unwrap();
        set_force_barrier_unit_fail(true);
        let outcome = fsync_barrier(&[p]);
        set_force_barrier_unit_fail(false);
        assert!(outcome.unit_failed, "forced seam must report unit failure");
        // And the flag is per-thread and cleared — a subsequent barrier is fine.
        let outcome2 = fsync_barrier(&[]);
        assert!(!outcome2.unit_failed);
    }

    #[test]
    fn write_tmp_fsync_rename_creates_file() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("a.txt");
        write_tmp_fsync_rename(&dest, b"hello").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn write_tmp_fsync_rename_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("nested/deep/file.txt");
        write_tmp_fsync_rename(&dest, b"x").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"x");
    }

    #[test]
    fn write_tmp_fsync_rename_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("a.txt");
        fs::write(&dest, b"old").unwrap();
        write_tmp_fsync_rename(&dest, b"new").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }

    #[test]
    fn write_tmp_fsync_rename_cleans_tmp_on_rename_failure() {
        // Make the rename fail by pointing dest at a non-empty directory.
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("blocked");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("inner"), b"data").unwrap();

        let result = write_tmp_fsync_rename(&dest, b"data");
        assert!(result.is_err(), "expected rename to fail");

        // No tmp file lingering in the parent.
        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "expected no <dest>.tmp.* files after rename failure: {:?}",
            leftover.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_tmp_fsync_rename_leaves_pre_existing_dest_untouched_on_failure() {
        // P19: AC-4 promises "dest left untouched if rename fails." Pin
        // it explicitly with a sentinel value.
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("blocked");
        fs::create_dir(&dest).unwrap();
        let sentinel = dest.join("sentinel.txt");
        fs::write(&sentinel, b"sentinel-bytes").unwrap();

        let result = write_tmp_fsync_rename(&dest, b"would-overwrite");
        assert!(result.is_err());

        // The pre-existing dest (directory + its sentinel file) is intact.
        assert!(
            dest.is_dir(),
            "dest must remain a directory after failed write"
        );
        assert_eq!(
            fs::read(&sentinel).unwrap(),
            b"sentinel-bytes",
            "pre-existing content under dest must be byte-identical after failed write"
        );
    }

    #[test]
    fn unique_tmp_paths_allow_concurrent_writes_to_same_dest() {
        // P5: two writers to the same dest must not clobber each other's
        // in-flight tmp file. The final dest will hold whichever rename
        // landed last, but neither write should produce an error or
        // leave a tmp file behind.
        let dir = Arc::new(TempDir::new().unwrap());
        let dest = dir.path().join("contested.txt");

        let dest_a = dest.clone();
        let dir_a = Arc::clone(&dir);
        let a = std::thread::spawn(move || {
            for _ in 0..50 {
                write_tmp_fsync_rename(&dest_a, b"A").unwrap();
            }
            drop(dir_a);
        });

        let dest_b = dest.clone();
        let dir_b = Arc::clone(&dir);
        let b = std::thread::spawn(move || {
            for _ in 0..50 {
                write_tmp_fsync_rename(&dest_b, b"B").unwrap();
            }
            drop(dir_b);
        });

        a.join().unwrap();
        b.join().unwrap();

        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "tmp files leaked after concurrent writes: {:?}",
            leftover.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_tmp_fsync_rename_suppressed_keeps_dest_suppressed_within_grace() {
        // Iter-2 contract under timestamped suppression: after a
        // suppressed write returns, `dest` stays in the suppression set
        // for the dispatcher's grace window. This is what protects
        // kernel-delayed FSEvents from being dispatched to handlers.
        // Use a short 100 ms grace so the test can also verify the
        // suppression entry expires.
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let dest = dir.path().join("payload.txt");
        write_tmp_fsync_rename_suppressed(&dest, b"hello", &handle).unwrap();

        let logged: Arc<std::sync::Mutex<Vec<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let logged_clone = Arc::clone(&logged);
        let handler: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
            logged_clone.lock().unwrap().push(p.to_path_buf());
        });
        dispatcher.register_handler(dir.path(), handler);

        // Within the grace window the dispatch is suppressed.
        dispatcher.dispatch(&dest);
        assert_eq!(
            logged.lock().unwrap().len(),
            0,
            "dest must stay suppressed during the grace window"
        );

        // After grace expires, dispatch fires.
        std::thread::sleep(std::time::Duration::from_millis(200));
        dispatcher.dispatch(&dest);
        assert_eq!(
            logged.lock().unwrap().len(),
            1,
            "dispatch must resume once the grace window passes"
        );
    }

    #[test]
    fn hash_file_is_deterministic_and_matches_known_vector() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("h.txt");
        fs::write(&path, b"hello world").unwrap();
        let a = hash_file(&path).unwrap();
        let b = hash_file(&path).unwrap();
        assert_eq!(a, b);
        // blake3 of "hello world" — verified via `b3sum`.
        let expected =
            hex_to_array("d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24");
        assert_eq!(a, expected);
    }

    #[test]
    fn hash_file_streams_large_input() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big.bin");
        let mut file = fs::File::create(&path).unwrap();
        // 2 MiB of zeros — well over any reasonable in-memory buffer.
        let chunk = vec![0u8; 64 * 1024];
        for _ in 0..32 {
            file.write_all(&chunk).unwrap();
        }
        file.sync_all().unwrap();
        let _ = hash_file(&path).unwrap();
    }

    #[test]
    fn file_mtime_ns_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.txt");
        fs::write(&path, b"x").unwrap();
        let t = file_mtime_ns(&path).unwrap();
        assert!(t > 0, "mtime should be after UNIX_EPOCH");
        assert!(!mtime_gate(&path, t).unwrap(), "same mtime → no drift");
    }

    #[test]
    fn mtime_gate_detects_drift() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("m.txt");
        fs::write(&path, b"x").unwrap();
        // Force drift by passing a stale value.
        assert!(mtime_gate(&path, 0).unwrap());
    }

    #[test]
    fn mtime_gate_propagates_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing.txt");
        let err = mtime_gate(&path, 0).expect_err("missing file should error");
        match err {
            Error::Io(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    // ====== structural ops: remove + rename (SW-CP-14 T1) ===========

    #[test]
    fn remove_deletes_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("gone.md");
        fs::write(&path, b"x").unwrap();
        remove(&path).unwrap();
        assert!(!path.exists(), "file must be removed");
    }

    #[test]
    fn remove_is_idempotent_on_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("never.md");
        // Goal-state semantics: removing an already-absent path is success.
        remove(&path).expect("removing a missing file must be Ok (idempotent)");
    }

    #[test]
    fn rename_moves_file_and_creates_parent() {
        let dir = TempDir::new().unwrap();
        let from = dir.path().join("old.md");
        let to = dir.path().join("nested/new.md");
        fs::write(&from, b"payload").unwrap();
        rename(&from, &to).unwrap();
        assert!(!from.exists(), "old path must be gone");
        assert_eq!(
            fs::read(&to).unwrap(),
            b"payload",
            "content preserved at new path"
        );
    }

    #[test]
    fn rename_propagates_not_found_for_missing_source() {
        let dir = TempDir::new().unwrap();
        let from = dir.path().join("missing.md");
        let to = dir.path().join("dest.md");
        let err = rename(&from, &to).expect_err("renaming a missing source must error");
        match err {
            Error::Io(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn remove_suppressed_keeps_path_suppressed_within_grace() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let path = dir.path().join("doc.md");
        fs::write(&path, b"x").unwrap();
        remove_suppressed(&path, &handle).unwrap();
        assert!(!path.exists());

        let logged: Arc<std::sync::Mutex<Vec<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let logged_clone = Arc::clone(&logged);
        let handler: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
            logged_clone.lock().unwrap().push(p.to_path_buf());
        });
        dispatcher.register_handler(dir.path(), handler);

        // The kernel-delayed delete event is dropped within the grace window.
        dispatcher.dispatch(&path);
        assert_eq!(
            logged.lock().unwrap().len(),
            0,
            "removed path must stay suppressed during the grace window"
        );

        std::thread::sleep(std::time::Duration::from_millis(200));
        dispatcher.dispatch(&path);
        assert_eq!(
            logged.lock().unwrap().len(),
            1,
            "dispatch must resume once the grace window passes"
        );
    }

    #[test]
    fn rename_suppressed_suppresses_both_paths_within_grace() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let from = dir.path().join("old.md");
        let to = dir.path().join("new.md");
        fs::write(&from, b"payload").unwrap();
        rename_suppressed(&from, &to, &handle).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"payload");

        let logged: Arc<std::sync::Mutex<Vec<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let logged_clone = Arc::clone(&logged);
        let handler: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
            logged_clone.lock().unwrap().push(p.to_path_buf());
        });
        dispatcher.register_handler(dir.path(), handler);

        // BOTH the delete-of-`from` and create-of-`to` events are suppressed.
        dispatcher.dispatch(&from);
        dispatcher.dispatch(&to);
        assert_eq!(
            logged.lock().unwrap().len(),
            0,
            "both rename endpoints must stay suppressed during the grace window"
        );

        std::thread::sleep(std::time::Duration::from_millis(200));
        dispatcher.dispatch(&from);
        dispatcher.dispatch(&to);
        assert_eq!(
            logged.lock().unwrap().len(),
            2,
            "dispatch must resume for both paths once the grace window passes"
        );
    }

    fn hex_to_array(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    // ====== SLW-CO-2: clone_or_copy primitive + fallback ============

    /// Free bytes on the volume containing `p` (statvfs — mirrors the pattern
    /// at `commands/deps.rs:660`). Test-only.
    #[cfg(unix)]
    fn avail_bytes(p: &Path) -> u64 {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        unsafe {
            let mut s: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c.as_ptr(), &mut s) == 0 {
                s.f_bavail as u64 * s.f_frsize
            } else {
                0
            }
        }
    }

    /// Write `size` bytes of a non-zero repeating pattern so APFS can't
    /// sparse-optimise the reference copy and understate its true space cost.
    #[cfg(unix)]
    fn write_nonzero_pattern(path: &Path, size: u64) {
        let chunk: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        let mut f = fs::File::create(path).unwrap();
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            f.write_all(&chunk[..n]).unwrap();
            left -= n as u64;
        }
        f.sync_all().unwrap();
    }

    /// AC-3 (always-on): the clone/copy result is byte-identical to the source.
    /// Reads the tmp back off disk (not a re-hash of the in-memory payload) so a
    /// corrupt/partial clone would actually be caught. Runs on every FS — APFS
    /// clones, a non-CoW CI volume falls back to a byte copy, and identity holds
    /// either way (so this also covers the fallback on those hosts).
    #[test]
    fn clone_or_copy_produces_byte_identical_tmp() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let tmp = pick_tmp_path(&dir.path().join("dest.bin"));
        let payload = b"clone_or_copy must land byte-identical content";
        fs::write(&src, payload).unwrap();

        clone_or_copy(&src, &tmp).unwrap();

        assert_eq!(
            hash_file(&src).unwrap(),
            hash_file(&tmp).unwrap(),
            "clone/copy tmp must be byte-identical to source"
        );
        assert_eq!(fs::read(&tmp).unwrap(), payload);
    }

    /// AC-2 / AC-4: the errno classifier swallows exactly the fallback-worthy
    /// conditions and nothing else — a real failure must propagate, never be
    /// silently converted into a copy.
    #[cfg(unix)]
    #[test]
    fn is_clone_unsupported_classifies_only_fallback_conditions() {
        // Common to clonefile + FICLONE on every Unix.
        for code in [libc::EXDEV, libc::ENOTSUP, libc::EOPNOTSUPP] {
            assert!(
                is_clone_unsupported(&io::Error::from_raw_os_error(code)),
                "errno {code} should trigger the byte-copy fallback"
            );
        }
        // Linux FICLONE-only "unsupported" signals.
        #[cfg(target_os = "linux")]
        for code in [libc::ENOTTY, libc::EINVAL] {
            assert!(
                is_clone_unsupported(&io::Error::from_raw_os_error(code)),
                "Linux FICLONE errno {code} should trigger the byte-copy fallback"
            );
        }
        // On macOS, EINVAL from clonefile is a genuine bad argument, NOT
        // "unsupported" — it must propagate, so it is not scoped in.
        #[cfg(target_os = "macos")]
        assert!(
            !is_clone_unsupported(&io::Error::from_raw_os_error(libc::EINVAL)),
            "macOS clonefile EINVAL is a real error and must propagate, not fall back"
        );
        for code in [libc::EACCES, libc::ENOSPC, libc::ENOENT] {
            assert!(
                !is_clone_unsupported(&io::Error::from_raw_os_error(code)),
                "errno {code} is a real failure and must propagate, not fall back"
            );
        }
        assert!(
            is_clone_unsupported(&io::Error::from(io::ErrorKind::Unsupported)),
            "the no-CoW-primitive platform sentinel must trigger the fallback"
        );
    }

    /// AC-4: when the clone reports an unsupported / off-volume condition, the
    /// byte-copy fallback runs and produces a byte-identical file. Driven by
    /// injecting an `EXDEV` failure so it is deterministic on any host — no
    /// second volume / RAM disk needed (that manual probe lives in the SLW-CO-1
    /// bench).
    #[cfg(unix)]
    #[test]
    fn fallback_byte_copies_and_is_identical_on_unsupported() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let tmp = pick_tmp_path(&dir.path().join("dest.bin"));
        let payload = b"fallback must reproduce the source exactly";
        fs::write(&src, payload).unwrap();

        finish_clone_or_copy(&src, &tmp, Err(io::Error::from_raw_os_error(libc::EXDEV)))
            .expect("EXDEV must fall back to a byte copy, not error");

        assert_eq!(hash_file(&src).unwrap(), hash_file(&tmp).unwrap());
        assert_eq!(fs::read(&tmp).unwrap(), payload);
    }

    /// AC-2: a genuine I/O error (not a fallback condition) propagates verbatim
    /// — the fallback must not mask real failures. `EACCES` hits the non-fallback
    /// arm, which returns the error without touching the filesystem.
    #[cfg(unix)]
    #[test]
    fn real_clone_error_propagates() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        let tmp = pick_tmp_path(&dir.path().join("dest.bin"));
        fs::write(&src, b"x").unwrap();

        let err = finish_clone_or_copy(&src, &tmp, Err(io::Error::from_raw_os_error(libc::EACCES)))
            .expect_err("a non-fallback clone error must propagate");
        assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    }

    /// AC-2/AC-4 negative: if the byte-copy fallback *itself* fails, the error
    /// propagates (never reported as success). A directory source makes
    /// `fs::copy` fail deterministically.
    ///
    /// Scope note: `std::fs::copy` rejects a non-regular-file source (its
    /// `is_file` check) **before** it opens/creates the destination, so this
    /// case fails before `tmp` is ever created — it exercises the propagation
    /// path, and confirms no orphan tmp, but does NOT cover the mid-stream
    /// partial-write cleanup branch (`remove_file` after a failed `fs::copy`).
    /// That branch requires an `ENOSPC`/`EIO` fault after `tmp` is opened, which
    /// is not portably injectable in a unit test; it reuses the exact
    /// cleanup-on-error shape already proven for `write_to_tmp_then_rename`
    /// (see `write_tmp_fsync_rename_cleans_tmp_on_rename_failure`).
    #[cfg(unix)]
    #[test]
    fn fallback_copy_failure_propagates() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("a_directory");
        fs::create_dir(&src_dir).unwrap();
        let tmp = pick_tmp_path(&dir.path().join("dest.bin"));

        // EXDEV routes into the fallback; `fs::copy(dir, tmp)` then fails.
        let res = finish_clone_or_copy(
            &src_dir,
            &tmp,
            Err(io::Error::from_raw_os_error(libc::EXDEV)),
        );
        assert!(
            res.is_err(),
            "a failed fallback copy must propagate, not report success"
        );
        assert!(
            !tmp.exists(),
            "a copy that failed before creating tmp must leave no orphan"
        );
    }

    /// AC-3a (capability-gated, `#[ignore]`): on a clone-capable volume the
    /// clone shares blocks — free-space delta after a clone is ~0 versus a full
    /// copy of the same file. Robust off-APFS: a runtime capability probe skips
    /// when the FS can't clone, and noisy/full-volume statvfs readings are
    /// treated as INCONCLUSIVE (skip), not failures. Ported from the SLW-CO-1
    /// bench (`ac3_space`). Uses a 64 MiB file so the copy delta clears
    /// live-volume statvfs noise; `#[ignore]` keeps that I/O out of the fast
    /// suite (run: `cargo test -- --ignored`).
    #[cfg(unix)]
    #[test]
    #[ignore = "writes a 64 MiB file to measure block-sharing; run explicitly with --ignored"]
    fn clone_shares_blocks_on_capable_fs() {
        let dir = TempDir::new().unwrap();

        // (1) runtime clone-capability probe — does THIS volume block-share?
        let probe_src = dir.path().join("probe");
        fs::write(&probe_src, b"probe").unwrap();
        let probe_tmp = pick_tmp_path(&dir.path().join("probe.dst"));
        match platform_clone(&probe_src, &probe_tmp) {
            Ok(()) => {
                let _ = fs::remove_file(&probe_tmp);
            }
            Err(e) if is_clone_unsupported(&e) => {
                eprintln!("SKIP clone_shares_blocks_on_capable_fs: FS can't block-share ({e})");
                return;
            }
            Err(e) => panic!("capability probe failed unexpectedly: {e}"),
        }

        // (2) 64 MiB non-sparse source so the copy delta dwarfs statvfs noise.
        let size: u64 = 64 << 20;
        let src = dir.path().join("big.bin");
        write_nonzero_pattern(&src, size);
        unsafe { libc::sync() };

        // Reference full copy — an explicit read + byte write, NOT `fs::copy`.
        // On macOS `fs::copy` uses `fcopyfile`, which itself CoW-clones on APFS
        // (~0 delta), so it can't serve as the "real copy" baseline; force a
        // genuine allocation so `copy_delta` reflects the true file cost (as the
        // SLW-CO-1 bench does at `ac3_space`).
        let copy_dst = dir.path().join("copy.bin");
        let before_copy = avail_bytes(dir.path());
        {
            let bytes = fs::read(&src).unwrap();
            let mut f = fs::File::create(&copy_dst).unwrap();
            f.write_all(&bytes).unwrap();
            f.sync_all().unwrap();
        }
        unsafe { libc::sync() };
        let copy_delta = before_copy.saturating_sub(avail_bytes(dir.path()));

        let clone_dst = dir.path().join("clone.bin");
        let before_clone = avail_bytes(dir.path());
        clone_or_copy(&src, &clone_dst).unwrap();
        unsafe { libc::sync() };
        let clone_delta = before_clone.saturating_sub(avail_bytes(dir.path()));

        // (3) INCONCLUSIVE-skip guards — don't judge the clone if the
        // measurement environment can't even show the copy's own cost.
        if before_copy == 0 || before_clone == 0 {
            eprintln!("SKIP: statvfs returned 0 — free space unavailable");
            return;
        }
        if copy_delta <= size / 2 {
            eprintln!(
                "SKIP: volume too noisy/full — full copy showed only {copy_delta} B for a {size} B file"
            );
            return;
        }
        assert!(
            clone_delta < copy_delta / 4,
            "clone consumed {clone_delta} B vs copy {copy_delta} B — blocks were not shared"
        );
    }

    // ====== SLW-CO-3: suppressed clone entry point ==================
    // The two forward write sites route through the no-fsync clone writer
    // instead of the byte writers (SLW-LC-2 removed the unused fsync clone
    // sibling). It must preserve EVERY invariant the byte writers hold:
    // byte-identical dest via tmp→rename, the [dest, tmp] suppression
    // window (AC-5, no reverse echo), and tmp cleanup on a failed rename
    // — while landing bytes via `cow::clone_or_copy` instead of
    // `write_all` (AC-1).

    /// AC-1/AC-3: the no-fsync clone entry point lands a byte-identical
    /// dest. Atomicity comes from tmp→rename, not fsync, so dropping the
    /// flush must not change observable correctness.
    #[test]
    fn clone_tmp_rename_suppressed_no_fsync_lands_byte_identical() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let source = dir.path().join("source.bin");
        let dest = dir.path().join("dest.bin");
        let payload = b"SLW-CO-3 no-fsync clone must land byte-identical content";
        fs::write(&source, payload).unwrap();

        clone_tmp_rename_suppressed_no_fsync(&dest, &source, &handle).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert_eq!(hash_file(&source).unwrap(), hash_file(&dest).unwrap());
    }

    /// AC-5 (no reverse echo): a clone write registers the SAME `[dest, tmp]`
    /// suppression window as the byte writer, so a kernel-delayed watcher
    /// event for `dest` is dropped within the grace window and resumes
    /// after it. This is the unit-level proof that routing the forward
    /// write through the clone does not open a reverse-echo hole. Mirrors
    /// `write_tmp_fsync_rename_suppressed_keeps_dest_suppressed_within_grace`.
    #[test]
    fn clone_tmp_rename_suppressed_no_fsync_keeps_dest_suppressed_within_grace() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let source = dir.path().join("source.txt");
        let dest = dir.path().join("payload.txt");
        fs::write(&source, b"hello").unwrap();
        clone_tmp_rename_suppressed_no_fsync(&dest, &source, &handle).unwrap();

        let logged: Arc<std::sync::Mutex<Vec<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let logged_clone = Arc::clone(&logged);
        let handler: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
            logged_clone.lock().unwrap().push(p.to_path_buf());
        });
        dispatcher.register_handler(dir.path(), handler);

        dispatcher.dispatch(&dest);
        assert_eq!(
            logged.lock().unwrap().len(),
            0,
            "clone write must suppress dispatch of dest within the grace window (AC-5)"
        );

        std::thread::sleep(std::time::Duration::from_millis(200));
        dispatcher.dispatch(&dest);
        assert_eq!(
            logged.lock().unwrap().len(),
            1,
            "dispatch must resume once the grace window passes"
        );
    }

    /// AC-1 (crash-atomic tmp→rename): a failed rename leaves the
    /// pre-existing dest untouched and no `.tmp.` sibling behind — the
    /// clone path inherits `write_to_tmp_then_rename`'s cleanup contract.
    /// Force the rename to fail by pointing dest at a non-empty directory.
    #[test]
    fn clone_tmp_rename_suppressed_no_fsync_cleans_tmp_and_spares_dest_on_failure() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 100));
        let handle = dispatcher.suppression_handle();

        let source = dir.path().join("source.bin");
        fs::write(&source, b"would-overwrite").unwrap();

        let dest = dir.path().join("blocked");
        fs::create_dir(&dest).unwrap();
        let sentinel = dest.join("sentinel.txt");
        fs::write(&sentinel, b"sentinel-bytes").unwrap();

        let result = clone_tmp_rename_suppressed_no_fsync(&dest, &source, &handle);
        assert!(result.is_err(), "rename onto a non-empty dir must fail");
        assert!(dest.is_dir(), "dest must remain a directory");
        assert_eq!(
            fs::read(&sentinel).unwrap(),
            b"sentinel-bytes",
            "pre-existing content under dest must be byte-identical after a failed clone write"
        );

        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "tmp files must be cleaned up after a failed clone write: {:?}",
            leftover.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }
}
