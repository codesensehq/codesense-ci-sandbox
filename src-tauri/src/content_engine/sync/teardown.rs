// SPDX-License-Identifier: MIT
//! Whole-repo teardown — the inverse of attach/materialize (SS-FU-17).
//!
//! Attach auto-materializes a repo's resolved content; detach must undo it.
//! [`teardown_repo`] is that inverse: walk the repo's v3 lockfile, remove
//! every materialized file that is still byte-identical to what codesense
//! wrote (hash-guarded so local edits are never destroyed), strip the
//! codesense block from `.git/info/exclude`, then delete the lockfile.
//!
//! ## Reuse, not reinvention
//!
//! The per-file hash-guard + race-tolerant unlink is **not** duplicated
//! here — it is the shared [`super::materialize::verify_and_remove_dest`]
//! helper, the same code the projection materialize's source-delete branch runs.
//! Teardown is a thin orchestrator over that primitive plus
//! [`super::super::git_exclude`] and lockfile deletion.
//!
//! ## Failure model (SS-FU-17 AC-1 / AC-2 coupling)
//!
//! - **Per-entry isolation.** A failure removing one file is recorded in
//!   `TeardownReport::errors` and the walk continues — never panic, never
//!   abort the whole teardown over one bad entry.
//! - **Soft vs hard.** A locally-modified file (skipped, preserved) and an
//!   exclude-strip hiccup are *soft* — teardown still completes and the
//!   caller may drop the attachment record. An unreadable lockfile (can't
//!   enumerate what to remove) or a lockfile that won't delete (the exact
//!   phantom that dispatches dead-path waves) are *hard* — they propagate
//!   as `Err` so the caller keeps the attachment record and the user can
//!   retry. The file pass is idempotent, so a retry is always safe.
//! - **Idempotent.** A second teardown (lockfile already gone) is an
//!   `Ok(empty report)` no-op.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use log::warn;
use serde::{Deserialize, Serialize};

use super::super::git_exclude;
use super::super::layers::{is_safe_logical_name, ResolverContext};
use super::super::lockfile::reconcile_stale_slug_lockfile;
use super::super::Error;
use super::materialize::{verify_and_remove_dest, RemoveDisposition};
use super::wave::WaveCoordinator;
use crate::domain::SourceRepoSlug;

/// Outcome of a [`teardown_repo`] / [`teardown_repo_blocking`] call.
///
/// Surfaced to the UI (AC-2) so a detach can report which files it left in
/// place. `removed` / `skipped_locally_modified` carry logical names;
/// `errors` carries human-readable per-entry/stage lines for logs + toast.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeardownReport {
    /// Logical names whose materialized file was removed (or was already
    /// absent — same post-condition).
    pub removed: Vec<String>,
    /// Logical names left in place because the file diverged from
    /// codesense's last write (user edit / non-regular / raced). The
    /// lockfile entry is still dropped; the file becomes a plain untracked
    /// file the user owns.
    pub skipped_locally_modified: Vec<String>,
    /// Human-readable per-entry or per-stage failures that did not abort
    /// the teardown (per-entry I/O errors, exclude-strip hiccups).
    pub errors: Vec<String>,
}

/// Synchronous teardown core. Reads the v3 lockfile at `lockfile_path`,
/// hash-guards + removes each materialized file under `code_repo_path`,
/// strips the codesense `.git/info/exclude` block, then deletes the
/// lockfile. Pure blocking I/O — no async, no locks; the caller
/// ([`teardown_repo`]) owns the per-repo lock + `spawn_blocking`.
///
/// Returns `Ok(report)` on a completed teardown (including the idempotent
/// "lockfile already gone" no-op). Returns `Err` only on a **hard**
/// failure that must keep the caller's attachment record: the lockfile
/// could not be read (can't enumerate) or could not be deleted (would
/// leave a wave-dispatching phantom).
pub fn teardown_repo_blocking(
    ctx: &ResolverContext,
    code_repo_path: &Path,
) -> Result<TeardownReport, Error> {
    let mut report = TeardownReport::default();

    // SW-XS-8 (review r2): the workspace is going — drop the merged-file
    // excludes XS-14 left in the clone's SHARED block for it (it may never
    // have relaunched to move them into its own block).
    crate::agents::prepare::record::release_on_teardown(
        &ctx.codesense_home,
        &ctx.principal(),
        code_repo_path,
    );

    // Read the install-state from the store (SW-IS-3b: the sole writer); `read_through`
    // cold-imports a surviving JSON lockfile on a store miss until 3c. Absent → nothing
    // to tear down (idempotent no-op: a second detach, or a detach of a
    // never-materialized repo). A store-open failure or read error is HARD: we cannot
    // enumerate what to remove, so propagate and let the caller keep the attachment
    // record for retry rather than orphaning files under a now-forgotten install.
    let store = crate::content_engine::lockfile::open_sync_state_store(&ctx.codesense_home)
        .ok_or_else(|| Error::Io(io::Error::other("install store unavailable")))?;
    let lockfile =
        match crate::content_engine::sync_state_store::read_through(&store, ctx, code_repo_path) {
            Ok(Some(lf)) => lf,
            Ok(None) => return Ok(report),
            Err(e) => {
                return Err(Error::Io(io::Error::other(format!(
                    "install store read: {e}"
                ))))
            }
        };

    // Per-entry hash-guarded removal. Per-entry isolation: a hard error on
    // one entry is recorded and the walk continues.
    //
    // Trust model (matches the projection materialize): the **validated**
    // `logical_name` is the source of truth and the dest is recomputed as
    // `code_repo_path.join(logical_name)`. The lockfile's stored `dest_path`
    // is informational and is NOT trusted — a corrupt / foreign / mis-rebased
    // lockfile must never delete a file outside this repo, and
    // `is_safe_logical_name` rejects `..` / absolute / NUL segments before
    // the join. `seen` guards against drift where one logical_name appears
    // twice (which would otherwise double-count the report).
    let mut seen: HashSet<&str> = HashSet::new();
    for entry in &lockfile.entries {
        if !seen.insert(entry.logical_name.as_str()) {
            continue; // duplicate logical_name — first occurrence already handled
        }
        if !is_safe_logical_name(&entry.logical_name) {
            let msg = format!("{}: unsafe logical_name — skipped", entry.logical_name);
            warn!(target: "content_engine::sync::teardown", "{}", msg);
            report.errors.push(msg);
            continue;
        }
        let dest = code_repo_path.join(&entry.logical_name);
        match verify_and_remove_dest(&dest, entry) {
            Ok(RemoveDisposition::Removed) => report.removed.push(entry.logical_name.clone()),
            Ok(RemoveDisposition::SkippedLocallyModified) => report
                .skipped_locally_modified
                .push(entry.logical_name.clone()),
            Err(e) => {
                let msg = format!("{}: {}", entry.logical_name, e);
                warn!(
                    target: "content_engine::sync::teardown",
                    "teardown entry failed: {}", msg
                );
                report.errors.push(msg);
            }
        }
    }

    // Strip the entire codesense-managed block from `.git/info/exclude`.
    // Read the live managed entries (resilient to drift between the
    // lockfile and the exclude block) and remove them in one pass. A
    // failure here is *soft*: the materialized files are the load-bearing
    // teardown; a lingering exclude block is cosmetic and re-stripped on
    // the next teardown.
    match git_exclude::read_managed_entries(code_repo_path) {
        Ok(managed) if !managed.is_empty() => {
            let refs: Vec<&str> = managed.iter().map(String::as_str).collect();
            if let Err(e) = git_exclude::remove_entries(code_repo_path, &refs) {
                let msg = format!(".git/info/exclude strip failed: {}", e);
                warn!(target: "content_engine::sync::teardown", "{}", msg);
                report.errors.push(msg);
            }
        }
        Ok(_) => {} // no managed block — nothing to strip.
        Err(e) => {
            let msg = format!(".git/info/exclude read failed: {}", e);
            warn!(target: "content_engine::sync::teardown", "{}", msg);
            report.errors.push(msg);
        }
    }

    // SW-IS-3c: the store is the sole substrate — the install's rows are dropped by
    // the caller ([`teardown_repo`]) via `delete_all_for_path`. There is no longer a
    // JSON lockfile to delete here.
    Ok(report)
}

/// Run a blocking lifecycle op `f` under `code_repo_path`'s per-repo lock —
/// the **same** lock the wave takes, so the op can't interleave with an
/// in-flight materialize wave on this repo — and on the blocking pool, so it
/// never stalls the async executor or the UI. The guard is held across the
/// `spawn_blocking` await (mirroring the wave's own lock-under-spawn_blocking
/// discipline). Shared by [`teardown_repo`] and [`reconcile_reattach`].
async fn with_repo_lock_blocking<T, F>(
    coord: &WaveCoordinator,
    code_repo_path: &Path,
    f: F,
) -> Result<T, Error>
where
    F: FnOnce() -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    let _repo_guard = coord.acquire_repo_lock(code_repo_path).await;
    tokio::task::spawn_blocking(f).await.map_err(|join_err| {
        Error::Io(io::Error::other(format!(
            "spawn_blocking join error: {join_err}"
        )))
    })?
}

/// Whole-repo teardown — the user-driven detach/uninstall verb (AC-1).
///
/// Runs the blocking teardown for `(ctx, code_repo_path)` under the per-repo
/// lock on the blocking pool (see [`with_repo_lock_blocking`]).
///
/// See [`teardown_repo_blocking`] for the file/exclude/lockfile semantics
/// and the soft-vs-hard failure model.
pub async fn teardown_repo(
    coord: &WaveCoordinator,
    ctx: &ResolverContext,
    code_repo_path: &Path,
) -> Result<TeardownReport, Error> {
    let code_repo_path_buf = code_repo_path.to_path_buf();
    // SW-IS-3c: the store is the SOLE substrate, so the teardown reads install-state from
    // it (enumerate) and drops this worktree's rows — owned `ctx` is moved into the
    // blocking closure so both the store read and the SQLite delete run on the blocking
    // pool inside the per-repo lock.
    let ctx_owned = ctx.clone();
    let codesense_home = ctx.codesense_home.clone();
    with_repo_lock_blocking(coord, code_repo_path, move || {
        let report = teardown_repo_blocking(&ctx_owned, &code_repo_path_buf)?;
        // Best-effort: remove EVERY team's row at this worktree path (the composite key
        // can own one row per team). An unavailable store or a delete failure is
        // warn-logged, not fatal — the file teardown already succeeded, and a lingering
        // store row is a rebuildable-cache artifact.
        if let Some(store) = crate::content_engine::lockfile::open_sync_state_store(&codesense_home)
        {
            if let Err(e) = store.delete_all_for_path(&code_repo_path_buf) {
                warn!(
                    target: "content_engine::sync::teardown",
                    "teardown: install-store delete failed for {} (store rows leak persist): {}",
                    code_repo_path_buf.display(), e
                );
            }
        } else {
            warn!(
                target: "content_engine::sync::teardown",
                "teardown: install store unavailable for {} — store rows not pruned",
                code_repo_path_buf.display()
            );
        }
        Ok(report)
    })
    .await
}

/// SS-FU-17 AC-8: async wrapper for [`reconcile_stale_slug_lockfile`].
///
/// Runs under the **new path's** per-repo lock on the blocking pool (see
/// [`with_repo_lock_blocking`]) — the same lock a materialize wave takes, so
/// reconcile and a wave on this path are mutually exclusive (they can't
/// interleave). They are not strictly *ordered*, but the
/// "new-path-already-has-a-lockfile wins" guard in
/// [`reconcile_stale_slug_lockfile`] makes a wave-runs-first race safe (no
/// clobber — worst case the baseline isn't carried forward and the next wave
/// rebuilds from empty). In the normal flow the caller awaits this before
/// triggering the attach wave, so reconcile lands first. Returns the
/// superseded stale lockfile path, if any.
pub async fn reconcile_reattach(
    coord: &WaveCoordinator,
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    new_code_repo_path: &Path,
) -> Result<Option<PathBuf>, Error> {
    let ctx = ctx.clone();
    let slug = slug.clone();
    let new_path = new_code_repo_path.to_path_buf();
    with_repo_lock_blocking(coord, new_code_repo_path, move || {
        reconcile_stale_slug_lockfile(&ctx, &slug, &new_path)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_engine::layers::LayerId;
    use crate::content_engine::lockfile::{Lockfile, MaterializedEntry};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init(dir: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(dir)
            .status()
            .expect("git init");
        assert!(status.success());
    }

    /// Build a lockfile entry for a file at `code_repo/<logical>` whose
    /// recorded hash is the blake3 of `recorded_content`. `mtime_ns` is
    /// pinned to `0` so the mtime fast-gate always drifts and the
    /// classifier falls to the hash compare — the test then controls the
    /// clean-vs-modified outcome purely via whether `recorded_content`
    /// matches what is on disk. Post-SW-CP-31b the store persists only
    /// projection entries (`projection_key = Some`), so the fixture carries a
    /// projection key and survives the load-side drain teardown reads through.
    fn entry(code_repo: &Path, logical: &str, recorded_content: &[u8]) -> MaterializedEntry {
        MaterializedEntry {
            last_materialized_size: None,
            logical_name: logical.to_string(),
            dest_path: code_repo.join(logical),
            source_layer: LayerId::TeamProjection,
            source_path: code_repo.join(".source").join(logical),
            last_materialized_hash: *blake3::hash(recorded_content).as_bytes(),
            last_materialized_mtime_ns: 0,
            projection_key: Some("proj".to_string()),
            source_oid: None,
        }
    }

    /// SW-IS-3c: the store is the sole install-state substrate. Seed the
    /// install target into the store under the `td_ctx` identity so
    /// `read_through` (now store-only) serves it to teardown. `home` is the
    /// codesense home the store lives under.
    fn seed_store(home: &Path, code_repo: &Path, entries: Vec<MaterializedEntry>) {
        let lf = Lockfile {
            schema_version: crate::content_engine::lockfile::SCHEMA_VERSION,
            org_slug: "acme/team-context".to_string(),
            team_name: Some("default".to_string()),
            code_repo_path: code_repo.to_path_buf(),
            entries,
        };
        let store = crate::content_engine::sync_state_store::SyncStateStore::open(
            crate::content_engine::sync_state_store::default_store_path(home),
        )
        .expect("open install store");
        store
            .upsert(42, &td_slug(), &lf)
            .expect("seed install store");
    }

    /// A `ResolverContext` whose identity axes (`org_slug` / `team_name`) match
    /// what [`write_lockfile`] stamps, so `read_through`'s identity guard admits
    /// the cold-imported JSON lockfile.
    fn td_ctx(home: &Path) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: "acme/team-context".to_string(),
            team_name: Some("default".to_string()),
        }
    }

    fn td_slug() -> SourceRepoSlug {
        SourceRepoSlug::new("acme", "team-context").unwrap()
    }

    /// A repo with a git working tree, a materialized file, a matching
    /// lockfile, and a codesense exclude block. Returns (tmp, code_repo,
    /// ctx, slug, lockfile_path).
    fn fixture() -> (TempDir, PathBuf, ResolverContext, SourceRepoSlug) {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        git_init(&code_repo);

        // A clean materialized file: bytes on disk match the recorded hash.
        fs::write(code_repo.join("skill.md"), b"codesense wrote this").unwrap();
        git_exclude::add_entries(&code_repo, &["skill.md"]).unwrap();

        let ctx = td_ctx(tmp.path());
        let slug = td_slug();
        seed_store(
            tmp.path(),
            &code_repo,
            vec![entry(&code_repo, "skill.md", b"codesense wrote this")],
        );
        (tmp, code_repo, ctx, slug)
    }

    #[test]
    fn removes_clean_entry_strips_exclude() {
        let (_tmp, code_repo, ctx, _slug) = fixture();

        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        assert_eq!(report.removed, vec!["skill.md".to_string()]);
        assert!(report.skipped_locally_modified.is_empty());
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            !code_repo.join("skill.md").exists(),
            "file should be removed"
        );
        assert!(
            git_exclude::read_managed_entries(&code_repo)
                .unwrap()
                .is_empty(),
            "exclude block should be stripped"
        );
    }

    #[test]
    fn skips_locally_modified_file_and_records_it() {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        git_init(&code_repo);

        // On-disk bytes diverge from the recorded hash → locally modified.
        fs::write(code_repo.join("edited.md"), b"USER EDITED THIS").unwrap();
        git_exclude::add_entries(&code_repo, &["edited.md"]).unwrap();
        let ctx = td_ctx(tmp.path());
        seed_store(
            tmp.path(),
            &code_repo,
            vec![entry(&code_repo, "edited.md", b"original codesense bytes")],
        );

        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        assert!(report.removed.is_empty());
        assert_eq!(
            report.skipped_locally_modified,
            vec!["edited.md".to_string()]
        );
        assert!(
            code_repo.join("edited.md").exists(),
            "locally-modified file must be preserved"
        );
        assert_eq!(
            fs::read(code_repo.join("edited.md")).unwrap(),
            b"USER EDITED THIS",
            "preserved file bytes must be untouched"
        );
    }

    #[test]
    fn mixed_clean_and_modified_partitions_report() {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        git_init(&code_repo);

        fs::write(code_repo.join("clean.md"), b"clean bytes").unwrap();
        fs::write(code_repo.join("dirty.md"), b"user changed").unwrap();
        git_exclude::add_entries(&code_repo, &["clean.md", "dirty.md"]).unwrap();
        let ctx = td_ctx(tmp.path());
        seed_store(
            tmp.path(),
            &code_repo,
            vec![
                entry(&code_repo, "clean.md", b"clean bytes"),
                entry(&code_repo, "dirty.md", b"original bytes"),
            ],
        );

        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        assert_eq!(report.removed, vec!["clean.md".to_string()]);
        assert_eq!(
            report.skipped_locally_modified,
            vec!["dirty.md".to_string()]
        );
        assert!(!code_repo.join("clean.md").exists());
        assert!(code_repo.join("dirty.md").exists());
    }

    /// SW-CP-31b: a legacy install row (`projection_key = None` on an install
    /// layer) is drained on store read, so teardown — which reads through the
    /// store via `read_through` — never sees it and never touches its dest. A
    /// projection entry (`projection_key = Some`) at a DIFFERENT dest survives
    /// the drain and IS torn down. Proves the drain seam holds through teardown.
    #[test]
    fn teardown_never_sees_drained_legacy_install_row() {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        git_init(&code_repo);

        // A legacy install row: projection_key = None on an install layer — exactly
        // the shape the load-side drain drops on read.
        let install_row = |logical: &str, recorded_content: &[u8]| {
            let mut e = entry(&code_repo, logical, recorded_content);
            e.projection_key = None;
            e.source_layer = LayerId::TeamProjection;
            e
        };

        // Both files clean on disk (bytes match recorded hash), both excluded.
        fs::write(code_repo.join("legacy.md"), b"legacy install bytes").unwrap();
        fs::write(code_repo.join("projected.md"), b"projected bytes").unwrap();
        git_exclude::add_entries(&code_repo, &["legacy.md", "projected.md"]).unwrap();
        let ctx = td_ctx(tmp.path());
        seed_store(
            tmp.path(),
            &code_repo,
            vec![
                install_row("legacy.md", b"legacy install bytes"),
                entry(&code_repo, "projected.md", b"projected bytes"),
            ],
        );
        // SW-XS-8: the live writer only writes `TeamProjection`; tag the row with
        // a retired install tier the way a pre-XS-8 binary wrote it.
        rusqlite::Connection::open(
            crate::content_engine::sync_state_store::default_store_path(tmp.path()),
        )
        .unwrap()
        .execute(
            "UPDATE materialized_entry SET source_layer = 'TeamRepo' WHERE logical_name = 'legacy.md'",
            [],
        )
        .unwrap();

        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        // The projection entry is torn down; the drained install row is not — teardown
        // never saw it because `read_through` dropped it on decode.
        assert_eq!(
            report.removed,
            vec!["projected.md".to_string()],
            "only the projection entry is removed; the drained install row is invisible to teardown"
        );
        assert!(
            !report.removed.contains(&"legacy.md".to_string()),
            "the drained legacy install row is NOT in the removal report"
        );
        assert!(
            !code_repo.join("projected.md").exists(),
            "projection dest is removed"
        );
        assert!(
            code_repo.join("legacy.md").exists(),
            "the drained install row's dest is UNTOUCHED — teardown never enumerated it"
        );
        assert_eq!(
            fs::read(code_repo.join("legacy.md")).unwrap(),
            b"legacy install bytes",
            "the untouched dest's bytes are intact"
        );
    }

    #[test]
    fn second_teardown_is_idempotent_noop() {
        let (_tmp, code_repo, ctx, _slug) = fixture();

        teardown_repo_blocking(&ctx, &code_repo).unwrap();
        // Mirror the store-row prune the async `teardown_repo` runs after the
        // blocking core (via `delete_all_for_path`): the blocking core leaves the
        // store rows for its async caller to drop. Without this, the second call
        // would read the still-present store row rather than the "install already
        // gone" state the idempotency contract models.
        crate::content_engine::lockfile::open_sync_state_store(&ctx.codesense_home)
            .unwrap()
            .delete_all_for_path(&code_repo)
            .unwrap();
        // Second call: install already gone (store empty) → empty report, no error.
        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        assert!(report.removed.is_empty());
        assert!(report.skipped_locally_modified.is_empty());
        assert!(report.errors.is_empty());
    }

    #[test]
    fn missing_lockfile_is_ok_empty() {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        let ctx = td_ctx(tmp.path());
        // The store is empty and no JSON lockfile exists → `read_through` returns
        // `Ok(None)` → empty report.
        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();
        assert_eq!(report, TeardownReport::default());
    }

    /// Review H1: teardown recomputes dest from the validated `logical_name`
    /// and does NOT trust the lockfile's stored `dest_path` — a corrupt /
    /// foreign `dest_path` pointing outside the repo must never be deleted.
    #[test]
    fn teardown_ignores_stored_dest_path_and_uses_logical_name() {
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(&code_repo).unwrap();
        git_init(&code_repo);

        let content = b"codesense bytes";
        fs::write(code_repo.join("skill.md"), content).unwrap();

        // A decoy OUTSIDE the repo holding the same bytes (so the hash guard
        // would match if teardown were to trust the stored dest_path).
        let outside = tmp.path().join("outside-decoy.md");
        fs::write(&outside, content).unwrap();

        let ctx = td_ctx(tmp.path());
        let mut e = entry(&code_repo, "skill.md", content);
        e.dest_path = outside.clone(); // poisoned stored dest_path
        seed_store(tmp.path(), &code_repo, vec![e]);

        let report = teardown_repo_blocking(&ctx, &code_repo).unwrap();

        assert_eq!(report.removed, vec!["skill.md".to_string()]);
        assert!(
            !code_repo.join("skill.md").exists(),
            "the in-repo file (recomputed from logical_name) is removed"
        );
        assert!(
            outside.exists(),
            "a file outside the repo (stored dest_path) must NOT be touched"
        );
    }
}
