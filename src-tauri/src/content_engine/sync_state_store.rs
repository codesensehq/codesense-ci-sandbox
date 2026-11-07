// SPDX-License-Identifier: MIT
//! `SyncStateStore` — the central SQLite store for per-worktree sync state (the
//! materialized-entry ledger). Persisted via the legacy
//! `install_target` schema, whose on-disk names are kept to avoid a DB migration.
//!
//! Today each install target carries its own JSON lockfile
//! ([`super::lockfile`]); the cross-worktree operations (enumerate attached
//! repos, which teams materialized a repo, boot-time prune) are filesystem walks
//! that open and fully parse every lockfile. This store holds the same state in
//! one indexed database so "what is installed / which worktrees are orphaned"
//! becomes a single `SELECT` instead of a tree-walk, and concurrent setup stops
//! fsync-storming N separate files.
//!
//! The lockfile is a **cache, not a document**: every [`MaterializedEntry`] field
//! is recomputable from disk plus the layer source. So durability is not load-bearing — WAL plus
//! `synchronous=NORMAL` (the shared [`crate::shared::sqlite`] plumbing) is
//! sufficient, exactly like the automation coordinator.
//!
//! SW-IS-3a wires this store into the install-state **readers** (the reaper's
//! enumerate + `lockfile_mtime`, and `materialized_teams_for_repo`'s siblings):
//! each reads the store's indexed SELECT unioned with a JSON-completeness
//! fallback, primed by a one-time backfill and kept current by a best-effort
//! dual-write at the end-of-wave persist. The JSON lockfile ([`super::lockfile`])
//! remains the durable truth this slice — the writer repoint and JSON retirement
//! land in SW-IS-3b / 3c.
//!
//! ## Blocking boundary
//!
//! Every method is a plain synchronous `fn` that opens a connection and may
//! `busy_timeout`-wait, exactly like the automation store. It is **not** `async`
//! and must never be called from the async runtime directly — a consumer wraps
//! its calls in the command layer's `spawn_blocking` / `await_blocking` boundary
//! so the DB work runs on the blocking pool, never on the UI thread.

use crate::shared::sqlite::{append_suffix, quarantine_corrupt_db};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

mod selected;

use crate::domain::SourceRepoSlug;

use super::layers::{LayerId, ResolverContext};
use super::lockfile::{
    read_slug_lockfiles, Lockfile, MaterializedEntry, WireLayerId, RETIRED_INSTALL_LAYERS,
    SCHEMA_VERSION,
};

#[derive(Debug, Clone)]
pub struct PendingDestinationEffect {
    pub operation_id: uuid::Uuid,
    pub intent: serde_json::Value,
    pub newly_prepared: bool,
}

/// The install-state schema. Two-level: an install `target` (one row) fans out to
/// its materialized `entries` (N rows) and `applied packs` (N rows).
///
/// `install_target.id` is a surrogate primary key — the stable internal identity
/// the child foreign keys point at — while `(org_slug, team_name, code_repo_path)`
/// is the mutable natural key: the same identity the JSON lockfile keys on
/// (`lockfile_path` puts the team segment in the path), so the *same* worktree
/// path can hold distinct per-team materialize state. `team_name` stores the
/// `_none` sentinel (never NULL) for the no-team context — a NULL there would be
/// `UNIQUE`-distinct in SQLite and break `ON CONFLICT` for no-team rows (see
/// [`team_name_to_db`]). Three indexes back the query surface: the composite
/// `UNIQUE(org_slug, team_name, code_repo_path)` auto-index serves every by-target
/// read (full-key seek) and the `upsert` `ON CONFLICT`, and its `(org_slug,
/// team_name)` prefix serves the per-team enumerate; `idx_install_target_org_team_slug`
/// `(org_slug, team_name, source_repo_slug)` serves the per-slug enumerate (whose
/// `source_repo_slug` filter the `UNIQUE` cannot seek — its third column is
/// `code_repo_path`); and `idx_install_target_code_repo_path` backs the reaper's
/// path-level `MAX(updated_at)` (which spans a path's per-team rows). `source_repo_slug`
/// is the source repo's identity (owner/repo/provider/host) stored as its lossless
/// serde JSON, since the lockfile derives it from the on-disk path rather than
/// storing it.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS install_target (
    id               INTEGER PRIMARY KEY,
    code_repo_path   TEXT,
    destination_kind TEXT NOT NULL DEFAULT 'local',
    destination_identity TEXT,
    org_slug         TEXT NOT NULL,
    team_name        TEXT NOT NULL,
    source_repo_slug TEXT NOT NULL,
    user_id          INTEGER NOT NULL,
    updated_at       INTEGER,
    UNIQUE(org_slug, team_name, code_repo_path),
    CHECK ((destination_kind = 'local' AND code_repo_path IS NOT NULL AND destination_identity IS NULL)
        OR (destination_kind = 'remote' AND code_repo_path IS NULL AND destination_identity IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_install_target_remote_identity
    ON install_target(org_slug, team_name, source_repo_slug, destination_identity)
    WHERE destination_kind = 'remote';
CREATE INDEX IF NOT EXISTS idx_install_target_org_team_slug
    ON install_target(org_slug, team_name, source_repo_slug);
CREATE INDEX IF NOT EXISTS idx_install_target_code_repo_path
    ON install_target(code_repo_path);

CREATE TABLE IF NOT EXISTS materialized_entry (
    target_id                  INTEGER NOT NULL
                                   REFERENCES install_target(id) ON DELETE CASCADE,
    logical_name               TEXT NOT NULL,
    dest_path                  TEXT NOT NULL,
    source_layer               TEXT NOT NULL,
    source_path                TEXT NOT NULL,
    last_materialized_hash     BLOB NOT NULL,
    last_materialized_mtime_ns BLOB NOT NULL,
    -- A lockfile entry's identity is (logical_name, projection_key), NOT logical_name
    -- alone: a wave (install) entry and a story_linker projection entry legitimately
    -- share a `logical_name` (the SW-FX-14 fold preserves both). `projection_key` is
    -- therefore part of the PK — and NOT NULL, storing the empty-string sentinel for a
    -- wave entry's `None` (SQLite treats NULLs as distinct in a key, so a NULL here
    -- would let two wave entries sharing a logical_name coexist). See [`projection_key_to_db`].
    projection_key             TEXT NOT NULL,
    source_oid                 TEXT,
    last_materialized_size     INTEGER,
    -- The composite PK indexes `target_id` as its leftmost column, so the
    -- per-target read and the `ON DELETE CASCADE` from `install_target` are
    -- index-backed without a separate index on the FK column.
    PRIMARY KEY (target_id, logical_name, projection_key)
);

-- SW-IS-3c dual-slug (SW-EW-6): a monorepo clone attached under two owner/repo
-- slugs shares ONE materialized target (same path, same entries) but must
-- enumerate under BOTH slugs. The single `install_target.source_repo_slug`
-- column can hold only the most-recent primary, so slug aliases are a set here.
-- Per-slug enumerate JOINs through this table; a whole-worktree delete cascades.
--
-- INVARIANT (the per-slug/per-team INNER JOINs in `enumerate_for_slug`,
-- `enumerate_for_team`, and `teams_for_materialized_path` rely on it): every
-- `install_target` has >=1 `target_slug` row. It is seeded by `upsert` (its alias
-- insert), by `register_or_alias`, and by the v4->v5 backfill (whose `INSERT ... SELECT`
-- reads `install_target.source_repo_slug`, which is `NOT NULL`, so it always produces
-- a row). A target with zero aliases would be invisible to those readers; no write
-- path ever leaves one.
CREATE TABLE IF NOT EXISTS target_slug (
    target_id        INTEGER NOT NULL REFERENCES install_target(id) ON DELETE CASCADE,
    source_repo_slug TEXT NOT NULL,
    PRIMARY KEY (target_id, source_repo_slug)
);
CREATE INDEX IF NOT EXISTS idx_target_slug_slug ON target_slug(source_repo_slug);
CREATE INDEX IF NOT EXISTS idx_target_slug_owner ON target_slug(json_extract(source_repo_slug, '$.owner'));

-- Intent durability is separate from applied baselines. An uncertain remote
-- effect retains its immutable UUID until reconciliation and state persistence.
CREATE TABLE IF NOT EXISTS destination_effect (
    user_id INTEGER NOT NULL,
    org_slug TEXT NOT NULL,
    team_name TEXT NOT NULL,
    source_repo_slug TEXT NOT NULL,
    destination_identity TEXT NOT NULL,
    effect_key TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    intent TEXT NOT NULL,
    PRIMARY KEY(user_id, org_slug, team_name, source_repo_slug, destination_identity, effect_key)
);

CREATE TABLE IF NOT EXISTS context_lifecycle (
    destination_identity TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK(generation > 0)
);
CREATE TABLE IF NOT EXISTS context_operation_path (
    destination_identity TEXT NOT NULL,
    path TEXT NOT NULL,
    authority TEXT NOT NULL,
    request_id TEXT NOT NULL,
    PRIMARY KEY(destination_identity,path)
);
CREATE TABLE IF NOT EXISTS context_source_binding (
    authority TEXT NOT NULL,
    destination_identity TEXT NOT NULL,
    path TEXT NOT NULL,
    binding TEXT NOT NULL,
    PRIMARY KEY(authority,destination_identity,path)
);
CREATE TABLE IF NOT EXISTS context_operation (
    authority TEXT NOT NULL,
    destination_identity TEXT NOT NULL,
    request_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    envelope TEXT NOT NULL,
    PRIMARY KEY(authority, destination_identity, request_id)
);
CREATE TABLE IF NOT EXISTS context_operation_progress (
    authority TEXT NOT NULL,
    destination_identity TEXT NOT NULL,
    request_id TEXT NOT NULL,
    identity TEXT NOT NULL,
    progress TEXT,
    PRIMARY KEY(authority, destination_identity, request_id),
    FOREIGN KEY(authority, destination_identity, request_id)
      REFERENCES context_operation(authority, destination_identity, request_id)
);
-- Compatibility barrier for already-open pre-v11 connections. They must not
-- admit unreadable progress or overwrite immutable wrapped intent. Historical
-- raw rows remain honestly current through their original full-envelope writes.
CREATE TRIGGER IF NOT EXISTS context_operation_require_wrapper
BEFORE INSERT ON context_operation
WHEN coalesce(json_extract(NEW.envelope,'$.format') IN ('selected-context-progress-v1','selected-context-progress-v2'),0)=0
  OR coalesce(json_type(NEW.envelope,'$.intent')='object',0)=0
  OR (SELECT count(*) FROM json_each(NEW.envelope)) != 2
BEGIN SELECT RAISE(ABORT,'selected context requires a compatible writer'); END;
CREATE TRIGGER IF NOT EXISTS context_operation_immutable_wrapper
BEFORE UPDATE ON context_operation
WHEN json_type(OLD.envelope,'$.format') IS NOT NULL
  OR json_type(NEW.envelope,'$.format') IS NOT NULL
BEGIN SELECT RAISE(ABORT,'selected context wrapped intent is immutable'); END;
CREATE TRIGGER IF NOT EXISTS context_progress_wrapped_insert
BEFORE INSERT ON context_operation_progress
WHEN NOT EXISTS(SELECT 1 FROM context_operation o
  WHERE o.authority=NEW.authority AND o.destination_identity=NEW.destination_identity AND o.request_id=NEW.request_id
    AND json_extract(o.envelope,'$.format') IN ('selected-context-progress-v1','selected-context-progress-v2'))
BEGIN SELECT RAISE(ABORT,'selected progress requires matching wrapped intent'); END;
-- INSERT proved parent representation; the parent cannot change afterward.
-- Do not parse its potentially MiB-sized JSON again on every progress update.
CREATE TRIGGER IF NOT EXISTS context_progress_wrapped_update
BEFORE UPDATE ON context_operation_progress
WHEN NEW.authority != OLD.authority OR NEW.destination_identity != OLD.destination_identity
  OR NEW.request_id != OLD.request_id OR NEW.identity != OLD.identity
BEGIN SELECT RAISE(ABORT,'selected progress identity is immutable'); END;

"#;

/// The store's own schema version, stamped into `PRAGMA user_version` on a fresh
/// open so a later migration has a version to branch on. Distinct from the
/// lockfile document's [`SCHEMA_VERSION`].
///
/// - v1 — the original two-level `install_target`/`materialized_entry`/`applied_pack`
///   schema.
/// - v2 — adds `install_target.updated_at` (per-target "last materialized"
///   recency), healed in field on `open` for a DB created at v1.
/// - v3 — the natural key becomes `UNIQUE(org_slug, team_name, code_repo_path)`
///   (was `UNIQUE(code_repo_path)`) so a path can hold per-team state, and
///   `team_name` stores the `_none` sentinel instead of NULL. SQLite can't alter a
///   `UNIQUE` in place and the store is a rebuildable cache, so a pre-v3 DB is
///   dropped + recreated and re-imported from the JSON truth (see [`try_open`]).
/// - v4 — the `materialized_entry` PK becomes `(target_id, logical_name,
///   projection_key)` (was `(target_id, logical_name)`) so a wave entry and a
///   projection entry that share a `logical_name` can coexist (the SW-FX-14 fold);
///   `projection_key` is NOT NULL, storing the empty-string sentinel for a wave
///   entry's `None`. Same drop + rebuild-from-JSON migration as v3.
/// - v5 — adds the `target_slug` alias table (SW-IS-3c dual-slug, SW-EW-6) so one
///   clone attached under multiple owner/repo slugs enumerates under each. This is
///   an ADDITIVE change (a new table backfilled from `install_target.source_repo_slug`),
///   so it heals in place on a v4 DB — the first exercise of `try_open`'s forward
///   rule (additive → heal, never drop+rebuild), no quarantine.
const STORE_SCHEMA_VERSION: i64 = 12;

/// Columns of `materialized_entry` in the order [`row_to_raw`] reads them. One
/// declaration shared by the full-target read and the single-entry lookup so the
/// two projections cannot drift.
const ENTRY_COLUMNS: &str = "logical_name, dest_path, source_layer, source_path, \
     last_materialized_hash, last_materialized_mtime_ns, projection_key, \
     source_oid, last_materialized_size";

/// A store operation failed. `Io`/`Sqlite` mirror the shared open plumbing's
/// error, `Json` covers stored JSON (de)serialization, `Data` flags
/// stored/supplied bytes that violate an invariant (a wrong-length blob, a
/// non-UTF-8 path key), and `NotFound` reports a referenced target that does not
/// exist — a distinct, benign caller condition, not corruption.
#[derive(Debug)]
pub enum SyncStateStoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Data(String),
    NotFound(String),
    /// The database opened but `PRAGMA quick_check` reported structural damage.
    /// Carries the first non-`ok` line quick_check returned. A recoverable
    /// condition [`SyncStateStore::open`] quarantines and rebuilds from, not a
    /// caller-facing error under normal operation.
    Corrupt(String),
}

impl std::fmt::Display for SyncStateStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncStateStoreError::Io(e) => write!(f, "install store io error: {e}"),
            SyncStateStoreError::Sqlite(e) => write!(f, "install store sqlite error: {e}"),
            SyncStateStoreError::Json(e) => write!(f, "install store json error: {e}"),
            SyncStateStoreError::Data(m) => write!(f, "install store data error: {m}"),
            SyncStateStoreError::NotFound(m) => write!(f, "install store target not found: {m}"),
            SyncStateStoreError::Corrupt(m) => {
                write!(f, "install store integrity check failed: {m}")
            }
        }
    }
}

impl std::error::Error for SyncStateStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SyncStateStoreError::Io(e) => Some(e),
            SyncStateStoreError::Sqlite(e) => Some(e),
            SyncStateStoreError::Json(e) => Some(e),
            SyncStateStoreError::Data(_)
            | SyncStateStoreError::NotFound(_)
            | SyncStateStoreError::Corrupt(_) => None,
        }
    }
}

impl From<std::io::Error> for SyncStateStoreError {
    fn from(e: std::io::Error) -> Self {
        SyncStateStoreError::Io(e)
    }
}

impl From<rusqlite::Error> for SyncStateStoreError {
    fn from(e: rusqlite::Error) -> Self {
        SyncStateStoreError::Sqlite(e)
    }
}

impl From<serde_json::Error> for SyncStateStoreError {
    fn from(e: serde_json::Error) -> Self {
        SyncStateStoreError::Json(e)
    }
}

impl From<crate::shared::sqlite::OpenError> for SyncStateStoreError {
    fn from(e: crate::shared::sqlite::OpenError) -> Self {
        match e {
            crate::shared::sqlite::OpenError::Io(e) => SyncStateStoreError::Io(e),
            crate::shared::sqlite::OpenError::Sqlite(e) => SyncStateStoreError::Sqlite(e),
        }
    }
}

type Result<T> = std::result::Result<T, SyncStateStoreError>;

/// The natural-key identity of one install target: the `(org_slug, team_name,
/// code_repo_path)` triple the store keys on — the same identity the JSON lockfile
/// path encodes, so the *same* worktree path can address distinct per-team state.
/// Borrowed (no allocation at the call site); `team_name` is the domain `Option`
/// (`None` = no-team), normalized to the `_none` sentinel at the SQL boundary via
/// [`team_name_to_db`]. Build one directly, or with [`TargetKey::from_ctx`] from a
/// [`ResolverContext`] + path.
#[derive(Debug, Clone, Copy)]
pub struct TargetKey<'a> {
    pub org_slug: &'a str,
    pub team_name: Option<&'a str>,
    pub code_repo_path: &'a Path,
}

impl<'a> TargetKey<'a> {
    /// The key for `code_repo_path` under the context's `(org_slug, team_name)` —
    /// the pairing the materialize path (and `read_through`) already resolves.
    pub fn from_ctx(ctx: &'a ResolverContext, code_repo_path: &'a Path) -> Self {
        TargetKey {
            org_slug: &ctx.org_slug,
            team_name: ctx.team_name.as_deref(),
            code_repo_path,
        }
    }

    /// The natural-key columns bound in `(org_slug, team_name, code_repo_path)`
    /// order for a `WHERE org_slug=?1 AND team_name=?2 AND code_repo_path=?3` seek:
    /// the `_none`-normalized team ([`team_name_to_db`]) and the raw-string path
    /// ([`path_to_str`], which rejects a non-UTF-8 path). The single place that maps
    /// the key to SQL params, so every by-target seek binds it identically.
    fn seek_params(&self) -> Result<(&str, &str, String)> {
        // The by-target read/delete seam mirrors `upsert`'s write-seam reservation:
        // a key literally naming team `_none` would seek/delete the no-team sentinel
        // row instead of a distinct target. Both seams route through the one check.
        reject_reserved_team(self.team_name)?;
        Ok((
            self.org_slug,
            team_name_to_db(self.team_name),
            path_to_str(self.code_repo_path)?,
        ))
    }
}

/// The default durable location for the install-state DB:
/// `{codesense_home}/install-state/install.sqlite3`. A rebuildable cache — never
/// the context repo — and a different file from the automation coordinator.
pub fn default_store_path(codesense_home: &Path) -> PathBuf {
    codesense_home.join("install-state").join("install.sqlite3")
}

/// The central install-state store. Cheap to `clone` (just the path); a fresh
/// connection is opened per operation, so a cloned handle shared across threads
/// is the intended concurrency model (WAL readers never block; writers wait on
/// `busy_timeout`).
#[derive(Debug, Clone)]
pub struct SyncStateStore {
    path: PathBuf,
}

/// One durable synthetic product owner at a canonical destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductRegistration {
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    pub slug: SourceRepoSlug,
}

impl SyncStateStore {
    /// Open (creating + stamping the schema on first use) a store at `path`.
    ///
    /// Local-only install metadata is a rebuildable cache, so a corrupt or non-SQLite
    /// file on disk is a recoverable condition, never a crash: it is quarantined
    /// (moved aside, not deleted) and replaced with a fresh empty store. A missing
    /// file is simply created. `open` never panics or propagates a corruption
    /// error that would abort app start. Recovery clears the one-time backfill marker so
    /// [`backfill_json_once`](Self::backfill_json_once) re-imports from any JSON that still
    /// survives (a corruption during the migration window, before the boot deleter ran);
    /// once the JSON is gone the rebuilt store simply stays empty and repopulates as
    /// worktrees reprovision. A second `open` on the now-healthy file is an ordinary open.
    /// Once remote state or pending UUIDs exist, a durable guard disables this
    /// empty-cache recovery: missing/corrupt/replaced state is preserved and refused.
    /// SW-XS-27: the store for a read-only caller, only when it exists: never
    /// created, migrated or quarantined (each query opens a connection; a
    /// corrupt file is a query error, left exactly where it is).
    pub fn open_existing(path: impl Into<PathBuf>) -> Option<Self> {
        let path = path.into();
        path.is_file().then_some(Self { path })
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        remote_guard_requires_existing_database(&path)?;
        match Self::try_open(&path) {
            Ok(store) => Ok(store),
            Err(e) if is_corruption(&e) => {
                if read_remote_guard(&path)?.is_some() {
                    return Err(SyncStateStoreError::Data(format!("remote effect journal is corrupt; preserve database and reconcile before recovery: {e}")));
                }
                // SW-IS-3c: post-JSON-retirement the store is authoritative and has no
                // JSON to rebuild from, so a fresh store starts empty. Everything
                // re-derives on the next wave (worktrees reprovision from an empty base).
                log::warn!(
                    target: "content_engine::sync_state_store",
                    "install-state store at {} is unusable ({e}); quarantining and recreating — install-state resets to empty and re-derives on the next wave",
                    path.display(),
                );
                quarantine_corrupt_db(&path)?;
                // The rebuilt store is empty. Re-arm the one-time JSON backfill by clearing
                // its completion marker, so `backfill_json_once` re-imports from any surviving
                // JSON (a corruption in the migration window, before the boot deleter ran)
                // instead of seeing a stale "done" marker over an empty store. This NARROWS —
                // does not fully close — the window against the boot deleter: the deleter only
                // reads the marker's presence and the clear happens lazily on this first open,
                // so a same-boot deleter that already observed a prior-boot marker can still
                // delete first. That residual is the story's accepted corruption-loss case
                // (the store rebuilds empty and re-derives on the next wave); once the JSON is gone there is nothing to re-import anyway.
                // Best-effort: an absent marker (the common post-cutover case, no JSON left) is
                // fine; any other failure is warn-logged.
                if let Err(e) = std::fs::remove_file(backfill_marker_path_for(&path)) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        log::warn!(
                            target: "content_engine::sync_state_store",
                            "could not clear the backfill marker after quarantine ({e}); the JSON backfill may not re-arm on the rebuilt store",
                        );
                    }
                }
                // On a clean slate the open must succeed; a failure here is a real
                // environment fault (permissions, disk), propagated — never looped.
                Self::try_open(&path)
            }
            Err(e) => Err(e),
        }
    }

    /// Open + integrity-check + apply/heal schema, surfacing a corrupt or non-SQLite
    /// file as an error that [`open`](Self::open) recovers from. Split out so recovery
    /// can re-run the exact same open sequence on the fresh file.
    fn try_open(path: &Path) -> Result<Self> {
        // Parent-dir create + WAL set/assert come from the shared plumbing.
        let mut conn = crate::shared::sqlite::open_wal_db(path)?;
        validate_remote_guard(path, &conn)?;
        // Integrity-check BEFORE applying the schema, so a structurally corrupt but
        // openable file is caught without first writing to it — the quarantined
        // bytes stay closer to their found state. A fresh empty DB passes quick_check.
        //
        // `quick_check` is an O(DB) full-btree scan. The store opens per call on hot
        // paths (the reaper enumerate, wave resolution, subject providers), so
        // running it on EVERY open turned it into a startup/derive CPU sink (a
        // whole-DB scan per open, dozens of them concurrent, super-linear as the DB
        // grows). Mirror the sibling automation store — which opens per call WITHOUT
        // an integrity scan — by verifying once per DB path per process: corruption
        // present at first open is still caught (and recovered by `open`'s
        // quarantine-and-rebuild), and a rebuildable-cache DB that corrupts
        // mid-session is caught on the next launch. The lock is held across the
        // check so concurrent first-opens don't stampede N simultaneous scans; a
        // failed check leaves the path unrecorded so recovery re-verifies the fresh
        // file.
        {
            // Key on the canonicalized path so the same DB reached via a symlink /
            // relative / trailing-slash spelling still dedups to one integrity check
            // ("once per DB file", not "once per path string"). `open_wal_db` above
            // already created/opened the file, so `canonicalize` resolves; fall back
            // to the raw path if it somehow can't.
            let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let mut checked = integrity_checked_paths()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !checked.contains(&key) {
                assert_not_corrupt(&conn)?;
                checked.insert(key);
            }
        }
        // Schema creation / migration is a ONE-TIME cost, gated on the stored
        // version: once the DB is stamped at the current version it is skipped, so a
        // hot-path reader that opens the store per call (the repointed reaper
        // enumerate / `lockfile_mtime`) pays nothing on the common open. `<` (not
        // `!=`) never downgrades a DB written by a future version.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            // A fresh DB (`user_version` defaults to 0 and — because schema-create and
            // the version stamp commit atomically below — no tables). Create the current
            // schema and stamp the version in ONE transaction: a crash mid-create rolls
            // back wholesale and the next open retries from a consistent state, never a
            // half-created schema stamped at 0. The `DROP TABLE IF EXISTS` is defensive —
            // a no-op on a genuinely fresh DB, but it discards any stale-shape tables a
            // hand-built or truncated file left stamped at 0 (which would otherwise
            // survive `CREATE TABLE IF NOT EXISTS` and get stamped at the current
            // version), mirroring the pre-3c create path.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "DROP TABLE IF EXISTS materialized_entry;\n\
                 DROP TABLE IF EXISTS target_slug;\n\
                 DROP TABLE IF EXISTS install_target;",
            )?;
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            tx.commit()?;
        } else if version < 4 {
            // SW-IS-3c: the store is now the AUTHORITATIVE substrate — the per-file
            // JSON lockfile that used to rebuild it is retired. A pre-v4 store carries
            // a superseded KEY SHAPE SQLite cannot ALTER in place (v1/v2's
            // `UNIQUE(code_repo_path)` / nullable `team_name`, or v3's
            // `materialized_entry` PK without `projection_key`), and we can no longer
            // rebuild its rows from JSON. Rather than DROP the tables and silently
            // strand every worktree with an empty store, treat the un-migratable file
            // as a recoverable fault: surface a corruption-class error so `open`
            // quarantines it (bytes preserved, never deleted) and starts fresh.
            // Worktrees then reprovision from an empty base. No in-the-wild store predates
            // v4 — the whole install-state epic ships in one release and upgraders
            // arrive pre-store — so this is a defensive backstop, not a live upgrade.
            //
            // FORWARD RULE (post-3c): a `STORE_SCHEMA_VERSION` bump MUST migrate data in
            // place — an ADDITIVE change (a new table/column) heals here (see the v4→v5
            // branch below); a KEY-SHAPE change needs a `CREATE new … INSERT … SELECT …`
            // row copy inside this transaction. It must NEVER drop-and-rebuild: there is
            // no JSON to restore from.
            return Err(SyncStateStoreError::Corrupt(format!(
                "install-state schema v{version} predates the JSON retirement (current v{STORE_SCHEMA_VERSION}) and cannot be migrated in place"
            )));
        } else if (6..STORE_SCHEMA_VERSION).contains(&version) {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // v12 recognizes batch-bearing v2 wrappers. Refresh only admission
            // representation guards; immutable intent/progress ownership stays.
            tx.execute_batch("DROP TRIGGER IF EXISTS context_operation_require_wrapper; DROP TRIGGER IF EXISTS context_progress_wrapped_insert;")?;
            tx.execute_batch(SCHEMA)?;
            // Rebuild unfinished ownership with shared local alias keys. Conflicting
            // older intents fail closed instead of selecting an arbitrary winner.
            selected::rebuild_path_ownership(&tx)?;
            tx.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            tx.commit()?;
        } else if version < STORE_SCHEMA_VERSION {
            // Rebuild only the parent table, preserving IDs and every child row.
            // Foreign-key enforcement must be changed outside the transaction;
            // its integrity is checked again before the migration commits.
            conn.pragma_update(None, "foreign_keys", false)?;
            let migrated = (|| -> Result<()> {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                if current >= STORE_SCHEMA_VERSION {
                    tx.commit()?;
                    return Ok(());
                }
                let target_schema = SCHEMA
                    .split_once("CREATE UNIQUE INDEX")
                    .ok_or_else(|| {
                        SyncStateStoreError::Data("target schema delimiter missing".into())
                    })?
                    .0
                    .replace("install_target (", "install_target_new (");
                tx.execute_batch(&target_schema)?;
                tx.execute_batch("INSERT INTO install_target_new
                    (id, code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at)
                    SELECT id, code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at FROM install_target;
                    DROP TABLE install_target;
                    ALTER TABLE install_target_new RENAME TO install_target;")?;
                tx.execute_batch(SCHEMA)?;
                tx.execute(
                    "INSERT OR IGNORE INTO target_slug (target_id, source_repo_slug)
                    SELECT id, source_repo_slug FROM install_target",
                    [],
                )?;
                if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
                    return Err(SyncStateStoreError::Data(
                        "destination migration would orphan child rows".into(),
                    ));
                }
                tx.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
                tx.commit()?;
                Ok(())
            })();
            conn.pragma_update(None, "foreign_keys", true)?;
            migrated?;
        }

        // version == STORE_SCHEMA_VERSION: already current. A `>` version is never
        // downgraded (a DB written by a future build stays untouched).
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Open a fresh connection with the shared per-connection pragmas
    /// (busy-timeout + `synchronous=NORMAL` + `foreign_keys=ON`).
    fn conn(&self) -> Result<Connection> {
        remote_guard_requires_existing_database(&self.path)?;
        let conn = crate::shared::sqlite::open_conn(&self.path)?;
        validate_remote_guard(&self.path, &conn)?;
        Ok(conn)
    }

    /// Upsert an install target and its entries in one `IMMEDIATE`
    /// transaction — the batching best practice: one transaction per materialize
    /// wave, not one per file. A mid-batch failure rolls back the whole write, so
    /// the target's prior state is never left half-updated. Stamps `updated_at`
    /// with `now`; the bulk JSON backfill uses [`upsert_with_updated_at`] to seed
    /// each target's real recency instead.
    pub fn upsert(
        &self,
        user_id: u64,
        source_repo_slug: &SourceRepoSlug,
        lock: &Lockfile,
    ) -> Result<()> {
        self.upsert_with_updated_at(user_id, source_repo_slug, lock, now_unix_nanos())
    }

    /// [`upsert`](Self::upsert) stamping an explicit `updated_at` (UNIX-epoch
    /// nanoseconds). `upsert` passes `now`; the JSON backfill passes each
    /// lockfile's on-disk mtime so the reaper's oldest-first victim ordering and
    /// idle-grace floor survive the cutover, instead of collapsing every
    /// backfilled target onto import time.
    pub(crate) fn upsert_with_updated_at(
        &self,
        user_id: u64,
        source_repo_slug: &SourceRepoSlug,
        lock: &Lockfile,
        updated_at_ns: i64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Resolve-or-create the parent target row (advancing `updated_at` + recording
        // the slug alias) — the write-seam shared with the targeted `apply_entry_changes`.
        let target_id =
            resolve_target_for_write(&tx, lock, source_repo_slug, user_id, updated_at_ns)?;

        // SW-IS-F8 delta upsert: write only the changed `materialized_entry` rows
        // instead of DELETE-all + re-INSERT-all. In the common one-file change the
        // writer lock is held for a single row op rather than the time to re-insert the
        // whole (up to ~9,400-row) set — killing F6's large-cell p99 and the intermittent
        // SQLITE_BUSY on a concurrent reprovision. (A wave that genuinely changes many
        // files still writes proportionally many rows, but never more than the full set.)
        // The parent upsert above already advanced `updated_at` (the recency the reaper
        // needs). This subsumes the SW-IS-3b entry no-op: a truly-unchanged set computes
        // an empty delta and issues zero child-row ops.
        write_entries_delta(&tx, target_id, &lock.entries)?;

        tx.commit()?;
        Ok(())
    }

    /// SW-IS-F9 targeted persist: apply a **bounded, explicit changeset** to a target's
    /// `materialized_entry` rows — UPSERT `upserts`, DELETE `removals` by identity —
    /// WITHOUT reading the rest of the manifest. O(touched), not O(manifest): unlike the
    /// full [`upsert`](Self::upsert), this path issues no `load_entries` snapshot and no
    /// full-table `SELECT` of `materialized_entry`, so the `IMMEDIATE` writer lock is held
    /// only for the passed rows. On the N-peer incremental fan-out this collapses the
    /// F8 read-amplification lock convoy (the persist reads+decodes ~9,400 rows per call
    /// today; here it touches only what changed).
    ///
    /// **The caller owns completeness.** Because this path performs NO reconcile, a
    /// removal it is not told about is NOT swept — so it is only sound for a wave whose
    /// mutation set IS the complete delta (a scoped `ContentLayerEvent`).
    /// A wave whose removals can only be discovered by diffing the whole manifest
    /// (a full reconcile or team switch, both retired) MUST use [`upsert`](Self::upsert), which does
    /// the full snapshot + DELETE. `removals` are stored-space `(logical_name,
    /// projection_key)` identities (the [`entry_identity`] space, `""` sentinel for a wave
    /// entry's `None`). `updated_at` advances and the slug alias is recorded identically to
    /// the full path (both go through [`resolve_target_for_write`]).
    ///
    /// SW-CP-33 (S4b): the live scoped Forward projection fan-out
    /// (`story_linker::materialize_projection` under a `ContentLayerEvent`) persists
    /// through this targeted write — its changeset is the complete delta for the scoped
    /// logicals, so it collapses the F8 convoy on the N-peer fan-out. The full
    /// [`upsert`](Self::upsert) still serves Reconcile / unscoped full-walk waves.
    pub fn apply_entry_changes(
        &self,
        user_id: u64,
        source_repo_slug: &SourceRepoSlug,
        lock: &Lockfile,
        upserts: &[&MaterializedEntry],
        removals: &[(&str, &str)],
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_id =
            resolve_target_for_write(&tx, lock, source_repo_slug, user_id, now_unix_nanos())?;

        // DELETE the explicitly-removed rows by identity — one indexed row op each, no
        // full-table scan. `projection_key` is already the stored `""` sentinel space
        // ([`entry_identity`]), matching the PK columns directly.
        if !removals.is_empty() {
            let mut delete_stmt = prepare_entry_delete(&tx)?;
            for (logical_name, projection_key) in removals {
                delete_stmt.execute(params![target_id, logical_name, projection_key])?;
            }
        }

        // UPSERT the changed rows via the shared bind (INSERT … ON CONFLICT DO UPDATE).
        // No change-detection read: the caller passes only what it materialized this pass,
        // so every row is a genuine write — the point of the O(touched) fast path.
        let mut write_stmt = prepare_entry_upsert(&tx)?;
        for e in upserts {
            execute_entry_upsert(&mut write_stmt, target_id, e)?;
        }
        drop(write_stmt);

        tx.commit()?;
        Ok(())
    }

    /// Record a reconcile wave against a target WITHOUT ever reading or diffing its
    /// `materialized_entry` rows: [`resolve_target_for_write`] resolves-or-creates the row,
    /// advances `updated_at` through the SAME seam [`upsert`](Self::upsert) uses (so the
    /// reaper's `MAX(updated_at)` recency — see [`updated_at`](Self::updated_at) — is
    /// byte-identical to a real upsert) and records the slug alias. Its monotonic clamp
    /// means an out-of-order wave can't drag recency backward.
    ///
    /// It never writes entries, so the target's existing projection entries are left
    /// intact — passing an entries-empty `lock` to `upsert` would instead sweep them all as
    /// stale. `upsert` remains the persist path for the projection engine.
    pub(crate) fn persist_reconcile(
        &self,
        user_id: u64,
        source_repo_slug: &SourceRepoSlug,
        lock: &Lockfile,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        resolve_target_for_write(&tx, lock, source_repo_slug, user_id, now_unix_nanos())?;
        tx.commit()?;
        Ok(())
    }

    /// Load a target's full state for the `(org_slug, team_name, code_repo_path)`
    /// [`TargetKey`], or `None` if not present. Team-scoped: the same worktree path
    /// under a different team is a distinct target and is never returned here.
    /// Entries whose stored `source_layer` is an unknown/legacy variant are dropped
    /// on load, matching [`Lockfile::read`].
    pub fn read(&self, key: &TargetKey) -> Result<Option<Lockfile>> {
        let conn = self.conn()?;
        let (org_slug, team, path) = key.seek_params()?;
        let target = conn
            .query_row(
                "SELECT id, org_slug, team_name FROM install_target \
                 WHERE org_slug = ?1 AND team_name = ?2 AND code_repo_path = ?3",
                params![org_slug, team, path],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((target_id, org_slug, team_name)) = target else {
            return Ok(None);
        };

        Ok(Some(Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug,
            team_name: team_name_from_db(&team_name),
            code_repo_path: key.code_repo_path.to_path_buf(),
            entries: load_entries(&conn, target_id)?,
        }))
    }

    /// Remote targets live in the same applied-state store, but never enter the
    /// local path enumeration used by watchers, reapers, or attachment import.
    /// Commit the immutable intent before remote dispatch. A previously pending
    /// intent wins even when this wave wants different bytes: reconcile it first.
    pub fn prepare_destination_effect(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
        effect_key: &str,
        intent: &serde_json::Value,
    ) -> Result<PendingDestinationEffect> {
        self.prepare_named_destination_effect(
            ctx,
            slug,
            identity,
            effect_key,
            intent,
            uuid::Uuid::new_v4(),
        )
    }

    pub fn prepare_named_destination_effect(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
        effect_key: &str,
        intent: &serde_json::Value,
        operation_id: uuid::Uuid,
    ) -> Result<PendingDestinationEffect> {
        validate_remote_owner(ctx, identity)?;
        if effect_key.is_empty() {
            return Err(SyncStateStoreError::Data(
                "empty destination effect key".into(),
            ));
        }
        let mut conn = self.conn()?;
        // Unlike recomputable applied metadata, this row prevents replay after
        // ambiguity and therefore must survive the dispatch boundary durably.
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        establish_remote_guard(&self.path, &tx)?;
        let identity = serde_json::to_string(identity)?;
        let slug = slug_to_db(slug)?;
        let key = params![
            ctx.user_id,
            ctx.org_slug,
            team_name_to_db(ctx.team_name.as_deref()),
            slug,
            identity,
            effect_key
        ];
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT operation_id, intent FROM destination_effect
             WHERE user_id=?1 AND org_slug=?2 AND team_name=?3 AND source_repo_slug=?4
             AND destination_identity=?5 AND effect_key=?6",
                key,
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let result = if let Some((operation_id, intent)) = prior {
            PendingDestinationEffect {
                operation_id: uuid::Uuid::parse_str(&operation_id)
                    .map_err(|e| SyncStateStoreError::Data(e.to_string()))?,
                intent: serde_json::from_str(&intent)?,
                newly_prepared: false,
            }
        } else {
            tx.execute(
                "INSERT INTO destination_effect(user_id, org_slug, team_name, source_repo_slug,
                 destination_identity, effect_key, operation_id, intent) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![ctx.user_id, ctx.org_slug, team_name_to_db(ctx.team_name.as_deref()),
                    slug, identity, effect_key, operation_id.to_string(), serde_json::to_string(intent)?],
            )?;
            PendingDestinationEffect {
                operation_id,
                intent: intent.clone(),
                newly_prepared: true,
            }
        };
        tx.commit()?;
        Ok(result)
    }

    pub fn pending_destination_effects(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
    ) -> Result<Vec<PendingDestinationEffect>> {
        validate_remote_owner(ctx, identity)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT operation_id, intent FROM destination_effect WHERE user_id=?1 AND org_slug=?2
             AND team_name=?3 AND source_repo_slug=?4 AND destination_identity=?5 ORDER BY operation_id",
        )?;
        let rows = stmt.query_map(
            params![
                ctx.user_id,
                ctx.org_slug,
                team_name_to_db(ctx.team_name.as_deref()),
                slug_to_db(slug)?,
                serde_json::to_string(identity)?
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (operation_id, intent) = row?;
            Ok(PendingDestinationEffect {
                operation_id: uuid::Uuid::parse_str(&operation_id)
                    .map_err(|e| SyncStateStoreError::Data(e.to_string()))?,
                intent: serde_json::from_str(&intent)?,
                newly_prepared: false,
            })
        })
        .collect()
    }

    /// Call only after definitive non-application or after the acknowledged
    /// effect's baseline has been persisted. Unknown effects remain pending.
    pub fn settle_destination_effect(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
        operation_id: uuid::Uuid,
    ) -> Result<()> {
        validate_remote_owner(ctx, identity)?;
        let conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "DELETE FROM destination_effect WHERE user_id=?1 AND org_slug=?2 AND team_name=?3
             AND source_repo_slug=?4 AND destination_identity=?5 AND operation_id=?6",
            params![
                ctx.user_id,
                ctx.org_slug,
                team_name_to_db(ctx.team_name.as_deref()),
                slug_to_db(slug)?,
                serde_json::to_string(identity)?,
                operation_id.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn read_remote_target(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
    ) -> Result<Option<super::destination::DestinationLockfile>> {
        use super::destination::{DestinationIdentity, DestinationReference};
        validate_remote_owner(ctx, identity)?;
        let conn = self.conn()?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM install_target WHERE destination_kind = 'remote'
             AND org_slug = ?1 AND team_name = ?2 AND source_repo_slug = ?3
             AND destination_identity = ?4 AND user_id = ?5",
                params![
                    ctx.org_slug,
                    team_name_to_db(ctx.team_name.as_deref()),
                    slug_to_db(slug)?,
                    serde_json::to_string(identity)?,
                    ctx.user_id
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let entries = load_entries::<DestinationReference>(&conn, id)?;
        validate_remote_entries(identity, &entries)?;
        Ok(Some(Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: ctx.org_slug.clone(),
            team_name: ctx.team_name.clone(),
            code_repo_path: DestinationIdentity::Remote(identity.clone()),
            entries,
        }))
    }

    /// SW-XS-9: drop this remote target's records of `paths` — files the
    /// repo tracks now (`update_workspace_base` replaced the managed copies).
    pub fn forget_remote_entries(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &super::destination::RemoteDestinationIdentity,
        paths: &[String],
    ) -> Result<()> {
        use super::destination::DestinationReference;
        let Some(mut lock) = self.read_remote_target(ctx, slug, identity)? else {
            return Ok(());
        };
        let before = lock.entries.len();
        lock.entries.retain(|entry| {
            !matches!(&entry.dest_path, DestinationReference::Remote { relative, .. }
                if paths.iter().any(|path| path == relative.as_str()))
        });
        if lock.entries.len() != before {
            self.upsert_remote_target(ctx, slug, &lock)?;
        }
        Ok(())
    }

    pub fn upsert_remote_target(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        lock: &super::destination::DestinationLockfile,
    ) -> Result<()> {
        use super::destination::DestinationIdentity;
        let DestinationIdentity::Remote(identity) = &lock.code_repo_path else {
            return Err(SyncStateStoreError::Data(
                "remote target requires remote identity".into(),
            ));
        };
        validate_remote_owner(ctx, identity)?;
        validate_remote_entries(identity, &lock.entries)?;
        if lock.org_slug != ctx.org_slug
            || lock.team_name != ctx.team_name
            || lock.schema_version != SCHEMA_VERSION
        {
            return Err(SyncStateStoreError::Data(
                "remote applied state does not match captured context".into(),
            ));
        }
        let encoded = serde_json::to_string(identity)?;
        let slug = slug_to_db(slug)?;
        let team = team_name_to_db(ctx.team_name.as_deref());
        let mut conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        establish_remote_guard(&self.path, &tx)?;
        tx.execute("INSERT INTO install_target
            (destination_kind, destination_identity, code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at)
            VALUES ('remote', ?1, NULL, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(org_slug, team_name, source_repo_slug, destination_identity) WHERE destination_kind = 'remote'
            DO UPDATE SET updated_at = MAX(COALESCE(updated_at, excluded.updated_at), excluded.updated_at)",
            params![encoded, ctx.org_slug, team, slug, ctx.user_id, now_unix_nanos()])?;
        let id: i64 = tx.query_row("SELECT id FROM install_target WHERE destination_kind = 'remote'
            AND destination_identity = ?1 AND org_slug = ?2 AND team_name = ?3 AND source_repo_slug = ?4",
            params![encoded, ctx.org_slug, team, slug], |row| row.get(0))?;
        tx.execute(
            "INSERT OR IGNORE INTO target_slug (target_id, source_repo_slug) VALUES (?1, ?2)",
            params![id, slug],
        )?;
        write_entries_delta(&tx, id, &lock.entries)?;
        tx.commit()?;
        Ok(())
    }

    /// The recency of `code_repo_path` as a [`SystemTime`], or `None` when there is
    /// no row (or every matching row's column is still NULL on a v1-healed target
    /// not yet re-materialized). **Path-level, spanning every row at the path:** a
    /// path can now have one row per team (and, in principle, per org — though a
    /// physical worktree realistically belongs to one), and the reaper reaps a
    /// worktree, not a team or org, so this is a `MAX(updated_at)` over all the path's
    /// rows — the "freshest wins" recency 3a's per-team backfill intended, which is
    /// also fail-safe (a recent sibling row protects the worktree from being reaped). The store-side replacement for the lockfile's
    /// filesystem mtime: the reaper orders victims oldest-first and honours an
    /// idle-grace floor off this, and an absent value reads as *not reapable* — the
    /// same fail-safe as a missing/unstat-able lockfile. One indexed SELECT
    /// (`idx_install_target_code_repo_path`).
    pub fn updated_at(&self, code_repo_path: &Path) -> Result<Option<SystemTime>> {
        let conn = self.conn()?;
        // `MAX` over an aggregate always yields exactly one row (NULL when no rows
        // match or all are NULL), so no `.optional()` is needed.
        let ns: Option<i64> = conn.query_row(
            "SELECT MAX(updated_at) FROM install_target WHERE code_repo_path = ?1",
            params![path_to_str(code_repo_path)?],
            |r| r.get::<_, Option<i64>>(0),
        )?;
        Ok(ns.map(unix_nanos_to_system_time))
    }

    /// Every `(source_repo_slug, code_repo_path)` attached under `(org_slug,
    /// team_name)` via one index-backed SELECT — the enumerate that replaces the
    /// per-slug tree-walk. `None` for `team_name` selects the no-team (`_none`)
    /// targets. Returns the slug so a caller can rebuild the attached-repo set
    /// (the lockfile derives the slug from the on-disk path; the store carries it).
    pub fn enumerate_for_team(
        &self,
        org_slug: &str,
        team_name: Option<&str>,
    ) -> Result<Vec<(SourceRepoSlug, PathBuf)>> {
        let conn = self.conn()?;
        // `team_name` is stored as the `_none` sentinel for no-team (never NULL), so a
        // single bound `= ?2` seeks the `(org_slug, team_name, …)` index for both the
        // team and no-team forms — no `IS NULL` split.
        // Slugs are a set in `target_slug` (SW-IS-3c dual-slug), so JOIN it to emit one
        // `(slug, path)` row per alias — a clone attached under two slugs yields both.
        // Callers that fan out per team dedup by `code_repo_path`, so the repeated path
        // is harmless.
        let rows: Vec<(String, String)> = conn
            .prepare(
                "SELECT ts.source_repo_slug, it.code_repo_path FROM install_target it \
                 JOIN target_slug ts ON ts.target_id = it.id \
                 WHERE it.destination_kind = 'local' AND it.org_slug = ?1 AND it.team_name = ?2",
            )?
            .query_map(params![org_slug, team_name_to_db(team_name)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(slug, path)| Ok((slug_from_db(&slug)?, PathBuf::from(path))))
            .collect()
    }

    /// Fast indexed admission check for the usual registered spelling; canonical
    /// fallback also covers historic raw paths and symlink aliases.
    pub(crate) fn is_registered_product(&self, root: &Path) -> Result<bool> {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let found: bool = self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM install_target it JOIN target_slug ts ON ts.target_id = it.id
             WHERE it.destination_kind = 'local' AND it.code_repo_path IN (?1, ?2)
             AND json_extract(ts.source_repo_slug, '$.owner') = ?3)",
            params![path_to_str(root)?, path_to_str(&canonical)?, crate::domain::EPIC_OWNER_SENTINEL],
            |row| row.get(0),
        )?;
        Ok(found || !self.product_registrations(root)?.is_empty())
    }

    /// Product roots may be shared across teams. Resolve only the captured owner,
    /// refusing ambiguity within that owner rather than selecting an arbitrary alias.
    pub(crate) fn product_registration_for(
        &self,
        root: &Path,
        user_id: u64,
        org: &str,
        team: Option<&str>,
    ) -> Result<Option<ProductRegistration>> {
        let all = self.product_registrations(root)?;
        if all.is_empty() {
            return Ok(None);
        }
        let mut owned = all.into_iter().filter(|row| {
            row.user_id == user_id && row.org_slug == org && row.team_name.as_deref() == team
        });
        let registration = owned.next().ok_or_else(|| {
            SyncStateStoreError::Data(
                "product ownership differs from captured principal/team".into(),
            )
        })?;
        if owned.next().is_some() {
            return Err(SyncStateStoreError::Data(
                "product identity is ambiguous within captured principal/team".into(),
            ));
        }
        Ok(Some(registration))
    }

    /// Durable product ownership, including aliases and dormant roots. Never infer
    /// workspace identity from a directory name or require a live session.
    pub(crate) fn product_registrations(&self, root: &Path) -> Result<Vec<ProductRegistration>> {
        let conn = self.conn()?;
        let rows: Vec<(u64, String, String, String, String)> = conn.prepare(
            "SELECT it.user_id, it.org_slug, it.team_name, ts.source_repo_slug, it.code_repo_path
             FROM install_target it JOIN target_slug ts ON ts.target_id = it.id
             WHERE it.destination_kind = 'local' AND json_extract(ts.source_repo_slug, '$.owner') = ?1",
        )?.query_map([crate::domain::EPIC_OWNER_SENTINEL], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut matches = Vec::new();
        for (user, org, team, slug, path) in rows {
            let slug = slug_from_db(&slug)?;
            if !crate::domain::is_epic_workspace_slug(&slug) {
                continue;
            }
            let path = PathBuf::from(path);
            if std::fs::canonicalize(&path).unwrap_or(path) == canonical {
                let registration = ProductRegistration {
                    user_id: user,
                    org_slug: org,
                    team_name: (team != super::lockfile::NO_TEAM_SEGMENT).then_some(team),
                    slug,
                };
                if !matches.contains(&registration) {
                    matches.push(registration);
                }
            }
        }
        Ok(matches)
    }

    /// The `code_repo_path`s attached under `(org_slug, team_name, source_repo_slug)`
    /// — the per-slug enumerate. JOINs the `target_slug` alias set (SW-IS-3c dual-slug)
    /// so a clone attached under multiple slugs resolves under each; the
    /// `idx_target_slug_slug` index backs the slug filter.
    pub fn enumerate_for_slug(
        &self,
        org_slug: &str,
        team_name: Option<&str>,
        source_repo_slug: &SourceRepoSlug,
    ) -> Result<Vec<PathBuf>> {
        let conn = self.conn()?;
        let slug = slug_to_db(source_repo_slug)?;
        // Single bound `team_name = ?2` (sentinel-normalized) for both the team and
        // no-team forms. The slug lives in `target_slug` (a set per target), so match
        // any alias, not the single `install_target.source_repo_slug` primary column.
        let paths: Vec<String> = conn
            .prepare(
                "SELECT it.code_repo_path FROM install_target it \
                 JOIN target_slug ts ON ts.target_id = it.id \
                 WHERE it.destination_kind = 'local' AND it.org_slug = ?1 AND it.team_name = ?2 AND ts.source_repo_slug = ?3",
            )?
            .query_map(params![org_slug, team_name_to_db(team_name), slug], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(paths.into_iter().map(PathBuf::from).collect())
    }

    /// Idempotently register `source_repo_slug` as an attached alias of `lock`'s
    /// target, creating an EMPTY target if none exists — the whole find-or-create plus
    /// alias-insert in ONE `IMMEDIATE` transaction (SW-IS-3c dual-slug, SW-EW-6). The
    /// atomicity matters: a plain "check on one connection, then `upsert` an empty
    /// lockfile on another" races a concurrent materialize — if the wave creates the
    /// target and writes entries between the check and the empty upsert, the upsert
    /// wipes them. Here the `IMMEDIATE` write-lock serializes the two: whichever runs
    /// first, the other sees the target and only adds an alias, never replacing
    /// entries. Returns `true` when a new target row OR a new slug alias was recorded,
    /// `false` when the slug already aliased an existing target. Never touches an
    /// existing target's `materialized_entry` rows — use
    /// [`upsert`](Self::upsert) to write entries.
    pub fn register_or_alias(
        &self,
        user_id: u64,
        source_repo_slug: &SourceRepoSlug,
        lock: &Lockfile,
    ) -> Result<bool> {
        let path = path_to_str(&lock.code_repo_path)?;
        let slug = slug_to_db(source_repo_slug)?;
        reject_reserved_team(lock.team_name.as_deref())?;
        let user_id = i64::try_from(user_id)
            .map_err(|_| SyncStateStoreError::Data(format!("user_id {user_id} exceeds i64")))?;
        let team = team_name_to_db(lock.team_name.as_deref());
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM install_target \
                 WHERE org_slug = ?1 AND team_name = ?2 AND code_repo_path = ?3",
                params![lock.org_slug, team, path],
                |r| r.get(0),
            )
            .optional()?;
        let created = existing.is_none();
        let target_id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO install_target \
                     (code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![path, lock.org_slug, team, slug, user_id, now_unix_nanos()],
                )?;
                tx.last_insert_rowid()
            }
        };
        let alias_added = insert_slug_alias(&tx, target_id, &slug)?;
        tx.commit()?;
        Ok(created || alias_added)
    }

    /// The distinct teams that hold a NON-EMPTY materialization of `code_repo_path`
    /// under `(org_slug, source_repo_slug)` — the store-backed answer to "which teams
    /// materialized this path", used by cross-team teardown (e.g. an interrupted re-team
    /// whose stale team must still be torn down). `None` in the result is the no-team
    /// (`_none`) materialization. Only targets with ≥1 `materialized_entry` are returned,
    /// matching the non-empty filter of the JSON-scanning `materialized_teams_for_repo`.
    pub fn teams_for_materialized_path(
        &self,
        org_slug: &str,
        source_repo_slug: &SourceRepoSlug,
        code_repo_path: &Path,
    ) -> Result<Vec<Option<String>>> {
        let conn = self.conn()?;
        let slug = slug_to_db(source_repo_slug)?;
        // Match the slug against the `target_slug` alias set (SW-IS-3c dual-slug), not
        // the single primary column, so a path attached under this slug as a secondary
        // alias is still discovered.
        // The raw `EXISTS` non-empty check must mirror the load-side drain that
        // `raw_to_entry` applies on decode (SW-CP-31b): a target whose ONLY rows are
        // drained legacy install rows (`projection_key = ''` sentinel + a
        // `TeamRepo`/`TeamWorkspace` layer) decodes to an EMPTY `read()`, so it must
        // NOT be reported here either. Require at least one NON-drained row — a
        // projection entry (`projection_key <> ''`) or any non-install layer. The
        // `NOT (...)` fragment is derived from `is_drained_install_row`'s mappings via
        // `drained_install_row_sql`, so it can never drift from the load-side predicate.
        let query = format!(
            "SELECT t.team_name FROM install_target t \
             JOIN target_slug ts ON ts.target_id = t.id \
             WHERE t.org_slug = ?1 AND ts.source_repo_slug = ?2 AND t.code_repo_path = ?3 \
               AND EXISTS(SELECT 1 FROM materialized_entry me WHERE me.target_id = t.id \
                 AND NOT ({}))",
            drained_install_row_sql()
        );
        let teams: Vec<String> = conn
            .prepare(&query)?
            .query_map(params![org_slug, slug, path_to_str(code_repo_path)?], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(teams.iter().map(|t| team_name_from_db(t)).collect())
    }

    /// SW-XS-4: every LOCAL target `(org_slug, team_name, code_repo_path)` recorded
    /// for `user_id` (or for every user when `None`), in path order — the
    /// enumeration the one-time unpublished-edits notice, the reaper's
    /// unpublished-edit refusal and the launch reconcile's gone-target reap walk.
    /// `team_name` is `None` for the no-team (`_none`) target.
    pub fn local_targets(
        &self,
        user_id: Option<u64>,
    ) -> Result<Vec<(String, Option<String>, PathBuf)>> {
        let conn = self.conn()?;
        let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String, String)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        };
        let rows: Vec<(String, String, String)> = match user_id {
            Some(user_id) => conn
                .prepare(
                    "SELECT org_slug, team_name, code_repo_path FROM install_target \
                     WHERE destination_kind = 'local' AND user_id = ?1 \
                     ORDER BY code_repo_path, org_slug, team_name",
                )?
                .query_map(params![user_id as i64], map)?
                .collect::<rusqlite::Result<_>>()?,
            None => conn
                .prepare(
                    "SELECT org_slug, team_name, code_repo_path FROM install_target \
                     WHERE destination_kind = 'local' \
                     ORDER BY code_repo_path, org_slug, team_name",
                )?
                .query_map([], map)?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(rows
            .into_iter()
            .map(|(org, team, path)| (org, team_name_from_db(&team), PathBuf::from(path)))
            .collect())
    }

    /// Remove a target and (by `ON DELETE CASCADE`) its entries. Returns
    /// whether a target row existed. Team-scoped: only the `(org, team, path)` row
    /// is removed, not a sibling team's state at the same path. The prune operation,
    /// as an indexed DELETE.
    pub fn delete(&self, key: &TargetKey) -> Result<bool> {
        let conn = self.conn()?;
        let (org_slug, team, path) = key.seek_params()?;
        let n = conn.execute(
            "DELETE FROM install_target \
             WHERE org_slug = ?1 AND team_name = ?2 AND code_repo_path = ?3",
            params![org_slug, team, path],
        )?;
        Ok(n > 0)
    }

    /// Remove **every** team's target row at `code_repo_path` (each with its entries +
    /// packs, by `ON DELETE CASCADE`), returning how many rows were deleted. The
    /// worktree-level prune: a reap or a boot orphan-prune acts on a physical worktree
    /// path, which under the composite `(org, team, path)` key can own one row per team;
    /// [`delete`](Self::delete) is team-scoped and would leave the sibling teams' rows
    /// as ghosts (DW-IS3ab-1). Index-backed by `idx_install_target_code_repo_path`.
    pub fn delete_all_for_path(&self, code_repo_path: &Path) -> Result<usize> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM install_target WHERE code_repo_path = ?1",
            params![path_to_str(code_repo_path)?],
        )?;
        Ok(n)
    }

    /// Marker-gated one-time bulk backfill: prime the store from every surviving
    /// JSON lockfile in `user_id`'s org tree, so the repointed readers serve from the
    /// indexed SELECT instead of re-parsing lockfiles. Delegates the config-tuple
    /// walk + import to [`import_all_from_json`] (the same primitive SW-IS-4's
    /// corruption rebuild uses — built once), and only adds the run-at-most-once
    /// gate: a marker file beside the DB records completion, so a later reaper tick
    /// or app launch is a cheap marker check rather than a re-walk of every
    /// (multi-MB) lockfile.
    ///
    /// A run that discovers **nothing** (config not loaded yet, or a genuinely empty
    /// install) does NOT arm the marker — a later tick gets a real shot, and an
    /// empty tree re-walks cheaply. A marker-write failure is best-effort (the import
    /// succeeded; worst case is a harmless idempotent re-walk next launch). Returns
    /// whether it ran (`false` = already done or nothing to do).
    pub(crate) fn backfill_json_once(&self, codesense_home: &Path, user_id: u64) -> Result<bool> {
        let marker = self.backfill_marker_path();
        if marker.exists() {
            return Ok(false);
        }
        // A target the store already holds is newer than its JSON lockfile (the store
        // is the source of truth once a row exists), so the backfill never overwrites it.
        let summary = import_from_json(self, codesense_home, user_id, true);
        // Nothing found → don't gate the store as "backfilled" against a still-empty
        // tree.
        if summary.imported == 0 && summary.skipped == 0 && summary.already_present == 0 {
            return Ok(false);
        }
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&marker, b"") {
            log::warn!(
                target: "content_engine::sync_state_store",
                "install store backfill imported {} target(s) but the completion marker could not be written ({e}); it may re-run next launch",
                summary.imported
            );
        }
        Ok(true)
    }

    /// The completion marker beside the DB (`…/install-state/json-backfill-complete.marker`).
    /// A rebuild of the store (SW-IS-4) removes it to re-arm the one-time backfill.
    fn backfill_marker_path(&self) -> PathBuf {
        backfill_marker_path_for(&self.path)
    }
}

/// The backfill completion marker beside the DB at `store_path`. A free function so
/// the boot-time JSON deleter (`content_engine::lockfile`, which has no store handle)
/// can gate deletion on the marker — the JSON is removed only once the store has been
/// seeded from it (SW-IS-3c).
pub(crate) fn backfill_marker_path_for(store_path: &Path) -> PathBuf {
    store_path.with_file_name("json-backfill-complete.marker")
}

// Remote pending UUIDs cannot be recomputed like local applied metadata. This
// sticky external generation is not an effect journal: all intent/state stays
// in SQLite. It only prevents cache-recovery from silently erasing that journal.
fn remote_guard_path(path: &Path) -> PathBuf {
    append_suffix(path, ".remote-effects")
}
fn read_remote_guard(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(remote_guard_path(path))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() != 36
    {
        return Err(SyncStateStoreError::Data(
            "remote journal guard is unsafe or incomplete; preserve existing state".into(),
        ));
    }
    let mut value = String::new();
    file.take(37).read_to_string(&mut value)?;
    uuid::Uuid::parse_str(&value)
        .map_err(|_| SyncStateStoreError::Data("remote journal guard is invalid".into()))?;
    Ok(Some(value))
}
fn remote_guard_requires_existing_database(path: &Path) -> Result<()> {
    if read_remote_guard(path)?.is_some() && !path.try_exists()? {
        return Err(SyncStateStoreError::Data(
            "remote effect journal is missing; no empty-state recovery is allowed".into(),
        ));
    }
    Ok(())
}
fn remote_database_generation(conn: &Connection) -> Result<Option<String>> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='remote_effect_journal_identity')", [], |row| row.get(0))?;
    if !exists {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT generation FROM remote_effect_journal_identity WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}
fn validate_remote_guard(path: &Path, conn: &Connection) -> Result<()> {
    match (read_remote_guard(path)?, remote_database_generation(conn)?) {
        (None, None) => Ok(()),
        (Some(marker), Some(database)) if marker == database => Ok(()),
        _ => Err(SyncStateStoreError::Data("remote effect journal generation is missing or changed; preserve state and reconcile before recovery".into())),
    }
}
fn establish_remote_guard(path: &Path, conn: &Connection) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    validate_remote_guard(path, conn)?;
    if remote_database_generation(conn)?.is_some() {
        return Ok(());
    }
    let generation = uuid::Uuid::new_v4().to_string();
    conn.execute_batch("CREATE TABLE remote_effect_journal_identity (id INTEGER PRIMARY KEY CHECK(id=1), generation TEXT NOT NULL)")?;
    conn.execute(
        "INSERT INTO remote_effect_journal_identity(id,generation) VALUES(1,?1)",
        [&generation],
    )?;
    // Before the enclosing FULL transaction can commit and authorize dispatch,
    // persist this marker and its directory entry. An interrupted first setup
    // conservatively refuses recovery; it never invents a new remote UUID.
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(remote_guard_path(path))?;
    marker.write_all(generation.as_bytes())?;
    marker.sync_all()?;
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| SyncStateStoreError::Data("journal path has no parent".into()))?,
    )?
    .sync_all()?;
    Ok(())
}

/// Process-global set of store DB paths whose integrity has already been verified
/// this run, so [`try_open`](SyncStateStore::try_open) runs the O(DB)
/// `quick_check` once per path instead of on every (per-call) open. See the gate
/// in `try_open` for the corruption-detection semantics this preserves.
fn integrity_checked_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static CHECKED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    CHECKED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Test-only reset of the process-global integrity-check dedup set. Rust runs a
/// crate's unit tests in one process, so the set persists across tests; a test that
/// reuses a DB path (open-healthy → corrupt-the-same-path → re-open) must clear it to
/// force the re-open to actually run `quick_check` instead of hitting the cached
/// "already verified" entry. Prevents silent coverage loss + order-dependence.
#[cfg(test)]
pub(crate) fn reset_integrity_checked_paths_for_test() {
    integrity_checked_paths()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Test-only membership probe for the process-global integrity-check set, so a
/// dedup test can assert that opening the same DB file under two path spellings
/// records exactly one (canonical) key rather than one key per spelling.
#[cfg(test)]
pub(crate) fn integrity_checked_contains_for_test(path: &Path) -> bool {
    integrity_checked_paths()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(path)
}

/// Run `PRAGMA quick_check` — cheaper than `integrity_check` and sufficient for a
/// cache — and surface any structural damage as [`SyncStateStoreError::Corrupt`]. A
/// healthy database returns a single `ok` row; the first non-`ok` line is the
/// signal that the file must be quarantined and rebuilt.
fn assert_not_corrupt(conn: &Connection) -> Result<()> {
    let first: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if first != "ok" {
        return Err(SyncStateStoreError::Corrupt(first));
    }
    Ok(())
}

/// Whether an open error means the on-disk file is corrupt / not a database — the
/// recoverable condition [`SyncStateStore::open`] quarantines and rebuilds
/// from. An unrelated fault (I/O, permissions) is NOT corruption and must
/// propagate unchanged so a transient environment problem is never mistaken for a
/// bad DB (which would quarantine a healthy-but-unreachable file).
fn is_corruption(e: &SyncStateStoreError) -> bool {
    match e {
        SyncStateStoreError::Corrupt(_) => true,
        SyncStateStoreError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) => matches!(
            err.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        _ => false,
    }
}



/// Outcome of a rebuild-from-JSON pass: targets re-imported and lockfiles that
/// failed to import. `skipped` counts upsert-level failures — a parseable file the
/// store rejected, each warn-logged. An *unparseable* JSON is warn-and-skipped one
/// layer down in [`read_slug_lockfiles`] and never reaches this counter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RebuildSummary {
    pub imported: usize,
    pub skipped: usize,
    /// Lockfiles left alone because the store already held a row for that target
    /// (the one-time backfill only; the rebuild imports every file).
    pub already_present: usize,
}

/// Rebuild the store from every surviving JSON lockfile under `user_id`'s org
/// tree.
///
/// Drives off the known `(org, team, slug)` tuples from loaded config, NOT a
/// blind path-walk, so both on-disk layouts (`HostAware` / `Legacy`) resolve
/// correctly. Reuses the exact parse + upsert primitives the lazy [`read_through`]
/// path is composed from (`read_slug_lockfiles` + [`SyncStateStore::upsert`]),
/// at bulk granularity — one read per file, no re-derivation of a path the walk
/// already holds.
///
/// Idempotent: `upsert` replaces a target's rows wholesale, so a re-run over the
/// same JSON converges. Synchronous like the rest of the store — a large rebuild
/// writes one row per target, so callers invoke it behind the blocking boundary,
/// never on the UI thread.
///
/// Scoped to a single `user_id`. The store is global across users, so a caller
/// recovering a multi-user home loops this over each user's id.
///
/// `pub` (not `pub(crate)`) so the lib exports it while still unwired — the same
/// posture as [`read_through`], and it keeps the `-D warnings` `--lib` clippy gate
/// green (a `pub(crate)` fn with no in-crate caller yet would read as dead code).
pub fn import_all_from_json(
    store: &SyncStateStore,
    codesense_home: &Path,
    user_id: u64,
) -> RebuildSummary {
    import_from_json(store, codesense_home, user_id, false)
}

/// [`import_all_from_json`], optionally leaving alone every target that already has
/// a store row (`keep_existing`, the one-time backfill).
fn import_from_json(
    store: &SyncStateStore,
    codesense_home: &Path,
    user_id: u64,
    keep_existing: bool,
) -> RebuildSummary {
    let mut summary = RebuildSummary::default();
    for (ctx, slug) in install_tuples_for_user(codesense_home, user_id) {
        import_slug_into_store(store, &ctx, &slug, keep_existing, &mut summary);
    }
    log::info!(
        target: "content_engine::sync_state_store",
        "install-state rebuild from JSON: {} targets imported, {} lockfiles skipped",
        summary.imported,
        summary.skipped,
    );
    summary
}

/// Every `(ResolverContext, SourceRepoSlug)` install tuple declared in `user_id`'s
/// on-disk org/team config. The context carries only what locates a lockfile —
/// `codesense_home` / `user_id` / `org_slug` / `team_name`; `team_chain` and
/// `provider_content_root` drive resolution/projection, which the lockfile-path
/// computation ([`read_slug_lockfiles`] → `lockfile_dir_path`) never reads, so
/// they are left at their defaults rather than resolved from disk.
fn install_tuples_for_user(
    codesense_home: &Path,
    user_id: u64,
) -> Vec<(ResolverContext, SourceRepoSlug)> {
    let mut tuples = Vec::new();
    for (org_slug, org_root) in
        crate::context_repo::org_enumeration::enumerate_user_orgs(codesense_home, user_id)
    {
        let Some(org) = crate::domain::load_org(org_slug.clone(), &org_root, None).org else {
            log::warn!(
                target: "content_engine::sync_state_store",
                "rebuild: skipping org {org_slug} — config did not load",
            );
            continue;
        };
        for team in org.teams {
            let ctx = ResolverContext {
                context_root: None,
                codesense_home: codesense_home.to_path_buf(),
                user_id,
                org_slug: org_slug.clone(),
                team_name: Some(team.name),
            };
            for slug in team.source_repos {
                tuples.push((ctx.clone(), slug));
            }
        }
    }
    tuples
}

/// Import every surviving lockfile for one `(ctx, slug)` tuple into the store.
/// Per-file tolerant: [`read_slug_lockfiles`] already warn-and-skips an
/// unreadable/unparseable lockfile and returns the rest, and an enumeration error
/// for the whole tuple (e.g. an invalid on-disk `org_slug`) is logged and skipped
/// rather than aborting the rebuild of every other target.
fn import_slug_into_store(
    store: &SyncStateStore,
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    keep_existing: bool,
    summary: &mut RebuildSummary,
) {
    let pairs = match read_slug_lockfiles(ctx, slug) {
        Ok(pairs) => pairs,
        Err(e) => {
            log::warn!(
                target: "content_engine::sync_state_store",
                "rebuild: skipping slug {slug:?} under org {}: {e}",
                ctx.org_slug,
            );
            return;
        }
    };
    for (path, lock) in pairs {
        // The file was located under this ctx's `(org, team)` directory, but `upsert`
        // keys the row on the lockfile's OWN `org_slug`/`team_name`. A lockfile whose
        // body diverges from its directory — a stale/relocated/hand-edited file, or a
        // path-hash collision — would import under a `(org, team, path)` key that a
        // reader scoped to this directory's team never finds, and could clobber the
        // divergent team's real row. Skip it (mirroring `read_through`'s guard on the
        // lazy path) rather than seed a mis-keyed row; the JSON walk stays its truth.
        // The path axis needs no check here — each file imports under its own
        // `code_repo_path` — only the org/team the row is keyed on.
        if lock.org_slug != ctx.org_slug
            || team_name_to_db(lock.team_name.as_deref())
                != team_name_to_db(ctx.team_name.as_deref())
        {
            summary.skipped += 1;
            log::warn!(
                target: "content_engine::sync_state_store",
                "rebuild: skipping {} — stamped org={:?} team={:?} but located under org={:?} team={:?}",
                path.display(),
                lock.org_slug,
                lock.team_name,
                ctx.org_slug,
                ctx.team_name,
            );
            continue;
        }
        if keep_existing
            && matches!(
                store.read(&TargetKey::from_ctx(ctx, &lock.code_repo_path)),
                Ok(Some(_))
            )
        {
            summary.already_present += 1;
            continue;
        }
        // Seed `updated_at` from the lockfile's on-disk mtime — NOT import time — so
        // each target's recency survives a rebuild / backfill instead of collapsing
        // every target onto now. A target whose mtime can't be stat'd falls back to
        // `now` (a safe, recent value).
        let updated_at = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(system_time_to_unix_nanos)
            .unwrap_or_else(|_| now_unix_nanos());
        match store.upsert_with_updated_at(ctx.user_id, slug, &lock, updated_at) {
            Ok(()) => summary.imported += 1,
            Err(e) => {
                summary.skipped += 1;
                log::warn!(
                    target: "content_engine::sync_state_store",
                    "rebuild: skipping unimportable lockfile {}: {e}",
                    path.display(),
                );
            }
        }
    }
}

/// Serve a target from the store, importing it from its JSON lockfile on first
/// access.
///
/// The store is the source of truth once a row exists; a miss falls back to the
/// on-disk lockfile at [`lockfile_path`], imports it through the tolerant
/// [`Lockfile::read`] — so unknown-layer entries drop exactly as they do on a
/// live read — in one [`SyncStateStore::upsert`] transaction, then serves the
/// result.
///
/// - `Ok(Some(_))` — present: a store hit (the JSON is never touched) or a freshly
///   imported lockfile.
/// - `Ok(None)` — neither a store row nor a JSON file at [`lockfile_path`]: a
///   genuinely fresh target.
/// - `Err(SyncStateStoreError::Lockfile(_))` — the lockfile path could not be
///   computed or the file was unreadable/corrupt; the `content_engine::Error` is
///   preserved. Nothing is written.
/// - `Err(SyncStateStoreError::Data(_))` — the located lockfile tracks a different
///   repo than requested (a stale/relocated file or a path-hash collision), so
///   importing it would key the row under the wrong path; refused, not imported.
///
/// `user_id`/`org`/`team` come from `ctx`; the target is keyed by the ctx's full
/// `(org_slug, team_name, code_repo_path)`. Synchronous like the rest of the store,
/// so callers invoke it behind the blocking boundary, never on the UI thread.
///
/// SW-IS-3c: the store is now the authoritative substrate — this is a pure store
/// read, no JSON cold-import. A store miss is a genuinely fresh target (`Ok(None)`);
/// the one-time boot backfill (`backfill_json_once`) seeds any pre-store JSON before
/// meaningful reads. A target read before that backfill completes reprovisions once
/// from an empty base (correctness-preserving), rather than lazily importing JSON.
pub fn read_through(
    store: &SyncStateStore,
    ctx: &ResolverContext,
    code_repo_path: &Path,
) -> Result<Option<Lockfile>> {
    store.read(&TargetKey::from_ctx(ctx, code_repo_path))
}

/// Record `db_slug` (the DB-serialized [`SourceRepoSlug`]) as an alias of
/// `target_id` in `target_slug`, returning whether a NEW row was inserted
/// (`false` when the target already carried this slug). `INSERT OR IGNORE`, so a
/// re-provision under a known slug is a no-op. Accepts anything that derefs to a
/// [`Connection`] (a bare connection or a live transaction), so the `upsert` and
/// [`register_or_alias`](SyncStateStore::register_or_alias) write-seams share one
/// insert.
fn insert_slug_alias(conn: &Connection, target_id: i64, db_slug: &str) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO target_slug (target_id, source_repo_slug) VALUES (?1, ?2)",
        params![target_id, db_slug],
    )?;
    Ok(n > 0)
}

/// Resolve-or-create the parent `install_target` row for a write, returning its
/// `target_id`. The write-seam shared by the full [`upsert`](SyncStateStore::upsert)
/// and the targeted [`apply_entry_changes`](SyncStateStore::apply_entry_changes) so
/// both persist paths advance `updated_at` and record the slug alias **identically** — a
/// side effect can't be skipped on one path. Runs inside the caller's `IMMEDIATE`
/// transaction.
fn resolve_target_for_write(
    tx: &rusqlite::Transaction<'_>,
    lock: &Lockfile,
    source_repo_slug: &SourceRepoSlug,
    user_id: u64,
    updated_at_ns: i64,
) -> Result<i64> {
    let path = path_to_str(&lock.code_repo_path)?;
    let slug = slug_to_db(source_repo_slug)?;
    // Reject a target literally named `_none` — it would alias the no-team
    // sentinel row (see [`reject_reserved_team`]).
    reject_reserved_team(lock.team_name.as_deref())?;
    // SQLite integers are i64; a real user_id never approaches i64::MAX, but
    // reject an out-of-range value rather than silently wrapping it negative.
    let user_id = i64::try_from(user_id)
        .map_err(|_| SyncStateStoreError::Data(format!("user_id {user_id} exceeds i64")))?;
    let team = team_name_to_db(lock.team_name.as_deref());
    tx.execute(
        // The target is keyed on `(org_slug, team_name, code_repo_path)`, so a
        // conflict means those three already match — `DO UPDATE` refreshes only
        // the mutable columns. `updated_at` advances monotonically per row
        // (`MAX(existing, new)`); the reaper's freshest-ACROSS-teams recency is a
        // read-time `MAX` over a path's per-team rows (see `updated_at`), not a
        // cross-team collapse here. `COALESCE` guards a NULL (v1-healed) row,
        // since SQLite's scalar `max()` returns NULL if any argument is NULL.
        "INSERT INTO install_target (code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(org_slug, team_name, code_repo_path)
         DO UPDATE SET source_repo_slug = ?4, user_id = ?5,
                       updated_at = MAX(COALESCE(updated_at, ?6), ?6)",
        params![path, lock.org_slug, team, slug, user_id, updated_at_ns],
    )?;
    let target_id: i64 = tx.query_row(
        "SELECT id FROM install_target \
         WHERE org_slug = ?1 AND team_name = ?2 AND code_repo_path = ?3",
        params![lock.org_slug, team, path],
        |r| r.get(0),
    )?;

    // SW-IS-3c dual-slug (SW-EW-6): record this provision's slug as an alias of the
    // target. One clone attached under multiple owner/repo slugs shares ONE target
    // (same path/entries) but must enumerate under EACH slug, so aliases accumulate
    // here as a set. `INSERT OR IGNORE` keeps a re-provision under a known slug a
    // no-op; the `install_target.source_repo_slug` column above still tracks the
    // most-recent primary for callers that want a single representative slug.
    insert_slug_alias(tx, target_id, &slug)?;
    Ok(target_id)
}

/// Prepare the shared `materialized_entry` UPSERT statement (INSERT … ON CONFLICT DO
/// UPDATE over all 10 columns). ONE definition so the full-delta ([`write_entries_delta`])
/// and targeted ([`apply_entry_changes`](SyncStateStore::apply_entry_changes)) write
/// paths bind the identical column set — a schema/column change can't drift between two
/// copies. Pair with [`execute_entry_upsert`].
fn prepare_entry_upsert<'t>(tx: &'t rusqlite::Transaction<'t>) -> Result<rusqlite::Statement<'t>> {
    Ok(tx.prepare(
        "INSERT INTO materialized_entry
            (target_id, logical_name, dest_path, source_layer, source_path,
             last_materialized_hash, last_materialized_mtime_ns, projection_key,
             source_oid, last_materialized_size)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(target_id, logical_name, projection_key) DO UPDATE SET
             dest_path = excluded.dest_path,
             source_layer = excluded.source_layer,
             source_path = excluded.source_path,
             last_materialized_hash = excluded.last_materialized_hash,
             last_materialized_mtime_ns = excluded.last_materialized_mtime_ns,
             source_oid = excluded.source_oid,
             last_materialized_size = excluded.last_materialized_size",
    )?)
}

/// Prepare the shared single-row DELETE-by-identity statement (bind `target_id`,
/// `logical_name`, `projection_key`). ONE definition so the full-delta stale sweep
/// ([`write_entries_delta`]) and the targeted removal
/// ([`apply_entry_changes`](SyncStateStore::apply_entry_changes)) delete by the same
/// PK identity — `projection_key` is the stored `""` sentinel space ([`entry_identity`]).
fn prepare_entry_delete<'t>(tx: &'t rusqlite::Transaction<'t>) -> Result<rusqlite::Statement<'t>> {
    Ok(tx.prepare(
        "DELETE FROM materialized_entry \
         WHERE target_id = ?1 AND logical_name = ?2 AND projection_key = ?3",
    )?)
}

fn validate_remote_owner(
    ctx: &ResolverContext,
    identity: &super::destination::RemoteDestinationIdentity,
) -> Result<()> {
    reject_reserved_team(ctx.team_name.as_deref())?;
    if ctx.user_id != identity.owner_id
        || identity.machine_id.is_empty()
        || identity.server_id.is_empty()
        || identity.workspace_handle.is_empty()
    {
        return Err(SyncStateStoreError::Data(
            "invalid captured remote destination identity".into(),
        ));
    }
    Ok(())
}
fn validate_remote_entries(
    identity: &super::destination::RemoteDestinationIdentity,
    entries: &[super::destination::DestinationEntry],
) -> Result<()> {
    for entry in entries {
        if !matches!(&entry.dest_path, super::destination::DestinationReference::Remote { destination, .. } if destination == identity)
            || !entry.source_path.is_absolute()
        {
            return Err(SyncStateStoreError::Data(
                "remote applied entry crossed destination or source boundary".into(),
            ));
        }
    }
    Ok(())
}

trait StoredDestination: Sized {
    fn encode_destination(&self) -> Result<String>;
    fn decode_destination(value: String) -> Result<Self>;
}
impl StoredDestination for PathBuf {
    fn encode_destination(&self) -> Result<String> {
        path_to_str(self)
    }
    fn decode_destination(value: String) -> Result<Self> {
        Ok(value.into())
    }
}
impl StoredDestination for super::destination::DestinationReference {
    fn encode_destination(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
    fn decode_destination(value: String) -> Result<Self> {
        Ok(serde_json::from_str(&value)?)
    }
}

/// Bind + execute one entry against a [`prepare_entry_upsert`] statement. Exhaustive
/// destructure (no `..`) so adding a [`MaterializedEntry`] field is a compile error until
/// it is bound here — a new field can never silently fail to persist (AC-4). Identity
/// columns are the conflict key and never appear in the `DO UPDATE SET`.
fn execute_entry_upsert<D: StoredDestination>(
    stmt: &mut rusqlite::Statement<'_>,
    target_id: i64,
    e: &MaterializedEntry<D>,
) -> Result<()> {
    let MaterializedEntry {
        logical_name,
        dest_path,
        source_layer,
        source_path,
        last_materialized_hash,
        last_materialized_mtime_ns,
        projection_key,
        source_oid,
        last_materialized_size,
    } = e;
    let mtime = last_materialized_mtime_ns.to_be_bytes();
    stmt.execute(params![
        target_id,
        logical_name,
        dest_path.encode_destination()?,
        layer_to_db(*source_layer),
        path_to_str(source_path)?,
        &last_materialized_hash[..],
        &mtime[..],
        projection_key_to_db(projection_key.as_deref()),
        source_oid.as_deref(),
        last_materialized_size.map(|s| s as i64),
    ])?;
    Ok(())
}

/// The `(logical_name, projection_key)` identity of an entry, in the **stored** key
/// space — `projection_key` normalized through [`projection_key_to_db`] (a wave
/// entry's `None` and a `Some("")` both map to the `""` sentinel). This is exactly
/// the space the `materialized_entry` PK enforces uniqueness in, so keying the diff
/// here makes `None` vs `Some("")` collide the same way the DB would (rather than
/// looking distinct in domain space and then silently coalescing under `ON CONFLICT`).
/// One definition so every side of the delta keys entries identically.
fn entry_identity<D>(e: &MaterializedEntry<D>) -> (&str, &str) {
    (
        e.logical_name.as_str(),
        projection_key_to_db(e.projection_key.as_deref()),
    )
}

/// Apply the minimal set of `materialized_entry` row ops to bring `target_id`'s
/// stored rows to exactly `entries`, inside an existing transaction (SW-IS-F8):
/// INSERT rows new since the stored snapshot, UPDATE rows whose mutable columns
/// changed, DELETE rows no longer present, and touch nothing for a byte-identical
/// row. The generalization of the SW-IS-3b no-op — an unchanged set computes an
/// empty delta and issues zero row ops — so a one-file change is a one-row write
/// instead of a full DELETE-all + INSERT-all rewrite.
///
/// Entry identity is `(logical_name, projection_key)` (the row PK / SW-FX-14 fold:
/// a wave entry and a projection entry can share a `logical_name`). A duplicate
/// identity in `entries` is a malformed lockfile and is rejected before any write —
/// the same atomic rejection the old whole-set INSERT got from tripping the PK
/// mid-batch, now made explicit so it can never silently coalesce to one row.
fn write_entries_delta<D: StoredDestination + PartialEq>(
    tx: &rusqlite::Transaction<'_>,
    target_id: i64,
    entries: &[MaterializedEntry<D>],
) -> Result<()> {
    // Index the incoming set by stored-space identity, rejecting a duplicate (two rows
    // that would collide on the PK — a malformed lockfile). Borrows from `entries`.
    let mut incoming: HashMap<(&str, &str), &MaterializedEntry<D>> =
        HashMap::with_capacity(entries.len());
    for e in entries {
        if incoming.insert(entry_identity(e), e).is_some() {
            return Err(SyncStateStoreError::Data(format!(
                "lockfile for target {target_id} has two entries sharing identity (logical_name {:?}, projection_key {:?})",
                e.logical_name, e.projection_key
            )));
        }
    }

    // DELETE the stale rows, driven off the RAW stored identities — every
    // `materialized_entry` row for the target, INCLUDING any whose `source_layer` is an
    // unknown/legacy variant that the layer-decoding `load_entries` would drop. Deleting
    // from this raw set (rather than the decoded one) sweeps a stale unknown-layer row
    // exactly as the old wholesale `DELETE FROM materialized_entry WHERE target_id = ?1`
    // did — otherwise it would linger and still satisfy the raw `EXISTS(materialized_entry)`
    // non-empty check in [`SyncStateStore::teams_for_materialized_path`]. The stored
    // `projection_key` is already the `""` sentinel, i.e. the same key space as
    // [`entry_identity`], so it matches the incoming index directly. This is a distinct
    // read from the `load_entries` decode below (which drives change-detection and must
    // run after the DELETE); reading only the two key columns keeps it cheap.
    let stored_ids: Vec<(String, String)> = tx
        .prepare(
            "SELECT logical_name, projection_key FROM materialized_entry WHERE target_id = ?1",
        )?
        .query_map(params![target_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut delete_stmt = prepare_entry_delete(tx)?;
    for (logical_name, projection_key) in &stored_ids {
        if !incoming.contains_key(&(logical_name.as_str(), projection_key.as_str())) {
            delete_stmt.execute(params![target_id, logical_name, projection_key])?;
        }
    }

    // INSERT (new) + UPDATE (changed), unified via UPSERT: an identity absent from the
    // store inserts; a present-but-changed identity takes the DO UPDATE path; an
    // unchanged identity is skipped so it costs zero row ops (AC-1/AC-5). The
    // whole-struct `!=` is exhaustive over every field (`MaterializedEntry: Eq`), so a
    // new field can't silently escape change-detection. The snapshot is the layer-decoded
    // set read AFTER the DELETE (so it only decodes surviving rows); a stored unknown-layer
    // row is dropped here as on every read, and if the incoming set shares its identity the
    // UPSERT's `ON CONFLICT DO UPDATE` heals it to the valid layer in place.
    let stored = load_entries(tx, target_id)?;
    let stored_index: HashMap<(&str, &str), &MaterializedEntry<D>> =
        stored.iter().map(|e| (entry_identity(e), e)).collect();
    let mut write_stmt = prepare_entry_upsert(tx)?;
    for e in entries {
        if let Some(prev) = stored_index.get(&entry_identity(e)) {
            if *prev == e {
                continue; // byte-identical row — no op
            }
        }
        execute_entry_upsert(&mut write_stmt, target_id, e)?;
    }
    Ok(())
}

/// A `materialized_entry` row before its blob/layer fields are decoded into a
/// [`MaterializedEntry`]. Kept as an intermediate so the rusqlite row mapping
/// (which may only raise `rusqlite::Error`) is separate from the decode step
/// (which may raise an [`SyncStateStoreError::Data`] or drop an unknown layer).
struct RawEntry {
    logical_name: String,
    dest_path: String,
    source_layer: String,
    source_path: String,
    hash: Vec<u8>,
    mtime: Vec<u8>,
    // Stored NOT NULL (empty-string sentinel for a wave entry's `None`); decoded back
    // to `Option` via [`projection_key_from_db`] in `raw_to_entry`.
    projection_key: String,
    source_oid: Option<String>,
    last_materialized_size: Option<i64>,
}

/// Read a `materialized_entry` row (in [`ENTRY_COLUMNS`] order) into a [`RawEntry`].
fn row_to_raw(r: &Row<'_>) -> rusqlite::Result<RawEntry> {
    Ok(RawEntry {
        logical_name: r.get(0)?,
        dest_path: r.get(1)?,
        source_layer: r.get(2)?,
        source_path: r.get(3)?,
        hash: r.get(4)?,
        mtime: r.get(5)?,
        projection_key: r.get(6)?,
        source_oid: r.get(7)?,
        last_materialized_size: r.get(8)?,
    })
}

/// Decode a [`RawEntry`] into a [`MaterializedEntry`], or `Ok(None)` when the row
/// should be dropped on load — either its `source_layer` is an unknown/legacy
/// variant, or it is a retired install-layer entry (the SW-CP-31b load-side drain).
fn raw_to_entry<D: StoredDestination>(raw: RawEntry) -> Result<Option<MaterializedEntry<D>>> {
    let projection_key = projection_key_from_db(raw.projection_key);
    // SW-XS-8 load-time mapping (the one [`WireLayerId`] policy): a projection
    // entry tagged with a retired install tier loads as `TeamProjection`; a
    // wave-managed entry on a retired install tier is drained (no producer
    // since the install resolver was retired, SW-CP-31b) — a pure in-memory
    // drop that never touches the dest; the next `upsert` deletes the row. An
    // unknown name is dropped (SW-CP-7). `drained_install_row_sql` mirrors the
    // drain for `teams_for_materialized_path`.
    let Some(source_layer) = layer_from_db(&raw.source_layer, projection_key.as_deref()) else {
        log::warn!(
            target: "content_engine::sync_state_store",
            "dropping install-state entry '{}': retired or unknown source_layer '{}'",
            raw.logical_name,
            raw.source_layer
        );
        return Ok(None);
    };

    Ok(Some(MaterializedEntry {
        logical_name: raw.logical_name,
        dest_path: D::decode_destination(raw.dest_path)?,
        source_layer,
        source_path: PathBuf::from(raw.source_path),
        last_materialized_hash: hash_from_blob(&raw.hash)?,
        last_materialized_mtime_ns: mtime_from_blob(&raw.mtime)?,
        projection_key,
        source_oid: raw.source_oid,
        last_materialized_size: raw.last_materialized_size.map(|s| s as u64),
    }))
}

/// Load and decode all of a target's entries, dropping unknown-layer rows.
fn load_entries<D: StoredDestination>(
    conn: &Connection,
    target_id: i64,
) -> Result<Vec<MaterializedEntry<D>>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM materialized_entry WHERE target_id = ?1"
    ))?;
    let rows = stmt.query_map(params![target_id], row_to_raw)?;
    let mut out = Vec::new();
    for raw in rows {
        if let Some(entry) = raw_to_entry(raw?)? {
            out.push(entry);
        }
    }
    Ok(out)
}

/// Encode a [`LayerId`] as its bare variant name for storage. Exhaustive so a new
/// `LayerId` variant forces a decision here; the bare name (not the JSON-quoted
/// form) is what the tolerant [`WireLayerId`] read path expects.
fn layer_to_db(layer: LayerId) -> &'static str {
    match layer {
        LayerId::TeamProjection => "TeamProjection",
    }
}

/// The SQL boolean matching the load-side drain in [`WireLayerId::into_layer_id`]
/// in the STORED key space — the `projection_key_to_db(None)` empty-string
/// sentinel + the retired install tiers' stored names
/// ([`RETIRED_INSTALL_LAYERS`]). Operates on a `me` alias of
/// `materialized_entry`. The interpolated values are compile-fixed (no
/// external input).
fn drained_install_row_sql() -> String {
    let layers = RETIRED_INSTALL_LAYERS
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "me.projection_key = '{}' AND me.source_layer IN ({})",
        projection_key_to_db(None),
        layers
    )
}

/// Decode a stored `source_layer` name through the lockfile's tolerant
/// [`WireLayerId`] — the layer mapping and drop policy is reused, not
/// re-implemented.
fn layer_from_db(name: &str, projection_key: Option<&str>) -> Option<LayerId> {
    serde_json::from_value::<WireLayerId>(serde_json::Value::String(name.to_owned()))
        .ok()
        .and_then(|layer| layer.into_layer_id(projection_key))
}

/// A `u128` mtime does not fit SQLite's `i64`, so it is stored as a 16-byte
/// big-endian blob. Ordering is never needed (the sole consumer is an equality
/// gate), so big-endian is chosen only for arch-portability.
fn mtime_from_blob(b: &[u8]) -> Result<u128> {
    let arr: [u8; 16] = b.try_into().map_err(|_| {
        SyncStateStoreError::Data(format!("mtime blob must be 16 bytes, got {}", b.len()))
    })?;
    Ok(u128::from_be_bytes(arr))
}

/// Decode a 32-byte hash blob back into its fixed array.
fn hash_from_blob(b: &[u8]) -> Result<[u8; 32]> {
    b.try_into().map_err(|_| {
        SyncStateStoreError::Data(format!("hash blob must be 32 bytes, got {}", b.len()))
    })
}

/// Encode a [`SourceRepoSlug`] as its lossless serde JSON for storage. The slug
/// is a structured identity (owner/repo/provider/host), not a bare string, so
/// its serde form — not `Display`, which drops provider/host — is the key.
fn slug_to_db(slug: &SourceRepoSlug) -> Result<String> {
    Ok(serde_json::to_string(slug)?)
}

/// Decode a stored slug JSON back into a [`SourceRepoSlug`].
fn slug_from_db(s: &str) -> Result<SourceRepoSlug> {
    Ok(serde_json::from_str(s)?)
}

/// Encode a `team_name` for the `install_target.team_name` key column. The no-team
/// context maps to the `_none` sentinel (the same segment the JSON lockfile path
/// uses) rather than NULL, because SQLite treats NULLs as distinct in a `UNIQUE`
/// constraint — a NULL there would break `ON CONFLICT(org_slug, team_name,
/// code_repo_path)` for the no-team row, inserting a duplicate on every upsert. One
/// conversion point (mirrors [`slug_to_db`]), so no by-team query needs an
/// `IS NULL` special-case.
fn team_name_to_db(team_name: Option<&str>) -> &str {
    team_name.unwrap_or(super::lockfile::NO_TEAM_SEGMENT)
}

/// Reject a `team_name` that collides with the `_none` no-team sentinel
/// (case-insensitive, matching `validate_team_name` at the JSON boundary). The
/// store maps `None` → `_none` for the key column, so a target literally named
/// `_none` would alias the no-team row. Both seams that put a team into a key —
/// the write seam ([`SyncStateStore::upsert_with_updated_at`]) and the
/// by-target read/delete seam ([`TargetKey::seek_params`]) — route through this one
/// check so the reservation is enforced identically. `None` (the real no-team case)
/// is always allowed.
fn reject_reserved_team(team_name: Option<&str>) -> Result<()> {
    match team_name {
        Some(name) if name.eq_ignore_ascii_case(super::lockfile::NO_TEAM_SEGMENT) => {
            Err(SyncStateStoreError::Data(format!(
                "team_name {name:?} is reserved (collides with the no-team sentinel)"
            )))
        }
        _ => Ok(()),
    }
}

/// Decode a stored `team_name` back into the domain `Option<String>`, mapping the
/// `_none` sentinel to `None`. A team can never legitimately be named `_none`
/// (`validate_team_name` reserves it), so the mapping is unambiguous.
fn team_name_from_db(stored: &str) -> Option<String> {
    if stored == super::lockfile::NO_TEAM_SEGMENT {
        None
    } else {
        Some(stored.to_owned())
    }
}

/// Encode a `MaterializedEntry::projection_key` for the `materialized_entry.projection_key`
/// key column. A wave (install) entry has `None`; it maps to the empty-string sentinel
/// (never NULL) because the column is part of the row's PRIMARY KEY and SQLite treats
/// NULLs as distinct in a key — two wave entries sharing a `logical_name` would then
/// coexist instead of conflicting. A real projection key is always a non-empty
/// projection identity, so `""` is an unambiguous stand-in for `None`. Mirrors
/// [`team_name_to_db`].
fn projection_key_to_db(projection_key: Option<&str>) -> &str {
    projection_key.unwrap_or("")
}

/// Decode a stored `projection_key` back into the domain `Option<String>`, mapping the
/// empty-string sentinel to `None` (a wave entry).
fn projection_key_from_db(stored: String) -> Option<String> {
    if stored.is_empty() {
        None
    } else {
        Some(stored)
    }
}

/// Paths are stored as TEXT keys. A non-UTF-8 path is unrepresentable and is
/// rejected rather than lossily coerced — the same contract as the lockfile
/// (serde fails on a non-UTF-8 `PathBuf`), so a lossy key can never silently
/// collide two distinct paths onto one row.
fn path_to_str(p: &Path) -> Result<String> {
    p.to_str().map(str::to_owned).ok_or_else(|| {
        SyncStateStoreError::Data(format!("path is not valid UTF-8: {}", p.display()))
    })
}

/// UNIX-epoch nanoseconds for `now`, in the `updated_at` storage type. Real
/// wall-clock times sit far below `i64::MAX` (year ~2262), so the saturation only
/// guards a pathologically skewed clock rather than any real value.
fn now_unix_nanos() -> i64 {
    system_time_to_unix_nanos(SystemTime::now())
}

/// A [`SystemTime`] as UNIX-epoch nanoseconds, saturated into `i64`. A time at or
/// before the epoch (a badly-skewed clock) clamps to 0 — still a valid, oldest
/// recency rather than an error.
pub(crate) fn system_time_to_unix_nanos(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Reconstruct a [`SystemTime`] from stored UNIX-epoch nanoseconds, the inverse of
/// [`system_time_to_unix_nanos`].
fn unix_nanos_to_system_time(ns: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(ns.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::super::lockfile::lockfile_path;
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, SyncStateStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStateStore::open(dir.path().join("install.sqlite3")).unwrap();
        (dir, store)
    }

    /// A generic store fixture. Post-SW-CP-31b the store persists only projection
    /// entries (`projection_key = Some`), so the default fixture carries a
    /// projection key and survives the load-side drain. Tests that need a legacy
    /// wave/install row (`projection_key = None`) use [`install_row`].
    fn entry(logical: &str, layer: LayerId, mtime: u128) -> MaterializedEntry {
        let mut e = install_row(logical, layer, mtime);
        e.projection_key = Some("proj".to_string());
        e
    }

    /// A legacy wave/install row: `projection_key = None` on a resolver install
    /// layer. This is exactly the shape the SW-CP-31b load-side drain drops on
    /// read; only the drain test and the identity-semantics tests build one.
    fn install_row(logical: &str, layer: LayerId, mtime: u128) -> MaterializedEntry {
        MaterializedEntry {
            logical_name: logical.to_string(),
            dest_path: PathBuf::from(format!("/repo/{logical}")),
            source_layer: layer,
            source_path: PathBuf::from(format!("/src/{logical}")),
            last_materialized_hash: [7u8; 32],
            last_materialized_mtime_ns: mtime,
            projection_key: None,
            source_oid: None,
            last_materialized_size: Some(1234),
        }
    }

    /// SW-XS-8: tag stored rows with a retired install-tier name, as a
    /// pre-XS-8 binary wrote them (the live writer only writes
    /// `TeamProjection`).
    fn retag(store: &SyncStateStore, logical: &str, layer: &str) {
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE materialized_entry SET source_layer = ?1 WHERE logical_name = ?2",
                rusqlite::params![layer, logical],
            )
            .unwrap();
    }

    fn lockfile(path: &str, team: Option<&str>, entries: Vec<MaterializedEntry>) -> Lockfile {
        Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: "acme".to_string(),
            team_name: team.map(str::to_string),
            code_repo_path: PathBuf::from(path),
            entries,
        }
    }

    fn slug(owner: &str, repo: &str) -> SourceRepoSlug {
        SourceRepoSlug::new(owner, repo).unwrap()
    }

    /// A [`TargetKey`] matching the `lockfile()` helper's org (`"acme"`) — the
    /// read-side counterpart to `up()` for the team-scoped by-target reads. `team`
    /// must match the team the target was upserted under.
    fn key<'a>(path: &'a str, team: Option<&'a str>) -> TargetKey<'a> {
        TargetKey {
            org_slug: "acme",
            team_name: team,
            code_repo_path: Path::new(path),
        }
    }

    /// Upsert with a default slug for tests that don't exercise the slug axis.
    fn up(store: &SyncStateStore, user_id: u64, lock: &Lockfile) {
        store.upsert(user_id, &slug("acme", "api"), lock).unwrap();
    }

    fn remote_guard_fixture(
        store: &SyncStateStore,
        home: &Path,
    ) -> (
        ResolverContext,
        SourceRepoSlug,
        super::super::destination::RemoteDestinationIdentity,
        uuid::Uuid,
    ) {
        let context = ctx(home, Some("team-a"));
        let repo = slug("acme", "api");
        let identity = super::super::destination::RemoteDestinationIdentity {
            owner_id: context.user_id,
            machine_id: "machine".into(),
            server_id: "server".into(),
            workspace_handle: "workspace".into(),
        };
        let pending = store
            .prepare_destination_effect(
                &context,
                &repo,
                &identity,
                "selected.md",
                &json!({"bytes":"original"}),
            )
            .unwrap();
        assert!(remote_guard_path(&store.path).is_file());
        (context, repo, identity, pending.operation_id)
    }
    #[test]
    fn remote_journal_guard_keeps_same_pending_uuid_after_reopen() {
        let (dir, store) = store();
        let (context, repo, identity, id) = remote_guard_fixture(&store, dir.path());
        let reopened = SyncStateStore::open(&store.path).unwrap();
        let pending = reopened
            .prepare_destination_effect(
                &context,
                &repo,
                &identity,
                "selected.md",
                &json!({"bytes":"later changed source"}),
            )
            .unwrap();
        assert_eq!(pending.operation_id, id);
        assert_eq!(pending.intent, json!({"bytes":"original"}));
        assert!(!pending.newly_prepared);
    }
    #[test]
    fn remote_journal_guard_refuses_corruption_without_quarantine_or_replay() {
        let (dir, store) = store();
        let (context, repo, identity, _) = remote_guard_fixture(&store, dir.path());
        let evidence = b"corrupt remote effect database: preserve this evidence";
        std::fs::write(&store.path, evidence).unwrap();
        assert!(SyncStateStore::open(&store.path).is_err());
        assert_eq!(std::fs::read(&store.path).unwrap(), evidence);
        assert!(!append_suffix(&store.path, ".corrupt-0").exists());
        assert!(store
            .prepare_destination_effect(
                &context,
                &repo,
                &identity,
                "selected.md",
                &json!({"bytes":"must not replay"})
            )
            .is_err());
        assert_eq!(std::fs::read(&store.path).unwrap(), evidence);
    }
    #[test]
    fn remote_journal_guard_refuses_missing_and_replaced_database() {
        let (dir, store) = store();
        let (context, repo, identity, _) = remote_guard_fixture(&store, dir.path());
        let preserved = append_suffix(&store.path, ".preserved");
        std::fs::rename(&store.path, &preserved).unwrap();
        assert!(SyncStateStore::open(&store.path).is_err());
        assert!(store
            .prepare_destination_effect(&context, &repo, &identity, "selected.md", &json!({}))
            .is_err());
        assert!(
            !store.path.exists(),
            "missing authoritative database must not be recreated"
        );
        let fresh_path = dir.path().join("fresh.sqlite3");
        SyncStateStore::open(&fresh_path).unwrap();
        std::fs::copy(fresh_path, &store.path).unwrap();
        assert!(SyncStateStore::open(&store.path).is_err());
        assert!(store
            .prepare_destination_effect(&context, &repo, &identity, "selected.md", &json!({}))
            .is_err());
        assert!(preserved.is_file());
    }
    #[test]
    fn remote_journal_guard_does_not_change_local_only_corruption_recovery() {
        let (dir, store) = store();
        assert!(!remote_guard_path(&store.path).exists());
        std::fs::write(&store.path, b"local-only cache corruption").unwrap();
        SyncStateStore::open(&store.path).unwrap();
        assert!(dir.path().join("install.sqlite3.corrupt-0").is_file());
        assert!(!remote_guard_path(&store.path).exists());
    }

    /// SW-XS-9 re-review B: records of paths the repo now tracks are dropped;
    /// the others stay.
    #[test]
    fn xs9_forget_remote_entries_drops_only_the_named_paths() {
        use super::super::destination::*;
        let (dir, store) = store();
        let context = ctx(dir.path(), Some("team-a"));
        let repo = slug("acme", "api");
        let identity = RemoteDestinationIdentity {
            owner_id: context.user_id,
            machine_id: "machine-one".into(),
            server_id: "server".into(),
            workspace_handle: "remote-workspace-x".into(),
        };
        let remote = |name: &str| {
            entry(name, LayerId::TeamProjection, 1).map_destination(|_| {
                DestinationReference::Remote {
                    destination: identity.clone(),
                    relative: name.to_string().try_into().unwrap(),
                }
            })
        };
        let lock = Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: context.org_slug.clone(),
            team_name: context.team_name.clone(),
            code_repo_path: DestinationIdentity::Remote(identity.clone()),
            entries: vec![remote("now/tracked.md"), remote("still/managed.md")],
        };
        store.upsert_remote_target(&context, &repo, &lock).unwrap();
        store
            .forget_remote_entries(&context, &repo, &identity, &["now/tracked.md".into()])
            .unwrap();
        let names: Vec<String> = store
            .read_remote_target(&context, &repo, &identity)
            .unwrap()
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.logical_name)
            .collect();
        assert_eq!(names, vec!["still/managed.md".to_string()]);
    }

    #[test]
    fn destination_state_remote_rows_preserve_host_identity_and_never_enumerate_as_local() {
        use super::super::destination::*;
        let (dir, store) = store();
        let context = ctx(dir.path(), Some("team-a"));
        let repo = slug("acme", "api");
        let identity = RemoteDestinationIdentity {
            owner_id: context.user_id,
            machine_id: "machine-one".into(),
            server_id: "server".into(),
            workspace_handle: "same-worktree-path".into(),
        };
        let make =
            |identity: RemoteDestinationIdentity| Lockfile {
                schema_version: SCHEMA_VERSION,
                org_slug: context.org_slug.clone(),
                team_name: context.team_name.clone(),
                code_repo_path: DestinationIdentity::Remote(identity.clone()),
                entries: vec![entry("notes/file.md", LayerId::TeamProjection, 123)
                    .map_destination(|_| DestinationReference::Remote {
                        destination: identity,
                        relative: "notes/file.md".to_string().try_into().unwrap(),
                    })],
            };
        let first = make(identity.clone());
        let mut other = identity.clone();
        other.machine_id = "machine-two".into();
        let mut second = make(other.clone());
        second.entries[0].last_materialized_hash = [9; 32];
        store.upsert_remote_target(&context, &repo, &first).unwrap();
        store
            .upsert_remote_target(&context, &repo, &second)
            .unwrap();
        assert_eq!(
            store
                .read_remote_target(&context, &repo, &identity)
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            store.read_remote_target(&context, &repo, &other).unwrap(),
            Some(second)
        );
        assert!(store
            .enumerate_for_team(&context.org_slug, context.team_name.as_deref())
            .unwrap()
            .is_empty());
        assert!(store
            .enumerate_for_slug(&context.org_slug, context.team_name.as_deref(), &repo)
            .unwrap()
            .is_empty());
        let mut foreign = context.clone();
        foreign.user_id += 1;
        assert!(store
            .read_remote_target(&foreign, &repo, &identity)
            .is_err());
    }

    #[test]
    fn destination_state_migration_preserves_parent_id_entries_and_aliases() {
        let (dir, store) = store();
        let lock = lockfile(
            "/repo/local",
            Some("team-a"),
            vec![entry("notes/file.md", LayerId::TeamProjection, 123)],
        );
        up(&store, 42, &lock);
        store
            .register_or_alias(42, &slug("acme", "alias"), &lock)
            .unwrap();
        let conn = store.conn().unwrap();
        let old_id: i64 = conn
            .query_row("SELECT id FROM install_target", [], |row| row.get(0))
            .unwrap();
        // Remove the additive fields to reproduce an actual pre-namespace schema.
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute_batch("CREATE TABLE install_target_old (id INTEGER PRIMARY KEY, code_repo_path TEXT NOT NULL, org_slug TEXT NOT NULL, team_name TEXT NOT NULL, source_repo_slug TEXT NOT NULL, user_id INTEGER NOT NULL, updated_at INTEGER, UNIQUE(org_slug, team_name, code_repo_path));
            INSERT INTO install_target_old SELECT id, code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at FROM install_target;
            DROP TABLE install_target;
            ALTER TABLE install_target_old RENAME TO install_target;
            PRAGMA user_version = 5;").unwrap();
        drop(conn);
        let reopened = SyncStateStore::open(dir.path().join("install.sqlite3")).unwrap();
        assert_eq!(
            reopened.read(&key("/repo/local", Some("team-a"))).unwrap(),
            Some(lock)
        );
        assert_eq!(
            reopened
                .enumerate_for_slug("acme", Some("team-a"), &slug("acme", "alias"))
                .unwrap(),
            vec![PathBuf::from("/repo/local")]
        );
        let conn = reopened.conn().unwrap();
        assert_eq!(
            conn.query_row("SELECT id FROM install_target", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            old_id
        );
        assert!(!conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    // AC-1: open creates the DB + parent dirs, WAL asserted, schema applied idempotently.
    #[test]
    fn ac1_open_is_idempotent_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dirs/install.sqlite3");
        let s1 = SyncStateStore::open(&path).unwrap();
        assert!(path.exists(), "DB file and parent dirs created");
        // Re-open the same file: schema is CREATE ... IF NOT EXISTS, so this is a no-op.
        let _s2 = SyncStateStore::open(&path).unwrap();
        // The store is usable after a re-open.
        assert!(s1.read(&key("/nope", None)).unwrap().is_none());
    }

    // AC-2: round-trip a full target incl. an mtime > i64::MAX, the 32-byte hash,
    // and source_oid.
    #[test]
    fn ac2_round_trip_full_target() {
        let (_d, store) = store();
        let mut e = entry("skills/a.md", LayerId::TeamProjection, u128::MAX);
        e.projection_key = Some("story".to_string());
        e.source_oid = Some("deadbeef".to_string());
        e.last_materialized_size = None;
        let lock = lockfile("/repo/one", Some("team-a"), vec![e]);

        up(&store, 42, &lock);
        let got = store
            .read(&key("/repo/one", Some("team-a")))
            .unwrap()
            .expect("present");
        assert_eq!(got, lock, "structs round-trip via PartialEq");
        assert_eq!(
            got.entries[0].last_materialized_mtime_ns,
            u128::MAX,
            "u128::MAX mtime survives the BLOB(16) round-trip"
        );
    }

    // Transaction atomicity (duplicate-identity rejection and mid-write-loop failure)
    // is covered by `delta_rejects_duplicate_incoming_identity` and
    // `delta_write_loop_failure_rolls_back` (SW-IS-F8); the former subsumes what the
    // retired `ac3_mid_batch_failure_is_atomic` asserted once duplicates are rejected
    // before any write.

    // AC-4: enumerate returns all targets for (org, team) via one SELECT, carrying
    // the slug; the per-slug enumerate narrows by source repo; None = _none.
    #[test]
    fn ac4_enumerate_by_org_team() {
        let (_d, store) = store();
        let api = slug("acme", "api");
        let web = slug("acme", "web");
        store
            .upsert(1, &api, &lockfile("/repo/a", Some("team-a"), vec![]))
            .unwrap();
        store
            .upsert(1, &web, &lockfile("/repo/b", Some("team-a"), vec![]))
            .unwrap();
        store
            .upsert(1, &api, &lockfile("/repo/c", Some("team-b"), vec![]))
            .unwrap();
        store
            .upsert(1, &api, &lockfile("/repo/none", None, vec![]))
            .unwrap();

        let mut a: Vec<PathBuf> = store
            .enumerate_for_team("acme", Some("team-a"))
            .unwrap()
            .into_iter()
            .map(|(_slug, p)| p)
            .collect();
        a.sort();
        assert_eq!(a, vec![PathBuf::from("/repo/a"), PathBuf::from("/repo/b")]);
        // The slug is carried back on the team enumerate.
        let with_slugs = store.enumerate_for_team("acme", Some("team-a")).unwrap();
        assert!(with_slugs
            .iter()
            .any(|(s, p)| *s == web && p == Path::new("/repo/b")));

        let b = store.enumerate_for_team("acme", Some("team-b")).unwrap();
        assert_eq!(b, vec![(api.clone(), PathBuf::from("/repo/c"))]);

        let none = store.enumerate_for_team("acme", None).unwrap();
        assert_eq!(none, vec![(api.clone(), PathBuf::from("/repo/none"))]);

        // Per-slug enumerate narrows team-a to just the `api` repo, not `web`.
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("team-a"), &api)
                .unwrap(),
            vec![PathBuf::from("/repo/a")]
        );
    }

    // AC-5: N concurrent upserts of distinct targets all succeed, zero SQLITE_BUSY.
    #[test]
    fn ac5_concurrent_upserts_no_busy() {
        let (_d, store) = store();
        const THREADS: usize = 16;
        std::thread::scope(|s| {
            for i in 0..THREADS {
                let store = store.clone();
                s.spawn(move || {
                    let lock = lockfile(
                        &format!("/repo/{i}"),
                        Some("team"),
                        vec![entry("e", LayerId::TeamProjection, i as u128)],
                    );
                    // A busy-timeout failure would surface here as an Err.
                    store
                        .upsert(i as u64, &slug("acme", "api"), &lock)
                        .expect("no SQLITE_BUSY under busy_timeout");
                });
            }
        });
        let all = store.enumerate_for_team("acme", Some("team")).unwrap();
        assert_eq!(all.len(), THREADS, "every concurrent upsert committed");
    }

    // AC-6: the install-store default path differs from the orchestration coordinator path.
    #[test]
    fn ac6_default_path_differs_from_orchestration() {
        let home = Path::new("/tmp/cshome");
        let install = default_store_path(home);
        let orch = crate::coordinator::sqlite::default_store_path(home);
        assert_ne!(install, orch, "install store is a different DB file");
        assert!(install.ends_with("install.sqlite3"));
    }

    // AC-9: foreign keys are enforced — an entry OR a pack with a dangling
    // target_id is rejected, and the pragma is provably live on a fresh conn.
    #[test]
    fn ac9_foreign_keys_enforced() {
        let (_d, store) = store();
        let conn = store.conn().unwrap();

        // The store's own conn() must have foreign_keys ON, not just error by luck.
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk_on, 1, "foreign_keys pragma must be live on conn()");

        // A dangling entry FK is rejected — pinned to the specific FK violation so
        // an unrelated constraint could not make this pass by accident.
        let entry_err = conn
            .execute(
                "INSERT INTO materialized_entry
                    (target_id, logical_name, dest_path, source_layer, source_path,
                     last_materialized_hash, last_materialized_mtime_ns)
                 VALUES (999, 'x', '/d', 'TeamRepo', '/s', X'00', X'00')",
                [],
            )
            .expect_err("FK to a non-existent target_id must be rejected");
        assert!(
            matches!(
                entry_err,
                rusqlite::Error::SqliteFailure(e, _)
                    if e.code == rusqlite::ErrorCode::ConstraintViolation
            ),
            "expected a constraint violation, got {entry_err:?}"
        );
    }

    // AC-10: the enumerate SELECT is index-backed, not a full scan.
    #[test]
    fn ac10_enumerate_uses_index() {
        let (_d, store) = store();
        let conn = store.conn().unwrap();
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT code_repo_path FROM install_target \
                 WHERE org_slug = ?1 AND team_name = ?2",
                params!["acme", "team-a"],
                |r| r.get(3),
            )
            .unwrap();
        // Accept a plain or COVERING index seek — the composite `UNIQUE` auto-index
        // covers the `(org_slug, team_name)` prefix, so SQLite may pick either.
        assert!(
            (plan.contains("USING INDEX") || plan.contains("USING COVERING INDEX"))
                && !plan.contains("SCAN install_target"),
            "per-team enumerate must SEARCH via index, plan was: {plan}"
        );

        // The per-slug enumerate uses the full three-column key, also index-backed.
        let slug_plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT code_repo_path FROM install_target \
                 WHERE org_slug = ?1 AND team_name = ?2 AND source_repo_slug = ?3",
                params!["acme", "team-a", "{}"],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            (slug_plan.contains("USING INDEX") || slug_plan.contains("USING COVERING INDEX"))
                && !slug_plan.contains("SCAN install_target"),
            "per-slug enumerate must SEARCH via index, plan was: {slug_plan}"
        );
    }

    // AC-11: a fresh open stamps the store schema_version (PRAGMA user_version).
    #[test]
    fn ac11_fresh_open_stamps_user_version() {
        let (_d, store) = store();
        let conn = store.conn().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, STORE_SCHEMA_VERSION);
    }

    // AC-3: `upsert` stamps `updated_at` = now, and `updated_at()` reads it back as a
    // recent SystemTime. A target with no row reads None (fail-safe not-reapable).
    #[test]
    fn updated_at_is_stamped_on_upsert_and_absent_reads_none() {
        let (_d, store) = store();
        let before = SystemTime::now();
        up(&store, 7, &lockfile("/repo/one", Some("team-a"), vec![]));
        let stamped = store.updated_at(Path::new("/repo/one")).unwrap();
        let stamped = stamped.expect("upsert stamps updated_at");
        assert!(
            stamped >= before - Duration::from_secs(5),
            "updated_at is stamped ~now, not the epoch"
        );
        assert!(
            store.updated_at(Path::new("/absent")).unwrap().is_none(),
            "an absent target has no recency → not reapable"
        );
    }

    // AC-4 primitive: the backfill seeds an explicit recency (each lockfile's real
    // on-disk mtime) instead of import-time now, so the reaper's age ordering
    // survives the cutover. Verify the explicit timestamp round-trips.
    #[test]
    fn upsert_with_updated_at_seeds_explicit_recency() {
        let (_d, store) = store();
        // 2001-09-09T01:46:40Z — a fixed, clearly-not-now recency.
        let seeded = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        store
            .upsert_with_updated_at(
                7,
                &slug("acme", "api"),
                &lockfile("/repo/old", Some("team-a"), vec![]),
                system_time_to_unix_nanos(seeded),
            )
            .unwrap();
        assert_eq!(
            store.updated_at(Path::new("/repo/old")).unwrap(),
            Some(seeded)
        );
    }

    // AC-1/AC-3: a repo attached under several teams is one row per team (composite
    // key), each with its own `updated_at`; `updated_at(path)` is a read-time `MAX`
    // across them, so the reaper's freshest-across-teams recency survives a later
    // upsert of an OLDER team — it can't be dragged back by whichever team the
    // backfill loop visited last, which would risk reaping a fresh worktree. Each
    // row's own `updated_at` also advances monotonically on a real (now) wave.
    #[test]
    fn upsert_keeps_the_freshest_updated_at_across_teams() {
        let (_d, store) = store();
        let s = slug("acme", "api");
        let repo = "/repo/shared";
        let newer = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let older = UNIX_EPOCH + Duration::from_secs(1_000_000_000);

        // Team A (newer) imported first, then team B (older) — last-write must NOT win.
        store
            .upsert_with_updated_at(
                7,
                &s,
                &lockfile(repo, Some("team-a"), vec![]),
                system_time_to_unix_nanos(newer),
            )
            .unwrap();
        store
            .upsert_with_updated_at(
                7,
                &s,
                &lockfile(repo, Some("team-b"), vec![]),
                system_time_to_unix_nanos(older),
            )
            .unwrap();
        assert_eq!(
            store.updated_at(Path::new(repo)).unwrap(),
            Some(newer),
            "the freshest team's recency survives a later older upsert"
        );

        // A real wave (now) always advances past a seeded historical value.
        let before = SystemTime::now();
        store
            .upsert(7, &s, &lockfile(repo, Some("team-a"), vec![]))
            .unwrap();
        assert!(store.updated_at(Path::new(repo)).unwrap().unwrap() >= before);
    }

    // AC-4: the marker-gated backfill (which now delegates the config walk + import
    // to `import_all_from_json`) seeds `updated_at` from each lockfile's real on-disk
    // mtime — not import-time now — so the reaper's oldest-first ordering survives the
    // cutover; and it is marker-gated (a second run is a no-op).
    #[test]
    fn ac4_backfill_seeds_file_mtime_and_is_marker_gated() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = SyncStateStore::open(default_store_path(home)).unwrap();

        seed_org_config(home, 42, "acme/ctx", "team-a", "acme", "api");
        let c = ctx(home, Some("team-a"));
        let s = slug("acme", "api");

        // Two lockfiles on disk with DISTINCT, old mtimes (older vs newer).
        let older = "/code/older";
        let newer = "/code/newer";
        for (repo, secs) in [(older, 1_000_000_000u64), (newer, 1_600_000_000u64)] {
            let lock = lockfile_for(repo, "team-a", &c.org_slug, vec![]);
            let path = write_lockfile_on_disk(&c, &s, &lock);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        }

        assert!(
            store.backfill_json_once(home, 42).unwrap(),
            "first run imports"
        );

        let t_older = store.updated_at(Path::new(older)).unwrap().unwrap();
        let t_newer = store.updated_at(Path::new(newer)).unwrap().unwrap();
        assert_eq!(t_older, UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        assert_eq!(t_newer, UNIX_EPOCH + Duration::from_secs(1_600_000_000));
        assert!(
            t_older < t_newer,
            "seeded from file mtime → the reaper's age ordering survives the cutover"
        );

        // Marker-gated: a second run does not re-import.
        assert!(
            !store.backfill_json_once(home, 42).unwrap(),
            "second run is gated by the completion marker"
        );
    }

    // SW-XS-17: the one-time backfill (now run from the launch reconcile) never
    // overwrites a target the store already holds — the store row is newer than
    // the JSON lockfile it once came from. A missing target is still imported, and
    // the marker is armed even when every target was already present.
    #[test]
    fn backfill_skips_a_target_that_already_has_a_store_row() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = SyncStateStore::open(default_store_path(home)).unwrap();
        seed_org_config(home, 42, "acme/ctx", "team-a", "acme", "api");
        let c = ctx(home, Some("team-a"));
        let s = slug("acme", "api");

        let kept = "/code/kept";
        let fresh = "/code/fresh";
        for repo in [kept, fresh] {
            let lock = lockfile_for(repo, "team-a", &c.org_slug, vec![]);
            let path = write_lockfile_on_disk(&c, &s, &lock);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000_000))
                .unwrap();
        }
        // The store already holds `kept`, written later than its JSON lockfile.
        let row_time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        store
            .upsert_with_updated_at(
                42,
                &s,
                &lockfile_for(kept, "team-a", &c.org_slug, vec![]),
                system_time_to_unix_nanos(row_time),
            )
            .unwrap();

        assert!(store.backfill_json_once(home, 42).unwrap());

        assert_eq!(
            store.updated_at(Path::new(kept)).unwrap(),
            Some(row_time),
            "the existing row is not overwritten from JSON"
        );
        assert_eq!(
            store.updated_at(Path::new(fresh)).unwrap(),
            Some(UNIX_EPOCH + Duration::from_secs(1_000_000_000)),
            "a target with no row is imported"
        );
    }

    // SW-IS-3c: a pre-retirement v1 store carries a key shape that can no longer be
    // migrated in place (there is no JSON left to rebuild from). Opening it is a
    // recoverable fault: the un-migratable file is quarantined (bytes preserved) and a
    // fresh empty store takes its place, stamped at the current version.
    #[test]
    fn pre_current_version_v1_store_is_quarantined_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        {
            // A v1 DB: install_target WITHOUT updated_at, stamped user_version = 1,
            // carrying one pre-existing row.
            let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE install_target (
                     id INTEGER PRIMARY KEY,
                     code_repo_path TEXT NOT NULL UNIQUE,
                     org_slug TEXT NOT NULL,
                     team_name TEXT,
                     source_repo_slug TEXT NOT NULL,
                     user_id INTEGER NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO install_target (code_repo_path, org_slug, team_name, source_repo_slug, user_id)
                 VALUES ('/repo/pre', 'acme', 'team-a', '{}', 7)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1i64).unwrap();
        }
        // Drop the WAL sidecars so the whole v1 state lives in the main file, and
        // capture the original bytes to prove quarantine preserves (never deletes) them.
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(append_suffix(&path, suffix));
        }
        let original = std::fs::read(&path).unwrap();

        let store = SyncStateStore::open(&path).unwrap();

        // The un-migratable v1 file was quarantined (moved aside, bytes intact).
        let quarantined = dir.path().join("install.sqlite3.corrupt-0");
        assert_eq!(
            std::fs::read(&quarantined).unwrap(),
            original,
            "the original v1 bytes are preserved at the quarantine sibling, not dropped"
        );

        // The store reopened at `path` is a fresh empty store: the pre-existing row is
        // gone (not carried forward).
        assert!(
            store
                .read(&key("/repo/pre", Some("team-a")))
                .unwrap()
                .is_none(),
            "the fresh store starts empty — the pre-v3 row was not migrated"
        );

        // The fresh file is stamped at the current schema version.
        let conn = store.conn().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            v, STORE_SCHEMA_VERSION,
            "fresh store stamped at the current version"
        );

        // And it is usable: a fresh upsert reads back.
        up(&store, 7, &lockfile("/repo/pre", Some("team-a"), vec![]));
        assert!(store
            .read(&key("/repo/pre", Some("team-a")))
            .unwrap()
            .is_some());
    }

    // SW-IS-3c AC-2: when `open` quarantines a corrupt file and rebuilds it empty, it
    // MUST clear the backfill-completion marker so `backfill_json_once` re-arms — a stale
    // "done" marker over an empty store would strand any surviving JSON, and the boot
    // deleter (gated on the same marker) would delete the JSON the reset store needs.
    #[test]
    fn quarantine_clears_backfill_marker_to_rearm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");

        // Pre-arm the marker as if a prior backfill had completed.
        std::fs::write(backfill_marker_path_for(&path), b"").unwrap();
        assert!(
            backfill_marker_path_for(&path).exists(),
            "precondition: marker present"
        );

        // Make the file unopenable as a DB (triggers the NotADatabase corruption path).
        std::fs::write(&path, b"not a database").unwrap();

        // Open must succeed (quarantine + rebuild empty).
        let store = SyncStateStore::open(&path).unwrap();
        // Sanity: the rebuilt store is usable.
        assert!(store.conn().is_ok());

        // The marker is cleared → the JSON backfill is re-armed on the rebuilt store.
        assert!(
            !backfill_marker_path_for(&path).exists(),
            "recovery must clear the backfill marker so backfill re-arms"
        );

        // The corrupt bytes were quarantined aside (not deleted).
        assert!(
            dir.path().join("install.sqlite3.corrupt-0").exists(),
            "the corrupt file is preserved at the .corrupt-0 sibling"
        );
    }

    // AC-12: source_layer round-trips through the reused WireLayerId tolerance, and
    // the stored form is the bare variant name (not JSON-quoted).
    #[test]
    fn ac12_layer_encoding_and_drop_on_unknown() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/l",
                Some("t"),
                vec![entry("k", LayerId::TeamProjection, 1)],
            ),
        );

        // Written form is the bare name, not `"\"TeamProjection\""`.
        let conn = store.conn().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT source_layer FROM materialized_entry LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "TeamProjection");

        // SW-XS-8 load-time mapping: a projection row a pre-XS-8 binary tagged
        // with a retired install tier loads as team content.
        for retired in ["TeamRepo", "TeamWorkspace"] {
            conn.execute("UPDATE materialized_entry SET source_layer = ?1", [retired])
                .unwrap();
            let got = store.read(&key("/repo/l", Some("t"))).unwrap().unwrap();
            assert_eq!(got.entries.len(), 1, "{retired}");
            assert_eq!(got.entries[0].source_layer, LayerId::TeamProjection);
        }

        // An unknown stored layer drops the entry on read (parity with Lockfile::read).
        conn.execute("UPDATE materialized_entry SET source_layer = 'Core'", [])
            .unwrap();
        let got = store.read(&key("/repo/l", Some("t"))).unwrap().unwrap();
        assert!(
            got.entries.is_empty(),
            "unknown-layer entry dropped on load"
        );
    }

    // Entry identity is `(logical_name, projection_key)`, not `logical_name` alone, so
    // two projection entries that differ only by `projection_key` coexist as distinct
    // rows under one `logical_name`. (Post-SW-CP-31b the store persists only projection
    // entries; the retired wave-entry arm of this invariant is covered by the load-side
    // drain test below.)
    #[test]
    fn two_projection_entries_share_a_logical_name() {
        let (_d, store) = store();
        let mut docs = entry("skills/x.md", LayerId::TeamProjection, 1);
        docs.projection_key = Some("docs".to_string());
        let mut runbooks = entry("skills/x.md", LayerId::TeamProjection, 2);
        runbooks.projection_key = Some("runbooks".to_string());
        up(
            &store,
            1,
            &lockfile("/repo/pw", Some("t"), vec![docs, runbooks]),
        );

        let got = store
            .read(&key("/repo/pw", Some("t")))
            .unwrap()
            .expect("present");
        assert_eq!(
            got.entries.len(),
            2,
            "two projection keys coexist under one logical_name"
        );
        assert!(
            got.entries
                .iter()
                .any(|e| e.projection_key.as_deref() == Some("docs")),
            "docs projection entry present"
        );
        assert!(
            got.entries
                .iter()
                .any(|e| e.projection_key.as_deref() == Some("runbooks")),
            "runbooks projection entry present"
        );
    }

    // SW-CP-31b (DW-CP31-2): the load-side drain drops a legacy wave/install row
    // (`projection_key = None` on a resolver install layer) on read — a pure
    // in-memory drop that leaves the row's on-disk dest untouched (`content/` owns
    // that path now). A projection entry (`projection_key = Some`) at the same
    // target survives even though it shares the `TeamWorkspace` layer tag.
    #[test]
    fn read_drains_legacy_install_rows_without_touching_dest() {
        let (dir, store) = store();
        // A real on-disk dest for the legacy install row — the drain must NOT delete it.
        let dest = dir.path().join("legacy_dest.md");
        std::fs::write(&dest, b"content-owned bytes").unwrap();

        let mut legacy_ws = install_row("skills/ws.md", LayerId::TeamProjection, 1);
        legacy_ws.dest_path = dest.clone();
        let legacy_repo = install_row("skills/repo.md", LayerId::TeamProjection, 2);
        let mut projection = entry("docs/keep.md", LayerId::TeamProjection, 3);
        projection.projection_key = Some("docs".to_string());

        up(
            &store,
            1,
            &lockfile(
                "/repo/drain",
                Some("t"),
                vec![legacy_ws, legacy_repo, projection],
            ),
        );
        retag(&store, "skills/ws.md", "TeamWorkspace");
        retag(&store, "skills/repo.md", "TeamRepo");
        retag(&store, "docs/keep.md", "TeamWorkspace");

        let got = store
            .read(&key("/repo/drain", Some("t")))
            .unwrap()
            .expect("target present");
        assert_eq!(
            got.entries.len(),
            1,
            "both legacy install rows (TeamWorkspace + TeamRepo, projection_key=None) drained on read"
        );
        assert_eq!(
            got.entries[0].projection_key.as_deref(),
            Some("docs"),
            "the projection entry survives the drain"
        );
        assert_eq!(got.entries[0].source_layer, LayerId::TeamProjection);
        assert!(
            dest.exists(),
            "the drain is a pure in-memory drop — the legacy row's dest is untouched"
        );
    }

    // SW-CP-31b: `teams_for_materialized_path` must mirror the load-side drain — a
    // target whose ONLY rows are drained legacy install rows reads back EMPTY, so it
    // must NOT be reported as materialized; a target with a live projection entry MUST.
    #[test]
    fn teams_for_materialized_path_mirrors_the_load_side_drain() {
        let (_d, store) = store();
        let sl = slug("acme", "api");

        // team-legacy: only drained install rows (projection_key = None on install layers).
        up(
            &store,
            1,
            &lockfile(
                "/repo/drainpath",
                Some("team-legacy"),
                vec![
                    install_row("skills/a.md", LayerId::TeamProjection, 1),
                    install_row("skills/b.md", LayerId::TeamProjection, 2),
                ],
            ),
        );
        retag(&store, "skills/a.md", "TeamRepo");
        retag(&store, "skills/b.md", "TeamWorkspace");
        // team-live: a real projection entry (survives the drain).
        let mut projection = entry("docs/keep.md", LayerId::TeamProjection, 3);
        projection.projection_key = Some("docs".to_string());
        up(
            &store,
            1,
            &lockfile("/repo/drainpath", Some("team-live"), vec![projection]),
        );

        // read() confirms the drain shape: the legacy-only target is empty, the other lives.
        assert!(
            store
                .read(&key("/repo/drainpath", Some("team-legacy")))
                .unwrap()
                .unwrap()
                .entries
                .is_empty(),
            "legacy-only target decodes to empty (drain)"
        );
        assert_eq!(
            store
                .read(&key("/repo/drainpath", Some("team-live")))
                .unwrap()
                .unwrap()
                .entries
                .len(),
            1,
            "projection target keeps its entry"
        );

        // Only the live team is reported — the drained-only team is excluded, matching read().
        let teams = store
            .teams_for_materialized_path("acme", &sl, Path::new("/repo/drainpath"))
            .unwrap();
        assert_eq!(
            teams,
            vec![Some("team-live".to_string())],
            "a target whose only rows are drained legacy install rows is NOT reported; \
             the projection-entry target IS"
        );
    }

    // SW-IS-3b (DW-IS3ab-1): `delete_all_for_path` removes every team's row at a
    // worktree path (the reap / boot-prune prune), unlike the team-scoped `delete`.
    #[test]
    fn delete_all_for_path_removes_every_team_row() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/w",
                Some("team-a"),
                vec![entry("a", LayerId::TeamProjection, 1)],
            ),
        );
        up(
            &store,
            1,
            &lockfile(
                "/repo/w",
                Some("team-b"),
                vec![entry("b", LayerId::TeamProjection, 2)],
            ),
        );
        assert_eq!(store.delete_all_for_path(Path::new("/repo/w")).unwrap(), 2);
        assert!(store
            .read(&key("/repo/w", Some("team-a")))
            .unwrap()
            .is_none());
        assert!(store
            .read(&key("/repo/w", Some("team-b")))
            .unwrap()
            .is_none());
        // Idempotent: a second call removes nothing.
        assert_eq!(store.delete_all_for_path(Path::new("/repo/w")).unwrap(), 0);
    }

    // SW-IS-F6 no-op change-detection: re-upserting a byte-identical lockfile must NOT
    // rewrite the child `materialized_entry` rows (the write-heavy DELETE+INSERT that
    // drove F6's concurrent-provision contention) — only the parent `updated_at`
    // advances. Probe with a persistent trigger-counter on `materialized_entry` (fires
    // across the store's per-op connections); a rowid check would be unreliable since
    // SQLite reuses rowids after a DELETE-all.
    #[test]
    fn upsert_noop_skips_the_child_row_rewrite() {
        let (_d, store) = store();
        let lf = lockfile(
            "/repo/noop",
            Some("t"),
            vec![
                entry("a", LayerId::TeamProjection, 1),
                entry("b", LayerId::TeamProjection, 2),
            ],
        );
        up(&store, 1, &lf);

        // Persist a write-counter on the child table (triggers live in the DB schema, so
        // they fire for the store's own upsert connection too).
        store
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _rewrite_probe (n INTEGER NOT NULL);\n\
                 INSERT INTO _rewrite_probe VALUES (0);\n\
                 CREATE TRIGGER _probe_ins AFTER INSERT ON materialized_entry \
                     BEGIN UPDATE _rewrite_probe SET n = n + 1; END;\n\
                 CREATE TRIGGER _probe_del AFTER DELETE ON materialized_entry \
                     BEGIN UPDATE _rewrite_probe SET n = n + 1; END;",
            )
            .unwrap();
        let probe = |store: &SyncStateStore| -> i64 {
            store
                .conn()
                .unwrap()
                .query_row("SELECT n FROM _rewrite_probe", [], |r| r.get(0))
                .unwrap()
        };

        // Re-upsert the IDENTICAL lockfile → no-op detection skips the child rewrite.
        up(&store, 1, &lf);
        assert_eq!(
            probe(&store),
            0,
            "a byte-identical upsert must not touch child rows (F6 no-op)"
        );
        // updated_at still advances (recency the reaper needs) even on the no-op.
        assert!(store.updated_at(Path::new("/repo/noop")).unwrap().is_some());

        // Control: a CHANGED upsert DOES rewrite the child rows (trigger fires).
        let lf2 = lockfile(
            "/repo/noop",
            Some("t"),
            vec![entry("a", LayerId::TeamProjection, 99)],
        );
        up(&store, 1, &lf2);
        assert!(
            probe(&store) > 0,
            "a changed upsert rewrites the child rows"
        );
    }

    // SW-IS-F8 AC-1/AC-5: a delta upsert issues exactly one child-row op per changed
    // file (INSERT for an add, UPDATE for an edit, DELETE for a removal), not N — and
    // zero for an unchanged reprovision. Probes with per-kind persistent trigger
    // counters on `materialized_entry` (they live in the DB schema, so they fire
    // across the store's per-op connections), proving the write amplification is gone.
    #[test]
    fn delta_upsert_issues_one_row_op_per_changed_file() {
        let (_d, store) = store();
        // A large manifest, so a full DELETE-all + INSERT-all would be ~2N row ops.
        const N: usize = 500;
        let entries: Vec<MaterializedEntry> = (0..N)
            .map(|i| {
                entry(
                    &format!("skills/f{i}.md"),
                    LayerId::TeamProjection,
                    i as u128,
                )
            })
            .collect();
        up(
            &store,
            1,
            &lockfile("/repo/big", Some("t"), entries.clone()),
        );

        store
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _ops (ins INTEGER NOT NULL, upd INTEGER NOT NULL, del INTEGER NOT NULL);\n\
                 INSERT INTO _ops VALUES (0, 0, 0);\n\
                 CREATE TRIGGER _op_ins AFTER INSERT ON materialized_entry \
                     BEGIN UPDATE _ops SET ins = ins + 1; END;\n\
                 CREATE TRIGGER _op_upd AFTER UPDATE ON materialized_entry \
                     BEGIN UPDATE _ops SET upd = upd + 1; END;\n\
                 CREATE TRIGGER _op_del AFTER DELETE ON materialized_entry \
                     BEGIN UPDATE _ops SET del = del + 1; END;",
            )
            .unwrap();
        let ops = |store: &SyncStateStore| -> (i64, i64, i64) {
            store
                .conn()
                .unwrap()
                .query_row("SELECT ins, upd, del FROM _ops", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap()
        };
        let reset = |store: &SyncStateStore| {
            store
                .conn()
                .unwrap()
                .execute("UPDATE _ops SET ins = 0, upd = 0, del = 0", [])
                .unwrap();
        };

        // No-op: an identical reprovision issues zero child-row ops.
        up(
            &store,
            1,
            &lockfile("/repo/big", Some("t"), entries.clone()),
        );
        assert_eq!(
            ops(&store),
            (0, 0, 0),
            "unchanged reprovision writes zero child rows"
        );

        // Add one file → exactly one INSERT.
        reset(&store);
        let mut added = entries.clone();
        added.push(entry("skills/NEW.md", LayerId::TeamProjection, 1));
        up(&store, 1, &lockfile("/repo/big", Some("t"), added.clone()));
        assert_eq!(ops(&store), (1, 0, 0), "adding one file is one INSERT");

        // Edit one file → exactly one UPDATE.
        reset(&store);
        let mut edited = added.clone();
        edited[0].last_materialized_mtime_ns = 999_999;
        up(&store, 1, &lockfile("/repo/big", Some("t"), edited.clone()));
        assert_eq!(ops(&store), (0, 1, 0), "editing one file is one UPDATE");

        // Remove one file → exactly one DELETE.
        reset(&store);
        let mut removed = edited.clone();
        removed.pop(); // drop skills/NEW.md
        up(&store, 1, &lockfile("/repo/big", Some("t"), removed));
        assert_eq!(ops(&store), (0, 0, 1), "removing one file is one DELETE");
    }

    /// SW-IS-F9 AC-1: `apply_entry_changes` touches ONLY the passed rows — one child-row
    /// op per changed identity, regardless of the (large) manifest size — and issues no
    /// full-table read/rewrite of the rest. Probed with the same per-kind persistent
    /// trigger counters as the F8 delta test; the KEY assertion is that a 1-file change
    /// against a 500-row manifest is exactly one row op (O(touched), not O(manifest)).
    #[test]
    fn f9_apply_entry_changes_touches_only_passed_rows() {
        let (_d, store) = store();
        const N: usize = 500;
        let entries: Vec<MaterializedEntry> = (0..N)
            .map(|i| {
                entry(
                    &format!("skills/f{i}.md"),
                    LayerId::TeamProjection,
                    i as u128,
                )
            })
            .collect();
        let ident = lockfile("/repo/big", Some("t"), Vec::new());
        up(
            &store,
            1,
            &lockfile("/repo/big", Some("t"), entries.clone()),
        );

        store
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _ops (ins INTEGER NOT NULL, upd INTEGER NOT NULL, del INTEGER NOT NULL);\n\
                 INSERT INTO _ops VALUES (0, 0, 0);\n\
                 CREATE TRIGGER _op_ins AFTER INSERT ON materialized_entry \
                     BEGIN UPDATE _ops SET ins = ins + 1; END;\n\
                 CREATE TRIGGER _op_upd AFTER UPDATE ON materialized_entry \
                     BEGIN UPDATE _ops SET upd = upd + 1; END;\n\
                 CREATE TRIGGER _op_del AFTER DELETE ON materialized_entry \
                     BEGIN UPDATE _ops SET del = del + 1; END;",
            )
            .unwrap();
        let ops = |store: &SyncStateStore| -> (i64, i64, i64) {
            store
                .conn()
                .unwrap()
                .query_row("SELECT ins, upd, del FROM _ops", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap()
        };
        let reset = |store: &SyncStateStore| {
            store
                .conn()
                .unwrap()
                .execute("UPDATE _ops SET ins = 0, upd = 0, del = 0", [])
                .unwrap();
        };

        // Add one file → exactly one INSERT (not 501).
        let new_entry = entry("skills/NEW.md", LayerId::TeamProjection, 1);
        store
            .apply_entry_changes(1, &slug("acme", "api"), &ident, &[&new_entry], &[])
            .unwrap();
        assert_eq!(ops(&store), (1, 0, 0), "adding one file is one INSERT");

        // Edit one existing file → exactly one UPDATE (ON CONFLICT DO UPDATE).
        reset(&store);
        let mut edited = entries[0].clone();
        edited.last_materialized_mtime_ns = 999_999;
        store
            .apply_entry_changes(1, &slug("acme", "api"), &ident, &[&edited], &[])
            .unwrap();
        assert_eq!(ops(&store), (0, 1, 0), "editing one file is one UPDATE");

        // Remove one file by identity → exactly one DELETE.
        reset(&store);
        store
            .apply_entry_changes(
                1,
                &slug("acme", "api"),
                &ident,
                &[],
                &[("skills/NEW.md", "proj")],
            )
            .unwrap();
        assert_eq!(ops(&store), (0, 0, 1), "removing one file is one DELETE");
    }

    /// SW-IS-F9 AC-1(a) / AC-3 boundary: the targeted write performs NO reconcile — a row
    /// it is not told about is left exactly as it was (neither swept nor read). This is the
    /// behavioral proof there is no full-manifest scan, and the accepted trade-off that the
    /// fast path does not proactively sweep unrelated rows (that stays the full `upsert`'s
    /// job on a reconcile wave).
    #[test]
    fn f9_apply_entry_changes_leaves_untouched_rows_intact() {
        let (_d, store) = store();
        let ident = lockfile("/repo/x", Some("t"), Vec::new());
        let e_keep = entry("keep.md", LayerId::TeamProjection, 1);
        up(
            &store,
            1,
            &lockfile("/repo/x", Some("t"), vec![e_keep.clone()]),
        );

        // Touch a DIFFERENT identity; the untouched row must remain.
        let e_new = entry("new.md", LayerId::TeamProjection, 2);
        store
            .apply_entry_changes(1, &slug("acme", "api"), &ident, &[&e_new], &[])
            .unwrap();

        let mut got = store
            .read(&key("/repo/x", Some("t")))
            .unwrap()
            .unwrap()
            .entries;
        got.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        assert_eq!(
            got,
            vec![e_keep, e_new],
            "untouched row survives; new row added"
        );
    }

    /// SW-IS-F9 AC-3: a targeted DELETE of the last entry clears the raw
    /// `EXISTS(materialized_entry)` non-empty check — `teams_for_materialized_path` stops
    /// reporting the target, so a fast-path deletion leaves NO orphan.
    #[test]
    fn f9_apply_entry_changes_delete_clears_orphan_check() {
        let (_d, store) = store();
        let sl = slug("acme", "api");
        let ident = lockfile("/repo/x", Some("t"), Vec::new());
        let only = entry("only.md", LayerId::TeamProjection, 1);
        up(&store, 1, &lockfile("/repo/x", Some("t"), vec![only]));
        assert_eq!(
            store
                .teams_for_materialized_path("acme", &sl, Path::new("/repo/x"))
                .unwrap(),
            vec![Some("t".to_string())],
            "seeded target is reported as materialized"
        );

        // Delete the sole entry via the targeted path.
        store
            .apply_entry_changes(1, &sl, &ident, &[], &[("only.md", "proj")])
            .unwrap();
        assert!(
            store
                .teams_for_materialized_path("acme", &sl, Path::new("/repo/x"))
                .unwrap()
                .is_empty(),
            "after the last entry is deleted, the target is no longer materialized (no orphan)"
        );
    }

    /// SW-IS-F9 AC-4: the targeted write advances `updated_at` and records the slug alias,
    /// parity with the full `upsert`'s parent side effects (both go through
    /// `resolve_target_for_write`). Alias parity is proved by discovering the target under a
    /// NEW slug that only the `apply_entry_changes` call registered.
    #[test]
    fn f9_apply_entry_changes_advances_updated_at_and_records_alias() {
        let (_d, store) = store();
        let ident = lockfile("/repo/x", Some("t"), Vec::new());
        let e0 = entry("a.md", LayerId::TeamProjection, 1);
        // Seed under slug s1 with an explicit early recency.
        store
            .upsert_with_updated_at(
                1,
                &slug("acme", "s1"),
                &lockfile("/repo/x", Some("t"), vec![e0.clone()]),
                1_000,
            )
            .unwrap();
        let before = store.updated_at(Path::new("/repo/x")).unwrap().unwrap();

        // Targeted write under a DIFFERENT slug s2.
        let e1 = entry("b.md", LayerId::TeamProjection, 2);
        store
            .apply_entry_changes(1, &slug("acme", "s2"), &ident, &[&e1], &[])
            .unwrap();

        // updated_at advanced (now() ≫ the seeded 1_000ns).
        let after = store.updated_at(Path::new("/repo/x")).unwrap().unwrap();
        assert!(after > before, "targeted write advances updated_at");

        // The target is discoverable under s2 — the alias was recorded on the targeted path.
        assert_eq!(
            store
                .teams_for_materialized_path("acme", &slug("acme", "s2"), Path::new("/repo/x"))
                .unwrap(),
            vec![Some("t".to_string())],
            "targeted write records the slug alias (parity with full upsert)"
        );
    }

    /// SW-IS-F9 AC-2: the `read()` after a targeted write is SET-EQUAL to what a full
    /// `upsert` of the same incremental change would produce — across edit, add, delete,
    /// and a mixed multi-file wave. Two stores seeded identically; one converges via the
    /// full path, the other via `apply_entry_changes` with just the delta.
    #[test]
    fn f9_apply_entry_changes_matches_full_upsert_result() {
        let sl = slug("acme", "api");
        let base = vec![
            entry("a.md", LayerId::TeamProjection, 1),
            entry("b.md", LayerId::TeamProjection, 2),
            entry("c.md", LayerId::TeamProjection, 3),
        ];
        // The mixed incremental change: edit a.md, add d.md, remove c.md.
        let mut a_edited = base[0].clone();
        a_edited.last_materialized_mtime_ns = 111;
        a_edited.source_layer = LayerId::TeamProjection; // a real column change too
        let d_new = entry("d.md", LayerId::TeamProjection, 4);
        let full_new = vec![base[1].clone(), a_edited.clone(), d_new.clone()]; // {a', b, d}

        // Full path.
        let (_df, store_full) = store();
        up(
            &store_full,
            1,
            &lockfile("/repo/p", Some("t"), base.clone()),
        );
        store_full
            .upsert(1, &sl, &lockfile("/repo/p", Some("t"), full_new.clone()))
            .unwrap();

        // Targeted path — same base, converged via the explicit delta.
        let (_dt, store_targeted) = store();
        up(
            &store_targeted,
            1,
            &lockfile("/repo/p", Some("t"), base.clone()),
        );
        let ident = lockfile("/repo/p", Some("t"), Vec::new());
        store_targeted
            .apply_entry_changes(1, &sl, &ident, &[&a_edited, &d_new], &[("c.md", "proj")])
            .unwrap();

        let sorted = |store: &SyncStateStore| -> Vec<MaterializedEntry> {
            let mut got = store
                .read(&key("/repo/p", Some("t")))
                .unwrap()
                .unwrap()
                .entries;
            got.sort_by(|x, y| x.logical_name.cmp(&y.logical_name));
            got
        };
        assert_eq!(
            sorted(&store_targeted),
            sorted(&store_full),
            "targeted write is set-equal to the full upsert for the same change"
        );
    }

    /// SW-IS-F9 AC-1(a): the targeted write performs NO full-manifest read — proven by a
    /// row only a full read would decode. A healthy entry and a `poison` entry are seeded,
    /// then `poison`'s stored hash is corrupted (wrong blob length) so `load_entries` errors
    /// when it decodes the manifest. The full `upsert` (poison kept in the incoming set, so
    /// it is not deleted before the change-detection `load_entries`) surfaces that error;
    /// `apply_entry_changes` touches only the healthy entry, never reads `poison`, and
    /// succeeds. This is the read-observing guard the write-op trigger counters cannot give:
    /// a regression that reintroduced a full-manifest read on the targeted path fails here.
    #[test]
    fn f9_apply_entry_changes_does_not_read_the_full_manifest() {
        let (_d, store) = store();
        let sl = slug("acme", "api");
        let ident = lockfile("/repo/x", Some("t"), Vec::new());
        up(
            &store,
            1,
            &lockfile(
                "/repo/x",
                Some("t"),
                vec![
                    entry("healthy.md", LayerId::TeamProjection, 1),
                    entry("poison.md", LayerId::TeamProjection, 1),
                ],
            ),
        );

        // Corrupt the STORED poison row's hash (4 bytes ≠ 32) so only a decode trips on it.
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE materialized_entry SET last_materialized_hash = ?1 WHERE logical_name = 'poison.md'",
            params![&[1u8, 2, 3, 4][..]],
        )
        .unwrap();
        drop(conn);

        // Control: the full upsert keeps poison in the incoming set (so it is NOT deleted),
        // then decodes the whole manifest via load_entries → errors on the corrupt row.
        let full = store.upsert(
            1,
            &sl,
            &lockfile(
                "/repo/x",
                Some("t"),
                vec![
                    entry("healthy.md", LayerId::TeamProjection, 2),
                    entry("poison.md", LayerId::TeamProjection, 1),
                ],
            ),
        );
        assert!(
            full.is_err(),
            "the full upsert decodes the whole manifest and must surface the corrupt row"
        );

        // The targeted write touches only healthy.md and never reads poison.md → succeeds.
        let edited = entry("healthy.md", LayerId::TeamProjection, 3);
        store
            .apply_entry_changes(1, &sl, &ident, &[&edited], &[])
            .expect(
                "apply_entry_changes must not read the full manifest (poison row is never decoded)",
            );
    }

    // SW-IS-F8 AC-2/AC-4: after a delta upsert the read() result equals the full
    // incoming set (order-independent) — identical to what a full rewrite would have
    // produced — across add, edit (every mutable column at once), reorder, and remove.
    #[test]
    fn delta_upsert_matches_full_rewrite_result() {
        let (_d, store) = store();
        let assert_set_eq = |expected: &[MaterializedEntry]| {
            let mut got = store
                .read(&key("/repo/d", Some("t")))
                .unwrap()
                .unwrap()
                .entries;
            let mut want = expected.to_vec();
            let by_identity =
                |e: &MaterializedEntry| (e.logical_name.clone(), e.projection_key.clone());
            got.sort_by_key(by_identity);
            want.sort_by_key(by_identity);
            assert_eq!(got, want);
        };

        let e_a = entry("a", LayerId::TeamProjection, 1);
        let e_b = entry("b", LayerId::TeamProjection, 2);
        up(
            &store,
            1,
            &lockfile("/repo/d", Some("t"), vec![e_a.clone(), e_b.clone()]),
        );
        assert_set_eq(&[e_a.clone(), e_b.clone()]);

        // Add.
        let e_c = entry("c", LayerId::TeamProjection, 3);
        up(
            &store,
            1,
            &lockfile(
                "/repo/d",
                Some("t"),
                vec![e_a.clone(), e_b.clone(), e_c.clone()],
            ),
        );
        assert_set_eq(&[e_a.clone(), e_b.clone(), e_c.clone()]);

        // Edit every mutable column of e_a at once — the UPDATE must persist all of them.
        let mut e_a2 = e_a.clone();
        e_a2.dest_path = PathBuf::from("/repo/a-moved");
        e_a2.source_layer = LayerId::TeamProjection;
        e_a2.source_path = PathBuf::from("/src/a-moved");
        e_a2.last_materialized_hash = [9u8; 32];
        e_a2.last_materialized_mtime_ns = u128::MAX;
        e_a2.source_oid = Some("cafe".to_string());
        e_a2.last_materialized_size = None;
        up(
            &store,
            1,
            &lockfile(
                "/repo/d",
                Some("t"),
                vec![e_a2.clone(), e_b.clone(), e_c.clone()],
            ),
        );
        assert_set_eq(&[e_a2.clone(), e_b.clone(), e_c.clone()]);

        // Reorder with no content change → same set (and a no-op; row-op count covered
        // by the trigger test).
        up(
            &store,
            1,
            &lockfile(
                "/repo/d",
                Some("t"),
                vec![e_c.clone(), e_a2.clone(), e_b.clone()],
            ),
        );
        assert_set_eq(&[e_a2.clone(), e_b.clone(), e_c.clone()]);

        // Remove e_b.
        up(
            &store,
            1,
            &lockfile("/repo/d", Some("t"), vec![e_a2.clone(), e_c.clone()]),
        );
        assert_set_eq(&[e_a2, e_c]);
    }

    // SW-IS-F8 AC-3: a delta that changes one projection entry leaves a sibling
    // projection entry that shares its `logical_name` untouched — identity is
    // `(logical_name, projection_key)`, so the two are distinct rows.
    #[test]
    fn delta_respects_projection_key_identity() {
        let (_d, store) = store();
        let mut docs = entry("skills/x.md", LayerId::TeamProjection, 1);
        docs.projection_key = Some("docs".to_string());
        let mut runbooks = entry("skills/x.md", LayerId::TeamProjection, 2);
        runbooks.projection_key = Some("runbooks".to_string());
        up(
            &store,
            1,
            &lockfile("/repo/pk", Some("t"), vec![docs.clone(), runbooks.clone()]),
        );

        // Change ONLY the runbooks entry.
        let mut runbooks2 = runbooks.clone();
        runbooks2.last_materialized_mtime_ns = 42;
        up(
            &store,
            1,
            &lockfile("/repo/pk", Some("t"), vec![docs.clone(), runbooks2]),
        );

        let got = store.read(&key("/repo/pk", Some("t"))).unwrap().unwrap();
        let docs_got = got
            .entries
            .iter()
            .find(|e| e.projection_key.as_deref() == Some("docs"))
            .expect("docs entry present");
        let runbooks_got = got
            .entries
            .iter()
            .find(|e| e.projection_key.as_deref() == Some("runbooks"))
            .expect("runbooks entry present");
        assert_eq!(*docs_got, docs, "docs entry untouched by the runbooks edit");
        assert_eq!(
            runbooks_got.last_materialized_mtime_ns, 42,
            "runbooks entry updated in place"
        );
    }

    // SW-IS-F8: two incoming entries sharing an identity are a malformed lockfile —
    // rejected before any write, so the prior state is intact (the atomic rejection
    // the old whole-set INSERT got from tripping the PK, now made explicit).
    #[test]
    fn delta_rejects_duplicate_incoming_identity() {
        let (_d, store) = store();
        let good = lockfile(
            "/repo/dup",
            Some("t"),
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        up(&store, 1, &good);

        // Same (logical_name, projection_key) twice — distinct only by a mutable column.
        let dup = lockfile(
            "/repo/dup",
            Some("t"),
            vec![
                entry("clash", LayerId::TeamProjection, 1),
                entry("clash", LayerId::TeamProjection, 2),
            ],
        );
        let err = store.upsert(1, &slug("acme", "api"), &dup).unwrap_err();
        assert!(
            matches!(err, SyncStateStoreError::Data(m) if m.contains("identity")),
            "duplicate identity must be rejected as a Data error"
        );
        assert_eq!(
            store.read(&key("/repo/dup", Some("t"))).unwrap().unwrap(),
            good,
            "prior state intact after the rejected upsert"
        );
    }

    // SW-IS-F8: identity is keyed in the STORED space, so a wave entry (`None`) and a
    // `Some("")` entry sharing a `logical_name` collide on the `""` sentinel — the same
    // PK collision the old whole-set INSERT tripped on. They must be rejected as a
    // duplicate, not silently coalesced to one row by the UPSERT.
    #[test]
    fn delta_rejects_none_vs_empty_projection_key_collision() {
        let (_d, store) = store();
        let wave = install_row("skills/x.md", LayerId::TeamProjection, 1);
        let mut empty_pk = entry("skills/x.md", LayerId::TeamProjection, 2);
        empty_pk.projection_key = Some(String::new());
        let err = store
            .upsert(
                1,
                &slug("acme", "api"),
                &lockfile("/repo/emptypk", Some("t"), vec![wave, empty_pk]),
            )
            .unwrap_err();
        assert!(
            matches!(err, SyncStateStoreError::Data(m) if m.contains("identity")),
            "None and Some(\"\") share the stored identity and must be rejected"
        );
    }

    // SW-IS-F8: reprovisioning a previously-non-empty target down to an empty entry set
    // sweeps every row — `read()` is empty and `teams_for_materialized_path` (a raw
    // `EXISTS` check) no longer reports the team. Guards the emptying transition and a
    // future "skip the DELETE when incoming is empty" short-circuit.
    #[test]
    fn delta_empties_a_previously_populated_target() {
        let (_d, store) = store();
        let api = slug("acme", "api");
        store
            .upsert(
                1,
                &api,
                &lockfile(
                    "/repo/drain",
                    Some("t"),
                    vec![entry("a", LayerId::TeamProjection, 1)],
                ),
            )
            .unwrap();
        assert_eq!(
            store
                .teams_for_materialized_path("acme", &api, Path::new("/repo/drain"))
                .unwrap(),
            vec![Some("t".to_string())],
            "populated target is reported"
        );

        // Reprovision with no entries → the delta deletes every stored row.
        store
            .upsert(1, &api, &lockfile("/repo/drain", Some("t"), vec![]))
            .unwrap();
        assert!(
            store
                .read(&key("/repo/drain", Some("t")))
                .unwrap()
                .unwrap()
                .entries
                .is_empty(),
            "emptying reprovision clears all rows"
        );
        assert!(
            store
                .teams_for_materialized_path("acme", &api, Path::new("/repo/drain"))
                .unwrap()
                .is_empty(),
            "a drained target drops out of the non-empty EXISTS check"
        );
    }

    // SW-IS-F8: the delta's DELETE is driven off the RAW stored identities, so a stored
    // row whose `source_layer` is an unknown/legacy variant (invisible to the
    // layer-decoding `load_entries`, but still counted by the raw `EXISTS` non-empty
    // check) is swept when the incoming set drops it — parity with the old wholesale
    // DELETE. A collision on the same identity is instead healed in place by the UPSERT.
    #[test]
    fn delta_sweeps_or_heals_unknown_layer_rows() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/orph",
                Some("t"),
                vec![entry("keep", LayerId::TeamProjection, 1)],
            ),
        );
        let conn = store.conn().unwrap();
        let target_id: i64 = conn
            .query_row(
                "SELECT id FROM install_target WHERE code_repo_path = ?1",
                params!["/repo/orph"],
                |r| r.get(0),
            )
            .unwrap();
        // Inject unknown-layer rows directly — `layer_to_db` never writes such a value,
        // so this can only arise from a future binary. `ghost` is dropped by every
        // incoming set; `healme` collides with an incoming known entry.
        let inject_unknown = |logical: &str| {
            conn.execute(
                "INSERT INTO materialized_entry
                    (target_id, logical_name, dest_path, source_layer, source_path,
                     last_materialized_hash, last_materialized_mtime_ns, projection_key,
                     source_oid, last_materialized_size)
                 VALUES (?1, ?2, '/d', 'LegacyBogusLayer', '/s', ?3, ?4, '', NULL, NULL)",
                params![target_id, logical, &[0u8; 32][..], &[0u8; 16][..]],
            )
            .unwrap();
        };
        inject_unknown("ghost");
        inject_unknown("healme");

        // Reprovision: keep `keep`, add a KNOWN-layer `healme`; do not carry `ghost`.
        up(
            &store,
            1,
            &lockfile(
                "/repo/orph",
                Some("t"),
                vec![
                    entry("keep", LayerId::TeamProjection, 1),
                    entry("healme", LayerId::TeamProjection, 5),
                ],
            ),
        );

        let raw_count = |logical: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM materialized_entry WHERE target_id = ?1 AND logical_name = ?2",
                params![target_id, logical],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(raw_count("ghost"), 0, "stale unknown-layer orphan is swept");
        assert_eq!(
            raw_count("healme"),
            1,
            "collision healed to exactly one row"
        );
        let got = store.read(&key("/repo/orph", Some("t"))).unwrap().unwrap();
        assert!(
            got.entries
                .iter()
                .any(|e| e.logical_name == "healme" && e.source_layer == LayerId::TeamProjection),
            "healed row now reads back as a known-layer entry"
        );
    }

    // SW-IS-F8: change-detection compares a DB-round-tripped stored entry against the
    // fresh incoming one. If any edge value were lossy across write→read (u128::MAX
    // mtime, `size = None`, a projection key), `*prev == e` would be false and issue a
    // spurious UPDATE — the very amplification the feature removes. Assert zero row ops.
    #[test]
    fn delta_noop_on_round_tripped_edge_values() {
        let (_d, store) = store();
        let mut edge = entry("edge", LayerId::TeamProjection, u128::MAX);
        edge.projection_key = Some("docs".to_string());
        edge.source_oid = Some("beef".to_string());
        edge.last_materialized_size = None;
        let lf = lockfile("/repo/edge", Some("t"), vec![edge]);
        up(&store, 1, &lf);

        store
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _n (c INTEGER NOT NULL);\n\
                 INSERT INTO _n VALUES (0);\n\
                 CREATE TRIGGER _ni AFTER INSERT ON materialized_entry BEGIN UPDATE _n SET c = c + 1; END;\n\
                 CREATE TRIGGER _nu AFTER UPDATE ON materialized_entry BEGIN UPDATE _n SET c = c + 1; END;\n\
                 CREATE TRIGGER _nd AFTER DELETE ON materialized_entry BEGIN UPDATE _n SET c = c + 1; END;",
            )
            .unwrap();

        // The stored side is a DB round-trip; the incoming side is freshly built.
        up(&store, 1, &lf);
        let c: i64 = store
            .conn()
            .unwrap()
            .query_row("SELECT c FROM _n", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            c, 0,
            "round-tripped edge-value entry re-upserts to zero row ops"
        );
    }

    // SW-IS-F8: a failure partway through the write loop rolls the whole delta back —
    // the atomicity `ac3_mid_batch_failure_is_atomic` used to cover via the mid-INSERT
    // PK trip, now that duplicates are rejected before any write. A non-UTF-8 dest_path
    // fails `path_to_str` at bind time on the second new row, after the first was
    // written and the prior row deleted — all of which the enclosing txn unwinds.
    #[cfg(unix)]
    #[test]
    fn delta_write_loop_failure_rolls_back() {
        use std::os::unix::ffi::OsStrExt;
        let (_d, store) = store();
        let good = lockfile(
            "/repo/mid",
            Some("t"),
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        up(&store, 1, &good);

        let ok_new = entry("c", LayerId::TeamProjection, 3);
        let mut bad_path = entry("b", LayerId::TeamProjection, 2);
        bad_path.dest_path = PathBuf::from(std::ffi::OsStr::from_bytes(&[0x66, 0x80, 0x6f]));
        // Ordered so the good row writes first, then the bad row fails mid-loop; the
        // stale `a` is also deleted first — the rollback must restore all of it.
        let bad = lockfile("/repo/mid", Some("t"), vec![ok_new, bad_path]);
        assert!(
            store.upsert(1, &slug("acme", "api"), &bad).is_err(),
            "non-UTF-8 dest_path fails the write"
        );
        assert_eq!(
            store.read(&key("/repo/mid", Some("t"))).unwrap().unwrap(),
            good,
            "a mid-write-loop failure rolls the whole delta back"
        );
    }

    // The mutable-natural-key update path: re-upserting the SAME
    // `(org, team, code_repo_path)` refreshes user_id and replaces the entry set
    // (ON CONFLICT DO UPDATE) IN PLACE — one row, not a duplicate. A second slug for
    // the same path ACCUMULATES as an alias (SW-IS-3c dual-slug), so the path
    // enumerates under both. Re-materializing under a DIFFERENT team is a distinct
    // row, covered by `same_path_two_teams_are_distinct_rows`.
    #[test]
    fn upsert_updates_existing_target_in_place() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/u",
                Some("team-a"),
                vec![entry("old", LayerId::TeamProjection, 1)],
            ),
        );

        // Same (org, team, path), new entry set — and a second slug (api + web) to
        // prove the entries update in place while the new slug is added as an alias.
        let updated = lockfile(
            "/repo/u",
            Some("team-a"),
            vec![entry("new", LayerId::TeamProjection, 2)],
        );
        store.upsert(9, &slug("acme", "web"), &updated).unwrap();

        // Still ONE target for team-a (in-place update), now carrying BOTH slug aliases.
        let mut for_team = store.enumerate_for_team("acme", Some("team-a")).unwrap();
        for_team.sort_by(|a, b| a.0.repo.cmp(&b.0.repo));
        assert_eq!(
            for_team,
            vec![
                (slug("acme", "api"), PathBuf::from("/repo/u")),
                (slug("acme", "web"), PathBuf::from("/repo/u")),
            ]
        );
        // Both slugs now enumerate to the shared clone — the alias set, not a single
        // primary column.
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("team-a"), &slug("acme", "web"))
                .unwrap(),
            vec![PathBuf::from("/repo/u")]
        );
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("team-a"), &slug("acme", "api"))
                .unwrap(),
            vec![PathBuf::from("/repo/u")]
        );
        let got = store
            .read(&key("/repo/u", Some("team-a")))
            .unwrap()
            .unwrap();
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0].logical_name, "new");
        // user_id was updated too (write is not lossy for a realistic id).
        let conn = store.conn().unwrap();
        let uid: i64 = conn
            .query_row(
                "SELECT user_id FROM install_target WHERE code_repo_path = '/repo/u'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(uid, 9);
    }

    // SW-IS-3c dual-slug (SW-EW-6): registering a second slug for one clone path records
    // an ALIAS — not a new row, not a wiped entry set — so both slugs enumerate to the
    // shared target; a whole-path delete cascades the alias rows.
    #[test]
    fn register_or_alias_lets_one_clone_enumerate_under_two_slugs() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/mono",
                Some("t"),
                vec![entry("e", LayerId::TeamProjection, 1)],
            ),
        );
        // `up` seeded slug acme/api with a real entry. Registering acme/web for the SAME
        // path adds a second alias WITHOUT wiping the entries — `register_or_alias` never
        // touches an existing target's child rows.
        let web_lock = lockfile("/repo/mono", Some("t"), Vec::new());
        assert!(store
            .register_or_alias(1, &slug("acme", "web"), &web_lock)
            .unwrap());
        assert!(
            !store
                .register_or_alias(1, &slug("acme", "web"), &web_lock)
                .unwrap(),
            "re-registering an existing alias is a no-op"
        );
        // Both slugs resolve to the one clone; the entry set is untouched.
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("t"), &slug("acme", "api"))
                .unwrap(),
            vec![PathBuf::from("/repo/mono")]
        );
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("t"), &slug("acme", "web"))
                .unwrap(),
            vec![PathBuf::from("/repo/mono")]
        );
        assert_eq!(
            store
                .read(&key("/repo/mono", Some("t")))
                .unwrap()
                .unwrap()
                .entries
                .len(),
            1,
            "adding an alias does not wipe the materialized entries"
        );
        // A whole-worktree delete cascades the alias rows (FK ON DELETE CASCADE).
        store.delete_all_for_path(Path::new("/repo/mono")).unwrap();
        assert!(store
            .enumerate_for_slug("acme", Some("t"), &slug("acme", "web"))
            .unwrap()
            .is_empty());
        assert!(store
            .enumerate_for_slug("acme", Some("t"), &slug("acme", "api"))
            .unwrap()
            .is_empty());
    }

    // SW-IS-3c: a v4 store (no `target_slug` table) heals to the current version IN
    // PLACE — the additive migration creates the alias table and backfills it from the
    // `source_repo_slug` primary column — rather than quarantining. This is the
    // forward-rule's additive branch (data-preserving, no drop).
    #[test]
    fn v4_store_heals_in_place_and_backfills_slug_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        // Provision a target at the current schema, then simulate a pre-v5 DB: drop the
        // alias table and stamp the version back to v4.
        {
            let store = SyncStateStore::open(&path).unwrap();
            up(
                &store,
                7,
                &lockfile(
                    "/repo/heal",
                    Some("t"),
                    vec![entry("e", LayerId::TeamProjection, 1)],
                ),
            );
            let conn = store.conn().unwrap();
            conn.execute_batch("DROP TABLE target_slug;").unwrap();
            conn.pragma_update(None, "user_version", 4i64).unwrap();
        }
        let store = SyncStateStore::open(&path).unwrap();
        // Healed in place — NOT quarantined.
        assert!(
            !append_suffix(&path, ".corrupt-0").exists(),
            "an additive v4→v5 heal must not quarantine the store"
        );
        {
            let conn = store.conn().unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, STORE_SCHEMA_VERSION, "version advanced to current");
        }
        // Data preserved: the target still reads (entries intact) and its slug alias was
        // backfilled, so the per-slug enumerate finds it.
        assert_eq!(
            store
                .read(&key("/repo/heal", Some("t")))
                .unwrap()
                .unwrap()
                .entries
                .len(),
            1
        );
        assert_eq!(
            store
                .enumerate_for_slug("acme", Some("t"), &slug("acme", "api"))
                .unwrap(),
            vec![PathBuf::from("/repo/heal")]
        );
    }

    #[test]
    fn upsert_rejects_out_of_range_user_id() {
        let (_d, store) = store();
        // A user_id above i64::MAX must error, not wrap negative.
        assert!(matches!(
            store.upsert(
                u64::MAX,
                &slug("acme", "api"),
                &lockfile("/repo/big", None, vec![])
            ),
            Err(SyncStateStoreError::Data(_))
        ));
    }

    #[test]
    fn delete_cascades_children() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/d",
                Some("t"),
                vec![entry("a", LayerId::TeamProjection, 1)],
            ),
        );
        assert!(store.delete(&key("/repo/d", Some("t"))).unwrap());
        assert!(store.read(&key("/repo/d", Some("t"))).unwrap().is_none());
        // The child entry is gone too (ON DELETE CASCADE), leaving no orphan rows.
        let conn = store.conn().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM materialized_entry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "cascade removed the child entries");
        assert!(
            !store.delete(&key("/repo/d", Some("t"))).unwrap(),
            "already gone"
        );
    }

    // ===== SW-IS-2: read-through import =====

    /// A `ResolverContext` rooted at `home` — the caller-supplied identity the
    /// read-through derives `user_id` + the lockfile path from.
    fn ctx(home: &Path, team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            // `lockfile_path` validates the org slug as the context-repo `owner/repo`.
            org_slug: "acme/ctx".to_string(),
            team_name: team.map(str::to_string),
        }
    }

    /// Write `lock`'s JSON to the exact path `read_through` will look for it.
    fn write_lockfile_on_disk(c: &ResolverContext, s: &SourceRepoSlug, lock: &Lockfile) -> PathBuf {
        let path = lockfile_path(c, s, &lock.code_repo_path).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(lock).unwrap()).unwrap();
        path
    }

    // AC-2: once a row exists, read_through serves the store and never reads the
    // JSON — proven by planting garbage at the lockfile path and still getting the
    // seeded state back (a JSON read would error on the garbage).
    #[test]
    fn read_through_is_idempotent_and_skips_json_on_store_hit() {
        let (d, store) = store();
        let c = ctx(d.path(), Some("t"));
        let s = slug("acme", "api");
        let repo = Path::new("/repo/idem");

        let mut seeded = lockfile(
            "/repo/idem",
            Some("t"),
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        // The store row must be keyed under the ctx's org so the (org-aware) store
        // read in `read_through` hits it — the ctx org is the context-repo owner/repo.
        seeded.org_slug = c.org_slug.clone();
        store.upsert(42, &s, &seeded).unwrap();

        let path = lockfile_path(&c, &s, repo).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not valid json").unwrap();

        let served = read_through(&store, &c, repo).unwrap().expect("present");
        assert_eq!(served, seeded, "store hit served without touching the JSON");
    }

    // AC-5: a store miss with no JSON on disk is a fresh worktree — None, no error,
    // no row written.
    #[test]
    fn read_through_returns_none_when_no_row_and_no_json() {
        let (d, store) = store();
        let c = ctx(d.path(), Some("t"));
        let repo = Path::new("/repo/fresh");

        assert!(read_through(&store, &c, repo).unwrap().is_none());
        assert!(
            store
                .read(&TargetKey::from_ctx(&c, repo))
                .unwrap()
                .is_none(),
            "no row written"
        );
    }

    // ===== SW-IS-4: corruption detection + quarantine + rebuild =====

    /// Write raw bytes to the store path so the next `open` sees them in place of a
    /// valid DB.
    fn write_bytes(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    // AC-1 + AC-2: a non-SQLite header (garbage) is detected on open (SQLITE_NOTADB),
    // quarantined (not deleted), and replaced with a fresh usable store.
    #[test]
    fn ac1_ac2_non_sqlite_file_is_quarantined_and_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        let garbage = b"this is definitely not a sqlite database";
        write_bytes(&path, garbage);

        let store = SyncStateStore::open(&path).unwrap();

        // Fresh + usable.
        assert!(store.read(&key("/nope", None)).unwrap().is_none());
        // Quarantined, not deleted: the bad bytes survive at a `.corrupt-0` sibling.
        let quarantined = dir.path().join("install.sqlite3.corrupt-0");
        assert_eq!(
            std::fs::read(&quarantined).unwrap(),
            garbage,
            "quarantined bytes remain readable"
        );
    }

    // AC-1 + AC-2: a valid SQLite header followed by garbage pages is caught by
    // quick_check / SQLITE_CORRUPT and recovered the same way.
    #[test]
    fn ac1_ac2_structurally_corrupt_db_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        // Seed a real store with data, then corrupt the pages after the header.
        {
            let store = SyncStateStore::open(&path).unwrap();
            up(
                &store,
                1,
                &lockfile(
                    "/repo/a",
                    Some("t"),
                    vec![entry("e", LayerId::TeamProjection, 1)],
                ),
            );
        }
        // Drop the WAL sidecars so all state lives in the main file, then keep a
        // valid 100-byte header but fill the page bodies with garbage.
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(append_suffix(&path, suffix));
        }
        let mut bytes = std::fs::read(&path).unwrap();
        for b in bytes.iter_mut().skip(100) {
            *b = 0xFF;
        }
        std::fs::write(&path, &bytes).unwrap();

        // Force the re-open to re-run quick_check as the corruption detector. The
        // first open above recorded this path in the process-global integrity set, so
        // without this reset the second open would SKIP quick_check (once-per-path)
        // and the corruption would only be caught downstream — silently changing what
        // this AC-1/AC-2 test actually exercises.
        super::reset_integrity_checked_paths_for_test();

        let store = SyncStateStore::open(&path).unwrap();
        assert!(
            store.read(&key("/repo/a", Some("t"))).unwrap().is_none(),
            "recovered to a fresh empty store"
        );
        assert!(dir.path().join("install.sqlite3.corrupt-0").exists());
    }

    // The integrity dedup keys on the CANONICAL path, so the same DB file reached via
    // a different spelling (here a symlink) is verified once — not once per spelling.
    // Regressing the key back to the raw path would fail this: the symlink spelling
    // would record a second key and re-run the O(DB) quick_check.
    #[cfg(unix)]
    #[test]
    fn integrity_check_dedups_by_canonical_path_not_spelling() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("install.sqlite3");
        let link = dir.path().join("via-symlink.sqlite3");
        // First open at the real path records the canonical key + runs quick_check.
        {
            let _ = SyncStateStore::open(&real).unwrap();
        }
        symlink(&real, &link).unwrap();
        // Second open via the symlink spelling of the SAME underlying file.
        {
            let _ = SyncStateStore::open(&link).unwrap();
        }
        let canonical = real.canonicalize().unwrap();
        // The canonical key is present; the raw symlink spelling is NOT a second key
        // (raw-path keying would record `link` and re-run quick_check per spelling).
        assert!(
            super::integrity_checked_contains_for_test(&canonical),
            "canonical key recorded"
        );
        assert!(
            !super::integrity_checked_contains_for_test(&link),
            "symlink spelling did not create a second integrity-check key"
        );
    }

    // AC-2: quarantine moves the main file aside and clears the stale -wal/-shm
    // sidecars (tested on the primitive, since a fresh WAL store recreates its own).
    #[test]
    fn ac2_quarantine_moves_db_and_clears_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        std::fs::write(&path, b"corrupt main").unwrap();
        std::fs::write(append_suffix(&path, "-wal"), b"stale wal").unwrap();
        std::fs::write(append_suffix(&path, "-shm"), b"stale shm").unwrap();

        quarantine_corrupt_db(&path).unwrap();

        assert!(!path.exists(), "main file moved aside");
        let quarantined = dir.path().join("install.sqlite3.corrupt-0");
        assert_eq!(std::fs::read(&quarantined).unwrap(), b"corrupt main");
        // Sidecars cleared from the live path so the fresh store can't replay them,
        // and preserved alongside the quarantined main file (moved, not deleted).
        assert!(
            !append_suffix(&path, "-wal").exists(),
            "stale -wal cleared from the live path"
        );
        assert!(
            !append_suffix(&path, "-shm").exists(),
            "stale -shm cleared from the live path"
        );
        assert_eq!(
            std::fs::read(append_suffix(&quarantined, "-wal")).unwrap(),
            b"stale wal",
            "-wal preserved beside the quarantined DB"
        );
        assert_eq!(
            std::fs::read(append_suffix(&quarantined, "-shm")).unwrap(),
            b"stale shm",
            "-shm preserved beside the quarantined DB"
        );
    }

    // AC-4: recovery is one-shot — a second open sees a healthy store and does not
    // re-quarantine (no `.corrupt-1` appears).
    #[test]
    fn ac4_recovery_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        write_bytes(&path, b"garbage");

        let _ = SyncStateStore::open(&path).unwrap();
        let _ = SyncStateStore::open(&path).unwrap();

        assert!(dir.path().join("install.sqlite3.corrupt-0").exists());
        assert!(
            !dir.path().join("install.sqlite3.corrupt-1").exists(),
            "a healthy re-open must not quarantine again"
        );
    }

    // AC-5: a missing DB (never created) opens fresh with no error and no quarantine
    // — the missing and corrupt paths must not diverge into a panic.
    #[test]
    fn ac5_missing_file_opens_fresh_without_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/dir/install.sqlite3");

        let store = SyncStateStore::open(&path).unwrap();

        assert!(store.read(&key("/x", None)).unwrap().is_none());
        assert!(
            !dir.path()
                .join("sub/dir/install.sqlite3.corrupt-0")
                .exists(),
            "a missing file is created fresh, never quarantined"
        );
    }

    /// Scaffold a minimal loadable org on disk: a parseable `org.yml` (the RF-31
    /// marker) + a `teams.yml` declaring one team with one source repo —
    /// exactly what `import_all_from_json`'s config walk reads to discover
    /// install tuples.
    fn seed_org_config(
        home: &Path,
        user_id: u64,
        org_slug: &str,
        team: &str,
        owner: &str,
        repo: &str,
    ) {
        let org_root = home
            .join("users")
            .join(user_id.to_string())
            .join("orgs")
            .join(org_slug);
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            format!(
                "slug: {org_slug}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"
            ),
        )
        .unwrap();
        // people.yml is a required file for a composable org, even with no members.
        std::fs::write(org_root.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            format!("teams:\n  - name: {team}\n    source_repos:\n      - owner: {owner}\n        repo: {repo}\n"),
        )
        .unwrap();
    }

    /// A lockfile stamped with `org_slug` (matching the ctx a rebuild walk builds),
    /// so the rebuild tests need not repeat the `lock.org_slug = ...` assignment.
    fn lockfile_for(
        path: &str,
        team: &str,
        org_slug: &str,
        entries: Vec<MaterializedEntry>,
    ) -> Lockfile {
        let mut lock = lockfile(path, Some(team), entries);
        lock.org_slug = org_slug.to_string();
        lock
    }

    // A backfilled lockfile whose body team diverges from its directory is skipped,
    // not imported under the body's key: `read_slug_lockfiles` finds it under team-a's
    // dir but the body stamps team-b, so importing `&lock` would mis-key it under
    // (org, team-b, path) — invisible to team-a readers and able to clobber team-b's
    // real row. The eager path mirrors `read_through`'s divergence guard.
    #[test]
    fn import_skips_a_lockfile_whose_body_team_diverges_from_its_directory() {
        let (d, store) = store();
        seed_org_config(d.path(), 42, "acme/ctx", "team-a", "acme", "api");
        let dir_ctx = ctx(d.path(), Some("team-a"));
        let s = slug("acme", "api");

        // File lands in team-a's directory (path computed from `dir_ctx`), but its
        // body is stamped team-b.
        let divergent = lockfile_for(
            "/repo/one",
            "team-b",
            &dir_ctx.org_slug,
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        write_lockfile_on_disk(&dir_ctx, &s, &divergent);

        let summary = import_all_from_json(&store, d.path(), 42);

        assert_eq!(summary.imported, 0, "the divergent file is not imported");
        assert!(summary.skipped >= 1, "it is counted as skipped");
        // Neither the directory's team nor the body's team gets a row.
        assert!(store
            .read(&key("/repo/one", Some("team-a")))
            .unwrap()
            .is_none());
        assert!(store
            .read(&key("/repo/one", Some("team-b")))
            .unwrap()
            .is_none());
    }

    // AC-3: import_slug rebuilds a target value-for-value against a fresh import,
    // recovering entries from the surviving JSON.
    #[test]
    fn ac3_import_slug_reconstructs_targets_and_entries() {
        let (d, store) = store();
        let c = ctx(d.path(), Some("team-a"));
        let s = slug("acme", "api");

        let mut e = entry("skills/a.md", LayerId::TeamProjection, u128::MAX);
        e.source_oid = Some("deadbeef".to_string());
        let lock = lockfile_for("/repo/one", "team-a", &c.org_slug, vec![e]);
        let disk = write_lockfile_on_disk(&c, &s, &lock);

        let mut summary = RebuildSummary::default();
        import_slug_into_store(&store, &c, &s, false, &mut summary);

        assert_eq!(
            summary,
            RebuildSummary {
                imported: 1,
                skipped: 0,
                already_present: 0,
            }
        );
        let stored = store
            .read(&TargetKey::from_ctx(&c, Path::new("/repo/one")))
            .unwrap()
            .expect("present");
        assert_eq!(
            stored,
            Lockfile::read(&disk).unwrap(),
            "rebuilt target is value-equivalent to a fresh import"
        );
    }

    // AC-3: a parseable-but-unimportable lockfile is skipped without aborting the
    // rebuild of the rest (per-target tolerance).
    #[test]
    fn ac3_import_slug_skips_unimportable_and_imports_the_rest() {
        let (d, store) = store();
        let c = ctx(d.path(), Some("t"));
        let s = slug("acme", "api");

        let good = lockfile_for(
            "/repo/good",
            "t",
            &c.org_slug,
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        write_lockfile_on_disk(&c, &s, &good);
        // A duplicate logical_name trips the (target_id, logical_name) PK inside
        // upsert, so this target is skipped while the good one still lands.
        let bad = lockfile_for(
            "/repo/bad",
            "t",
            &c.org_slug,
            vec![
                entry("dup", LayerId::TeamProjection, 1),
                entry("dup", LayerId::TeamProjection, 2),
            ],
        );
        write_lockfile_on_disk(&c, &s, &bad);

        let mut summary = RebuildSummary::default();
        import_slug_into_store(&store, &c, &s, false, &mut summary);

        assert_eq!(
            summary,
            RebuildSummary {
                imported: 1,
                skipped: 1,
                already_present: 0,
            }
        );
        assert!(store
            .read(&TargetKey::from_ctx(&c, Path::new("/repo/good")))
            .unwrap()
            .is_some());
        assert!(
            store
                .read(&TargetKey::from_ctx(&c, Path::new("/repo/bad")))
                .unwrap()
                .is_none(),
            "the unimportable target was skipped"
        );
    }

    // AC-3: an unparseable surviving JSON is warn-and-skipped (inside
    // read_slug_lockfiles) while the rest still rebuild — the test-plan tolerance
    // case. Distinct from the upsert-level skip above: it never reaches the store.
    #[test]
    fn ac3_import_slug_tolerates_an_unparseable_lockfile() {
        let (d, store) = store();
        let c = ctx(d.path(), Some("t"));
        let s = slug("acme", "api");

        let good = lockfile_for(
            "/repo/good",
            "t",
            &c.org_slug,
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        let good_path = write_lockfile_on_disk(&c, &s, &good);
        // Drop a garbage `*.lock.json` into the same per-slug dir the walk scans.
        std::fs::write(
            good_path.parent().unwrap().join("garbage.lock.json"),
            b"{ not valid json",
        )
        .unwrap();

        let mut summary = RebuildSummary::default();
        import_slug_into_store(&store, &c, &s, false, &mut summary);

        assert_eq!(
            summary.imported, 1,
            "the good lockfile imported despite the unparseable sibling"
        );
        assert!(store
            .read(&TargetKey::from_ctx(&c, Path::new("/repo/good")))
            .unwrap()
            .is_some());
    }

    // AC-3 (end-to-end): import_all_from_json discovers install tuples from the
    // on-disk org/team config and re-imports their lockfiles into the store.
    #[test]
    fn ac3_import_all_from_json_walks_config_tuples() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = SyncStateStore::open(home.join("install-state/install.sqlite3")).unwrap();

        seed_org_config(home, 42, "acme/ctx", "team-a", "acme", "api");
        let c = ctx(home, Some("team-a"));
        let s = slug("acme", "api");
        let lock = lockfile_for(
            "/repo/one",
            "team-a",
            &c.org_slug,
            vec![entry("skills/a.md", LayerId::TeamProjection, 7)],
        );
        write_lockfile_on_disk(&c, &s, &lock);

        let summary = import_all_from_json(&store, home, 42);

        assert_eq!(summary.imported, 1, "the one declared target was imported");
        let got = store
            .read(&TargetKey::from_ctx(&c, Path::new("/repo/one")))
            .unwrap()
            .expect("present");
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0].logical_name, "skills/a.md");
    }

    // AC-6: the rebuild is blocking-pool-safe — it completes with no ambient Tokio
    // runtime (a plain sync `fn`, not runtime-bound).
    #[test]
    fn ac6_rebuild_runs_without_a_tokio_runtime() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test must run with no ambient Tokio runtime"
        );
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = SyncStateStore::open(home.join("install-state/install.sqlite3")).unwrap();

        seed_org_config(home, 42, "acme/ctx", "team-a", "acme", "api");
        let c = ctx(home, Some("team-a"));
        let s = slug("acme", "api");
        let lock = lockfile_for(
            "/repo/x",
            "team-a",
            &c.org_slug,
            vec![entry("a", LayerId::TeamProjection, 1)],
        );
        write_lockfile_on_disk(&c, &s, &lock);

        let summary = import_all_from_json(&store, home, 42);
        assert_eq!(summary.imported, 1);
    }

    // ===== SW-IS-3ab: per-(org, team, code_repo_path) keying =====

    // AC-1 + AC-2: the same worktree path materialized under two teams is two
    // distinct rows (a team switch's transient state), and a team-scoped read
    // never returns the sibling team's state.
    #[test]
    fn same_path_two_teams_are_distinct_rows() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/shared",
                Some("team-a"),
                vec![entry("a", LayerId::TeamProjection, 1)],
            ),
        );
        up(
            &store,
            1,
            &lockfile(
                "/repo/shared",
                Some("team-b"),
                vec![entry("b", LayerId::TeamProjection, 2)],
            ),
        );

        let a = store
            .read(&key("/repo/shared", Some("team-a")))
            .unwrap()
            .expect("team-a row");
        let b = store
            .read(&key("/repo/shared", Some("team-b")))
            .unwrap()
            .expect("team-b row");
        assert_eq!(
            a.entries[0].logical_name, "a",
            "team-a read never returns team-b's state"
        );
        assert_eq!(b.entries[0].logical_name, "b");
        // Two physical rows for the one path.
        let conn = store.conn().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_target WHERE code_repo_path = '/repo/shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "one row per (team, path)");

        // Re-upserting ONE team must not clobber the sibling's children — proves the
        // post-INSERT id-fetch resolves the correct per-team row.
        up(
            &store,
            1,
            &lockfile(
                "/repo/shared",
                Some("team-a"),
                vec![entry("a2", LayerId::TeamProjection, 3)],
            ),
        );
        let a2 = store
            .read(&key("/repo/shared", Some("team-a")))
            .unwrap()
            .expect("team-a row");
        assert_eq!(a2.entries[0].logical_name, "a2", "team-a entries replaced");
        let b_after = store
            .read(&key("/repo/shared", Some("team-b")))
            .unwrap()
            .expect("team-b row survives");
        assert_eq!(
            b_after.entries[0].logical_name, "b",
            "team-b's entries untouched by team-a's re-upsert"
        );

        // delete is team-scoped: removing team-a leaves team-b intact.
        assert!(store.delete(&key("/repo/shared", Some("team-a"))).unwrap());
        assert!(store
            .read(&key("/repo/shared", Some("team-a")))
            .unwrap()
            .is_none());
        assert!(store
            .read(&key("/repo/shared", Some("team-b")))
            .unwrap()
            .is_some());
    }

    // teams_for_materialized_path returns one team per non-empty row for the path, the
    // no-team row as `None`, and skips a target that has no entries — the store-backed
    // answer to "which teams materialized this path" for cross-team teardown.
    #[test]
    fn teams_for_materialized_path_lists_every_non_empty_team() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/shared",
                Some("team-a"),
                vec![entry("a", LayerId::TeamProjection, 1)],
            ),
        );
        up(
            &store,
            1,
            &lockfile(
                "/repo/shared",
                None,
                vec![entry("n", LayerId::TeamProjection, 2)],
            ),
        );
        // An entry-less row for the same path must NOT be reported.
        up(
            &store,
            1,
            &lockfile("/repo/shared", Some("team-empty"), vec![]),
        );
        // A different path must not bleed in.
        up(
            &store,
            1,
            &lockfile(
                "/repo/other",
                Some("team-x"),
                vec![entry("x", LayerId::TeamProjection, 3)],
            ),
        );

        let mut got = store
            .teams_for_materialized_path("acme", &slug("acme", "api"), Path::new("/repo/shared"))
            .unwrap();
        got.sort();
        assert_eq!(got, vec![None, Some("team-a".to_string())]);
    }

    // AC-3: the non-unique `code_repo_path` index exists so the reaper's path-only
    // `MAX(updated_at)` seek stays index-backed (the composite UNIQUE can't serve a
    // bare `code_repo_path` lookup — it's the third key column).
    #[test]
    fn code_repo_path_index_backs_the_path_level_recency_lookup() {
        let (_d, store) = store();
        let conn = store.conn().unwrap();
        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_install_target_code_repo_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_index, "the code_repo_path index must exist");
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT MAX(updated_at) FROM install_target \
                 WHERE code_repo_path = ?1",
                params!["/repo/x"],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            plan.contains("USING INDEX") || plan.contains("USING COVERING INDEX"),
            "path-level recency must seek via index, plan was: {plan}"
        );
    }

    // AC-1: the no-team target is stored as the `_none` sentinel (never NULL), so a
    // second no-team upsert hits ON CONFLICT and updates in place rather than
    // inserting a duplicate; team_name round-trips back to `None` on read.
    #[test]
    fn no_team_sentinel_round_trips_and_conflict_updates() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/nt",
                None,
                vec![entry("a", LayerId::TeamProjection, 1)],
            ),
        );
        up(
            &store,
            1,
            &lockfile(
                "/repo/nt",
                None,
                vec![entry("b", LayerId::TeamProjection, 2)],
            ),
        );

        let conn = store.conn().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_target WHERE code_repo_path = '/repo/nt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "no-team upsert is idempotent (sentinel, not NULL-distinct)"
        );

        let got = store
            .read(&key("/repo/nt", None))
            .unwrap()
            .expect("present");
        assert_eq!(got.team_name, None, "sentinel maps back to None on read");
        assert_eq!(
            got.entries[0].logical_name, "b",
            "the second upsert replaced the entry set"
        );
        let stored_team: String = conn
            .query_row(
                "SELECT team_name FROM install_target WHERE code_repo_path = '/repo/nt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_team, "_none",
            "no-team stored as the sentinel, never NULL"
        );
    }

    // AC-1: a no-team (`_none`) target and a real-team target at the SAME path are
    // distinct rows with non-crossing children — the sentinel-vs-real-team boundary.
    #[test]
    fn no_team_and_real_team_at_same_path_are_distinct_rows() {
        let (_d, store) = store();
        up(
            &store,
            1,
            &lockfile(
                "/repo/x",
                None,
                vec![entry("nt", LayerId::TeamProjection, 1)],
            ),
        );
        up(
            &store,
            1,
            &lockfile(
                "/repo/x",
                Some("team-a"),
                vec![entry("ta", LayerId::TeamProjection, 2)],
            ),
        );

        let conn = store.conn().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_target WHERE code_repo_path = '/repo/x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "no-team and team-a are distinct rows at one path");
        let nt = store.read(&key("/repo/x", None)).unwrap().expect("no-team");
        let ta = store
            .read(&key("/repo/x", Some("team-a")))
            .unwrap()
            .expect("team-a");
        assert_eq!(nt.team_name, None);
        assert_eq!(nt.entries[0].logical_name, "nt");
        assert_eq!(ta.entries[0].logical_name, "ta", "no crossing between them");
    }

    // AC-1 (defense-in-depth): the store rejects a target literally named `_none`
    // (any case) rather than let it alias the no-team sentinel row.
    #[test]
    fn upsert_rejects_a_team_named_like_the_no_team_sentinel() {
        let (_d, store) = store();
        for reserved in ["_none", "_None", "_NONE"] {
            assert!(
                matches!(
                    store.upsert(
                        1,
                        &slug("acme", "api"),
                        &lockfile("/repo/r", Some(reserved), vec![])
                    ),
                    Err(SyncStateStoreError::Data(_))
                ),
                "team named {reserved:?} must be rejected, not aliased onto no-team"
            );
        }
        // And the no-team row is untouched by the rejected attempts.
        assert!(store.read(&key("/repo/r", None)).unwrap().is_none());
    }

    // Defense-in-depth symmetry: the read/delete seam (via `seek_params`) rejects a
    // `_none` team key too, so a target literally named `_none` cannot alias the
    // no-team sentinel row on read/delete the way `upsert` already prevents on write.
    #[test]
    fn read_and_delete_reject_a_team_named_like_the_no_team_sentinel() {
        let (_d, store) = store();
        // A real no-team row an alias would otherwise hit.
        up(
            &store,
            1,
            &lockfile(
                "/repo/s",
                None,
                vec![entry("nt", LayerId::TeamProjection, 1)],
            ),
        );

        for reserved in ["_none", "_None", "_NONE"] {
            assert!(
                matches!(
                    store.read(&key("/repo/s", Some(reserved))),
                    Err(SyncStateStoreError::Data(_))
                ),
                "read with team {reserved:?} must be rejected, not aliased onto no-team"
            );
            assert!(
                matches!(
                    store.delete(&key("/repo/s", Some(reserved))),
                    Err(SyncStateStoreError::Data(_))
                ),
                "delete with team {reserved:?} must be rejected"
            );
        }
        // The real no-team row survives the rejected reads/deletes untouched.
        assert!(store.read(&key("/repo/s", None)).unwrap().is_some());
    }

    // AC-3: the per-row `updated_at` clamp never goes backward — re-upserting the SAME
    // (org, team, path) with an OLDER timestamp keeps the stored (newer) recency.
    #[test]
    fn upsert_updated_at_does_not_regress_on_an_older_rewrite() {
        let (_d, store) = store();
        let newer = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let older = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let s = slug("acme", "api");
        store
            .upsert_with_updated_at(
                1,
                &s,
                &lockfile("/repo/mono", Some("t"), vec![]),
                system_time_to_unix_nanos(newer),
            )
            .unwrap();
        store
            .upsert_with_updated_at(
                1,
                &s,
                &lockfile("/repo/mono", Some("t"), vec![]),
                system_time_to_unix_nanos(older),
            )
            .unwrap();
        assert_eq!(
            store.updated_at(Path::new("/repo/mono")).unwrap(),
            Some(newer),
            "an older rewrite of the same row must not drag recency backward"
        );
    }

    // `persist_reconcile` on an EXISTING target with an unchanged pack set issues ZERO
    // `materialized_entry` row ops (the same trigger probe the F6 no-op test uses) yet
    // still advances the reaper's recency — the whole point of the no-op reconcile fast
    // path (skip the entry read/diff, keep the `updated_at` bump).
    #[test]
    fn persist_reconcile_advances_updated_at_with_zero_entry_ops() {
        let (_d, store) = store();
        let s = slug("acme", "api");
        // Seed an existing target with real entries — a subsequent touch must not
        // touch those child rows.
        let lf = lockfile(
            "/repo/touch",
            Some("t"),
            vec![
                entry("a", LayerId::TeamProjection, 1),
                entry("b", LayerId::TeamProjection, 2),
            ],
        );
        store.upsert(1, &s, &lf).unwrap();
        let before = store.updated_at(Path::new("/repo/touch")).unwrap().unwrap();

        // Install a persistent child-row op counter (fires across the store's per-op
        // connections, like the F6 no-op probe).
        store
            .conn()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _rewrite_probe (n INTEGER NOT NULL);\n\
                 INSERT INTO _rewrite_probe VALUES (0);\n\
                 CREATE TRIGGER _probe_ins AFTER INSERT ON materialized_entry \
                     BEGIN UPDATE _rewrite_probe SET n = n + 1; END;\n\
                 CREATE TRIGGER _probe_del AFTER DELETE ON materialized_entry \
                     BEGIN UPDATE _rewrite_probe SET n = n + 1; END;\n\
                 CREATE TRIGGER _probe_upd AFTER UPDATE ON materialized_entry \
                     BEGIN UPDATE _rewrite_probe SET n = n + 1; END;",
            )
            .unwrap();
        let probe = |store: &SyncStateStore| -> i64 {
            store
                .conn()
                .unwrap()
                .query_row("SELECT n FROM _rewrite_probe", [], |r| r.get(0))
                .unwrap()
        };

        // Ensure a strictly-later wall clock so the monotonic clamp can advance.
        std::thread::sleep(Duration::from_millis(2));
        store.persist_reconcile(1, &s, &lf).unwrap();

        assert_eq!(
            probe(&store),
            0,
            "a no-op reconcile persist must not touch any materialized_entry row"
        );
        let after = store.updated_at(Path::new("/repo/touch")).unwrap().unwrap();
        assert!(
            after > before,
            "the touch advances the target's recency (before={before:?}, after={after:?})"
        );
    }

    // `persist_reconcile` on a target with NO row CREATES the row (the fresh-target
    // correctness guard). A bare `UPDATE … SET updated_at` would touch zero rows and
    // silently lose a first-ever reconcile's target.
    #[test]
    fn persist_reconcile_creates_a_fresh_target_row() {
        let (_d, store) = store();
        let s = slug("acme", "api");
        assert!(
            store
                .updated_at(Path::new("/repo/fresh"))
                .unwrap()
                .is_none(),
            "precondition: no row for the fresh target yet"
        );
        // An empty lockfile (mirrors the reconcile shell's `Lockfile::empty` for a
        // read-through miss).
        let lf = lockfile("/repo/fresh", Some("t"), vec![]);
        store.persist_reconcile(1, &s, &lf).unwrap();

        assert!(
            store
                .updated_at(Path::new("/repo/fresh"))
                .unwrap()
                .is_some(),
            "the touch INSERTs the target row on a first-ever reconcile"
        );
        assert!(
            store
                .read(&key("/repo/fresh", Some("t")))
                .unwrap()
                .is_some(),
            "the fresh target is readable back after the touch"
        );
    }

    // `persist_reconcile` inherits the seam's monotonic clamp — it can never drag recency
    // backward (mirrors `upsert_updated_at_does_not_regress_on_an_older_rewrite`). Seed a
    // NEWER upsert, then a persist (which stamps `now`, older than the far-future seed)
    // must leave the stored recency at the newer value.
    #[test]
    fn persist_reconcile_does_not_regress_updated_at() {
        let (_d, store) = store();
        let s = slug("acme", "api");
        // Seed a far-future recency so a `now`-stamped touch is strictly older.
        let newer = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
        let lf = lockfile("/repo/monotouch", Some("t"), vec![]);
        store
            .upsert_with_updated_at(1, &s, &lf, system_time_to_unix_nanos(newer))
            .unwrap();

        store.persist_reconcile(1, &s, &lf).unwrap();

        assert_eq!(
            store.updated_at(Path::new("/repo/monotouch")).unwrap(),
            Some(newer),
            "a persist stamping `now` must not drag a newer recency backward"
        );
    }

    // `persist_reconcile` leaves the target's materialized entries untouched — passing its
    // entries-empty lock to `upsert` would sweep the real entries as stale.
    #[test]
    fn persist_reconcile_leaves_entries() {
        let (_d, store) = store();
        let s = slug("acme", "api");
        let seeded = lockfile(
            "/repo/pr",
            Some("t"),
            vec![
                entry("a", LayerId::TeamProjection, 1),
                entry("b", LayerId::TeamProjection, 2),
            ],
        );
        store.upsert(1, &s, &seeded).unwrap();

        // The reconcile shell's lock carries no entries: this seam never reads/writes them.
        let shell = lockfile("/repo/pr", Some("t"), vec![]);
        store.persist_reconcile(1, &s, &shell).unwrap();

        let after = store.read(&key("/repo/pr", Some("t"))).unwrap().unwrap();
        assert_eq!(
            after.entries.len(),
            2,
            "persist_reconcile leaves the target's materialized entries intact"
        );
    }

    // SW-IS-3c: opening a pre-3ab store (old `UNIQUE(code_repo_path)` schema, stamped
    // v3) can no longer be migrated in place — the JSON that used to rebuild it is
    // retired. `open` surfaces the un-migratable file as a recoverable fault: it is
    // quarantined (bytes preserved, never deleted) and a fresh empty store, stamped at
    // the current version, takes its place. The pre-v3 row is NOT carried forward.
    #[test]
    fn pre_current_version_populated_store_is_quarantined_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install-state/install.sqlite3");
        // Seed a v3 store: the old single-column-UNIQUE schema carrying a stale row.
        {
            let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE install_target (\
                     id INTEGER PRIMARY KEY, \
                     code_repo_path TEXT NOT NULL UNIQUE, \
                     org_slug TEXT NOT NULL, team_name TEXT, \
                     source_repo_slug TEXT NOT NULL, user_id INTEGER NOT NULL, \
                     updated_at INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO install_target \
                 (code_repo_path, org_slug, team_name, source_repo_slug, user_id, updated_at) \
                 VALUES ('/repo/stale', 'acme/ctx', NULL, '{}', 42, 1)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3i64).unwrap();
        }
        // Collapse the WAL into the main file so the whole v3 state lives on disk, then
        // capture the original bytes to prove quarantine preserves (never deletes) them.
        {
            let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
            conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
                .unwrap();
        }
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(append_suffix(&path, suffix));
        }
        let original = std::fs::read(&path).unwrap();

        let store = SyncStateStore::open(&path).unwrap();

        // The un-migratable v3 file was quarantined (moved aside, bytes intact).
        let quarantined = path.with_file_name("install.sqlite3.corrupt-0");
        assert_eq!(
            std::fs::read(&quarantined).unwrap(),
            original,
            "the original pre-v3 bytes are preserved at the quarantine sibling"
        );

        // The store reopened at `path` is a fresh empty store: the stale row is gone.
        let c = ctx(dir.path(), None);
        assert!(
            store
                .read(&TargetKey::from_ctx(&c, Path::new("/repo/stale")))
                .unwrap()
                .is_none(),
            "the pre-v3 row was not carried forward into the fresh store"
        );
        let n: i64 = store
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM install_target", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the fresh store starts empty");

        // The fresh file is stamped at the current schema version.
        let v: i64 = store
            .conn()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            v, STORE_SCHEMA_VERSION,
            "fresh store stamped at the current version"
        );
    }

    // AC-4 guard: opening a DB stamped at a FUTURE version is a no-op — the
    // `< STORE_SCHEMA_VERSION` gate must never drop tables or downgrade a DB written
    // by a newer build. Pins the gate against a regression that widens the migration
    // to fire on versions at or above the current one.
    #[test]
    fn future_version_store_open_does_not_migrate_or_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        // A healthy current-schema store with one row, then stamped one version ahead.
        {
            let store = SyncStateStore::open(&path).unwrap();
            up(
                &store,
                1,
                &lockfile(
                    "/repo/fut",
                    Some("team-a"),
                    vec![entry("a", LayerId::TeamProjection, 1)],
                ),
            );
            let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
            conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION + 1)
                .unwrap();
        }

        // Re-open: the row survives and the future stamp is left intact.
        let store = SyncStateStore::open(&path).unwrap();
        assert!(
            store
                .read(&key("/repo/fut", Some("team-a")))
                .unwrap()
                .is_some(),
            "a future-version DB is neither dropped nor downgraded on open"
        );
        let conn = store.conn().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            v,
            STORE_SCHEMA_VERSION + 1,
            "the future user_version is preserved, not clamped down"
        );
    }

    // A store already at the current schema reopens non-destructively: opening the
    // same path again neither quarantines nor drops it, so upserted state survives a
    // handle drop + re-open.
    #[test]
    fn current_version_populated_store_reopens_non_destructively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.sqlite3");
        {
            let store = SyncStateStore::open(&path).unwrap();
            up(
                &store,
                7,
                &lockfile(
                    "/repo/keep",
                    Some("team-a"),
                    vec![entry("a", LayerId::TeamProjection, 1)],
                ),
            );
        }

        // Re-open the SAME path on the current schema: no quarantine sibling appears,
        // and the target still reads back.
        let store = SyncStateStore::open(&path).unwrap();
        assert!(
            !dir.path().join("install.sqlite3.corrupt-0").exists(),
            "a current-schema store is not quarantined on re-open"
        );
        assert!(
            store
                .read(&key("/repo/keep", Some("team-a")))
                .unwrap()
                .is_some(),
            "the upserted target survives a handle drop + re-open"
        );
    }
}
