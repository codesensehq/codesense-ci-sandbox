// SPDX-License-Identifier: MIT
//! Tauri command surface for [`crate::content_engine`] consumers — SS-13.
//!
//! Consumed by the `sense_sync_*` MCP tools and the frontend (the merge
//! preview panel reads layers through it). Treat this module's public command set
//! as a contract — changing a command's name, input shape, output shape,
//! or `Error` / `*Outcome` variant set breaks those consumers. New
//! commands belong here; existing commands are versioned by adding new
//! variants to `*Outcome` enums rather than changing existing fields.
//!
//! AC-9 invariant: this module reads/writes only via `content_engine`
//! APIs and `std::fs` / `tokio::fs`. No `git` shell-outs (those belong
//! to `git_exclude`); no `notify::Watcher` instantiation (SS-4a); no
//! Tauri event emits (SS-4a/c).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::sync::Mutex;

use codesense_fs_core::filters::is_ignored;
use codesense_pty_core::agent_cli::{adapter, descriptor, AgentCli, Prepare, ProjectHooks};

use crate::content_engine::{
    self, bag_delta, content_layer_root, enumerate_attached_code_repos_for_team, hash_file,
    is_safe_logical_name, list_layer, read_library_at, read_overlay, read_team_library,
    seed_library_if_absent, team_libraries_dir, validate_org_slug, validate_path_segment,
    write_team_library_at, write_tmp_fsync_rename_suppressed, LayerId, Lockfile, ResolverContext,
    Suppression, SuppressionHandle, PRIORITY_ORDER,
};
// SS-4c: wave-engine entry points. Imported under `engine_*` aliases so
// the engine's `Error` type doesn't collide with SS-13's command-side
// `Error` enum.
use crate::content_engine::lockfile::open_sync_state_store;
use crate::content_engine::sync::{ctx_with_team, WaveCoordinator};
use crate::content_engine::sync_state_store::read_through;
use crate::content_engine::Error as EngineError;
use crate::domain::Principal;
use crate::domain::SourceRepoSlug;
// SW-TS-1: the worktree→story index the relink/registration paths read/update.
use crate::install::worktree_index::{self, WorktreeStoryRecord};

// ---------------------------------------------------------------------------
// Error enum (AC-7) — hard failures only. Soft rejections go through
// MoveOutcome / WriteOutcome.
// ---------------------------------------------------------------------------

/// Hard failures surfaced from any SS-13 command to TS. Soft rejections
/// (collision, missing source, etc.) flow through [`MoveOutcome`] /
/// [`WriteOutcome`] instead — those are *expected* call outcomes, not
/// programming or environmental errors.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "camelCase")]
pub enum Error {
    /// `logical_name` failed
    /// [`content_engine::is_safe_logical_name`] (absolute path, `..`
    /// traversal, NUL or control byte, etc.).
    InvalidLogicalName(String),
    /// `org_slug` or `team_name` failed
    /// [`content_engine::validate_path_segment`].
    InvalidContextSegment { field: &'static str, reason: String },
    /// A read against a layer whose `content_layer_root` returned `None`
    /// (Personal*, Org*, or any layer missing required inputs). Writes
    /// to such a layer surface [`WriteOutcome::RejectedTargetUnbacked`]
    /// instead.
    LayerUnbacked(LayerId),
    /// `read_layer_file` could not find the named file.
    FileNotFound {
        layer: LayerId,
        logical_name: String,
    },
    /// `Lockfile::read` returned a structured engine error (schema
    /// mismatch, invalid data, etc.).
    LockfileRead(String),
    /// `code_repo_path` for `sense_sync_scan_cwd` is not a git working
    /// tree (no `.git` directory or file present). Light-touch
    /// defense-in-depth so malformed Tauri payloads cannot drive the
    /// scanner against `/etc` or `/private/var`. Review-iter-5 (EH-14).
    NotAGitWorkingTree(PathBuf),
    /// Engine surfaced a variant that is not reachable from this module
    /// today (`Notify`, `NotAGitWorkingTree`, `GitNotFound`,
    /// `InvalidExcludeBlock`, `InvalidEntry`, `WatcherLockPoisoned`). If
    /// a future call site routes one of these through SS-13, TS consumers
    /// see a typed `internal` discriminator instead of the misleading
    /// `lockfileRead` label. Review-iter-5 (EH-21).
    Internal(String),
    /// Generic filesystem failure (rename, read, write, hash). `op` is
    /// one of `"rename" | "read" | "write" | "hash" | "stat" | "mkdir"`.
    Io {
        op: &'static str,
        path: PathBuf,
        message: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidLogicalName(n) => write!(f, "invalid logical_name {:?}", n),
            Error::InvalidContextSegment { field, reason } => {
                write!(f, "invalid {}: {}", field, reason)
            }
            Error::LayerUnbacked(l) => write!(f, "layer {:?} has no storage in this context", l),
            Error::FileNotFound {
                layer,
                logical_name,
            } => write!(f, "file {:?} not found in layer {:?}", logical_name, layer),
            Error::LockfileRead(msg) => write!(f, "failed to read lockfile: {}", msg),
            Error::NotAGitWorkingTree(path) => {
                write!(f, "path is not a git working tree: {}", path.display())
            }
            Error::Internal(msg) => write!(f, "internal: {}", msg),
            Error::Io { op, path, message } => {
                write!(f, "io {} on {}: {}", op, path.display(), message)
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    fn io(op: &'static str, path: &Path, err: std::io::Error) -> Self {
        Error::Io {
            op,
            path: path.to_path_buf(),
            message: err.to_string(),
        }
    }
}

/// Map an engine error to a boundary `Error` with as much typed-context
/// preservation as possible.
///
/// Review-iter-1: collapsed `Io` to `Error::Io { op: "engine", path: "" }`
/// and segment-validation failures to `InvalidContextSegment`.
///
/// Review-iter-2: reserve the `LockfileRead` arm specifically for
/// `UnsupportedSchemaVersion` and genuinely lockfile-shaped failures.
///
/// Review-iter-5 (EH-21): map all other engine variants to
/// `Error::Internal` instead of the misleading `LockfileRead` catchall.
/// `Notify`, `NotAGitWorkingTree`, `GitNotFound`, `InvalidExcludeBlock`,
/// `InvalidEntry`, `WatcherLockPoisoned`, etc. are not reachable from
/// this module today, but the typed `internal` discriminator surfaces
/// the mapping gap rather than disguising it as a lockfile failure.
fn engine_error_to_command_error(err: content_engine::Error) -> Error {
    match err {
        content_engine::Error::Io(io) => Error::Io {
            op: "engine",
            path: PathBuf::new(),
            message: io.to_string(),
        },
        content_engine::Error::InvalidTeamName { reason } => Error::InvalidContextSegment {
            field: "teamName",
            reason,
        },
        content_engine::Error::InvalidOrgSlug { reason } => Error::InvalidContextSegment {
            field: "orgSlug",
            reason,
        },
        content_engine::Error::InvalidSourceRepoSlug { field, reason } => {
            Error::InvalidContextSegment { field, reason }
        }
        content_engine::Error::UnsupportedSchemaVersion { found, expected } => {
            Error::LockfileRead(format!(
                "unsupported lockfile schema version: found {}, expected {}",
                found, expected
            ))
        }
        other => {
            // Review-iter-7 (Blind-H5 + Edge-L4): use `log::error!`
            // rather than `debug_assert!(false, ...)`. A panic here is
            // caught by `tokio::task::spawn_blocking` and surfaces as a
            // `JoinError` → mapped at the caller boundary to
            // `Error::Io { op: "engine" }`, undermining the F8
            // regression guard that pins malformed-JSON detection to
            // that exact variant. A logged error gives the same
            // missed-mapping signal without crashing dev/test runs.
            log::error!(
                target: "content_engine::commands",
                "unmapped engine error variant reached the catch-all: {}",
                other
            );
            Error::Internal(other.to_string())
        }
    }
}

impl From<content_engine::Error> for Error {
    fn from(err: content_engine::Error) -> Self {
        engine_error_to_command_error(err)
    }
}

// ---------------------------------------------------------------------------
// derive_resolver_context (AC-2, D-9) — the single seam from Tauri-shaped
// inputs to the engine-internal ResolverContext. No command calls
// content_engine::* until it has flowed through here.
// ---------------------------------------------------------------------------

/// Build a [`ResolverContext`] from JSON-deserialize-safe inputs.
/// Validates `org_slug` and `team_name` segments before construction so
/// callers cannot smuggle path-traversal sequences past
/// [`content_layer_root`]'s defensive guards.
///
/// Also rejects `team_name = "_none"` (case-insensitive) — that segment
/// is reserved by `content_engine::lockfile` for the no-team install
/// path. Without this check, `lockfile_path` rejected `_none` but the
/// read commands' `content_layer_root` accepted it, so the same input
/// produced different errors depending on which command consumed it.
/// Review-iter-1 uniform-rejection fix.
///
/// Review-iter-4 (P-1): also rejects empty / non-absolute `codesense_home`.
/// Without this, a caller passing `{ codesenseHome: "" }` produced
/// `ResolverContext { codesense_home: PathBuf::new(), .. }`, and downstream
/// `path_to_org_root` returned a relative path. `tokio::fs::read` /
/// `fs::rename` / `fs::create_dir_all` then executed against the process
/// CWD instead of the intended codesense home — silent privilege escape
/// surface for a malformed Tauri payload.
///
/// SS-4c rebase: `pub(crate)` so other command modules can build a validated
/// `ResolverContext`.
pub(crate) fn derive_resolver_context(
    codesense_home: PathBuf,
    user_id: u64,
    org_slug: String,
    team_name: Option<String>,
) -> Result<ResolverContext, Error> {
    if codesense_home.as_os_str().is_empty() {
        return Err(Error::InvalidContextSegment {
            field: "codesenseHome",
            reason: "empty path".into(),
        });
    }
    if !codesense_home.is_absolute() {
        return Err(Error::InvalidContextSegment {
            field: "codesenseHome",
            reason: "must be absolute path".into(),
        });
    }
    if let Some(reason) = validate_org_slug(&org_slug) {
        return Err(Error::InvalidContextSegment {
            field: "orgSlug",
            reason: reason.to_string(),
        });
    }
    if let Some(team) = team_name.as_deref() {
        if team.eq_ignore_ascii_case("_none") {
            return Err(Error::InvalidContextSegment {
                field: "teamName",
                reason: "reserved (collides with no-team segment, case-insensitive)".into(),
            });
        }
        if let Some(reason) = validate_path_segment(team) {
            return Err(Error::InvalidContextSegment {
                field: "teamName",
                reason: reason.to_string(),
            });
        }
    }
    Ok(ResolverContext {
        context_root: None,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    })
}

/// Validate a deserialized [`SourceRepoSlug`] against the same rules
/// `SourceRepoSlug::new` enforces. Serde's `Deserialize` derive on the
/// struct skips the constructor, so a caller can send
/// `{ owner: "alderco", repo: "codesense " }` (trailing space, control
/// chars, leading dot, etc.) and the slug reaches consumers unchecked.
/// Critical for write commands whose [`acquire_repo_lock`] keys a
/// process-wide DashMap on the slug — trivial textual variations would
/// otherwise produce distinct mutexes and bypass serialization.
/// Review-iter-4 (P-4).
fn validate_repo_slug(slug: &SourceRepoSlug) -> Result<(), Error> {
    if let Some(reason) = validate_path_segment(&slug.owner) {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoSlug.owner",
            reason: reason.to_string(),
        });
    }
    if let Some(reason) = validate_path_segment(&slug.repo) {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoSlug.repo",
            reason: reason.to_string(),
        });
    }
    if let Some(reason) = validate_path_segment(&slug.provider) {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoSlug.provider",
            reason: reason.to_string(),
        });
    }
    if !slug.has_valid_host() {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoSlug.host",
            reason: "must be a valid bare host matching the provider".to_string(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RepoLockRegistry (D-12) — temporary per-code-repo serialization bridge
// until SS-4c lands `content_engine::sync::lockfile_mutex_for_repo`.
// ---------------------------------------------------------------------------

// TODO(SS-4c): replace with content_engine::sync::lockfile_mutex_for_repo.
// When that lands, every `RepoLockRegistry::acquire(...).await` call here
// becomes `lockfile_mutex_for_repo(&slug).lock().await` and this module
// state goes away.
type LockRegistry = DashMap<String, Arc<Mutex<()>>>;

static REPO_LOCKS: OnceLock<LockRegistry> = OnceLock::new();

fn repo_locks() -> &'static LockRegistry {
    REPO_LOCKS.get_or_init(DashMap::new)
}

/// Acquire the per-repo serialization mutex. Returns an
/// [`tokio::sync::OwnedMutexGuard`] so the lifetime is detached from the
/// registry's borrow — required since the registry is keyed by `slug` and
/// concurrent writers across different repos must not block each other.
async fn acquire_repo_lock(slug: &SourceRepoSlug) -> tokio::sync::OwnedMutexGuard<()> {
    let key = slug.remote_key();
    let mutex: Arc<Mutex<()>> = repo_locks()
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    mutex.lock_owned().await
}

type WorkspaceLockRegistry = DashMap<PathBuf, Arc<Mutex<()>>>;

static WORKSPACE_LOCKS: OnceLock<WorkspaceLockRegistry> = OnceLock::new();

fn workspace_locks() -> &'static WorkspaceLockRegistry {
    WORKSPACE_LOCKS.get_or_init(DashMap::new)
}

/// Serialize moves on a workspace that has no repo slug (so
/// [`acquire_repo_lock`] doesn't apply). Keyed by a layer root.
async fn acquire_workspace_lock(key: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let mutex: Arc<Mutex<()>> = workspace_locks()
        .entry(key.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    mutex.lock_owned().await
}

/// Lock BOTH roots a workspace subtree move touches. A subtree move reads
/// and deletes from the source root and writes into the destination root,
/// so locking only one leaves the other open to a concurrent
/// opposite-direction move (A→B racing B→A) — keying on a single root
/// would let those two take different keys and interleave. Locks are
/// taken in canonical (sorted) order so no pair of moves can deadlock,
/// and identical roots (shouldn't happen — same-layer is pre-rejected)
/// are de-duplicated to avoid a self-deadlock on the non-reentrant mutex.
async fn acquire_workspace_locks(a: &Path, b: &Path) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut keys = [a, b];
    keys.sort();
    let mut guards = Vec::with_capacity(2);
    let mut prev: Option<&Path> = None;
    for key in keys {
        if prev == Some(key) {
            continue;
        }
        guards.push(acquire_workspace_lock(key).await);
        prev = Some(key);
    }
    guards
}

// ---------------------------------------------------------------------------
// RF-11: LayerCommandContext builder — the shared precondition seam.
//
// Every layer-scoped write command (`move_between_layers`,
// `sense_sync_write_to_layer`, `promote_content_entry`) repeats the same
// front-half staircase: carry the five context params → validate the slug →
// derive the resolver context (keeping the slug for lock keying). Bundling
// them here (a) collapses `move_between_layers`' former 11-parameter
// signature and (b) gives one place to attach cross-cutting concerns.
//
// Deliberately NOT a monolith: the pre-lock classifiers
// (`classify_move_precondition`, `classify_write_precondition`,
// `classify_promote_target`) fire at a command-specific point in the
// precondition ORDER, so each command still calls `validate_slug()` /
// `derive()` at its own point rather than through one fused method. This
// keeps the first-error-wins behavior byte-identical (RF-11 AC-3).
// ---------------------------------------------------------------------------

/// The five context parameters every layer command threads through. A plain,
/// still-unvalidated input bundle: `derive()` is what turns it into a
/// [`LayerCommandContext`].
struct LayerContextParams {
    code_repo_slug: Option<SourceRepoSlug>,
    codesense_home: String,
    user_id: u64,
    org_slug: String,
    team_name: Option<String>,
}

impl LayerContextParams {
    /// Validate the deserialized slug (if any) against the same rules
    /// `SourceRepoSlug::new` enforces. Call at each command's slug-validation
    /// point — kept separate from [`Self::derive`] because a `classify_*`
    /// guard runs between the two in `move`/`write`.
    fn validate_slug(&self) -> Result<(), Error> {
        if let Some(slug) = self.code_repo_slug.as_ref() {
            validate_repo_slug(slug)?;
        }
        Ok(())
    }

    /// Consume the params to derive the resolver context, retaining the slug
    /// for downstream lock keying. Moves the owned strings straight into
    /// [`derive_resolver_context`] (no clone). This is the one shared derive
    /// seam — identical across every write command.
    fn derive(self) -> Result<LayerCommandContext, Error> {
        let resolver = derive_resolver_context(
            PathBuf::from(self.codesense_home),
            self.user_id,
            self.org_slug,
            self.team_name,
        )?;
        Ok(LayerCommandContext {
            resolver,
            code_repo_slug: self.code_repo_slug,
        })
    }
}

/// A derived layer-command context: the resolved [`ResolverContext`] paired
/// with the slug that keys its per-repo lock. Produced by
/// [`LayerContextParams::derive`].
struct LayerCommandContext {
    resolver: ResolverContext,
    code_repo_slug: Option<SourceRepoSlug>,
}

impl LayerCommandContext {
    /// Borrow the repository identity for lock keying.
    fn slug(&self) -> Option<&SourceRepoSlug> {
        self.code_repo_slug.as_ref()
    }
}

/// The three lock shapes the layer write commands take, unified so the
/// "which lock, in what order" discipline lives in ONE place (RF-11 AC-4).
/// All variants borrow — the caller keeps owning its slugs/paths.
enum LockPlan<'a> {
    /// A single optional per-repo lock (single-file move, layer write).
    /// `None` → no lock: a workspace-only single-file op keeps today's
    /// bounded single-rename race rather than serializing.
    Repo(Option<&'a SourceRepoSlug>),
    /// Both workspace layer roots (a subtree move with no repo slug), taken
    /// in canonical sorted order with identical roots de-duplicated — a
    /// subtree move reads/deletes the source and writes the destination, so
    /// locking one root alone would let an opposite-direction move interleave.
    WorkspacePair(&'a Path, &'a Path),
    /// N per-repo locks (promote fans a slug-agnostic file across every
    /// attached repo). `acquire` sorts into one canonical global order and
    /// de-duplicates, so callers need not pre-sort and concurrent promotes
    /// can never deadlock — the ordering invariant is owned here, mirroring
    /// [`WorkspacePair`](LockPlan::WorkspacePair).
    RepoSet(&'a [SourceRepoSlug]),
}

impl LockPlan<'_> {
    /// Acquire the guard(s) for this plan. Returns a `Vec` so all three
    /// shapes share one hold/drop path (single lock → 1-elem vec; no lock →
    /// empty). Every acquisition stays `.lock_owned().await` — cooperative,
    /// never a blocking `try_lock` spin (RF-5 `.await`-vs-`try_` discipline),
    /// so the async command thread is never parked off-runtime.
    async fn acquire(&self) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        match self {
            LockPlan::Repo(Some(slug)) => vec![acquire_repo_lock(slug).await],
            LockPlan::Repo(None) => Vec::new(),
            LockPlan::WorkspacePair(a, b) => acquire_workspace_locks(a, b).await,
            LockPlan::RepoSet(slugs) => {
                // Own the deadlock-safety invariant here (mirrors
                // `acquire_workspace_locks`): sort into ONE canonical global
                // order and de-duplicate, so a repeated slug can't self-deadlock
                // the non-reentrant mutex and concurrent promotes always take
                // locks in the same order — independent of caller order, and
                // enforced in release builds (not just a debug assertion).
                let mut ordered: Vec<&SourceRepoSlug> = slugs.iter().collect();
                ordered.sort_by(|a, b| (&a.owner, &a.repo).cmp(&(&b.owner, &b.repo)));
                ordered.dedup();
                let mut guards = Vec::with_capacity(ordered.len());
                for slug in ordered {
                    guards.push(acquire_repo_lock(slug).await);
                }
                guards
            }
        }
    }
}

/// Resolve the watcher suppression handle, turning the `Manager::state`
/// panic surface into a typed [`Error::Io`] when the watcher isn't
/// registered. Shared by every write command that installs suppression
/// before touching the filesystem (RF-11 DRY: was open-coded per command).
pub(crate) fn resolve_suppression_handle(
    app: &tauri::AppHandle,
) -> Result<SuppressionHandle, Error> {
    app.try_state::<crate::commands::watch::WatcherState>()
        .map(|s| s.dispatcher.suppression_handle())
        .ok_or_else(|| Error::Io {
            op: "state",
            path: PathBuf::new(),
            message: "WatcherState not registered with app".into(),
        })
}

// ---------------------------------------------------------------------------
// CONTENT_LAYER_PATTERNS (D-14) — "what could be codesense-managed" path
// list for sense_sync_scan_cwd's untracked detection. Consumer-shaped;
// distinct from codesense_fs_core::filters::IGNORE_GLOBS.
// ---------------------------------------------------------------------------

/// Glob patterns identifying code-repo-relative paths that *could* be
/// codesense-managed content. Used by [`sense_sync_scan_cwd`] to flag
/// local edits that have no lockfile entry.
pub const CONTENT_LAYER_PATTERNS: &[&str] = &[
    ".claude/skills/*/SKILL.md",
    ".claude/skills/*/**/*",
    ".claude/hooks/codesense-*",
    ".claude/hooks/sense-*",
    ".claude/agents/*.md",
    ".claude/commands/*.md",
];

fn content_layer_matcher() -> &'static [(globset::GlobMatcher, &'static str)] {
    static MATCHER: OnceLock<Vec<(globset::GlobMatcher, &'static str)>> = OnceLock::new();
    MATCHER.get_or_init(|| {
        CONTENT_LAYER_PATTERNS
            .iter()
            .map(|pat| {
                let glob = globset::Glob::new(pat)
                    .unwrap_or_else(|e| panic!("invalid CONTENT_LAYER_PATTERNS {:?}: {}", pat, e));
                (glob.compile_matcher(), *pat)
            })
            .collect()
    })
}

fn is_content_layer_path(rel: &Path) -> Option<&'static str> {
    for (matcher, pat) in content_layer_matcher() {
        if matcher.is_match(rel) {
            return Some(pat);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Response types (AC-4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerInfo {
    pub id: LayerId,
    pub label: String,
    pub writable: bool,
    pub backed: bool,
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerEntry {
    pub logical_name: String,
}

#[cfg(test)]
/// SLW-IN-4 / SLW-IN-7 (tests only since SW-XS-15 retired the composite overlay that
/// reported it): extract the winning team's name from a resolver source
/// path of the flat shape `…/{team}/{marker}/…` (install, repo, or content roots
/// all sit directly under the team dir). Anchors on the first layer-marker
/// (`install`/`content`/`repos`/`libraries`) and returns the segment
/// immediately BEFORE it, or `None` when the path has no such marker. Pure path
/// math — the single source of truth for "which team won" on the composite
/// payload (AC-4).
fn team_from_source_path(path: &std::path::Path) -> Option<String> {
    use crate::context_repo::LAYER_MARKER_DIRS;
    let mut prev: Option<String> = None;
    for c in path.components() {
        if let std::path::Component::Normal(seg) = c {
            let name = seg.to_str().map(str::to_string);
            if let Some(ref n) = name {
                if LAYER_MARKER_DIRS.contains(&n.as_str()) {
                    return prev;
                }
            }
            prev = name;
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MoveOutcome {
    /// Move succeeded. `count` is the number of files moved — `1` for a
    /// single-file move, the leaf count for a subtree move.
    Moved {
        count: usize,
    },
    /// A subtree move under `ConflictPolicy::SkipExisting` that moved
    /// `moved` leaves and left the `skipped` colliding leaves in place.
    /// `skipped` carries the colliding leaves' **logical names** (not just
    /// a count) so a per-file conflict resolver (the workbench
    /// `ConflictBatchDialog`) can render and replay them. Only the
    /// `SkipExisting` policy produces this; `RejectWhole` rejects instead.
    MovedPartial {
        moved: usize,
        skipped: Vec<String>,
    },
    RejectedSameLayer,
    RejectedSourceMissing,
    RejectedTargetExists,
    RejectedSourceUnbacked,
    RejectedTargetUnbacked,
    RejectedCrossFilesystem,
}

/// What to move: a single `logical_name`, or a directory subtree
/// identified by a logical-name prefix (e.g. `.claude/skills/bmad-tea`).
/// Both are validated by [`is_safe_logical_name`].
#[derive(Debug, Clone)]
pub enum MoveSelector {
    File(String),
    Subtree(String),
}

impl MoveSelector {
    /// The `logical_name` (file) or prefix (subtree) being moved.
    fn name(&self) -> &str {
        match self {
            MoveSelector::File(n) | MoveSelector::Subtree(n) => n,
        }
    }
}

/// How a subtree move resolves a per-leaf collision with an existing
/// target file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    /// Reject the whole move if any leaf collides; change nothing.
    #[default]
    RejectWhole,
    /// Move non-colliding leaves; report the colliding ones as skipped.
    SkipExisting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WriteOutcome {
    Written,
    RejectedTargetUnbacked,
    /// SS-6 Task 1.3 — atomic 3-way conflict detection. Returned when
    /// `WriteToLayerParams::expected_layer_hash` is `Some(h)`, the
    /// caller did NOT set `force=true`, and the current on-disk layer
    /// file's blake3 hash differs from `h`. Read + compare happen
    /// inside the per-repo lock acquired by
    /// [`sense_sync_write_to_layer`], so detection is atomic with the
    /// write — no TOCTOU window. `layer_current_hash` lets the caller
    /// surface the layer's actual state in the conflict prompt.
    ///
    /// Returned by `sense_sync_write_to_layer` when an `expected_layer_hash`
    /// gate mismatches. (The SS-6 manual push/revert MCP surface that once
    /// consumed this was retired in SW-CP-10 (T4) — automatic bidirectional
    /// install reverse handles tracked edits now.)
    RejectedConflict {
        // Review iter-3: the enum-level `#[serde(rename_all =
        // "camelCase")]` renames VARIANTS (`RejectedConflict` →
        // `rejectedConflict`) but does NOT cascade into struct-variant
        // FIELDS — that requires `rename_all_fields` (newer serde) or
        // an explicit per-field `#[serde(rename = ...)]`. Pinning
        // explicitly because the wire-shape test
        // (`write_outcome_rejected_conflict_serializes_with_camelcase_hash`)
        // is the contract for TS consumers.
        #[serde(rename = "layerCurrentHash")]
        layer_current_hash: [u8; 32],
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub dirty: Vec<DirtyEntry>,
    pub untracked: Vec<UntrackedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyEntry {
    pub logical_name: String,
    pub dest_path: PathBuf,
    pub source_layer: LayerId,
    pub expected_hash: [u8; 32],
    pub actual_hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntrackedEntry {
    pub code_repo_relative_path: PathBuf,
    pub matched_pattern: String,
}

fn layer_label(layer: LayerId) -> &'static str {
    match layer {
        LayerId::TeamProjection => "Team content",
    }
}

fn layer_is_writable(_layer: LayerId) -> bool {
    // Every remaining layer is a writable filesystem layer (the read-only
    // Core layer was removed in SW-CP-7).
    true
}

// ---------------------------------------------------------------------------
// Read-side commands (AC-3 rows 1-5 + 8, AC-8)
// ---------------------------------------------------------------------------

/// Tauri-deserialized parameters for [`list_layers`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListLayersParams {
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    pub code_repo_slug: Option<SourceRepoSlug>,
    pub writable_only: bool,
}

/// AC-3 row 1: enumerate the layers with backing/writability metadata.
///
/// Wraps [`content_layer_root`] per [`LayerId`].
/// `writable_only=true` filters out non-writable layers (none today).
#[tauri::command]
pub async fn list_layers(params: ListLayersParams) -> Result<Vec<LayerInfo>, Error> {
    let ListLayersParams {
        codesense_home,
        user_id,
        org_slug,
        team_name,
        code_repo_slug,
        writable_only,
    } = params;
    let ctx = match derive_resolver_context(
        PathBuf::from(codesense_home),
        user_id,
        org_slug,
        team_name,
    ) {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                target: "content_engine::commands",
                "list_layers context derivation failed: {}",
                e
            );
            return Err(e);
        }
    };
    if let Some(slug) = code_repo_slug.as_ref() {
        if let Err(e) = validate_repo_slug(slug) {
            log::error!(
                target: "content_engine::commands",
                "list_layers slug validation failed: {}",
                e
            );
            return Err(e);
        }
    }

    let mut out = Vec::with_capacity(PRIORITY_ORDER.len());
    for &layer in PRIORITY_ORDER {
        if writable_only && !layer_is_writable(layer) {
            continue;
        }
        let source_path = content_layer_root(layer, &ctx);
        let backed = source_path.is_some();
        out.push(LayerInfo {
            id: layer,
            label: layer_label(layer).to_string(),
            writable: layer_is_writable(layer),
            backed,
            source_path,
        });
    }
    log::info!(
        target: "content_engine::commands",
        "list_layers writable_only={} → {} layers",
        writable_only,
        out.len()
    );
    Ok(out)
}

/// Tauri-deserialized parameters for [`list_layer_contents`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListLayerContentsParams {
    pub layer_id: LayerId,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    pub code_repo_slug: Option<SourceRepoSlug>,
}

/// AC-3 row 2: per-layer file enumeration. Wraps
/// [`list_layer`].
#[tauri::command]
pub async fn list_layer_contents(
    params: ListLayerContentsParams,
) -> Result<Vec<LayerEntry>, Error> {
    let ListLayerContentsParams {
        layer_id,
        codesense_home,
        user_id,
        org_slug,
        team_name,
        code_repo_slug,
    } = params;
    let ctx = match derive_resolver_context(
        PathBuf::from(codesense_home),
        user_id,
        org_slug,
        team_name,
    ) {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                target: "content_engine::commands",
                "list_layer_contents {:?} context derivation failed: {}",
                layer_id,
                e
            );
            return Err(e);
        }
    };
    if let Some(slug) = code_repo_slug.as_ref() {
        if let Err(e) = validate_repo_slug(slug) {
            log::error!(
                target: "content_engine::commands",
                "list_layer_contents {:?} slug validation failed: {}",
                layer_id,
                e
            );
            return Err(e);
        }
    }
    // Review-iter-5 (EH-12): the walkdir-backed listing runs in
    // `spawn_blocking` so a large team content tree never blocks the tokio
    // runtime thread. Matches the NB-1 / NB-2 invariant.
    let join_result = tokio::task::spawn_blocking(move || -> Result<Vec<String>, Error> {
        Ok(list_layer(layer_id, &ctx))
    })
    .await;
    let names = match join_result {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            log::error!(
                target: "content_engine::commands",
                "list_layer_contents {:?} failed: {}",
                layer_id,
                e
            );
            return Err(e);
        }
        Err(join_err) => {
            log::error!(
                target: "content_engine::commands",
                "list_layer_contents {:?} blocking task failed: {}",
                layer_id,
                join_err
            );
            return Err(Error::Io {
                op: "engine",
                path: PathBuf::new(),
                message: format!("blocking task failed: {}", join_err),
            });
        }
    };
    log::info!(
        target: "content_engine::commands",
        "list_layer_contents {:?} → {} entries",
        layer_id,
        names.len()
    );
    Ok(names
        .into_iter()
        .map(|n| LayerEntry { logical_name: n })
        .collect())
}

/// Tauri-deserialized parameters for [`read_layer_file`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadLayerFileParams {
    pub layer_id: LayerId,
    pub logical_name: String,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    pub code_repo_slug: Option<SourceRepoSlug>,
}

/// AC-3 row 3: read a file from a specific layer. Every layer `fs::read`s
/// against [`content_layer_root`] joined with `logical_name`.
#[tauri::command]
pub async fn read_layer_file(params: ReadLayerFileParams) -> Result<Vec<u8>, Error> {
    read_layer_file_for(params, &Default::default()).await
}

pub(crate) async fn read_layer_file_for(
    params: ReadLayerFileParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<u8>, Error> {
    let ReadLayerFileParams {
        layer_id,
        logical_name,
        codesense_home,
        user_id,
        org_slug,
        team_name,
        code_repo_slug,
    } = params;
    if !is_safe_logical_name(&logical_name) {
        log::error!(
            target: "content_engine::commands",
            "read_layer_file rejected invalid logical_name {:?}",
            logical_name
        );
        return Err(Error::InvalidLogicalName(logical_name));
    }
    let ctx = derive_resolver_context(PathBuf::from(codesense_home), user_id, org_slug, team_name)?;
    if let Some(slug) = code_repo_slug.as_ref() {
        validate_repo_slug(slug)?;
    }

    let root = content_layer_root(layer_id, &ctx).ok_or(Error::LayerUnbacked(layer_id))?;
    let path = root.join(&logical_name);
    if !scope.allows(&path) {
        return Err(Error::FileNotFound {
            layer: layer_id,
            logical_name,
        });
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            log::info!(
                target: "content_engine::commands",
                "read_layer_file {:?} {:?} → {} bytes",
                layer_id,
                logical_name,
                bytes.len()
            );
            Ok(bytes)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::FileNotFound {
            layer: layer_id,
            logical_name,
        }),
        Err(e) => {
            log::error!(
                target: "content_engine::commands",
                "read_layer_file {:?} {:?} failed: {}",
                layer_id,
                logical_name,
                e
            );
            Err(Error::io("read", &path, e))
        }
    }
}

/// Tauri-deserialized parameters for [`read_lockfile_for_repo`].
///
/// `code_repo_path` is included so the empty-lockfile fallback carries a
/// meaningful `Lockfile.code_repo_path` field — review-iter-1 decision
/// (AC-3 row 5 deviation, documented in Dev Agent Record). Without it
/// the empty-fallback returned `PathBuf::new()` and TS consumers got
/// misleading state.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadLockfileForRepoParams {
    pub code_repo_slug: SourceRepoSlug,
    pub code_repo_path: PathBuf,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
}

/// AC-3 row 5: read the v3 lockfile for `(org, team, code_repo_slug)`.
/// Returns `Lockfile::empty(org_slug, team_name, code_repo_path)` when
/// the lockfile does not yet exist.
///
/// Race handling: previously this command pre-checked existence via
/// `tokio::fs::try_exists` then dispatched to `Lockfile::read`, which
/// opened a TOCTOU window. Review-iter-1 collapsed that to a single
/// `spawn_blocking` that calls `Lockfile::read` and maps a `NotFound`
/// Io error to `Lockfile::empty(...)` inside the same task — atomic from
/// the caller's perspective.
#[tauri::command]
pub async fn read_lockfile_for_repo(params: ReadLockfileForRepoParams) -> Result<Lockfile, Error> {
    let ReadLockfileForRepoParams {
        code_repo_slug,
        code_repo_path,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    } = params;
    let ctx = derive_resolver_context(
        PathBuf::from(codesense_home),
        user_id,
        org_slug.clone(),
        team_name.clone(),
    )?;
    validate_repo_slug(&code_repo_slug)?;
    // Review-iter-4 (P-3): validate `code_repo_path` BEFORE constructing
    // any Lockfile::empty fallback so a hostile/malformed caller can't
    // smuggle NUL bytes or other invalid bytes into the returned
    // lockfile. Non-empty + absolute mirrors the contract enforced on
    // `codesense_home`; both fields are paths the lockfile contract
    // depends on.
    if code_repo_path.as_os_str().is_empty() {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoPath",
            reason: "empty path".into(),
        });
    }
    if code_repo_path.to_string_lossy().as_bytes().contains(&0) {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoPath",
            reason: "NUL byte in path".into(),
        });
    }
    if !code_repo_path.is_absolute() {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoPath",
            reason: "must be absolute path".into(),
        });
    }
    let slug_for_log = code_repo_slug.clone();
    let path_for_log = code_repo_path.clone();
    // SW-IS-3c: read install-state from the store (the authoritative substrate).
    // Runs off the async worker via `spawn_blocking`, matching the prior read's
    // boundary. A store miss is a fresh target → an empty lockfile.
    let result = tokio::task::spawn_blocking(move || -> Result<Lockfile, Error> {
        let store = crate::content_engine::lockfile::open_sync_state_store(&ctx.codesense_home)
            .ok_or_else(|| Error::Io {
                op: "open",
                path: ctx.codesense_home.clone(),
                message: "install store unavailable".to_string(),
            })?;
        match crate::content_engine::sync_state_store::read_through(&store, &ctx, &code_repo_path) {
            Ok(Some(lf)) => Ok(lf),
            Ok(None) => Ok(Lockfile::empty(
                ctx.org_slug.clone(),
                ctx.team_name.clone(),
                code_repo_path.clone(),
            )),
            Err(e) => Err(Error::Io {
                op: "read",
                path: code_repo_path.clone(),
                message: format!("install store read: {e}"),
            }),
        }
    })
    .await
    .map_err(|e| Error::Io {
        op: "read",
        path: path_for_log.clone(),
        message: format!("blocking task failed: {}", e),
    })?;
    match &result {
        Ok(lf) => log::info!(
            target: "content_engine::commands",
            "read_lockfile_for_repo {} from {} → {} entries",
            slug_for_log,
            path_for_log.display(),
            lf.entries.len()
        ),
        Err(e) => log::error!(
            target: "content_engine::commands",
            "read_lockfile_for_repo {} from {} failed: {}",
            slug_for_log,
            path_for_log.display(),
            e
        ),
    }
    result
}

/// Tauri-deserialized parameters for [`sense_sync_scan_cwd`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenseSyncScanCwdParams {
    pub code_repo_slug: SourceRepoSlug,
    pub code_repo_path: PathBuf,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
}

/// AC-3 row 8: scan the code repo for dirty (locally edited) +
/// untracked (matches content patterns but absent from lockfile) files.
///
/// Algorithm: lockfile → hash each `MaterializedEntry::dest_path` and
/// compare to recorded hash → walk repo, filter by `CONTENT_LAYER_PATTERNS`
/// (excluding `IGNORE_GLOBS` noise), flag entries with no lockfile.
#[tauri::command]
pub async fn sense_sync_scan_cwd(params: SenseSyncScanCwdParams) -> Result<ScanReport, Error> {
    let SenseSyncScanCwdParams {
        code_repo_slug,
        code_repo_path,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    } = params;
    // Review-iter-4 (P-16): validate the caller-supplied `code_repo_path`
    // at the boundary. Relative input (`"."`, whitespace, etc.) makes
    // walkdir implicitly process-cwd-relative and `strip_prefix` against
    // absolute lockfile `dest_path` fails for every entry — producing a
    // falsely-flooded `untracked` list. Reject empty and non-absolute.
    if code_repo_path.as_os_str().is_empty() {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoPath",
            reason: "empty path".into(),
        });
    }
    if !code_repo_path.is_absolute() {
        return Err(Error::InvalidContextSegment {
            field: "codeRepoPath",
            reason: "must be absolute path".into(),
        });
    }
    // Review-iter-4 (P-21): reject a symlinked repo root before invoking
    // walkdir. Without this guard, a `code_repo_path` symlinked to `/`
    // makes the walk descend to depth 64 enumerating the system root.
    // `symlink_metadata` does NOT follow the symlink, so we see the link
    // itself and can reject it.
    match std::fs::symlink_metadata(&code_repo_path) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(Error::Io {
                op: "stat",
                path: code_repo_path.clone(),
                message: "code_repo_path is a symlink — refusing to scan".into(),
            });
        }
        Ok(_) => {}
        // Non-existent or unreadable root is OK to continue — the inner
        // `repo_root.is_dir()` guard treats missing dirs as "no untracked
        // candidates" rather than a hard error.
        Err(_) => {}
    }
    // Review-iter-5 (EH-14): require a `.git` (file or dir) at
    // `code_repo_path` before invoking walkdir. Light defense-in-depth so
    // a malformed Tauri payload cannot drive the scanner against `/etc`,
    // `/private/var`, or any non-repo directory. Callers only ever call
    // this command from inside a checked-out code repo. `.git` may be either a directory
    // (normal clone) or a file (linked worktree pointing back at the
    // primary's gitdir).
    //
    // Review-iter-7 (Blind-H6 + Edge-M2 + Edge-M3): only
    // `ErrorKind::NotFound` becomes `NotAGitWorkingTree`. Other stat
    // errors (permission-denied, etc.) propagate as typed
    // `Error::Io { op: "stat" }` so operators can distinguish "wrong
    // directory" from "I/O problem reading this directory".
    let git_marker = code_repo_path.join(".git");
    match std::fs::symlink_metadata(&git_marker) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotAGitWorkingTree(code_repo_path));
        }
        Err(e) => {
            return Err(Error::Io {
                op: "stat",
                path: git_marker,
                message: e.to_string(),
            });
        }
    }
    let lockfile = read_lockfile_for_repo(ReadLockfileForRepoParams {
        code_repo_slug,
        code_repo_path: code_repo_path.clone(),
        codesense_home,
        user_id,
        org_slug,
        team_name,
    })
    .await?;

    // Review-iter-1: warn (don't fail) if the caller-supplied path
    // diverges from the lockfile's recorded path. Mismatch usually
    // means the lockfile was written for a different clone of the
    // repo or the repo has been moved on disk; either is recoverable
    // by re-materializing, but worth logging.
    if !lockfile.code_repo_path.as_os_str().is_empty() && lockfile.code_repo_path != code_repo_path
    {
        log::warn!(
            target: "content_engine::commands",
            "sense_sync_scan_cwd: caller code_repo_path {} differs from lockfile's recorded {}",
            code_repo_path.display(),
            lockfile.code_repo_path.display()
        );
    }

    // Move into the blocking task for walkdir + hash; spawn_blocking with
    // owned data avoids holding a JoinHandle to non-Send refs. Clone an
    // owned copy for the post-await error path before the closure takes
    // ownership.
    let repo_root_for_err = code_repo_path.clone();
    let repo_root = code_repo_path;
    let result = tokio::task::spawn_blocking(move || -> Result<ScanReport, Error> {
        // Review-iter-4 (P-6): treat whitespace-only / relative lockfile
        // `code_repo_path` symmetrically with the empty case. An
        // `lockfile.code_repo_path = " "` passes the `is_empty()` check
        // but `strip_prefix(" ")` against absolute dest_paths returns
        // Err for every entry, producing the same false-empty HashSet
        // that the iter-3 empty-string guard was supposed to prevent.
        let lockfile_root = &lockfile.code_repo_path;
        let lockfile_root_usable =
            !lockfile_root.as_os_str().is_empty() && lockfile_root.is_absolute();

        // Review-iter-4 (P-2): security gate on lockfile-supplied
        // `dest_path`. The dirty loop hashes whatever path the lockfile
        // records — a corrupt or hand-edited lockfile carrying
        // `dest_path: "/etc/passwd"` would cause `hash_file("/etc/passwd")`
        // and surface its existence/mismatch in the response. Require
        // `dest_path` to live under either the caller's `repo_root` or
        // the lockfile's recorded `code_repo_path` (whichever is usable).
        // Any other path is treated as "skip — not within our scope."
        //
        // Review-iter-4 (P-15): also captures the relative path used by
        // untracked detection so the SAME file isn't double-reported as
        // both dirty (matched under repo_root) AND untracked (HashSet
        // missing because strip_prefix succeeded for hashing but the
        // entry was added below). Building the HashSet here lets us
        // attribute it to the same entries we hashed.
        let mut dirty: Vec<DirtyEntry> = Vec::new();
        let mut lockfile_relative_paths: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        for entry in &lockfile.entries {
            // Compute the entry's relative path against the usable roots.
            // Skip the entry entirely if neither root contains it —
            // either the lockfile is corrupt or the entry refers to a
            // file outside the caller's scope (security gate).
            let primary = entry.dest_path.strip_prefix(&repo_root).ok();
            let fallback = if lockfile_root_usable {
                entry.dest_path.strip_prefix(lockfile_root).ok()
            } else {
                None
            };
            // Review-iter-5 (F1 + EH-5): when the lockfile records a
            // different absolute `code_repo_path` than the caller's
            // (e.g. user moved the repo on disk), the fallback branch
            // hits. Previously we kept hashing `entry.dest_path` (the
            // stale absolute path), which NotFound-skipped silently
            // AND inserted the relative path into
            // `lockfile_relative_paths` — making locally-edited files
            // vanish from BOTH `dirty` AND `untracked`. Rewrite the
            // path to the caller's `repo_root` so stat+hash run
            // against the live file. The relative-path insertion is
            // deferred until after we confirm the live file exists,
            // so phantom entries from a stale lockfile don't suppress
            // legitimate untracked detection.
            let (rel, effective_dest_path) = match primary {
                Some(r) => (PathBuf::from(r), entry.dest_path.clone()),
                None => match fallback {
                    Some(r) => {
                        let rel_buf = PathBuf::from(r);
                        let rewritten = repo_root.join(&rel_buf);
                        log::info!(
                            target: "content_engine::commands",
                            "sense_sync_scan_cwd: rewriting stale lockfile entry {:?} \
                             from {} to {} (lockfile root {} differs from caller's {})",
                            entry.logical_name,
                            entry.dest_path.display(),
                            rewritten.display(),
                            lockfile_root.display(),
                            repo_root.display()
                        );
                        (rel_buf, rewritten)
                    }
                    None => {
                        log::warn!(
                            target: "content_engine::commands",
                            "sense_sync_scan_cwd: skipping lockfile entry {:?} \
                             — dest_path {} is outside repo_root {} and lockfile_root {}",
                            entry.logical_name,
                            entry.dest_path.display(),
                            repo_root.display(),
                            lockfile_root.display()
                        );
                        continue;
                    }
                },
            };
            // Record the relative path so the untracked walker below
            // skips it. The lockfile CLAIMS this relative location for
            // codesense; whether the live file is missing, regular, or
            // swapped to a symlink, the untracked walker must not
            // double-report it. Review-iter-5 (F1) moved this insert
            // after the symlink_metadata check, which regressed the
            // tracked-then-swapped-to-symlink case (file disappeared
            // from `dirty` AND surfaced in `untracked`). Review-iter-7
            // (Blind-C3) restores the unconditional insert; the F1
            // path-rewrite for stat+hash is independent and remains
            // applied to `effective_dest_path`.
            lockfile_relative_paths.insert(rel);

            // File deleted from disk → downstream sync concern, not dirty.
            // Use `symlink_metadata` so a symlink at the materialized path
            // is treated as "not a regular file" (resolver-style semantics).
            // `Path::is_file()` follows symlinks, which would hash the
            // target instead of recognizing that the user swapped the
            // materialized file for a link. Review-iter-3 fix.
            match std::fs::symlink_metadata(&effective_dest_path) {
                Ok(m) if m.is_file() => {}
                Ok(_) => continue, // symlink, directory, etc. — not "ours" anymore
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(Error::Io {
                        op: "stat",
                        path: effective_dest_path,
                        message: e.to_string(),
                    });
                }
            }

            // Race: hash_file may also see NotFound if the file was
            // deleted between `is_file()` and `hash_file()`. Treat that
            // case as "skip, not dirty" to honor the line-above intent
            // — review-iter-1 fix. Other I/O errors still propagate.
            let actual = match hash_file(&effective_dest_path) {
                Ok(h) => h,
                Err(content_engine::Error::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(e) => {
                    return Err(Error::Io {
                        op: "hash",
                        path: effective_dest_path,
                        message: e.to_string(),
                    });
                }
            };
            if actual != entry.last_materialized_hash {
                dirty.push(DirtyEntry {
                    logical_name: entry.logical_name.clone(),
                    dest_path: effective_dest_path,
                    source_layer: entry.source_layer,
                    expected_hash: entry.last_materialized_hash,
                    actual_hash: actual,
                });
            }
        }

        let mut untracked: Vec<UntrackedEntry> = Vec::new();
        // SS-FU-3 allowlist: every CONTENT_LAYER_PATTERNS entry lives
        // under `.claude/` (pinned by
        // `all_content_layer_patterns_under_dot_claude` test). Walking
        // only that subtree replaces the previous
        // `.git | node_modules | target | dist | .idea` blocklist with an
        // allowlist that's correct by construction — no out-of-scope
        // stat'ing of `node_modules`, `target`, linked worktrees under
        // `_codesense/`, etc.
        let claude_dir = repo_root.join(crate::context_repo::CLAUDE_CONFIG_DIR);
        // AC-5b: gate via `symlink_metadata` (does NOT follow symlinks)
        // so a symlinked `.claude/` is treated as "no scan target" —
        // matches today's behavior where walkdir starting at
        // `repo_root` would not descend into a symlinked `.claude/`
        // child. `Path::is_dir()` follows symlinks, and
        // `walkdir::WalkDir::new(symlinked_root)` enters the target of
        // a symlinked root even with `follow_links(false)` (the flag
        // governs descendants, not the walk root). The check below
        // mirrors the pattern used by the `repo_root` symlink guard at
        // the top of this command.
        //
        // Review iter-1 (Blind M1 / Edge L1) — there is a structural
        // TOCTOU window between this `symlink_metadata` and the
        // subsequent `WalkDir::new(&claude_dir)`. The repo owner is
        // outside our threat model (they own the tree we are scanning),
        // so we accept the race rather than holding an open directory
        // handle. The warn-log on non-NotFound stat errors gives
        // operators a signal that an unreadable `.claude/` exists.
        //
        // Review iter-1 (Edge M2 / Blind L5) — distinguish stat outcomes
        // so the operator gets a signal when `.claude/` exists but
        // cannot be stat'd (PermissionDenied, EIO, etc.) or when it
        // exists but is neither a directory nor a symlink (regular file,
        // FIFO, socket — typically a user accident).
        let claude_dir_is_real_dir = match std::fs::symlink_metadata(&claude_dir) {
            Ok(m) if m.file_type().is_dir() => true,
            Ok(m) if m.file_type().is_symlink() => {
                // Review iter-2 (Blind L1 / Edge L2) — surface
                // dangling/broken symlinks. `symlink_metadata` succeeds
                // for any symlink (the link itself exists), so a
                // dotfile-manager mishap that orphans `.claude/`'s
                // target would otherwise silently produce empty scans.
                // Follow the link via `metadata()` and warn when the
                // target is missing or unreadable. A symlink pointing
                // at a non-directory (e.g., regular file) also warns —
                // it's almost certainly a user accident.
                match std::fs::metadata(&claude_dir) {
                    Ok(target_meta) if target_meta.is_dir() => {
                        // Symlink to a real directory — gate still says
                        // "don't descend" per AC-5b. No warn; this is
                        // the intentional case.
                    }
                    Ok(target_meta) => {
                        log::warn!(
                            target: "content_engine::commands",
                            "sense_sync_scan_cwd: {} is a symlink whose target is not a directory \
                             (target file_type={:?}); treating as empty",
                            claude_dir.display(),
                            target_meta.file_type()
                        );
                    }
                    Err(e) => {
                        log::warn!(
                            target: "content_engine::commands",
                            "sense_sync_scan_cwd: {} is a symlink whose target is unreadable: {}; \
                             treating as empty",
                            claude_dir.display(),
                            e
                        );
                    }
                }
                false
            }
            Ok(m) => {
                log::warn!(
                    target: "content_engine::commands",
                    "sense_sync_scan_cwd: {} exists but is neither a directory \
                     nor a symlink (file_type={:?}); treating as empty",
                    claude_dir.display(),
                    m.file_type()
                );
                false
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                log::warn!(
                    target: "content_engine::commands",
                    "sense_sync_scan_cwd: stat {} failed: {}; treating as empty",
                    claude_dir.display(),
                    e
                );
                false
            }
        };
        if claude_dir_is_real_dir {
            // Review iter-1 (Blind M3 / Edge L3) — `max_depth(64)` is
            // measured from the WALK ROOT, which under SS-FU-3 is
            // `.claude/`, not `repo_root`. The effective depth budget
            // *inside* `.claude/` therefore increased slightly (`.claude/`
            // itself no longer consumes one level). 64 levels under
            // `.claude/` remains a pathological-protection cap; reachable
            // skill trees are nowhere near it.
            for entry_result in walkdir::WalkDir::new(&claude_dir)
                .min_depth(1)
                .max_depth(64)
                .into_iter()
            {
                let entry = match entry_result {
                    Ok(e) => e,
                    Err(e) => {
                        // Review iter-1 (Blind L1) — log both the walk
                        // root (`.claude/`) and the enclosing repo so
                        // operators can pinpoint the affected repo
                        // without re-deriving it from `claude_dir`.
                        log::warn!(
                            target: "content_engine::commands",
                            "sense_sync_scan_cwd walkdir error under {} (repo {}): {}; skipping subtree",
                            claude_dir.display(),
                            repo_root.display(),
                            e
                        );
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let abs = entry.path();
                // Note: strip_prefix is against `repo_root`, not
                // `claude_dir`, so emitted paths remain repo-root-
                // relative (`.claude/skills/...`). Downstream consumers
                // (`sense_sync_write_to_layer`, lockfile lookups) depend
                // on that shape.
                //
                // Review iter-1 (Blind L2 / Edge L7) — the `Err` arm is
                // structurally unreachable given the walk root is
                // `repo_root.join(".claude")` and walkdir does not
                // canonicalize paths. `debug_assert!` documents the
                // invariant loudly in debug builds; release continues
                // silently to preserve scan robustness against a future
                // refactor that introduces a multi-root walk.
                //
                // Review iter-2 (Edge L7) — canonicalize hazard: if a
                // future refactor wraps `code_repo_path` in
                // `std::fs::canonicalize()` (e.g., to resolve macOS
                // firmlinks `/var → /private/var` or to normalize
                // trailing slashes), make sure `claude_dir` is derived
                // from the same canonical form before reaching this
                // check, otherwise the debug_assert will fire on
                // legitimate inputs.
                let rel = match abs.strip_prefix(&repo_root) {
                    Ok(r) => r,
                    Err(_) => {
                        debug_assert!(
                            false,
                            "walkdir entry {} should be under repo_root {} but strip_prefix failed",
                            abs.display(),
                            repo_root.display()
                        );
                        continue;
                    }
                };
                let rel_str = match rel.to_str() {
                    Some(s) => s.replace('\\', "/"),
                    None => continue,
                };
                if is_ignored(&rel_str) {
                    continue;
                }
                let pat = match is_content_layer_path(Path::new(&rel_str)) {
                    Some(p) => p,
                    None => continue,
                };
                let rel_buf = PathBuf::from(&rel_str);
                if lockfile_relative_paths.contains(&rel_buf) {
                    continue;
                }
                untracked.push(UntrackedEntry {
                    code_repo_relative_path: rel_buf,
                    matched_pattern: pat.to_string(),
                });
            }
        }
        untracked.sort_by(|a, b| a.code_repo_relative_path.cmp(&b.code_repo_relative_path));

        Ok(ScanReport { dirty, untracked })
    })
    .await
    .map_err(|e| Error::Io {
        op: "read",
        path: repo_root_for_err.clone(),
        message: format!("blocking task failed: {}", e),
    })?;
    match &result {
        Ok(report) => log::info!(
            target: "content_engine::commands",
            "sense_sync_scan_cwd {} → {} dirty + {} untracked",
            repo_root_for_err.display(),
            report.dirty.len(),
            report.untracked.len()
        ),
        Err(e) => log::error!(
            target: "content_engine::commands",
            "sense_sync_scan_cwd {} failed: {}",
            repo_root_for_err.display(),
            e
        ),
    }
    result
}

// ---------------------------------------------------------------------------
// Write-side commands (AC-3 rows 6-7, AC-5, AC-8)
// ---------------------------------------------------------------------------

/// Tauri-deserialized parameters for [`move_file_between_layers`].
/// Flat-struct shape kept to match the existing project pattern
/// (`InstallParams` in `commands/install.rs`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFileParams {
    pub from_layer: LayerId,
    pub to_layer: LayerId,
    pub logical_name: String,
    pub code_repo_slug: Option<SourceRepoSlug>,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    /// When `true`, `logical_name` is treated as a directory-subtree
    /// prefix and the whole subtree is moved leaf-by-leaf
    /// ([`MoveSelector::Subtree`]); when `false` (default) it is a single
    /// file ([`MoveSelector::File`]). Additive — existing single-file
    /// callers omit it and keep today's behavior.
    #[serde(default)]
    pub move_as_subtree: bool,
    /// Conflict resolution for a subtree move. Ignored for single-file
    /// moves. Defaults to [`ConflictPolicy::RejectWhole`].
    #[serde(default)]
    pub conflict_policy: ConflictPolicy,
    /// When `true`, a single-file move overwrites an existing target
    /// instead of rejecting it (`RejectedTargetExists`). Powers the
    /// workbench "Replace" choice when promoting over an existing file —
    /// POSIX `rename(2)` replaces the destination file atomically. Ignored
    /// by subtree moves, which resolve collisions via `conflict_policy`.
    /// Additive — existing callers omit it and keep today's
    /// reject-on-collision behavior.
    #[serde(default)]
    pub force: bool,
}

/// Pre-lock classifier for [`move_file_between_layers`]. Returns
/// `Some(MoveOutcome::Rejected*)` for any rejection determinable without
/// filesystem access; `None` means "proceed under the per-repo lock."
///
/// Extracted as a free function so the precondition cascade can be unit-
/// tested without a Tauri `AppHandle` (review-iter-1 fix for AC-8
/// MoveOutcome behavioral coverage).
/// Layer-source resolution classifier for [`move_file_between_layers`].
/// Returns `Some(Rejected*Unbacked)` when either side's
/// [`content_layer_root`] yields `None`; `None` means both layers have
/// a backing root and the caller can proceed to filesystem checks.
///
/// Extracted as a free function so AC-8 can pin the two unbacked
/// variants without a Tauri `AppHandle` (review-iter-4 P-12, P-13).
/// The same logic runs inline in `move_file_between_layers` immediately
/// after `classify_move_precondition` — this helper exists only for
/// testability.
fn classify_move_unbacked(
    from_layer: LayerId,
    to_layer: LayerId,
    ctx: &ResolverContext,
) -> Option<MoveOutcome> {
    if content_layer_root(from_layer, ctx).is_none() {
        return Some(MoveOutcome::RejectedSourceUnbacked);
    }
    if content_layer_root(to_layer, ctx).is_none() {
        return Some(MoveOutcome::RejectedTargetUnbacked);
    }
    None
}

/// Filesystem-state preconditions for [`move_file_between_layers`].
/// Returns `Some(Rejected*)` when the source is absent (treated as
/// missing — symlinks at the source path are also rejected via
/// [`std::fs::symlink_metadata`]) or the target already exists; `None`
/// means proceed to the rename under the per-repo lock.
///
/// Extracted as a free function so AC-8 can pin
/// `RejectedSourceMissing` and `RejectedTargetExists` (review-iter-4
/// P-10, P-11). Mirrors the inline cascade in
/// `move_file_between_layers`, which calls this same helper post-lock.
///
/// When `force` is set the caller has opted into overwriting an existing
/// target (the workbench "Replace" choice, SS-FU-18): the target-exists
/// rejection is skipped so the subsequent POSIX `rename(2)` atomically
/// replaces the destination *file*. The source-missing check is NEVER
/// skipped — a missing source still rejects. A target that exists as a
/// *directory* is not special-cased here; the rename later fails (a file
/// cannot replace a non-empty dir), so it surfaces as a hard I/O error
/// rather than a silent clobber.
fn classify_move_fs_preconditions(
    from_path: &Path,
    to_path: &Path,
    force: bool,
) -> Result<Option<MoveOutcome>, Error> {
    match classify_dest_precondition(from_path, to_path, force)? {
        DestPrecondition::SourceMissing => Ok(Some(MoveOutcome::RejectedSourceMissing)),
        DestPrecondition::TargetExists => Ok(Some(MoveOutcome::RejectedTargetExists)),
        DestPrecondition::Proceed => Ok(None),
    }
}

/// The filesystem precondition of a single-file copy/move: is the source a
/// regular file, and would landing at `to_path` clobber an existing entry?
///
/// SLW-PT-3 extracted this from [`classify_move_fs_preconditions`] so the
/// content **move** path (which maps it onto [`MoveOutcome`]) and the provider
/// **promote** copy path (`commands::files::promote_provider_item`) share one
/// source of truth for the never-silent-overwrite guard.
pub(crate) enum DestPrecondition {
    /// Source is a regular file and the destination is clear (or `force` opted
    /// into replacing an existing regular file) — proceed.
    Proceed,
    /// The source is absent or not a regular file (a symlink/dir at the source
    /// path counts as missing) — reject.
    SourceMissing,
    /// A destination entry already exists and `force` was not set, or the
    /// existing destination is a non-regular file (symlink/dir) that must never
    /// be silently clobbered — reject.
    TargetExists,
}

/// Shared precondition classifier — see [`DestPrecondition`]. `force` skips the
/// target-exists rejection **only** for an existing regular file (a symlink or
/// directory target still rejects, so it is never clobbered out from under the
/// user); a missing source is never skipped.
pub(crate) fn classify_dest_precondition(
    from_path: &Path,
    to_path: &Path,
    force: bool,
) -> Result<DestPrecondition, Error> {
    match std::fs::symlink_metadata(from_path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Ok(DestPrecondition::SourceMissing),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DestPrecondition::SourceMissing);
        }
        Err(e) => return Err(Error::io("stat", from_path, e)),
    }
    match std::fs::symlink_metadata(to_path) {
        Ok(_) if !force => return Ok(DestPrecondition::TargetExists),
        Ok(m) if !m.is_file() => return Ok(DestPrecondition::TargetExists),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io("stat", to_path, e)),
    }
    Ok(DestPrecondition::Proceed)
}

/// Layer-source resolution classifier for
/// [`sense_sync_write_to_layer`]. Returns
/// `Some(WriteOutcome::RejectedTargetUnbacked)` when the layer's
/// [`content_layer_root`] is `None`; otherwise `None`.
///
/// Extracted for AC-8 coverage of `RejectedTargetUnbacked`
/// (review-iter-4 P-14). The handler runs the same check inline.
fn classify_write_unbacked(layer_id: LayerId, ctx: &ResolverContext) -> Option<WriteOutcome> {
    if content_layer_root(layer_id, ctx).is_none() {
        return Some(WriteOutcome::RejectedTargetUnbacked);
    }
    None
}

fn classify_move_precondition(
    from_layer: LayerId,
    to_layer: LayerId,
) -> Result<Option<MoveOutcome>, Error> {
    if from_layer == to_layer {
        return Ok(Some(MoveOutcome::RejectedSameLayer));
    }
    Ok(None)
}

/// Move a single `logical_name` from one writable layer to another.
///
/// Thin Tauri-command wrapper over [`move_between_layers`] — the shared,
/// command-free move cascade that the MCP `move_layer` handler and future
/// subtree callers also use.
#[tauri::command]
pub async fn move_file_between_layers(
    app: tauri::AppHandle,
    params: MoveFileParams,
) -> Result<MoveOutcome, Error> {
    let MoveFileParams {
        from_layer,
        to_layer,
        logical_name,
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
        move_as_subtree,
        conflict_policy,
        force,
    } = params;
    let selector = if move_as_subtree {
        MoveSelector::Subtree(logical_name)
    } else {
        MoveSelector::File(logical_name)
    };
    let ctx_params = LayerContextParams {
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    };
    move_between_layers(
        &app,
        ctx_params,
        selector,
        conflict_policy,
        force,
        from_layer,
        to_layer,
    )
    .await
}

// ---------------------------------------------------------------------------
// SW-LT-4: promote-to-team routing
// ---------------------------------------------------------------------------

/// Result of a [`promote_content_entry`] call. Mirrors [`MoveOutcome`]'s
/// tagged-enum wire shape so the workbench reuses the same success /
/// soft-reject rendering. Soft rejections are EXPECTED user outcomes (wrong
/// target, source vanished, cross-filesystem), not programming errors — `Err`
/// stays reserved for real I/O / validation failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PromoteOutcome {
    /// The source file was relocated into the target team's dir and `count`
    /// per-repo lockfiles had their matching entry's `source_path` rewritten.
    Promoted { count: usize },
    /// `from_team == to_team` — nothing to promote.
    RejectedSameTeam,
    /// `to_team` is not in the active team's reachable inheritance chain.
    RejectedTargetNotInChain,
    /// The source file is not present at the owning team's dir.
    RejectedSourceMissing,
    /// A file already exists at the target team's dir and `force` was not set.
    RejectedTargetExists,
    /// A layer/team pair yielded no backing path (e.g. no team).
    RejectedUnbacked,
    /// Source and target dirs are on different filesystems (the TS caller
    /// falls back to copy+delete, mirroring [`MoveOutcome::RejectedCrossFilesystem`]).
    RejectedCrossFilesystem,
}

/// Tauri-deserialized parameters for [`promote_content_entry`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteParams {
    /// The layer KIND being promoted — UNCHANGED by the move (only the owning
    /// team changes). Team content is the one layer since SW-XS-8.
    pub layer: LayerId,
    pub logical_name: String,
    pub code_repo_slug: Option<SourceRepoSlug>,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    /// The active viewing team — whose per-repo lockfiles hold the materialized
    /// entry, and whose reachable chain bounds the valid targets.
    pub team_name: Option<String>,
    /// The team that currently OWNS the file — derived on the frontend from the
    /// entry's `source_path` `_team/{team}/…` segment.
    pub from_team: String,
    /// The team to promote INTO — must be in the active team's chain.
    pub to_team: String,
    /// Overwrite an existing target file at the destination team's dir (the
    /// workbench "Replace" choice). Additive; defaults to reject-on-collision.
    #[serde(default)]
    pub force: bool,
}

/// Require an admitted source before moving ancestor content into the child.
fn inherited_promote_source(ctx: &ResolverContext, source: &Path) -> Option<PromoteOutcome> {
    (!ctx.view().is_some_and(|v| v.read_path(source).is_ok()))
        .then_some(PromoteOutcome::RejectedSourceMissing)
}

fn classify_promote_target(
    reachable_chain: &[String],
    from_team: &str,
    to_team: &str,
) -> Option<PromoteOutcome> {
    if from_team == to_team {
        return Some(PromoteOutcome::RejectedSameTeam);
    }
    if !reachable_chain.iter().any(|t| t == to_team) {
        return Some(PromoteOutcome::RejectedTargetNotInChain);
    }
    None
}

/// A team's configured source repos (all slugs), loaded from on-disk org state
/// for the team named on `ctx` — works for ANY team, not just the active one.
/// Used to fan the promote lockfile rewrite across EVERY attached repo of the
/// team — a team-content file is slug-agnostic, so its entry can live in all
/// of them. Best-effort: a no-team context or an org that fails to load yields an
/// empty list (the caller falls back to the viewed slug).
pub(crate) fn configured_team_source_repos(ctx: &ResolverContext) -> Vec<SourceRepoSlug> {
    let Some(team) = ctx.team_name.as_deref() else {
        return Vec::new();
    };
    let org_root = match crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    ) {
        Ok(root) => root,
        Err(error) => {
            log::warn!("{error}");
            return Vec::new();
        }
    };
    crate::domain::load_org(ctx.org_slug.clone(), &org_root, None)
        .org
        .and_then(|org| org.team(team).map(|t| t.source_repos.clone()))
        .unwrap_or_default()
}

/// AC-2b: after the source file is relocated, rewrite the `source_path` of the
/// matching lockfile entry across EVERY attached repo of the active team.
///
/// The entry stays in the active (viewing) team's lockfile — promote never moves
/// entries BETWEEN lockfiles; it only re-points the one field that encodes the
/// owning team. Enumerating across ALL of the team's slugs is required for the
/// team content (slug-agnostic): one file's entry can live in every attached
/// repo's lockfile — a single-slug rewrite would leave the team's other repos
/// dangling. Exact `(logical_name, old_source_path)` matching keeps it precise.
/// This closes the reverse-sync window deterministically (the move suppressed the
/// watcher, so a reconcile can't be relied on). A targeted single-field update —
/// NOT a resolver re-run — so it does not duplicate the engine's lockfile-writing
/// logic. Returns the count of lockfiles rewritten. Missing lockfiles are skipped.
fn rewrite_promoted_lockfile_entries(
    ctx: &ResolverContext,
    slugs: &[SourceRepoSlug],
    logical_name: &str,
    old_source_path: &Path,
    new_source_path: &Path,
) -> Result<usize, EngineError> {
    let repos = enumerate_attached_code_repos_for_team(ctx, slugs)?;
    // SW-IS-3b: the store is the SOLE source of truth. `read_through` maps a genuinely
    // absent target to `Ok(None)` — the `continue` the old NotFound branch took. Store
    // errors are surfaced as `EngineError::Io` (this fn's error type has no store variant).
    let store = open_sync_state_store(&ctx.codesense_home).ok_or_else(|| {
        EngineError::Io(std::io::Error::other(
            "install store unavailable (rewrite_promoted_lockfile_entries)",
        ))
    })?;
    let mut rewritten = 0usize;
    for repo in repos {
        let mut lockfile = match read_through(&store, ctx, &repo.code_repo_path) {
            Ok(Some(lf)) => lf,
            Ok(None) => continue,
            Err(e) => return Err(EngineError::Io(std::io::Error::other(e.to_string()))),
        };
        let mut changed = false;
        for entry in &mut lockfile.entries {
            if entry.logical_name == logical_name && entry.source_path == old_source_path {
                entry.source_path = new_source_path.to_path_buf();
                changed = true;
            }
        }
        if changed {
            store
                .upsert(ctx.user_id, &repo.slug, &lockfile)
                .map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

/// SW-LT-4: promote a materialized install file from its current team to
/// another team in the active team's inheritance chain (self → parents →
/// org-root, or back).
///
/// Records the target team by RELOCATING the source file into that team's
/// `_team/{team}/…` dir — the owning team is encoded in `source_path`, so there
/// is no `source_team` field to write (the source path stays the single source
/// of truth). Then it deterministically rewrites the matching lockfile
/// entry's `source_path` across the active team's attached repos so reverse-sync
/// routes to the new owner immediately, and fires a best-effort forward re-fan.
///
/// Reuses the hardened [`move_one_locked`] relocation core (lock +
/// watcher-suppression + atomic rename + EXDEV/ghost-dir handling). The two
/// things it adds over [`move_file_between_layers`] are the lockfile rewrite and
/// the re-fan (the move command deliberately omits both). No new file mover, no
/// `LayerId` change (kind is preserved), no lockfile schema change.
#[tauri::command]
pub async fn promote_content_entry(
    app: tauri::AppHandle,
    params: PromoteParams,
) -> Result<PromoteOutcome, Error> {
    promote_entry(params, || resolve_suppression_handle(&app)).await
}

async fn promote_entry(
    params: PromoteParams,
    resolve_suppression: impl FnOnce() -> Result<SuppressionHandle, Error>,
) -> Result<PromoteOutcome, Error> {
    let PromoteParams {
        layer,
        logical_name,
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
        from_team,
        to_team,
        force,
    } = params;

    if !is_safe_logical_name(&logical_name) {
        return Err(Error::InvalidLogicalName(logical_name));
    }
    let ctx_params = LayerContextParams {
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    };
    ctx_params.validate_slug()?;
    for (field, seg) in [("fromTeam", &from_team), ("toTeam", &to_team)] {
        if let Some(reason) = validate_path_segment(seg) {
            return Err(Error::InvalidContextSegment {
                field,
                reason: reason.to_string(),
            });
        }
    }

    // Destructure the derived context back into the original locals so the
    // divergent promote body (per-team `ctx_with_team`, chain guard, N-repo
    // fan-out) stays byte-identical below (RF-11 AC-3).
    let LayerCommandContext {
        resolver: ctx,
        code_repo_slug,
    } = ctx_params.derive()?;

    let reachable = ctx.view().map(|v| v.team_names()).unwrap_or_default();
    if let Some(rejection) = classify_promote_target(&reachable, &from_team, &to_team) {
        return Ok(rejection);
    }

    // Compute source (owning-team) and dest (target-team) paths by substituting
    // the team into the SAME layer (team content; no slug needed).
    let ctx_from = ctx_with_team(&ctx, Some(from_team.clone()));
    let ctx_to = ctx_with_team(&ctx, Some(to_team.clone()));
    let Some(from_root) = content_layer_root(layer, &ctx_from) else {
        return Ok(PromoteOutcome::RejectedUnbacked);
    };
    let Some(to_root) = content_layer_root(layer, &ctx_to) else {
        return Ok(PromoteOutcome::RejectedUnbacked);
    };
    let from_path = from_root.join(&logical_name);
    let to_path = to_root.join(&logical_name);
    if let Some(rejection) = inherited_promote_source(&ctx, &from_path) {
        return Ok(rejection);
    }

    // Compute the full affected-repo slug set UP FRONT. A team-content file is
    // slug-agnostic — materialized into every attached repo, so its stale
    // `source_path` lives in every per-repo lockfile. Slug set = the active team's
    // configured repos ∪ the viewed slug (a belt-and-suspenders fallback when the
    // org can't be loaded). Canonical lock ordering + de-dup is owned by
    // `LockPlan::RepoSet::acquire` below, so this set need not be pre-sorted.
    let mut slugs = configured_team_source_repos(&ctx);
    if let Some(slug) = code_repo_slug.clone() {
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }

    // Relocate + rewrite under EVERY affected repo's lock, reusing the hardened
    // move core. Holding all the slug locks (in canonical order) makes the source
    // relocation and the multi-repo lockfile rewrite atomic w.r.t. concurrent
    // command-side moves/promotes on any of those repos — not just the viewed one.
    // `move_one_locked` runs filesystem preconditions + suppression + atomic
    // rename with the same `layer` on both sides (only the team changes); the
    // same-layer rejection lives in `classify_move_precondition`, which we skip.
    let suppression = resolve_suppression()?;

    let _locks = LockPlan::RepoSet(&slugs).acquire().await;
    let move_outcome = move_one_locked(
        &from_path,
        &to_path,
        &to_root,
        &suppression,
        layer,
        layer,
        &logical_name,
        force,
    )
    .await?;
    match move_outcome {
        MoveOutcome::Moved { .. } => {}
        MoveOutcome::RejectedSourceMissing => return Ok(PromoteOutcome::RejectedSourceMissing),
        MoveOutcome::RejectedTargetExists => return Ok(PromoteOutcome::RejectedTargetExists),
        MoveOutcome::RejectedCrossFilesystem => return Ok(PromoteOutcome::RejectedCrossFilesystem),
        // `move_one_locked` only ever returns the four variants above (single-file
        // move). Any other variant is an internal contract violation, NOT a user
        // outcome — surface it as a real error rather than mislabelling it as a
        // benign "source missing" soft-reject.
        other => {
            log::error!(
                target: "content_engine::commands",
                "promote_content_entry: unexpected move outcome {:?}",
                other
            );
            return Err(Error::Io {
                op: "engine",
                path: from_path.clone(),
                message: format!("promote: unexpected move outcome {other:?}"),
            });
        }
    }

    // AC-2b: deterministic lockfile rewrite across every affected repo. The entry
    // stays in the active team's lockfile; only its `source_path` moves to the
    // target team's dir. Runs on the blocking pool (sync lockfile I/O), still
    // under the held repo locks so a concurrent command-side move can't read a
    // half-rewritten set.
    let count = {
        let ctx_active = ctx.clone();
        let logical_for_task = logical_name.clone();
        let old_path = from_path.clone();
        let new_path = to_path.clone();
        tokio::task::spawn_blocking(move || {
            rewrite_promoted_lockfile_entries(
                &ctx_active,
                &slugs,
                &logical_for_task,
                &old_path,
                &new_path,
            )
        })
        .await
        .map_err(|e| Error::Io {
            op: "engine",
            path: PathBuf::new(),
            message: format!("promote lockfile rewrite task failed: {e}"),
        })?
        .map_err(engine_error_to_command_error)?
    };
    drop(_locks);

    // SW-XS-3: a promote moves the file in team content and repoints the lockfile
    // rows; it no longer fans the change out to open workspaces.

    log::info!(
        target: "content_engine::commands",
        "promoted {} from team {} → {} ({} lockfile entries rewritten)",
        logical_name,
        from_team,
        to_team,
        count
    );
    Ok(PromoteOutcome::Promoted { count })
}

/// Command-free move cascade: runs the full pre-lock classify chain,
/// acquires the per-repo lock at most once, then delegates the locked
/// filesystem work to [`move_one_locked`].
///
/// Soft rejections (read-only target/source, same-layer, source missing,
/// target exists, unbacked, cross-filesystem) surface as [`MoveOutcome`]
/// variants — expected workbench drag-and-drop outcomes, not programming
/// errors. Hard failures (invalid name, missing slug for repo-scoped,
/// generic I/O, watcher state missing) flow through [`Error`].
///
/// **Lock/core split.** This outer owns lock acquisition; the inner
/// [`move_one_locked`] is lock-free. The split lets a subtree move hold a
/// single guard across many leaf moves — `tokio::sync::Mutex` is not
/// re-entrant, so re-acquiring per leaf would deadlock.
async fn move_between_layers(
    app: &tauri::AppHandle,
    ctx_params: LayerContextParams,
    selector: MoveSelector,
    policy: ConflictPolicy,
    force: bool,
    from_layer: LayerId,
    to_layer: LayerId,
) -> Result<MoveOutcome, Error> {
    let name = selector.name();
    if !is_safe_logical_name(name) {
        log::error!(
            target: "content_engine::commands",
            "move_between_layers rejected invalid logical_name {:?}",
            name
        );
        return Err(Error::InvalidLogicalName(name.to_string()));
    }
    ctx_params.validate_slug()?;
    if let Some(outcome) = classify_move_precondition(from_layer, to_layer)? {
        return Ok(outcome);
    }

    // Derive the resolver context (retaining the slug for lock keying) at the
    // SAME point in the order the open-coded staircase did — after the
    // pre-lock identity classifier, before the unbacked classifier.
    let lcx = ctx_params.derive()?;
    let ctx = &lcx.resolver;

    // Resolve layer source paths BEFORE the lock — these are pure path
    // math against the in-memory context, no I/O. Routed through
    // `classify_move_unbacked` so the two unbacked variants stay
    // unit-testable without an `AppHandle`.
    if let Some(outcome) = classify_move_unbacked(from_layer, to_layer, ctx) {
        return Ok(outcome);
    }
    // Safe to `unwrap()` — classify_move_unbacked just returned None,
    // meaning both content_layer_root calls return Some.
    let from_root = content_layer_root(from_layer, ctx)
        .expect("classify_move_unbacked returned None ⇒ from_layer is backed");
    let to_root = content_layer_root(to_layer, ctx)
        .expect("classify_move_unbacked returned None ⇒ to_layer is backed");

    // Resolve the suppression handle up front so the panic surface from
    // `Manager::state` becomes a typed Error::Io instead of a runtime
    // crash if the watcher hasn't been initialized.
    let suppression = resolve_suppression_handle(app)?;

    match selector {
        MoveSelector::File(logical_name) => {
            // Per-repo serialization. Workspace↔workspace single-file
            // moves keep the existing conditional lock (bounded race for
            // a single rename); the lock is held across `move_one_locked`.
            let _lock = LockPlan::Repo(lcx.slug()).acquire().await;
            let from_path = from_root.join(&logical_name);
            let to_path = to_root.join(&logical_name);
            let outcome = move_one_locked(
                &from_path,
                &to_path,
                &to_root,
                &suppression,
                from_layer,
                to_layer,
                &logical_name,
                force,
            )
            .await?;
            if matches!(outcome, MoveOutcome::Moved { .. }) {
                log::info!(
                    target: "content_engine::commands",
                    "moved {} from {:?} → {:?}",
                    logical_name,
                    from_layer,
                    to_layer
                );
            }
            Ok(outcome)
        }
        MoveSelector::Subtree(prefix) => {
            // A subtree move is multi-step (enumerate → N renames →
            // rollback on failure), so it holds a lock UNCONDITIONALLY for
            // the whole batch (and any rollback). Repo-scoped moves take
            // the single per-repo lock (its slug covers both layers);
            // workspace moves have no slug, so they lock BOTH layer roots
            // to protect the source as well as the destination.
            let _lock = match lcx.slug() {
                Some(slug) => LockPlan::Repo(Some(slug)).acquire().await,
                None => {
                    LockPlan::WorkspacePair(&from_root, &to_root)
                        .acquire()
                        .await
                }
            };
            move_subtree_locked(
                &from_root,
                &to_root,
                &prefix,
                &suppression,
                from_layer,
                to_layer,
                policy,
            )
            .await
        }
    }
}

/// Lock-free core of a single same-FS move: existence preconditions,
/// suppression install, parent mkdir, rename, and ghost-dir cleanup on
/// failure.
///
/// **The caller MUST already hold the per-repo lock.** This function
/// never acquires it, so a subtree move can call it once per leaf under a
/// single held guard without the re-entrant deadlock a
/// `tokio::sync::Mutex` would otherwise produce. Running the existence
/// checks here — under the caller's lock — keeps two concurrent moves on
/// the same repo from both passing the check and having the second
/// `rename(2)` silently overwrite the first.
///
/// `to_root` bounds post-failure ghost-dir cleanup to strict descendants
/// of the destination layer root.
// The arg list mirrors the locked move's irreducible inputs (paths, layers,
// suppression, logical name, force); bundling them into a struct just to
// satisfy the lint would obscure the call sites without simplifying the API.
// (RF-11 folded the CONTEXT params of `move_between_layers` into
// `LayerContextParams`, dropping its `#[allow]`; these execute-phase inputs
// are deliberately left as-is — they are not context.)
#[allow(clippy::too_many_arguments)]
async fn move_one_locked(
    from_path: &Path,
    to_path: &Path,
    to_root: &Path,
    suppression: &SuppressionHandle,
    from_layer: LayerId,
    to_layer: LayerId,
    logical_name: &str,
    force: bool,
) -> Result<MoveOutcome, Error> {
    // `symlink_metadata` so a symlink at the source does not silently
    // move just the link (the resolver rejects symlinks outright).
    // `force` bypasses only the target-exists rejection (the rename then
    // overwrites the destination file) — never the source-missing check.
    if let Some(outcome) = classify_move_fs_preconditions(from_path, to_path, force)? {
        return Ok(outcome);
    }

    // Compute the missing ancestor chain BEFORE mkdir. Used for both
    // suppression-set registration (so watcher events on intermediate
    // dirs are suppressed) and cleanup on rename failure (so ghost empty
    // folders aren't left behind). On first-time mkdir the literal parent
    // may not exist yet and create_dir_all may create N ancestors whose
    // FSEvents have canonicalized keys (e.g. /var → /private/var on macOS
    // firmlinks) that a lexically-registered suppression would miss.
    let to_parent: Option<&Path> = to_path.parent().filter(|p| !p.as_os_str().is_empty());
    let newly_created_parents: Vec<PathBuf> = match to_parent {
        Some(parent) => compute_missing_ancestors(parent),
        None => Vec::new(),
    };

    // Suppression installed BEFORE mkdir so the watcher does not react to
    // new intermediate directory creations or to our own rename. Cover
    // BOTH source and destination parents — `rename(2)` produces FSEvents
    // on both (delete-in-source-parent, create-in-dest-parent on macOS).
    // `canonical_ancestor_form` walks up to the nearest existing ancestor
    // before canonicalizing and lexically rejoins the missing tail, so
    // first-time-mkdir destinations and macOS firmlinks both register the
    // key the watcher's `stable_key` will later look up.
    use std::collections::HashSet;
    let mut suppression_set: HashSet<PathBuf> = HashSet::new();
    suppression_set.insert(canonical_ancestor_form(from_path));
    suppression_set.insert(canonical_ancestor_form(to_path));
    if let Some(parent) = from_path.parent() {
        if !parent.as_os_str().is_empty() {
            suppression_set.insert(canonical_ancestor_form(parent));
        }
    }
    if let Some(parent) = to_parent {
        suppression_set.insert(canonical_ancestor_form(parent));
    }
    for ancestor in &newly_created_parents {
        suppression_set.insert(canonical_ancestor_form(ancestor));
    }
    let suppression_paths: Vec<PathBuf> = suppression_set.into_iter().collect();
    let _suppression = Suppression::new(suppression, suppression_paths);

    if let Some(parent) = to_parent {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::io("mkdir", parent, e))?;
    }

    match tokio::fs::rename(from_path, to_path).await {
        Ok(()) => {
            // Per-leaf trace only — the user-facing summary is logged once
            // by the caller (single-file arm or `move_subtree_locked`), so a
            // 52-file subtree promotion doesn't emit 52 info lines.
            log::debug!(
                target: "content_engine::commands",
                "moved {} from {:?} → {:?}",
                logical_name,
                from_layer,
                to_layer
            );
            Ok(MoveOutcome::Moved { count: 1 })
        }
        Err(e) => {
            // EXDEV / CrossesDevices — TS caller falls back to copy+delete.
            // Use the raw OS error code (18 on Linux + macOS); reference
            // `libc::EXDEV` only on Unix targets to keep Windows builds
            // green.
            #[cfg(unix)]
            let is_xdev = e.raw_os_error() == Some(libc::EXDEV);
            #[cfg(not(unix))]
            let is_xdev = false;
            // Clean up the pre-created parent chain on EVERY rename
            // failure, not just EXDEV, so a permission-denied / ENOSPC /
            // EROFS failure doesn't leave ghost empty folders visible in
            // the workbench. `remove_dir` is non-recursive and only
            // succeeds for empty dirs, so a concurrent sibling caller's
            // content is preserved; cleanup is bounded to strict
            // descendants of `to_root`.
            remove_newly_created_parents(&newly_created_parents, to_root).await;
            if is_xdev {
                log::info!(
                    target: "content_engine::commands",
                    "move {} cross-filesystem ({:?} → {:?}); caller must fall back",
                    logical_name,
                    from_layer,
                    to_layer
                );
                return Ok(MoveOutcome::RejectedCrossFilesystem);
            }
            log::error!(
                target: "content_engine::commands",
                "move {} from {:?} → {:?} failed: {}",
                logical_name,
                from_layer,
                to_layer,
                e
            );
            Err(Error::io("rename", to_path, e))
        }
    }
}

/// True when both paths are regular files with identical contents. Lets a
/// subtree move treat a byte-identical collision as a no-op promote
/// (overwrite the matching target, count as moved) rather than a conflict —
/// mirroring ingest, where identical content never raises a conflict row.
/// Cheap length check first; the full byte read runs only when lengths
/// match, so a genuine edit (almost always a different length, or caught on
/// the first differing byte) costs ~nothing. Called only over the colliding
/// minority, under the held lock.
fn files_byte_identical(a: &Path, b: &Path) -> Result<bool, Error> {
    let am = std::fs::metadata(a).map_err(|e| Error::io("stat", a, e))?;
    let bm = std::fs::metadata(b).map_err(|e| Error::io("stat", b, e))?;
    if am.len() != bm.len() {
        return Ok(false);
    }
    let ab = std::fs::read(a).map_err(|e| Error::io("read", a, e))?;
    let bb = std::fs::read(b).map_err(|e| Error::io("read", b, e))?;
    Ok(ab == bb)
}

/// Drop a redundant source file whose byte-identical target already exists,
/// under the held lock. The target is left untouched — this only removes the
/// now-duplicate source so a byte-identical subtree leaf ends up wholly on
/// the target side (SS-FU-18 AC-1). Suppresses the watcher delete the same
/// way the move path suppresses its renames. Best-effort: a failed delete is
/// logged, not propagated — the target is already correct, so a stray source
/// must not fail an otherwise-successful subtree move (the next wave
/// reconciles it), matching the empty-dir cleanup's recovery class.
async fn remove_redundant_source(src: &Path, suppression: &SuppressionHandle) {
    use std::collections::HashSet;
    let mut suppression_set: HashSet<PathBuf> = HashSet::new();
    suppression_set.insert(canonical_ancestor_form(src));
    if let Some(parent) = src.parent() {
        if !parent.as_os_str().is_empty() {
            suppression_set.insert(canonical_ancestor_form(parent));
        }
    }
    let _suppression = Suppression::new(suppression, suppression_set);
    if let Err(e) = tokio::fs::remove_file(src).await {
        log::error!(
            target: "content_engine::commands",
            "subtree move could not remove redundant identical source {}: {}",
            src.display(),
            e
        );
    }
}

/// Move a directory subtree (`prefix`) between two layer roots, leaf by
/// leaf, under a lock the caller already holds. All-or-nothing in
/// process: any mid-batch failure rolls the already-moved leaves home and
/// removes the empty destination dirs the forward moves created, leaving
/// on-disk state as if the move never started. The rollback itself is
/// best-effort — a rollback rename that fails (rare; only this writer
/// holds the lock) is logged with the orphaned path rather than retried,
/// matching the single-file path's recovery class.
///
/// A subtree containing a symlink is rejected whole (the move operates on
/// regular files only; partially moving the tree would split it).
///
/// Always leaf-by-leaf — never a whole-directory rename. A directory
/// rename is crash-atomic but makes the watcher's per-event suppression
/// race (the tree mutates mid-syscall); individual file renames reuse the
/// accepted single-file move + suppression. Per-leaf collisions are
/// resolved by `policy` before any move: `RejectWhole` rejects the whole
/// move if any leaf collides (`RejectedTargetExists`, nothing changes);
/// `SkipExisting` moves the non-colliding leaves and reports the rest.
#[allow(clippy::too_many_arguments)]
async fn move_subtree_locked(
    from_root: &Path,
    to_root: &Path,
    prefix: &str,
    suppression: &SuppressionHandle,
    from_layer: LayerId,
    to_layer: LayerId,
    policy: ConflictPolicy,
) -> Result<MoveOutcome, Error> {
    struct Leaf {
        src: PathBuf,
        dst: PathBuf,
        logical_name: String,
    }

    let from_subtree = from_root.join(prefix);
    // The source prefix must resolve to an existing directory. A missing
    // or non-directory prefix is `RejectedSourceMissing`, mirroring the
    // file path's treatment of an absent source.
    match std::fs::symlink_metadata(&from_subtree) {
        Ok(m) if m.is_dir() => {}
        _ => return Ok(MoveOutcome::RejectedSourceMissing),
    }

    // Walk the source subtree once, off the async runtime (walkdir is
    // sync), capturing both the leaf files to move and the directories to
    // clean up afterward — the dir structure is unchanged by the moves, so
    // one walk serves both.
    let SubtreeListing {
        files,
        dirs_deepest_first,
        has_symlink,
    } = {
        let root = from_subtree.clone();
        tokio::task::spawn_blocking(move || enumerate_subtree(&root))
            .await
            .map_err(|e| Error::Io {
                op: "enumerate",
                path: from_subtree.clone(),
                message: format!("subtree enumeration task failed: {e}"),
            })??
    };
    // Refuse a subtree that contains a symlink rather than silently moving
    // only the regular files and stranding the link — that would split the
    // subtree across two layers. Mirrors the single-file path, which
    // rejects a symlink source as `RejectedSourceMissing`.
    if has_symlink {
        log::warn!(
            target: "content_engine::commands",
            "subtree move rejected: {} contains a symlink (cannot move as a unit)",
            from_subtree.display()
        );
        return Ok(MoveOutcome::RejectedSourceMissing);
    }
    if files.is_empty() {
        return Ok(MoveOutcome::RejectedSourceMissing);
    }

    let mut planned: Vec<Leaf> = Vec::with_capacity(files.len());
    for src in files {
        let rel = src.strip_prefix(from_root).map_err(|_| Error::Io {
            op: "strip_prefix",
            path: src.clone(),
            message: "enumerated leaf escaped from_root".into(),
        })?;
        let dst = to_root.join(rel);
        let logical_name = rel.to_string_lossy().into_owned();
        planned.push(Leaf {
            src,
            dst,
            logical_name,
        });
    }

    // Collision partition under the held lock (no TOCTOU). Three buckets:
    //   - movable:   dest absent → plain move (force=false).
    //   - identical: dest exists AND bytes match → NOT a conflict. The
    //                target is already correct, so the source is redundant;
    //                it's dropped post-success (below) and counted as moved,
    //                mirroring ingest, where identical content is a no-op,
    //                never a conflict row (SS-FU-18 AC-1).
    //   - conflicts: dest exists as a regular file whose bytes differ or
    //                cannot be read, OR dest exists as a non-regular file
    //                (dir/symlink/device). None of these are byte-identical,
    //                so none auto-resolve; each surfaces as a real collision
    //                for the caller to resolve per-file. A non-regular or
    //                unreadable target is never byte-compared, so it cannot
    //                error out the whole batch.
    // Run off the async runtime: the byte-identical check reads colliding
    // files, and the whole partition is sync fs work — the same reason the
    // enumeration walk above uses `spawn_blocking`. Doing it inline would
    // block a runtime worker on those file reads.
    let (movable, identical, conflicts) = tokio::task::spawn_blocking(move || {
        let mut movable: Vec<Leaf> = Vec::new();
        let mut identical: Vec<Leaf> = Vec::new();
        let mut conflicts: Vec<Leaf> = Vec::new();
        for leaf in planned {
            match std::fs::symlink_metadata(&leaf.dst) {
                // Target absent → plain move.
                Err(_) => movable.push(leaf),
                // Existing regular file → compare bytes. Identical is a
                // no-op promote; differing is a conflict. A read error means
                // we cannot confirm identity — treat as a conflict rather
                // than aborting the whole batch (a single unreadable leaf
                // must not sink a SkipExisting move), and log it.
                Ok(m) if m.is_file() => match files_byte_identical(&leaf.src, &leaf.dst) {
                    Ok(true) => identical.push(leaf),
                    Ok(false) => conflicts.push(leaf),
                    Err(e) => {
                        log::warn!(
                            target: "content_engine::commands",
                            "subtree move: cannot compare colliding leaf {} ({}); treating as conflict",
                            leaf.logical_name,
                            e
                        );
                        conflicts.push(leaf);
                    }
                },
                // Existing non-regular target (dir/symlink/device) → never
                // byte-compared or auto-resolved; surfaces as a conflict.
                Ok(_) => conflicts.push(leaf),
            }
        }
        Ok::<_, Error>((movable, identical, conflicts))
    })
    .await
    .map_err(|e| Error::Io {
        op: "partition",
        path: from_subtree.clone(),
        message: format!("subtree collision partition task failed: {e}"),
    })??;

    if !conflicts.is_empty() && policy == ConflictPolicy::RejectWhole {
        return Ok(MoveOutcome::RejectedTargetExists);
    }

    // Move the genuinely-movable leaves (absent target) leaf by leaf,
    // tracking them for rollback. `identical` leaves are NOT moved here —
    // their target is already byte-correct, so they're resolved by dropping
    // the redundant source on the success path below; keeping them out of
    // this loop means a mid-batch failure's rollback never has to restore an
    // overwritten target. Real `conflicts` are likewise never moved: under
    // SkipExisting they surface by name; a genuine "Replace" is the caller's
    // per-file force move (SS-FU-18).
    let mut moved: Vec<&Leaf> = Vec::with_capacity(movable.len());
    let mut failure: Option<Result<MoveOutcome, Error>> = None;
    for leaf in &movable {
        match move_one_locked(
            &leaf.src,
            &leaf.dst,
            to_root,
            suppression,
            from_layer,
            to_layer,
            &leaf.logical_name,
            // Movable leaves have an absent target; never an overwrite.
            false,
        )
        .await
        {
            Ok(MoveOutcome::Moved { .. }) => moved.push(leaf),
            // A per-leaf rejection (e.g. cross-filesystem) aborts the
            // whole move — the leaves share one from_root/to_root, so a
            // rejection on one applies to all.
            Ok(rejected) => {
                failure = Some(Ok(rejected));
                break;
            }
            Err(e) => {
                failure = Some(Err(e));
                break;
            }
        }
    }

    if let Some(result) = failure {
        // Roll back already-moved leaves, newest-first, by renaming each
        // dest back to its source. Best-effort: any non-success rollback
        // is logged with the orphaned path (the held lock means no
        // concurrent mutation, so this is the only writer).
        for leaf in moved.iter().rev() {
            match move_one_locked(
                &leaf.dst,
                &leaf.src,
                from_root,
                suppression,
                to_layer,
                from_layer,
                &leaf.logical_name,
                // Rollback restores into the just-vacated source — no
                // collision possible, so `force: false`.
                false,
            )
            .await
            {
                Ok(MoveOutcome::Moved { .. }) => {}
                Ok(other) => log::error!(
                    target: "content_engine::commands",
                    "subtree move rollback could not restore {}: {:?}",
                    leaf.src.display(),
                    other
                ),
                Err(e) => log::error!(
                    target: "content_engine::commands",
                    "subtree move rollback failed to restore {}: {}",
                    leaf.src.display(),
                    e
                ),
            }
        }
        // The forward moves created destination directory trees via
        // `create_dir_all`; rolling the files back leaves those dirs empty.
        // Remove them (deepest-first, bounded to `to_root`) so a
        // rolled-back move leaves no ghost dirs in the destination layer —
        // "as if the move never started". Dest dirs mirror the source dir
        // structure captured before the move.
        let dest_dirs: Vec<PathBuf> = dirs_deepest_first
            .iter()
            .filter_map(|d| d.strip_prefix(from_root).ok().map(|rel| to_root.join(rel)))
            .collect();
        remove_empty_dirs(&dest_dirs, to_root).await;
        return result;
    }

    // All risky moves landed. The `identical` leaves are byte-for-byte
    // already present at the target, so their source copy is redundant —
    // drop it now that the moves are known-good (doing this only
    // post-success keeps a mid-batch failure's rollback trivial: identical
    // leaves were never touched, so neither source nor target moved).
    for leaf in &identical {
        remove_redundant_source(&leaf.src, suppression).await;
    }

    // Remove the now-empty source subtree directories (captured during the
    // single enumeration walk above), then the now-empty intermediate
    // ancestors of the prefix (e.g. `.claude/skills` once its last skill
    // moves out), up to but not including `from_root`.
    remove_empty_dirs(&dirs_deepest_first, from_root).await;
    remove_empty_ancestors(&from_subtree, from_root).await;

    // `moved_count` counts the renamed leaves plus the byte-identical ones
    // resolved by dropping their redundant source — both end up wholly on
    // the target side. Only genuinely-differing `conflicts` are skipped.
    let moved_count = moved.len() + identical.len();
    log::info!(
        target: "content_engine::commands",
        "moved subtree {:?} ({} file(s), {} skipped) from {:?} → {:?}",
        prefix,
        moved_count,
        conflicts.len(),
        from_layer,
        to_layer
    );
    if policy == ConflictPolicy::SkipExisting && !conflicts.is_empty() {
        Ok(MoveOutcome::MovedPartial {
            moved: moved_count,
            skipped: conflicts.iter().map(|l| l.logical_name.clone()).collect(),
        })
    } else {
        Ok(MoveOutcome::Moved { count: moved_count })
    }
}

/// The regular files and directories under a subtree root, from a single
/// `walkdir` pass.
struct SubtreeListing {
    /// Regular files, sorted, so the move order (and thus which leaves a
    /// rollback restores) is deterministic.
    files: Vec<PathBuf>,
    /// Directories deepest-first, so empty-dir cleanup removes children
    /// before their parents.
    dirs_deepest_first: Vec<PathBuf>,
    /// True if any entry under the subtree is a symlink. walkdir does not
    /// follow links and a symlink is neither `is_file` nor `is_dir`, so it
    /// would otherwise be silently dropped from both lists — the caller
    /// rejects the whole move instead of moving the tree partially.
    has_symlink: bool,
}

/// Walk `root` once, collecting leaf files and directories. One walk
/// serves both the move (files) and the post-move cleanup (dirs) — the
/// directory structure is unchanged by renaming files out of it.
fn enumerate_subtree(root: &Path) -> Result<SubtreeListing, Error> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut has_symlink = false;
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|e| Error::Io {
            op: "walkdir",
            path: root.to_path_buf(),
            message: format!("subtree walk failed: {e}"),
        })?;
        let file_type = entry.file_type();
        if file_type.is_file() {
            files.push(entry.into_path());
        } else if file_type.is_dir() {
            dirs.push(entry.into_path());
        } else if file_type.is_symlink() {
            has_symlink = true;
        }
    }
    files.sort();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    Ok(SubtreeListing {
        files,
        dirs_deepest_first: dirs,
        has_symlink,
    })
}

/// Outcome of one best-effort directory removal — lets each caller log (or
/// not) and decide whether to keep going.
enum DirRemoval {
    /// The directory was empty and removed.
    Removed,
    /// `dir` is not a strict descendant of `boundary`; never touched.
    OutOfBounds,
    /// `remove_dir` failed — typically the dir is non-empty (a skipped
    /// leaf, a surviving sibling) or permission-denied.
    Failed(std::io::Error),
}

/// Remove `dir` iff it is empty AND a strict descendant of `boundary`.
/// `remove_dir` is non-recursive, so a dir still holding content is
/// preserved; the boundary gate means a bug can never remove the layer
/// root itself. The shared core of the three cleanup passes below.
async fn remove_dir_if_empty(dir: &Path, boundary: &Path) -> DirRemoval {
    if !dir.starts_with(boundary) || dir == boundary {
        return DirRemoval::OutOfBounds;
    }
    match tokio::fs::remove_dir(dir).await {
        Ok(()) => DirRemoval::Removed,
        Err(e) => DirRemoval::Failed(e),
    }
}

/// Remove `dirs` (expected deepest-first) when empty. Best effort and
/// silent — a surviving non-empty dir (e.g. a skipped leaf) is expected.
async fn remove_empty_dirs(dirs: &[PathBuf], boundary: &Path) {
    for dir in dirs {
        let _ = remove_dir_if_empty(dir, boundary).await;
    }
}

/// Remove the now-empty ancestor directories of `start`, walking up until
/// a non-empty dir, an error, or `boundary` (exclusive). Stops at the
/// first removal that doesn't succeed — a non-empty ancestor (e.g. a
/// sibling skill still present) ends the walk and is preserved.
async fn remove_empty_ancestors(start: &Path, boundary: &Path) {
    let mut cursor = start.parent();
    while let Some(dir) = cursor {
        if !matches!(
            remove_dir_if_empty(dir, boundary).await,
            DirRemoval::Removed
        ) {
            break;
        }
        cursor = dir.parent();
    }
}

/// Walk up `path` collecting non-existent ancestors leaf-first. Returns
/// an empty Vec when `path` already exists. The output is the chain
/// `create_dir_all(path)` would create — used for both suppression-set
/// registration (pre-mkdir) and cleanup on rename failure (post-mkdir).
/// Review-iter-5 (F6 + F7 + EH-7 + EH-23).
fn compute_missing_ancestors(path: &Path) -> Vec<PathBuf> {
    debug_assert!(
        path.is_absolute() || path.as_os_str().is_empty(),
        "compute_missing_ancestors expects an absolute path, got {:?}",
        path
    );
    let mut p = path.to_path_buf();
    let mut missing: Vec<PathBuf> = Vec::new();
    while !p.as_os_str().is_empty() && !p.exists() {
        missing.push(p.clone());
        match p.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => p = parent.to_path_buf(),
            _ => break,
        }
    }
    missing
}

/// `remove_dir` each entry leaf-first. Non-empty / permission-denied are
/// logged at warn level and otherwise ignored — the cleanup is
/// best-effort and must never mask the underlying error that triggered
/// it. Review-iter-5 (F6).
///
/// Review-iter-7 (Edge-H2): `boundary` is the layer's source root. Only
/// remove dirs that have `boundary` as a strict ancestor. Without this
/// gate, a fresh-install rename failure where `compute_missing_ancestors`
/// walked from `to_path` up to `/tmp` could wipe the entire layer root
/// (or `$HOME/.codesense/...`) leaf-first.
async fn remove_newly_created_parents(parents: &[PathBuf], boundary: &Path) {
    for p in parents {
        match remove_dir_if_empty(p, boundary).await {
            DirRemoval::Removed => {}
            DirRemoval::OutOfBounds => log::warn!(
                target: "content_engine::commands",
                "post-failure cleanup: refusing to remove {} — not a strict descendant of layer root {}",
                p.display(),
                boundary.display()
            ),
            DirRemoval::Failed(rm_err) => log::warn!(
                target: "content_engine::commands",
                "post-failure cleanup: failed to remove pre-created {}: {}",
                p.display(),
                rm_err
            ),
        }
    }
}

/// Mirror [`crate::content_engine::dispatcher::stable_key`]: canonicalize
/// the nearest existing ANCESTOR (never the leaf itself), then lexically
/// rejoin the missing tail. Crucially, this never canonicalizes through
/// a leaf symlink — `stable_key` deliberately pops the leaf before
/// canonicalizing so a symlink at the leaf position maps to its own
/// directory's canonical form rather than the symlink target. Without
/// this symmetry, suppression entries built from `canonical_ancestor_form`
/// would store the TARGET path for a symlinked leaf while the dispatcher
/// looks up the LINK path, leaking a watcher event past suppression.
///
/// Algorithm: pop leaf first, canonicalize parent (handles macOS firmlinks
/// like `/var` → `/private/var` regardless of whether the leaf exists),
/// then rejoin leaf + any deeper tail components.
///
/// Review-iter-5 (F4 + EH-7 + EH-23) introduced this helper.
/// Review-iter-7 (Blind-C1 + Edge-H1) corrected the leaf-canonicalization
/// bug — the iter-5 version called `canonicalize` on the full leafed
/// path which followed leaf symlinks, diverging from `stable_key`'s
/// parent-canonicalize semantics (regression test pinned at
/// `dispatcher.rs:614`).
fn canonical_ancestor_form(path: &Path) -> PathBuf {
    debug_assert!(
        path.is_absolute() || path.as_os_str().is_empty(),
        "canonical_ancestor_form expects an absolute path, got {:?}",
        path
    );
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        // Pop the leaf BEFORE existence-checking so we never canonicalize
        // through it. Preserve the leaf for lexical rejoin after parent
        // canonicalization.
        let leaf = current.file_name().map(|n| n.to_os_string());
        if !current.pop() {
            // No parent — `current` is `/` or empty. Return the original
            // lexical path; canonicalizing the root yields nothing useful.
            return path.to_path_buf();
        }
        if current.exists() {
            let canonical = std::fs::canonicalize(&current).unwrap_or(current);
            let mut out = canonical;
            if let Some(l) = leaf {
                out.push(l);
            }
            for component in tail.iter().rev() {
                out.push(component);
            }
            return out;
        }
        if let Some(l) = leaf {
            tail.push(l);
        }
    }
}

/// Tauri-deserialized parameters for [`sense_sync_write_to_layer`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToLayerParams {
    pub layer_id: LayerId,
    pub logical_name: String,
    pub content: Vec<u8>,
    pub code_repo_slug: Option<SourceRepoSlug>,
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
    /// SS-6 Task 1.3 — atomic 3-way conflict detection. When `Some(h)`
    /// and `force` is false, the write reads the current layer file
    /// (inside the per-repo lock) and compares its blake3 hash to `h`;
    /// on mismatch the write is skipped and
    /// [`WriteOutcome::RejectedConflict`] is returned with the current
    /// hash. `None` (default for SS-10 / migration callers) skips the
    /// check entirely — preserves the original SS-13 wire contract for
    /// every existing consumer.
    #[serde(default)]
    pub expected_layer_hash: Option<[u8; 32]>,
    /// SS-6 Task 1.3 — when `true`, skips the
    /// [`expected_layer_hash`] check even when a hash IS supplied.
    /// Set after the user picks `[P] push anyway` in the D/P/A conflict
    /// prompt. When `expected_layer_hash` is `None` this field is
    /// trivially unused (the check is gated on `Some(_)` first).
    /// Review iter-1 (M-5): doc clarified — the earlier wording said
    /// "no effect when None," which was true but invited future
    /// "simplification" that would break the legitimate force-override
    /// pathway.
    #[serde(default)]
    pub force: bool,
}

/// SS-6 Task 1.3 — outcome of the atomic conflict check that runs
/// inside [`sense_sync_write_to_layer`]'s per-repo lock window.
/// Extracted so the hash + compare + special-case-NotFound logic can
/// be unit-tested without a Tauri `AppHandle` (the rest of the
/// command's body requires one for Suppression).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConflictDecision {
    /// Hashes matched, OR layer file absent — fall through to the write.
    Proceed,
    /// Hashes differed under `force=false` — return this outcome verbatim.
    Reject(WriteOutcome),
    /// `spawn_blocking` panicked/cancelled — caller maps to `Error::Io`.
    JoinFailed(String),
    /// `hash_file` returned a non-NotFound I/O error — caller maps to `Error::Io`.
    HashFailed(String),
}

/// Decision logic for the conflict check. Takes the `Result` shape
/// `spawn_blocking(hash_file)` returns and the caller's expected hash;
/// folds the four-way outer/inner Result into one of four decisions.
///
/// Keeping the I/O outside this fn means tests can synthesize the
/// hash_result variants directly without staging tempdirs.
pub(crate) fn classify_conflict_check(
    hash_result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError>,
    expected: [u8; 32],
) -> ConflictDecision {
    match hash_result {
        Err(join_err) => {
            ConflictDecision::JoinFailed(format!("blocking task join error: {}", join_err))
        }
        Ok(Ok(actual)) => {
            if actual == expected {
                ConflictDecision::Proceed
            } else {
                ConflictDecision::Reject(WriteOutcome::RejectedConflict {
                    layer_current_hash: actual,
                })
            }
        }
        Ok(Err(content_engine::Error::Io(io))) if io.kind() == std::io::ErrorKind::NotFound => {
            // Layer file absent → no prior state to conflict against;
            // the write below creates it from scratch (first-time
            // Adopt semantics reaching this code path).
            ConflictDecision::Proceed
        }
        Ok(Err(other)) => ConflictDecision::HashFailed(other.to_string()),
    }
}

/// AC-3 row 7: write `content` into a writable layer's filesystem
/// location, wrapped in [`Suppression`].
#[tauri::command]
pub async fn sense_sync_write_to_layer(
    app: tauri::AppHandle,
    params: WriteToLayerParams,
) -> Result<WriteOutcome, Error> {
    write_to_layer_with_suppression(params, || resolve_suppression_handle(&app)).await
}

/// Shared writer core; the desktop adapter supplies its live watcher handle.
pub(crate) async fn write_to_layer_with_suppression(
    params: WriteToLayerParams,
    resolve: impl FnOnce() -> Result<SuppressionHandle, Error>,
) -> Result<WriteOutcome, Error> {
    let WriteToLayerParams {
        layer_id,
        logical_name,
        content,
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
        expected_layer_hash,
        force,
    } = params;
    if !is_safe_logical_name(&logical_name) {
        log::error!(
            target: "content_engine::commands",
            "sense_sync_write_to_layer rejected invalid logical_name {:?}",
            logical_name
        );
        return Err(Error::InvalidLogicalName(logical_name));
    }
    let ctx_params = LayerContextParams {
        code_repo_slug,
        codesense_home,
        user_id,
        org_slug,
        team_name,
    };
    ctx_params.validate_slug()?;
    let lcx = ctx_params.derive()?;
    let ctx = &lcx.resolver;
    // Review-iter-4 (P-14): routed through `classify_write_unbacked`
    // for AC-8 testability.
    if let Some(outcome) = classify_write_unbacked(layer_id, ctx) {
        return Ok(outcome);
    }
    let root = content_layer_root(layer_id, ctx)
        .expect("classify_write_unbacked returned None ⇒ layer is backed");
    let dest = root.join(&logical_name);
    // Capture for the post-write log before `content` is moved into the
    // blocking task — review-iter-1 fix to drop the sync `dest.metadata()`
    // call that previously blocked the async runtime.
    let content_len = content.len();

    // Suppression handle up front so a missing watcher becomes a typed
    // Error rather than a runtime panic from `Manager::state`.
    let handle = resolve()?;

    // Per-repo serialization (workspace path skips lock — see review
    // deferral note).
    let _lock = LockPlan::Repo(lcx.slug()).acquire().await;

    // SS-6 Task 1.3: atomic 3-way conflict detection. Read the current
    // layer file under the per-repo lock and compare its blake3 hash
    // to `expected_layer_hash`. On mismatch, return
    // `WriteOutcome::RejectedConflict { layer_current_hash }` without
    // writing — the caller decides what to do with the conflict.
    //
    // No conflict check when:
    // - `expected_layer_hash` is `None` (SS-10 / migration callers
    //   that don't request the gate);
    // - `force` is `true` (user picked `[P] push anyway`);
    // - the layer file doesn't exist (NotFound) — the write below
    //   creates it from scratch, equivalent to a first-time Adopt
    //   reaching this code path.
    //
    // Workspace-only writes (`code_repo_slug == None`) skip the
    // per-repo lock per the existing deferral note, which means the
    // conflict check ALSO isn't serialized against another concurrent
    // write to the same `(layer, logical_name)`. Documented in the
    // workspace-lock deferral; acceptable for the sub-epic scope
    // (workspace pushes are rare and target one file per call).
    if let Some(expected) = expected_layer_hash {
        if !force {
            let dest_for_hash = dest.clone();
            let hash_result = tokio::task::spawn_blocking(move || hash_file(&dest_for_hash)).await;
            match classify_conflict_check(hash_result, expected) {
                ConflictDecision::Proceed => {
                    // Layer hash matched expected, OR layer file absent
                    // (treated as no-conflict — write creates from
                    // scratch).
                }
                ConflictDecision::Reject(outcome) => {
                    log::info!(
                        target: "content_engine::commands",
                        "sense_sync_write_to_layer RejectedConflict {} into {:?}: \
                         layer hash differs from expected (force=false)",
                        logical_name,
                        layer_id,
                    );
                    return Ok(outcome);
                }
                ConflictDecision::JoinFailed(msg) => {
                    log::error!(
                        target: "content_engine::commands",
                        "sense_sync_write_to_layer conflict-check task failed: {}",
                        msg
                    );
                    return Err(Error::Io {
                        op: "hash",
                        path: dest.clone(),
                        message: msg,
                    });
                }
                ConflictDecision::HashFailed(msg) => {
                    log::error!(
                        target: "content_engine::commands",
                        "sense_sync_write_to_layer conflict-check hash failed: {}",
                        msg
                    );
                    return Err(Error::Io {
                        op: "hash",
                        path: dest.clone(),
                        message: msg,
                    });
                }
            }
        }
    }

    // Suppression installed BEFORE mkdir so intermediate directory
    // creation events don't escape the suppression window
    // (review-iter-1 fix).
    //
    // Review-iter-5 (F7 + EH-7 + EH-23): cover EVERY newly-created
    // ancestor, not just the immediate parent. `create_dir_all` may
    // create N missing intermediates; FSEvents from those creations
    // would otherwise escape suppression. Canonical form handles macOS
    // firmlink mismatch (lexical /var vs watcher-canonicalized
    // /private/var).
    let dest_parent: Option<&Path> = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let newly_created_parents: Vec<PathBuf> = match dest_parent {
        Some(parent) => compute_missing_ancestors(parent),
        None => Vec::new(),
    };
    use std::collections::HashSet;
    let mut suppression_set: HashSet<PathBuf> = HashSet::new();
    suppression_set.insert(canonical_ancestor_form(&dest));
    if let Some(parent) = dest_parent {
        suppression_set.insert(canonical_ancestor_form(parent));
    }
    for ancestor in &newly_created_parents {
        suppression_set.insert(canonical_ancestor_form(ancestor));
    }
    let suppression_paths: Vec<PathBuf> = suppression_set.into_iter().collect();
    let _suppression = Suppression::new(&handle, suppression_paths);

    if let Some(parent) = dest_parent {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::io("mkdir", parent, e))?;
    }

    let dest_for_blocking = dest.clone();
    let write_result = tokio::task::spawn_blocking(move || {
        write_tmp_fsync_rename_suppressed(&dest_for_blocking, &content, &handle)
    })
    .await;
    match write_result {
        Err(join_err) => {
            log::error!(
                target: "content_engine::commands",
                "write {} into {:?} blocking task failed: {}",
                logical_name,
                layer_id,
                join_err
            );
            // Review-iter-7 (Edge-M1): mirror the move handler's F6
            // cleanup. Without this, a write failure leaves the
            // freshly-created parent chain as ghost empty folders —
            // the same regression F6 fixed for the move handler.
            remove_newly_created_parents(&newly_created_parents, &root).await;
            return Err(Error::Io {
                op: "write",
                path: dest.clone(),
                message: format!("blocking task failed: {}", join_err),
            });
        }
        Ok(Err(e)) => {
            log::error!(
                target: "content_engine::commands",
                "write {} into {:?} failed: {}",
                logical_name,
                layer_id,
                e
            );
            // Review-iter-7 (Edge-M1): see above.
            remove_newly_created_parents(&newly_created_parents, &root).await;
            return Err(Error::Io {
                op: "write",
                path: dest.clone(),
                message: e.to_string(),
            });
        }
        Ok(Ok(())) => {}
    }

    log::info!(
        target: "content_engine::commands",
        "wrote {} into layer {:?} ({} bytes)",
        logical_name,
        layer_id,
        content_len
    );

    Ok(WriteOutcome::Written)
}

// ---------------------------------------------------------------------------
// SS-4c wave-engine entry points
// ---------------------------------------------------------------------------
//
// `set_active_team_for_wave` — team activation (watches + ActiveTeamState).
// SW-XS-3: the `trigger_startup_wave` sibling is gone; no wave fires on
// first mount or on a team switch.
//
// The command routes its ctx through SS-13's validating
// `derive_resolver_context` (post-rebase consolidation) — the iter-7
// empty/whitespace-padded team guards I added directly here are now
// redundant because `validate_path_segment` inside
// `derive_resolver_context` already rejects empty / non-segment-safe
// strings centrally. Kept the SS-13 boundary error mapping
// (`Error::InvalidContextSegment` → `String`) for parity with the rest
// of SS-13's command surface.

/// SW-EW-5: handle returned by [`ensure_epic_workspace`] — the registered
/// epic-workspace target, read by the frontend to flip the EW-1
/// integration-workspace row from "unmaterialized (stub)" to real.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpicWorkspaceHandle {
    /// The synthetic `_epic/{epic-id}` slug, as an `owner/repo` string.
    pub slug: String,
    /// Absolute path to the epic workspace dir on local disk
    /// (`{codesense_home}/users/{uid}/_epic/{org_slug}/{epic-id}/` — outside
    /// every context-repo clone; see `path_to_epic_workspace` / `EPIC_DIR`).
    pub dir_path: String,
    /// `true` once the dir exists and a lockfile registers it as a
    /// content-engine target.
    pub registered: bool,
}

/// How [`ensure_branch_workspace`] checks a branch out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchCheckout {
    /// The cached PR head (`origin/<branch>`) for review.
    #[default]
    Review,
    /// A branch that already exists locally (or as a cached `origin/` ref).
    Existing,
    /// A new branch created from the repository's local default branch.
    New,
}

/// SW-DX-18: handle returned by [`ensure_branch_workspace`] — a managed git
/// worktree checked out on an arbitrary branch/PR, with the codesense install
/// layer (skills/hooks/`.mcp.json`) materialized so an agent spawned in it can
/// run installed skills (`code-review`, `deep-research`). The FE binds this
/// `dir_path` as the spawn cwd.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchWorkspaceHandle {
    /// The source-repo slug the worktree is a checkout of (`owner/repo`).
    pub slug: String,
    /// Absolute path to the branch worktree on local disk
    /// (`{codesense_home}/users/{uid}/_branch/{org_slug}/{owner}/{repo}/{branch_slug}/`
    /// — a codesense-managed surface BY PATH; see `path_to_branch_workspace`).
    pub dir_path: String,
    /// `true` once the worktree exists, is confined to a managed surface, and a
    /// lockfile registers it as a content-engine target.
    pub registered: bool,
}

/// SW-EW-15: derive + validate the epic id from a doc path — the single guard
/// shared by all three epic commands (ensure/assemble/teardown). Rejects an
/// empty path, derives the parent-disambiguated `{parent}__{leaf}` id, and
/// confirms it constructs a valid `_epic/{id}` slug (returned so callers that
/// need it — `ensure` — don't re-derive). `cmd` prefixes the error message.
fn checked_epic_id_from_doc_path(
    epic_file_path: &str,
    cmd: &str,
) -> Result<(String, crate::domain::SourceRepoSlug), String> {
    if epic_file_path.trim().is_empty() {
        return Err(format!("{cmd}: invalid epic id (empty epic file path)"));
    }
    let epic_id = crate::domain::derive_epic_id_from_doc_path(Path::new(epic_file_path));
    let slug = crate::domain::derive_epic_workspace_slug(&epic_id).map_err(|e| {
        format!("{cmd}: invalid epic id {epic_id:?} (from {epic_file_path:?}): {e}")
    })?;
    Ok((epic_id, slug))
}

/// SW-EW-5: ensure an epic's workspace dir exists on disk and is registered
/// as a content-engine target, returning the [`EpicWorkspaceHandle`] the
/// panel reads.
///
/// Idempotent: re-open reuses the dir and the existing lockfile (preserving
/// its materialization baseline — never re-writes a populated lockfile back
/// to empty). The created dir is a **plain folder — no `.git`, no origin**;
/// the only git in the tree is the inner per-repo worktrees (EW-6).
///
/// **Non-blocking by design** (the EW-1 D5 invariant): the only
/// command-synchronous work is `create_dir_all` plus, the first time only,
/// one fsync'd lockfile write — no wave, no git. The frontend invokes this
/// fire-and-forget; content materialization rides the existing CP-3 wave once
/// the dir is enumerated (EW-7).
///
/// Registration here makes `enumerate_attached_code_repos_for_slug(ctx,
/// &slug)` return the dir. Including the synthetic slug in the *team* wave's
/// slug list (so CP-3 fans into it) is EW-7's job — EW-5 lands the primitive.
#[tauri::command]
pub async fn ensure_epic_workspace(
    app: tauri::AppHandle,
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    epic_file_path: String,
) -> Result<EpicWorkspaceHandle, String> {
    let _ = app;
    let (handle, _) = ensure_epic_workspace_inner(
        &codesense_home,
        github_user_id,
        &org_slug,
        &team,
        &epic_file_path,
    )?;
    // Product context is prepared only by an explicit operation or agent startup.

    Ok(handle)
}

/// SW-DX-18: check out an arbitrary branch/PR of a source repo into a MANAGED
/// worktree and materialize the codesense install layer into it — the `branch`
/// target of the `openWorkspace` primitive.
///
/// Unlike the epic path (a plain non-git folder), a branch workspace is a real
/// `git worktree` of `repo_path` checked out at `branch`, placed UNDER
/// `users/_branch/…` so it is a codesense-managed surface BY PATH
/// (`is_codesense_managed_target`) with NO story-centric worktree-index record.
/// It registers the checkout as an attached repo and writes the git-excluded
/// managed config (`.mcp.json`, hooks) a hand-typed agent reads. It copies no
/// team content: an agent launched here receives its declared context through
/// agent preparation (SW-XS Step 2), not through this command.
///
/// `repo_path` is the source repo's LOCAL clone (FE-resolved from the `repo`
/// slug); `repo_slug` (`owner/repo`) scopes the per-repo install layer + the
/// lockfile. Confinement (AC-5/AC-6/R10): if `create_worktree` adopts a
/// pre-existing checkout OUTSIDE a managed surface (e.g. the user's primary
/// checkout of that branch), refuse `unavailable:` before any spawn.
// A Tauri command takes flat JSON-deserializable params (the FE wrapper maps
// camelCase → these); bundling them into a struct just to satisfy the lint would
// fork the command ABI for no benefit (project-context: prefer `#[allow]` for
// structural lints over refactoring a stable API).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn ensure_branch_workspace(
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    repo_path: String,
    repo_slug: String,
    branch: String,
    review_head_repo: Option<String>,
    pull_number: Option<u64>,
    checkout: Option<BranchCheckout>,
) -> Result<BranchWorkspaceHandle, String> {
    let slug = SourceRepoSlug::parse_owner_repo(&repo_slug).ok_or_else(|| {
        format!("unavailable: invalid source repo slug '{repo_slug}' (want owner/repo)")
    })?;
    if branch.trim().is_empty() {
        return Err("unavailable: branch must be non-empty".to_string());
    }
    // The branch is dashboard-supplied → reject a ref that could be read as a git
    // flag or traverse the derived path (mirrors `fetch_and_create_worktree`'s
    // guard; `create_worktree` also uses a `--` separator as defense-in-depth).
    if branch.contains("..") || branch.starts_with('-') || branch.contains('\0') {
        return Err(format!("unavailable: invalid branch ref '{branch}'"));
    }

    let (workspace_ref, fork_pull_number) = review_checkout_identity(
        &repo_slug,
        review_head_repo.as_deref(),
        pull_number,
        &branch,
    )?;

    let home = PathBuf::from(&codesense_home);
    let branch_slug = branch_workspace_slug(&workspace_ref);
    let derived = crate::context_repo::path_to_branch_workspace(
        &home,
        &Principal::from(github_user_id),
        &org_slug,
        &slug,
        &branch_slug,
    );

    // A review opens the cached `origin/<branch>` (the PR head) and never mints a
    // branch at an unrelated HEAD. An agent launch opens an existing branch, or
    // creates a new one from the repo's local default branch. None of them fetch.
    let derived = derived.to_string_lossy().into_owned();
    let actual_path = match (checkout.unwrap_or_default(), fork_pull_number) {
        (BranchCheckout::Review, Some(number)) => {
            crate::commands::worktree::checkout_pull_request_worktree_for_review(
                repo_path, number, derived,
            )
            .await?
        }
        (BranchCheckout::Review, None) => {
            crate::commands::worktree::checkout_branch_worktree_for_review(
                repo_path,
                branch.clone(),
                derived,
            )
            .await?
        }
        (BranchCheckout::Existing | BranchCheckout::New, Some(_)) => {
            return Err("unavailable: a fork pull request opens only for review".to_string());
        }
        (mode @ (BranchCheckout::Existing | BranchCheckout::New), None) => {
            crate::commands::worktree::checkout_branch_worktree_for_agent(
                repo_path,
                branch.clone(),
                derived,
                mode == BranchCheckout::New,
            )
            .await?
        }
    };
    let dir = PathBuf::from(&actual_path);

    // Confinement (AC-5/AC-6/R10): the adopt path can return the user's primary
    // checkout when the branch is already checked out there — a NON-managed
    // surface. Refuse before any spawn (never open unmanaged, never materialize
    // the install layer into a user's main checkout).
    if !crate::install::worktree_index::is_codesense_managed_target(&dir, &home) {
        return Err(format!(
            "unavailable: branch '{branch}' is checked out in a non-managed location ({}) — cannot open as a managed workspace",
            dir.display()
        ));
    }

    let ctx = derive_resolver_context(
        home.clone(),
        github_user_id,
        org_slug.clone(),
        Some(team.clone()),
    )
    .map_err(|e| format!("ensure_branch_workspace: {e}"))?;

    // Register as a content-engine target (idempotent), then the git-excluded
    // config a hand-typed agent reads.
    //
    // SW-XS-3: this used to await an install-only `WorktreeDerive` wave. That wave
    // routed to `run_repo_reconcile`, which only records the wave in the sync
    // store and writes no file — a no-op — so it is gone, not moved.
    crate::content_engine::register_attached_code_repo(&ctx, &slug, &dir)
        .map_err(|e| format!("ensure_branch_workspace: register: {e}"))?;
    crate::commands::managed_worktree_config::write(&dir, &home);
    let handle_slug = slug.to_string();

    Ok(BranchWorkspaceHandle {
        slug: handle_slug,
        dir_path: actual_path,
        registered: true,
    })
}

/// Decide whether a branch review can use the base remote branch directly or
/// must use GitHub's base-repository pull ref. The returned workspace identity
/// includes the base-scoped PR number for forks, preventing same-name fork
/// branches from aliasing one managed worktree.
fn review_checkout_identity(
    base_repo: &str,
    head_repo: Option<&str>,
    pull_number: Option<u64>,
    branch: &str,
) -> Result<(String, Option<u64>), String> {
    if head_repo.is_some_and(|head| head != base_repo) {
        let number = pull_number.ok_or_else(|| {
            "unavailable: fork review requires a GitHub pull request number".to_string()
        })?;
        Ok((format!("pull/{number}/{branch}"), Some(number)))
    } else {
        Ok((branch.to_string(), None))
    }
}

/// SW-DX-18: a filesystem-safe, **injective** leaf name for a branch worktree
/// dir. `/` → `-` keeps it readable, and a short stable `blake3` suffix of the
/// RAW ref disambiguates refs that would otherwise alias — `feat/x` and the
/// literal `feat-x` both readable-slugify to `feat-x`, but they are DISTINCT git
/// refs and MUST map to distinct dirs (two reviews aliasing one worktree would
/// make the second `git worktree add` fail on the occupied path). Stable across
/// runs (a re-open re-derives the same dir → `create_worktree` adopts it).
fn branch_workspace_slug(branch: &str) -> String {
    let readable = branch.replace('/', "-");
    let digest = blake3::hash(branch.as_bytes()).to_hex();
    format!("{}-{}", readable, &digest.as_str()[..8])
}

/// The app-free unit of [`ensure_epic_workspace`]: create + register the epic
/// dir and place its `.mcp.json`, returning the handle plus the derived
/// `epic_id` (so the command can build the inheritance wave without re-deriving
/// it). Split out so the dir/register/idempotency behavior stays unit-testable
/// without an `AppHandle` — the install-layer materialize needs one to drive a
/// wave and lives in the command wrapper.
fn ensure_epic_workspace_inner(
    codesense_home: &str,
    github_user_id: u64,
    org_slug: &str,
    team: &str,
    epic_file_path: &str,
) -> Result<(EpicWorkspaceHandle, String), String> {
    // SW-EW-15: the epic id is derived ONCE from the doc path (`{parent}__{leaf}`)
    // so two same-leaf epics get distinct dirs/slugs. The FE never reproduces it
    // — it reads `dir_path` back from the returned handle.
    let (epic_id, slug) = checked_epic_id_from_doc_path(epic_file_path, "ensure_epic_workspace")?;

    let ctx = derive_resolver_context(
        PathBuf::from(codesense_home),
        github_user_id,
        org_slug.to_string(),
        Some(team.to_string()),
    )
    .map_err(|e| format!("ensure_epic_workspace: {e}"))?;

    let dir = crate::context_repo::path_to_epic_workspace(
        Path::new(codesense_home),
        &Principal::from(github_user_id),
        org_slug,
        &epic_id,
    );

    // The epic root: a plain folder (no `.git`). Idempotent.
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("ensure_epic_workspace: create {}: {e}", dir.display()))?;

    // Register as a content-engine target (idempotent — preserves an existing
    // lockfile's materialization baseline on re-open).
    crate::content_engine::register_attached_code_repo(&ctx, &slug, &dir)
        .map_err(|e| format!("ensure_epic_workspace: register: {e}"))?;

    // A launch through CodeSense brings its MCP bridge and status hooks per session; an
    // agent typed here by hand finds them in the dir's own config.
    crate::commands::managed_worktree_config::write(&dir, Path::new(codesense_home));

    Ok((
        EpicWorkspaceHandle {
            slug: slug.to_string(),
            dir_path: dir.to_string_lossy().into_owned(),
            registered: true,
        },
        epic_id,
    ))
}

/// SW-EW-35: the lifecycle events `cli`'s `codesense-notify` hook listens on to drive
/// the per-tab agent-status dot: exactly the events the status receiver maps for it
/// (its adapter's status hooks).
fn status_hook_events(cli: AgentCli) -> impl Iterator<Item = &'static str> {
    adapter(cli).status_hooks().iter().map(|hook| hook.event)
}

/// Where `cli` reads hooks in a folder, and the command its status hook runs (its
/// descriptor's): the script its tab's host installed (`codesense_pty_core::notify_hook`),
/// so it works in a non-git epic dir as well as a worktree and no folder carries it.
fn project_hooks(cli: AgentCli) -> Option<&'static ProjectHooks> {
    descriptor(cli).project_hooks.as_ref()
}

/// The folder settings of `cwd` that `cli` reads hooks from and that exist: its committed
/// file, then its personal one.
fn folder_settings(cwd: &Path, cli: AgentCli) -> Vec<PathBuf> {
    project_hooks(cli)
        .map(|hooks| vec![cwd.join(&hooks.committed), cwd.join(&hooks.local)])
        .unwrap_or_default()
}

/// `true` when `settings.hooks[event]` already runs a hook whose command names `marker`.
fn event_runs_hook(settings: &serde_json::Value, event: &str, marker: &str) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|groups| groups.as_array())
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                g.get("hooks")
                    .and_then(|hs| hs.as_array())
                    .is_some_and(|hs| {
                        hs.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains(marker))
                        })
                    })
            })
        })
}

/// `true` when `settings.hooks[event]` already runs the CodeSense status hook: a command
/// naming the host's installed script.
fn event_runs_notify_hook(settings: &serde_json::Value, event: &str) -> bool {
    event_runs_hook(
        settings,
        event,
        &codesense_pty_core::notify_hook::installed_script(),
    )
}

/// The inbox hooks of `cli`, whose settings body this is, from its inbox port: they run
/// the host's own helper, so they are never written into a folder.
fn arm_inbox(settings: &mut serde_json::Value, cli: AgentCli) -> Option<()> {
    let mut launch = Prepare::new(Vec::new(), std::mem::take(settings), "");
    let armed = adapter(cli).inbox_hooks(&mut launch);
    *settings = launch.settings;
    armed.ok()
}

/// Add the status hook to a per-launch settings body of `cli` for every lifecycle
/// event the launch folder's own settings (`settings.json`, `settings.local.json`)
/// do not already run it on, so an agent launched into a folder CodeSense never
/// writes to still drives its status dot, and its inbox hooks. The CLI merges hooks
/// across settings files, and the command is the same string the folder writer uses,
/// so a folder that has it never runs it twice. A body of an unexpected shape is left
/// as is.
pub(crate) fn merge_launch_notify_hooks(
    settings: &mut serde_json::Value,
    cwd: &Path,
    cli: AgentCli,
) {
    let own: Vec<serde_json::Value> = folder_settings(cwd, cli)
        .into_iter()
        .filter_map(|path| serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok())
        .collect();
    let needed: Vec<&str> = status_hook_events(cli)
        .filter(|ev| !own.iter().any(|c| event_runs_notify_hook(c, ev)))
        .collect();
    let _ = merge_notify_hooks(settings, cli, &needed);
    let _ = arm_inbox(settings, cli);
}

/// Required Agent startup checks existing folder settings, then projects hooks into
/// the private per-session body. It never repairs or writes the launch folder.
pub(crate) fn merge_required_launch_notify_hooks(
    settings: &mut serde_json::Value,
    cwd: &Path,
    cli: AgentCli,
) -> Result<(), String> {
    use crate::agents::content_reader::{ContentReader, FsContentReader};
    use std::io::Read;
    const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
    let mut own = Vec::new();
    for path in folder_settings(cwd, cli) {
        let reader = match FsContentReader.open(&path) {
            Ok(reader) => reader,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "required notification settings {}: {error}",
                    path.display()
                ))
            }
        };
        let mut bytes = Vec::new();
        reader
            .take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!("required notification settings {}: {error}", path.display())
            })?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(format!(
                "required notification settings {} exceed 1 MiB",
                path.display()
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!("required notification settings {}: {error}", path.display())
        })?;
        if !value.is_object() {
            return Err(format!(
                "required notification settings {} must be an object",
                path.display()
            ));
        }
        own.push(value);
    }
    let needed: Vec<&str> = status_hook_events(cli)
        .filter(|event| {
            !own.iter()
                .any(|config| event_runs_notify_hook(config, event))
        })
        .collect();
    merge_notify_hooks(settings, cli, &needed)
        .and_then(|_| arm_inbox(settings, cli))
        .ok_or_else(|| "required notification settings have invalid hooks".to_string())?;
    Ok(())
}

/// Add `cli`'s status hook to a managed worktree's own `settings.local.json` body for
/// every lifecycle event its committed `settings.json` does not already run it on, so an
/// agent typed by hand there drives its status dot. `None` when the body's `hooks` is not
/// the shape the CLI expects; otherwise whether anything changed.
pub(crate) fn merge_local_notify_hooks(
    local: &mut serde_json::Value,
    dir: &Path,
    cli: AgentCli,
) -> Option<bool> {
    let committed = project_hooks(cli)
        .and_then(|hooks| std::fs::read_to_string(dir.join(&hooks.committed)).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let needed: Vec<&str> = status_hook_events(cli)
        .filter(|ev| {
            !committed
                .as_ref()
                .is_some_and(|c| event_runs_notify_hook(c, ev))
        })
        .collect();
    merge_notify_hooks(local, cli, &needed)
}

/// Append the CodeSense hook group to each `events` entry of `settings.hooks`
/// that does not already run it, keeping every other key and hook. Returns
/// whether anything changed, or `None` when `hooks` (or an event's value) is
/// not the shape Claude expects — the caller then leaves the file alone.
/// Same required notification hooks for a private remote launch file of `cli`.
pub(crate) fn merge_remote_notify_hooks(
    settings: &mut serde_json::Value,
    cli: AgentCli,
) -> Result<(), String> {
    let events: Vec<&str> = status_hook_events(cli).collect();
    merge_notify_hooks(settings, cli, &events)
        .ok_or_else(|| "required notification settings cannot be merged".to_string())?;
    Ok(())
}

/// [`merge_hook_group`] of `cli`'s status hook at `events`; nothing for a CLI with no
/// project hooks, which has no status hooks either.
fn merge_notify_hooks(
    settings: &mut serde_json::Value,
    cli: AgentCli,
    events: &[&str],
) -> Option<bool> {
    let Some(hooks) = project_hooks(cli) else {
        return Some(false);
    };
    let status = merge_hook_group(
        settings,
        events,
        &hooks.status_command,
        &codesense_pty_core::notify_hook::installed_script(),
    );
    // The secret-file guard goes wherever the status hooks do, for the CLIs that have project
    // hooks.
    let guard = merge_hook_entry_matching(
        settings,
        &["PreToolUse"],
        codesense_pty_core::notify_hook::GUARD_MATCHER,
        serde_json::json!({
            "type": "command",
            "command": codesense_pty_core::notify_hook::guard_command(),
            "timeout": 5,
        }),
        &codesense_pty_core::notify_hook::installed_guard(),
    );
    let deny = merge_deny_rules(settings, codesense_pty_core::notify_hook::deny_rules());
    Some(status? | guard? | deny?)
}

/// Add git-ai's checkpoint hook (`command`) to a per-launch Claude settings body, on both
/// tool events, unless it is already there.
pub(crate) fn merge_attribution_hooks(
    settings: &mut serde_json::Value,
    command: &str,
) -> Option<bool> {
    merge_hook_entry_matching(
        settings,
        &["PreToolUse", "PostToolUse"],
        "*",
        serde_json::json!({
            "type": "command",
            "command": command,
            "timeout": crate::attribution::launch::HOOK_TIMEOUT,
        }),
        crate::attribution::managed::HOOK_MARKER,
    )
}

/// Replace only marked checkpoint entries, preserving every other hook and setting.
pub(crate) fn reconcile_attribution_hooks(
    settings: &mut serde_json::Value,
    command: Option<&str>,
) -> Option<bool> {
    let mut changed = false;
    if let Some(hooks) = settings.get_mut("hooks") {
        for groups in hooks.as_object_mut()?.values_mut() {
            let groups = groups.as_array_mut()?;
            groups.retain_mut(|group| {
                let Some(handlers) = group
                    .get_mut("hooks")
                    .and_then(serde_json::Value::as_array_mut)
                else {
                    return true;
                };
                let before = handlers.len();
                std::vec::Vec::retain(handlers, |handler| {
                    !handler
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|c| c.contains(crate::attribution::managed::HOOK_MARKER))
                });
                changed |= handlers.len() != before;
                !std::vec::Vec::is_empty(handlers)
            });
        }
    }
    if let Some(command) = command {
        changed |= merge_attribution_hooks(settings, command)?;
    }
    Some(changed)
}

/// Add `rules` to `settings.permissions.deny` that are not there yet, keeping every other rule.
/// `None` when `permissions` or `deny` is not the shape Claude expects; the file is then left alone.
fn merge_deny_rules(settings: &mut serde_json::Value, rules: &[String]) -> Option<bool> {
    let permissions = settings
        .as_object_mut()?
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()?;
    let deny = permissions
        .entry("deny")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()?;
    let mut changed = false;
    for rule in rules {
        if !deny.iter().any(|existing| existing.as_str() == Some(rule.as_str())) {
            deny.push(serde_json::json!(rule));
            changed = true;
        }
    }
    Some(changed)
}

/// Append a hook group running `command` to each `events` entry of `settings.hooks` that
/// does not already run a command naming `marker`, keeping every other key and hook.
fn merge_hook_group(
    settings: &mut serde_json::Value,
    events: &[&str],
    command: &str,
    marker: &str,
) -> Option<bool> {
    merge_hook_entry(
        settings,
        events,
        serde_json::json!({
            "type": "command",
            "command": command,
            "timeout": 5,
        }),
        marker,
    )
}

/// [`merge_hook_group`] for a hook entry of any shape (its command names `marker`).
fn merge_hook_entry(
    settings: &mut serde_json::Value,
    events: &[&str],
    hook: serde_json::Value,
    marker: &str,
) -> Option<bool> {
    merge_hook_entry_matching(settings, events, "", hook, marker)
}

/// [`merge_hook_entry`] for a group that matches only the tools in `matcher`.
fn merge_hook_entry_matching(
    settings: &mut serde_json::Value,
    events: &[&str],
    matcher: &str,
    hook: serde_json::Value,
    marker: &str,
) -> Option<bool> {
    let group = serde_json::json!({
        "matcher": matcher,
        "hooks": [hook],
    });
    let pending: Vec<&str> = events
        .iter()
        .copied()
        .filter(|ev| !event_runs_hook(settings, ev, marker))
        .collect();
    if pending.is_empty() {
        return Some(false);
    }
    let root = settings.as_object_mut()?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()?;
    for ev in pending {
        hooks
            .entry(ev.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()?
            .push(group.clone());
    }
    Some(true)
}

// ---------- SW-EW-6: integration-worktree assembly + teardown ----------

/// An epic-assigned repo with a resolved local clone (internal — the
/// commands resolve `epic.repos` → clone paths themselves; the frontend only
/// passes the epic doc path).
struct ResolvedRepo {
    slug: SourceRepoSlug,
    clone_path: PathBuf,
}

/// A successfully assembled (or idempotently already-present) worktree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledWorktree {
    pub owner: String,
    pub repo: String,
    pub path: String,
    pub branch: String,
    /// `true` when this call created the integration branch off the default.
    pub created_branch: bool,
    /// `true` when an idempotent re-call found the worktree already present.
    pub already_existed: bool,
    /// Non-fatal warnings (e.g. cross-volume fragility, D4).
    pub warnings: Vec<String>,
}

/// A per-repo assembly/teardown failure — surfaced to the toast's error
/// phase without aborting the sibling repos.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeFailure {
    pub owner: String,
    pub repo: String,
    pub error: String,
}

/// SLW-CO-8 (LD-A): a per-repo dev-env install run the FE overlay tracks to
/// aggregate epic batch readiness. `run_id` is the channel the streamed
/// `worktree-run-{run_id}` log lines AND the terminal `worktree-run-completed`
/// event fire on. The FE **cannot** reconstruct these run_ids (they embed the
/// backend-derived `{parent}__{leaf}` epic id it can't know), so they must be
/// returned explicitly. An empty/absent-`worktree.yaml` repo is still listed —
/// its run no-ops but still completes, so it must be counted toward "M of M".
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DevEnvRun {
    pub owner: String,
    pub repo: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssembleEpicResult {
    pub assembled: Vec<AssembledWorktree>,
    pub failures: Vec<WorktreeFailure>,
    /// The resolved integration branch (override or derived).
    pub branch: String,
    /// SLW-CO-8 (LD-A): the per-repo dev-env install runs the overlay awaits for
    /// aggregate readiness. `M(installs) = dev_env_runs.len()` — NOT
    /// `assembled.len()` (a repo assembled but with no resolved clone gets no
    /// run, and empty-config repos still get one).
    pub dev_env_runs: Vec<DevEnvRun>,
}

/// Bounded parallelism for per-repo git work: capped at the core count, 8,
/// and the number of repos (never 0).
fn worktree_parallelism(n: usize) -> usize {
    std::thread::available_parallelism()
        .map(|c| c.get())
        .unwrap_or(4)
        .min(8)
        .min(n.max(1))
        .max(1)
}

/// SLW-CO-8 (LD-B): the terminal completion signal for a per-repo dev-env
/// install. The `worktree-run-{run_id}` channel carries only streamed log lines
/// (no exit/terminal event), and for the epic the install runs fire-and-forget
/// backend-side — so the FE overlay has no way to tell when a repo's install
/// finished or whether it failed. Emit a discrete completion on the shared
/// `worktree-run-completed` channel keyed by `run_id`, folding the exit code /
/// error. `ok` is true only on a clean `Ok` with a zero exit code, so a failed
/// `npm ci` (non-zero exit → `Ok` today) and a provenance refusal (`Err`) both
/// register as failures (AC-4). The FE epic aggregator subscribes once and
/// filters by its expected run_id set.
pub(crate) const WORKTREE_RUN_COMPLETED_EVENT: &str = "worktree-run-completed";

fn emit_worktree_run_completed(
    sink: &dyn crate::commands::worktree::EventSink,
    run_id: &str,
    outcome: &Result<crate::commands::worktree::RunResult, String>,
) {
    let (ok, exit_code, error) = match outcome {
        Ok(r) => (r.exit_code == 0, r.exit_code, None),
        Err(e) => (false, -1, Some(e.clone())),
    };
    sink.emit(
        WORKTREE_RUN_COMPLETED_EVENT,
        serde_json::json!({
            "run_id": run_id,
            "ok": ok,
            "exit_code": exit_code,
            "error": error,
        }),
    );
}

/// SLW-CO-8 (review H1): guarantees a terminal `worktree-run-completed` for a
/// dev-env run even if `run_setup_for_worktree` **panics** mid-install. Armed for
/// the run's lifetime and `disarm`ed once the real outcome is emitted; on an
/// unwinding panic the `Drop` fires and emits a synthetic failure. Without this
/// a panicked install would never terminalize on the FE — the epic batch would
/// strand with a forever-`running` install (the overlay hangs, and the
/// installs-running gate keeps the start-watchdog from ever rescuing it).
struct RunCompletionGuard<'a> {
    sink: &'a dyn crate::commands::worktree::EventSink,
    run_id: &'a str,
    armed: bool,
}

impl<'a> RunCompletionGuard<'a> {
    fn new(sink: &'a dyn crate::commands::worktree::EventSink, run_id: &'a str) -> Self {
        Self {
            sink,
            run_id,
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunCompletionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            emit_worktree_run_completed(
                self.sink,
                self.run_id,
                &Err("dev-env install task did not complete (panicked)".to_string()),
            );
        }
    }
}

/// SLW-CO-8 (AC-1 / LD-D): run the per-repo dev-env installs **concurrently**,
/// bounded by the same `worktree_parallelism` cap as worktree creation, so N
/// `npm ci`s fan out (wall-clock ≈ the slowest repo, not the sum) without
/// thrashing disk/CPU. Each run reuses the shared `run_setup_for_worktree`
/// mid-layer (same provenance gate + on-disk config resolution + per-repo
/// isolation), then emits its terminal `worktree-run-completed` event (LD-B)
/// so the overlay can observe batch readiness even though this whole call is
/// spawned fire-and-forget (the command already returned).
///
/// Extracted AppHandle-free (takes an `Arc<dyn EventSink>`) so the fan-out +
/// terminal emit are unit-testable with a recording sink — mirrors the
/// `assemble_epic_worktrees_core` extraction pattern.
async fn run_epic_dev_env_setups(
    sink: Arc<dyn crate::commands::worktree::EventSink>,
    registry: crate::commands::worktree::WorktreeRunRegistry,
    org_root: PathBuf,
    authority: crate::commands::worktree::WorktreeConfigAuthority,
    setups: Vec<(String, PathBuf, PathBuf)>,
) {
    if setups.is_empty() {
        return;
    }
    let sem = Arc::new(tokio::sync::Semaphore::new(worktree_parallelism(
        setups.len(),
    )));
    let mut set = tokio::task::JoinSet::new();
    for (run_id, wt_path, clone_path) in setups {
        let sem = Arc::clone(&sem);
        let registry = registry.clone_arc();
        let sink = Arc::clone(&sink);
        let org_root = org_root.clone();
        let authority = authority.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore not closed");
            // Panic-safe: the guard emits a failure completion if the install
            // panics before we reach the real emit below (review H1).
            let mut guard = RunCompletionGuard::new(sink.as_ref(), &run_id);
            let outcome = crate::commands::worktree::run_setup_for_worktree(
                sink.as_ref(),
                &registry,
                &wt_path,
                &clone_path,
                &org_root,
                &authority,
                crate::commands::worktree::WorktreeConfig::default(),
                None,
                &run_id,
            )
            .await;
            // Fold the outcome into the terminal event (replaces the old
            // swallowed `log::warn`-only failure handling — AC-4).
            guard.disarm();
            emit_worktree_run_completed(sink.as_ref(), &run_id, &outcome);
            if let Err(e) = &outcome {
                log::warn!(
                    target: "content_engine",
                    "assemble_epic_worktrees: epic dev-env setup failed (run={run_id}): {e}"
                );
            }
        });
    }
    while set.join_next().await.is_some() {}
}

/// Read + parse an epic doc's frontmatter (for its `repos:` / `branch:`).
///
/// `pub(crate)` so the SW-EW-9 `read_epic_repos`/`set_epic_repos` commands
/// parse epic frontmatter through the *same* path as assembly — the panel's
/// repo list and the worktree assembler can never disagree on what `repos:` is.
pub(crate) fn load_epic_frontmatter(
    epic_file_path: &str,
) -> Result<crate::commands::pipeline_format::FrontmatterFields, String> {
    let path = Path::new(epic_file_path);
    let format = crate::commands::pipeline_format::format_of(path);
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read epic doc {}: {e}", path.display()))?;
    Ok(crate::commands::pipeline_format::extract_meta(
        format, &text,
    ))
}

/// Resolve a slug to a local clone path. Attachments win first (the registry
/// the `attach_source_repo_path` / `save_source_repo_attachment` /
/// `add_source_repo` tools write), falling back to the install store so
/// managed clones and synthetic targets (`register_attached_code_repo`) keep
/// resolving. If both miss, the caller returns the typed `no local clone`
/// refusal. The attachment path is accepted only if its folder still exists
/// — a stale row falls through to the store.
///
/// The gap this closes: an in-place user checkout outside the codesense home
/// has an `attachments.yml` row but no install-store row (the managed-surface
/// gate in `content_engine::sync::wave` never writes it for a non-managed,
/// non-registered-worktree, non-`users/`-rooted path). Reading from the store
/// alone turned a user-attached clone into a "no local clone" refusal even
/// though `list_source_repos` reported it as attached.
pub(crate) fn resolve_local_clone_for_slug(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
) -> Result<Option<PathBuf>, String> {
    let principal = Principal::from(ctx.user_id);
    // Fail-soft on the attachments branch: a malformed `attachments.yml` logs
    // and falls through to the store, so one corrupt row can't turn a managed
    // clone (store-only, pre-fix behaviour) into a spurious `no local clone`
    // refusal. A hard error here would be a NEW failure mode this diff
    // introduced for store-only slugs.
    match crate::commands::attachments::attached_clone_path_for_slug(
        &ctx.codesense_home,
        &principal,
        slug,
    ) {
        Ok(Some(path)) => return Ok(Some(path)),
        Ok(None) => {}
        Err(e) => log::warn!(
            target: "content_engine::resolve_local_clone",
            "attachments read failed for slug '{}': {e} — falling through to install store",
            slug.remote_key()
        ),
    }
    let store_paths = crate::content_engine::enumerate_attached_code_repos_for_slug(ctx, slug)
        .map_err(|e| e.to_string())?;
    Ok(store_paths.into_iter().next().map(|r| r.code_repo_path))
}

/// Resolve each assigned repo slug to its local clone path. Repos with no
/// local clone are returned separately — they can't be assembled (MC/attach
/// territory) and surface as a per-repo failure rather than aborting the
/// batch.
///
/// Resolves **per slug** via [`resolve_local_clone_for_slug`] (attachments →
/// install store) rather than the team-wide enumerator, which dedups by
/// `code_repo_path` — that dedup would silently drop a second slug pointing at
/// the same physical clone (e.g. a monorepo attached under two names) and
/// report it as a spurious "no local clone".
fn resolve_epic_repo_clones(
    ctx: &ResolverContext,
    repos: &[SourceRepoSlug],
) -> Result<(Vec<ResolvedRepo>, Vec<SourceRepoSlug>), String> {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    for slug in repos {
        match resolve_local_clone_for_slug(ctx, slug)
            .map_err(|e| format!("enumerate attached repos for {slug}: {e}"))?
        {
            Some(clone_path) => resolved.push(ResolvedRepo {
                slug: slug.clone(),
                clone_path,
            }),
            None => missing.push(slug.clone()),
        }
    }
    Ok((resolved, missing))
}

/// SW-EW-6 AC-1/2: assemble a writable integration worktree per assigned repo
/// into `{workspace_dir}/{owner}/{repo}/`, off each repo's clone, on the
/// epic's integration branch (created off the default if absent).
///
/// The backend is authoritative: it reads `epic.repos` / `branch` from the
/// epic doc itself (EW-5's `epic_repos()` / `resolve_integration_branch`) and
/// resolves each slug to its clone via the team's attached-repo lockfiles —
/// so the frontend only passes the epic doc path, never the repo list.
///
/// **Bounded concurrency (D2):** each per-repo `add_integration_worktree`
/// runs on `spawn_blocking` (git2 is blocking) gated by a `Semaphore`, so
/// distinct clones assemble in parallel without flooding the runtime. A
/// per-repo failure (incl. no-local-clone) is collected, never fatal.
///
/// **Non-blocking (D3):** the frontend fires this fire-and-forget after the
/// epic opens (it never gates render on the result); the reused worktree
/// toast reports progress/outcome. An empty `repos:` (a draft epic) is a
/// no-op success (EW-7 AC-6 empty-workspace path).
#[tauri::command]
pub async fn assemble_epic_worktrees(
    app: tauri::AppHandle,
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    epic_file_path: String,
) -> Result<AssembleEpicResult, String> {
    let worktree_authority = crate::commands::worktree::WorktreeConfigAuthority {
        codesense_home: PathBuf::from(&codesense_home),
        principal: Principal::from(github_user_id),
        team: team.clone(),
    };
    // SW-EW-15: derive + validate the epic id from the doc path FIRST — before
    // any path math (the single derivation site, shared with the other epic
    // commands) so the dir this assembles into agrees with the open path's, and
    // a malformed id can't escape `_epic/` via the create/inheritance path below
    // (review F2 — the create must not run on an unvalidated id).
    let (epic_id, _slug) =
        checked_epic_id_from_doc_path(&epic_file_path, "assemble_epic_worktrees")?;

    // Assembly can be invoked without opening first: retain registration and
    // essential wiring, but never enqueue broad product context inheritance.
    ensure_epic_workspace_inner(
        &codesense_home,
        github_user_id,
        &org_slug,
        &team,
        &epic_file_path,
    )?;

    let (result, org_root, setups) = assemble_epic_worktrees_core(
        codesense_home,
        github_user_id,
        org_slug,
        team,
        epic_id,
        epic_file_path,
    )
    .await?;

    // SW-WU-8 / SW-WU-13: the core laid the assembled git worktrees down on disk, so the
    // panel's git-first discovery (`discover_worktrees`) can now surface them (tagged
    // `kind=EpicIntegration`) — nudge it to refetch (one batch emit; the panel's debounced
    // listener coalesces it into a single re-enumeration). These integration worktrees are
    // NOT index-backed — discovery is disk/git, never `iter_records`. Reuses the shared
    // `worktree-changed` seam. Gated on `assembled` (not `failures`): if no clone resolved,
    // nothing was created — skip the pointless refetch. Emitted before the fire-and-forget
    // dev-env spawn below.
    if !result.assembled.is_empty() {
        crate::commands::worktree::emit_worktree_changed(&app);
    }

    // SW-EW-7 AC-2 / SLW-CO-8 AC-1: run dev-env per assembled inner worktree,
    // reusing the shared `run_setup_for_worktree` mid-layer (same provenance gate
    // + on-disk config resolution as the story-worktree flow). Each worktree
    // resolves its OWN repo's `worktree.yaml` via the repo's clone_path;
    // empty/absent setup is a no-op (the data-driven run/skip — no
    // `should_run_setup` predicate). Team-synced setup without a captured approval
    // hash is refused (not prompted) — correct for a headless N-repo loop.
    //
    // SLW-CO-8 (LD-D): the installs fan out CONCURRENTLY (bounded) rather than the
    // old serial `for … .await` loop — wall-clock ≈ the slowest repo, not the sum.
    // Still spawned fire-and-forget so `assemble` returns promptly (never blocks
    // the UI, DO-NOT); each run's terminal `worktree-run-completed` event (LD-B)
    // is how the FE overlay observes batch readiness + folds install failures.
    if !setups.is_empty() {
        let sink: Arc<dyn crate::commands::worktree::EventSink> =
            Arc::new(crate::commands::worktree::TauriEventSink(app.clone()));
        let registry = app
            .state::<crate::commands::worktree::WorktreeRunRegistry>()
            .inner()
            .clone_arc();
        tokio::spawn(run_epic_dev_env_setups(
            sink,
            registry,
            org_root,
            worktree_authority,
            setups,
        ));
    }

    Ok(result)
}

/// SW-EW-6/7: the `AppHandle`-free assembly core. Resolves `epic.repos` →
/// clones, assembles one integration worktree per repo (bounded), and returns
/// the result + `org_root` + the per-worktree dev-env jobs `(run_id,
/// worktree_path, clone_path)` the command runs via `run_setup_for_worktree`.
/// Extracted so the assembly logic is unit-testable without a Tauri harness —
/// the established pattern for AppHandle-gated commands in this module.
#[allow(clippy::type_complexity)]
async fn assemble_epic_worktrees_core(
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    epic_id: String,
    epic_file_path: String,
) -> Result<(AssembleEpicResult, PathBuf, Vec<(String, PathBuf, PathBuf)>), String> {
    // Validate epic_id before it touches any path math — rejects empty,
    // `.`/`..`, and path separators (mirrors ensure_epic_workspace), so a
    // malformed id can't escape the `_epic/{id}/` dir via path traversal.
    crate::domain::derive_epic_workspace_slug(&epic_id)
        .map_err(|e| format!("assemble_epic_worktrees: invalid epic id {epic_id:?}: {e}"))?;
    let ctx = derive_resolver_context(
        PathBuf::from(&codesense_home),
        github_user_id,
        org_slug.clone(),
        Some(team.clone()),
    )
    .map_err(|e| format!("assemble_epic_worktrees: {e}"))?;
    let org_root = crate::context_repo::path_to_org_root(
        Path::new(&codesense_home),
        &Principal::from(github_user_id),
        &org_slug,
    )?;
    let workspace_dir = crate::context_repo::path_to_epic_workspace(
        Path::new(&codesense_home),
        &Principal::from(github_user_id),
        &org_slug,
        &epic_id,
    );

    let fields = load_epic_frontmatter(&epic_file_path)
        .map_err(|e| format!("assemble_epic_worktrees: {e}"))?;
    let branch = fields.resolve_integration_branch(&epic_id);
    let repos = fields.epic_repos();

    let mut failures = Vec::new();
    let (resolved, missing) = resolve_epic_repo_clones(&ctx, &repos)
        .map_err(|e| format!("assemble_epic_worktrees: {e}"))?;
    for slug in missing {
        failures.push(WorktreeFailure {
            owner: slug.owner.clone(),
            repo: slug.repo.clone(),
            error: format!("no local clone attached for {slug} — attach or clone it first"),
        });
    }

    if resolved.is_empty() {
        return Ok((
            AssembleEpicResult {
                assembled: Vec::new(),
                failures,
                branch,
                dev_env_runs: Vec::new(),
            },
            org_root,
            Vec::new(),
        ));
    }

    // SW-EW-7 AC-2: capture each repo's clone_path keyed by its host-aware
    // integration-worktree path
    // before the JoinSet consumes `resolved`. The dev-env loop needs the
    // clone_path (not the assembled worktree path) because read_worktree_config
    // is slug-keyed off the clone, and `{epic-dir}/{owner}/{repo}/` has no
    // `.codesense/` of its own.
    let clone_by_worktree: std::collections::HashMap<PathBuf, PathBuf> = resolved
        .iter()
        .map(|r| {
            (
                crate::context_repo::path_to_integration_worktree(&workspace_dir, &r.slug),
                r.clone_path.clone(),
            )
        })
        .collect();

    let sem = Arc::new(tokio::sync::Semaphore::new(worktree_parallelism(
        resolved.len(),
    )));
    let mut set = tokio::task::JoinSet::new();
    for repo in resolved {
        let sem = Arc::clone(&sem);
        let workspace_dir = workspace_dir.clone();
        let branch = branch.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore not closed");
            let wt_path =
                crate::context_repo::path_to_integration_worktree(&workspace_dir, &repo.slug);
            let clone_path = repo.clone_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::context_repo::add_integration_worktree(&clone_path, &wt_path, &branch)
            })
            .await;
            (repo.slug.owner, repo.slug.repo, result)
        });
    }

    let mut assembled = Vec::new();
    while let Some(joined) = set.join_next().await {
        let (owner, repo, result) =
            joined.map_err(|e| format!("assemble_epic_worktrees: task join error: {e}"))?;
        match result {
            Ok(Ok(r)) => assembled.push(AssembledWorktree {
                owner,
                repo,
                path: r.path.to_string_lossy().into_owned(),
                branch: r.branch,
                created_branch: r.created_branch,
                already_existed: r.outcome
                    == crate::context_repo::AddWorktreeOutcome::AlreadyExisted,
                warnings: r.warnings,
            }),
            Ok(Err(e)) => failures.push(WorktreeFailure {
                owner,
                repo,
                error: e.to_string(),
            }),
            Err(join_err) => failures.push(WorktreeFailure {
                owner,
                repo,
                error: format!("worktree task panicked: {join_err}"),
            }),
        }
    }

    // SW-EW-7 AC-2: build the per-worktree dev-env jobs (run_id, worktree_path,
    // clone_path) the command runs via `run_setup_for_worktree`. Each worktree
    // resolves its OWN repo's `worktree.yaml` via the repo's clone_path (NOT the
    // assembled path — `read_worktree_config` is slug-keyed off the clone).
    // Built here (AppHandle-free); executed by the command wrapper.
    // SLW-CO-8 (LD-A): build the dev-env runs (returned to the FE) and the
    // execution jobs (`setups`) from the SAME iteration so the run_id set the
    // overlay awaits and the jobs the command runs can never disagree.
    //
    // SLW-CO-8 (review F1): a process-global sequence makes each run_id GLOBALLY
    // UNIQUE per attempt. Without it the id was deterministic
    // (`epic-{epic_id}-{owner}-{repo}`), so a close+reopen of the same epic while
    // a prior install was still running fire-and-forget would emit a completion
    // carrying the SAME id the reopen awaits — the fresh install would inherit
    // the stale outcome. The FE can't reconstruct the id anyway (LD-A) → it reads
    // it from `dev_env_runs`, so a per-run seq is transparent.
    static EPIC_DEV_ENV_RUN_SEQ: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let mut setups: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    let mut dev_env_runs: Vec<DevEnvRun> = Vec::new();
    for w in &assembled {
        if let Some(clone_path) = clone_by_worktree.get(Path::new(&w.path)) {
            let seq = EPIC_DEV_ENV_RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let run_id = format!("epic-{epic_id}-{}-{}-{seq}", w.owner, w.repo);
            dev_env_runs.push(DevEnvRun {
                owner: w.owner.clone(),
                repo: w.repo.clone(),
                run_id: run_id.clone(),
            });
            setups.push((run_id, PathBuf::from(&w.path), clone_path.clone()));
        }
    }

    Ok((
        AssembleEpicResult {
            assembled,
            failures,
            branch,
            dev_env_runs,
        },
        org_root,
        setups,
    ))
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TeardownEpicResult {
    /// Worktree paths that were dropped (branch kept).
    pub removed: Vec<String>,
    /// Worktree paths skipped because an open session references them.
    pub skipped_in_use: Vec<String>,
    /// Non-fatal warnings: dirty worktrees preserved (AC-6) or per-repo errors.
    pub warnings: Vec<String>,
}

/// True when an open session's cwd (`in_use`) is the worktree dir or nested
/// under it — i.e. tearing it down would reclaim a worktree a live session
/// references (AC-6: never reclaim in-use).
///
/// Compares both raw and canonicalized forms: the raw check covers the common
/// case (both derived from the same `codesense_home` spelling) plus
/// not-yet-existing nested cwds; the canonical check defends against symlink /
/// macOS `/tmp`-vs-`/private/tmp` divergence between the worktree path and a
/// session's reported cwd (canonicalization is best-effort — a cwd that can't
/// be canonicalized just falls back to the raw comparison).
fn worktree_is_in_use(wt_path: &Path, in_use: &[String]) -> bool {
    let wt_canon = wt_path.canonicalize().ok();
    in_use.iter().any(|p| {
        let cwd = Path::new(p);
        if cwd == wt_path || cwd.starts_with(wt_path) {
            return true;
        }
        match (cwd.canonicalize(), wt_canon.as_ref()) {
            (Ok(cwd_c), Some(wt_c)) => cwd_c == *wt_c || cwd_c.starts_with(wt_c),
            _ => false,
        }
    })
}

/// SW-EW-45: reconcile an epic's on-disk integration worktrees to its `repos:`
/// frontmatter — after a repo is removed from the epic, tear down the worktree
/// for any repo no longer assigned. Per worktree, under the per-clone lock:
/// **skip if an open session references it**; **warn (preserve) on uncommitted
/// changes** (`UncommittedChanges` is warn-not-fail — never auto-reset);
/// otherwise drop the worktree and **keep the branch**. Non-blocking +
/// fire-and-forget from the remove handler.
///
/// Reconcile-to-frontmatter (not "delete this one slug") self-heals drift: any
/// on-disk integration worktree whose repo is not in the current `repos:` is
/// torn down, so an externally-edited doc converges too. Reuses SW-EW-44's
/// disk-first [`discover_integration_worktrees`] (the single recognizer, shared
/// with the roster) and the shared [`teardown_integration_worktrees`]
/// removal loop, so recognition and teardown never diverge between assembly
/// and this.
///
/// Thin command wrapper — runs the AppHandle-free core, then emits
/// `WORKTREE_CHANGED_EVENT` once if anything was actually removed, so an open
/// Worktrees panel drops the torn-down rows live (symmetry with
/// `assemble_epic_worktrees`). Best-effort emit; a missing listener never fails
/// teardown. The core is extracted so the logic stays unit-testable without a
/// Tauri harness — the established pattern for AppHandle-gated commands here.
#[tauri::command]
pub async fn reconcile_epic_worktrees(
    app: tauri::AppHandle,
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    epic_file_path: String,
    in_use_paths: Vec<String>,
) -> Result<TeardownEpicResult, String> {
    let result = reconcile_epic_worktrees_core(
        codesense_home,
        github_user_id,
        org_slug,
        epic_file_path,
        in_use_paths,
    )
    .await?;
    if !result.removed.is_empty() {
        crate::commands::worktree::emit_worktree_changed(&app);
    }
    Ok(result)
}

async fn reconcile_epic_worktrees_core(
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    epic_file_path: String,
    in_use_paths: Vec<String>,
) -> Result<TeardownEpicResult, String> {
    // SW-EW-15: derive the id from the doc path (single derivation site) so this
    // targets the SAME `_epic/{parent}__{leaf}` dir the open path assembled into.
    let (epic_id, _slug) =
        checked_epic_id_from_doc_path(&epic_file_path, "reconcile_epic_worktrees")?;
    let workspace_dir = crate::context_repo::path_to_epic_workspace(
        Path::new(&codesense_home),
        &Principal::from(github_user_id),
        &org_slug,
        &epic_id,
    );

    // The assigned worktree paths, built with the SAME `path_to_integration_worktree`
    // builder assembly uses — a discovered worktree is de-assigned iff its path is
    // absent here. Compare by PATH (not owner/repo identity) on purpose: both sides
    // run through `physical_path_key`, so reserved/unicode repo names encode
    // identically, and both derive from the same `workspace_dir` with no one-sided
    // canonicalization (the macOS /tmp↔/private/tmp hazard). No clone-lockfile
    // resolution is needed (unlike assembly): discovery carries each worktree's own
    // clone + branch.
    let fields = load_epic_frontmatter(&epic_file_path)
        .map_err(|e| format!("reconcile_epic_worktrees: {e}"))?;
    let assigned: std::collections::HashSet<PathBuf> = fields
        .epic_repos()
        .iter()
        .map(|slug| crate::context_repo::path_to_integration_worktree(&workspace_dir, slug))
        .collect();

    // Discover the on-disk integration worktrees in ONE blocking hop (`read_dir`
    // walk + per-worktree git), off the async executor so a remove never janks the
    // UI or spins the terminal.
    let discovered =
        discover_integration_worktrees_off_thread(&workspace_dir, "reconcile_epic_discover")
            .await?;

    // Tear down exactly the worktrees whose repo is no longer assigned. A
    // recognized-but-unresolvable worktree (detached HEAD / no clone → `job` is
    // `None`) is already warned by discovery; the shared loop skips dirty/in-use.
    let jobs = discovered
        .into_iter()
        .filter(|d| !assigned.contains(&d.worktree_path))
        .filter_map(|d| d.job)
        .collect();

    teardown_integration_worktrees(jobs, Arc::new(in_use_paths)).await
}

/// One integration worktree to tear down: its on-disk path, the parent clone
/// (`git worktree remove` runs from there), and its branch (kept — used only to
/// resolve the libgit2 worktree name). The per-repo reconcile discovers these on
/// disk (carrying the ACTUAL path/clone/branch, so host-aware layouts never
/// drift).
#[derive(Debug)]
struct IntegrationWorktreeJob {
    clone_path: PathBuf,
    worktree_path: PathBuf,
    branch: String,
}

/// Tear down every integration worktree in `jobs`: per worktree, under the
/// per-clone lock, **skip if in-use**, else remove (branch always kept). Bounded
/// parallelism. Used by the per-repo [`reconcile_epic_worktrees`]. An empty
/// `jobs` is a no-op success.
async fn teardown_integration_worktrees(
    jobs: Vec<IntegrationWorktreeJob>,
    in_use: Arc<Vec<String>>,
) -> Result<TeardownEpicResult, String> {
    if jobs.is_empty() {
        return Ok(TeardownEpicResult::default());
    }

    let sem = Arc::new(tokio::sync::Semaphore::new(worktree_parallelism(
        jobs.len(),
    )));
    let mut set = tokio::task::JoinSet::new();
    for job in jobs {
        let sem = Arc::clone(&sem);
        let in_use = Arc::clone(&in_use);
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore not closed");
            let wt_display = job.worktree_path.to_string_lossy().into_owned();
            if worktree_is_in_use(&job.worktree_path, &in_use) {
                return TeardownOutcome::SkippedInUse(wt_display);
            }
            let worktree_path_for_untrust = job.worktree_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::context_repo::remove_integration_worktree(
                    &job.clone_path,
                    &job.worktree_path,
                    &job.branch,
                )
            })
            .await;
            // SW-WA-F13: on a confirmed removal, drop the trust key the
            // managed launch spine wrote at pretrust time. Best-effort; we
            // don't have principal in scope here so this cleans the default
            // `.claude.json` only — the per-account homes get swept the next
            // time the FE `remove_worktree` path runs for anything in that
            // account's workspace. A stale `projects["<path>"]` entry Claude
            // ignores (path is gone) is harmless.
            if matches!(
                &result,
                Ok(Ok(crate::context_repo::RemoveWorktreeOutcome::Removed))
                    | Ok(Ok(
                        crate::context_repo::RemoveWorktreeOutcome::RemovedBroken
                    ))
            ) {
                if let Some(home) = crate::commands::paths::codesense_home_dir() {
                    crate::agents::workspace_trust::untrust_managed_launch_dir(
                        &home,
                        None,
                        &worktree_path_for_untrust,
                    );
                }
            }
            TeardownOutcome::from_result(wt_display, result)
        });
    }

    let mut out = TeardownEpicResult::default();
    while let Some(joined) = set.join_next().await {
        match joined.map_err(|e| format!("teardown_integration_worktrees: task join error: {e}"))? {
            TeardownOutcome::Removed(p) => out.removed.push(p),
            TeardownOutcome::SkippedInUse(p) => out.skipped_in_use.push(p),
            TeardownOutcome::Warning(w) => out.warnings.push(w),
        }
    }
    Ok(out)
}

/// Per-worktree teardown disposition, collected across the bounded JoinSet.
enum TeardownOutcome {
    Removed(String),
    SkippedInUse(String),
    Warning(String),
}

impl TeardownOutcome {
    fn from_result(
        wt_display: String,
        result: Result<
            Result<
                crate::context_repo::RemoveWorktreeOutcome,
                crate::context_repo::RemoveWorktreeError,
            >,
            tokio::task::JoinError,
        >,
    ) -> Self {
        use crate::context_repo::RemoveWorktreeError;
        match result {
            Ok(Ok(_)) => TeardownOutcome::Removed(wt_display),
            // Uncommitted changes → preserve the work, warn (AC-6).
            Ok(Err(RemoveWorktreeError::UncommittedChanges { .. })) => TeardownOutcome::Warning(
                format!("{wt_display}: has uncommitted changes — left in place"),
            ),
            Ok(Err(e)) => TeardownOutcome::Warning(format!("{wt_display}: {e}")),
            Err(join_err) => TeardownOutcome::Warning(format!(
                "{wt_display}: teardown task panicked: {join_err}"
            )),
        }
    }
}

// ---------- SW-EW-44: integration worktree discovery ----------

/// One integration worktree discovered on disk. `job` is `Some` only when both
/// the parent clone and the checked-out branch resolved (the pair
/// [`remove_integration_worktree`] needs); `None` means "recognized but not
/// cleanly removable" (e.g. detached HEAD).
struct DiscoveredIntegrationWorktree {
    worktree_path: PathBuf,
    job: Option<IntegrationWorktreeJob>,
}

/// Discover every integration worktree physically present under an epic root.
/// Walks the root depth-bounded and recognizes a linked-worktree root two ways
/// that must agree: a gitlink `.git` FILE AND the shared [`classify_epic_worktree_path`]
/// accepting its shape — the SAME classifier the worktree roster trusts, so
/// reconcile and roster can never diverge on "what is an epic integration worktree"
/// (the depth cap is only a walk-safety bound, not the recognition rule). Each
/// result carries the actual on-disk path, plus a removal job when its parent clone ([`main_repo_of_worktree`]) and branch
/// ([`current_branch_of_worktree`] — `None` on detached HEAD, never a bogus
/// `"HEAD"`) both resolve — with no dependency on the epic doc's `repos:`/`branch:`.
/// Never descends into a worktree's own tree; generated siblings (`_bmad-output`,
/// `docs`, `.claude`) carry no nested worktree. Blocking (`read_dir` + git) —
/// call via `await_blocking`.
fn discover_integration_worktrees(workspace_dir: &Path) -> Vec<DiscoveredIntegrationWorktree> {
    // Cap the descent at the deepest real layout (`{host}/{owner}/{repo}`), so an
    // errant deep tree (or a worktree's own contents) is never walked.
    const MAX_DEPTH: usize = 3;
    fn walk(dir: &Path, depth: usize, out: &mut Vec<DiscoveredIntegrationWorktree>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // A linked worktree root: gitlink `.git` FILE + the shared classifier
            // agrees on the shape (reconcile ↔ roster never diverge).
            if path.join(".git").is_file()
                && crate::context_repo::classify_epic_worktree_path(&path).is_some()
            {
                let job = match (
                    crate::commands::worktree::main_repo_of_worktree(&path),
                    crate::commands::worktree::current_branch_of_worktree(&path),
                ) {
                    (Some(clone_path), Some(branch)) => Some(IntegrationWorktreeJob {
                        clone_path,
                        worktree_path: path.clone(),
                        branch,
                    }),
                    _ => {
                        // Recognized but not cleanly removable (detached HEAD /
                        // unresolved clone): left in place. Surfaced (not silent)
                        // per the guard-visibility contract.
                        log::warn!(
                            target: "commands::content_engine",
                            "integration worktree {} has no resolvable branch/clone (detached HEAD?) — skipping git-clean teardown",
                            path.display()
                        );
                        None
                    }
                };
                out.push(DiscoveredIntegrationWorktree {
                    worktree_path: path,
                    job,
                });
                // Don't descend into a worktree's own tree.
                continue;
            }
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(workspace_dir, 0, &mut out);
    out
}

/// Run [`discover_integration_worktrees`] off the async executor (the `read_dir`
/// walk + per-worktree git is blocking), for the per-repo
/// [`reconcile_epic_worktrees`]. `label` names the span for tracing.
async fn discover_integration_worktrees_off_thread(
    workspace_dir: &Path,
    label: &'static str,
) -> Result<Vec<DiscoveredIntegrationWorktree>, String> {
    let dir = workspace_dir.to_path_buf();
    crate::commands::async_helpers::await_blocking(label, move || {
        Ok(discover_integration_worktrees(&dir))
    })
    .await
}

fn validate_team_activation_profile(
    profile: &crate::commands::local_profile::LocalProfileState,
    principal: u64,
) -> Result<(), String> {
    // Called under the activation transaction before any watch or active-team mutation.
    profile.check_expected(Some(&principal.to_string()))
}

/// SS-4c AC-14 step 4 / AC-13: team activation. Re-registers the
/// team-scoped watches and commits `ActiveTeamState` (what MCP
/// `derive_active_context` reads). Idempotent; safe to call repeatedly with
/// the same `(org_slug, team)`.
///
/// SW-XS-3: activating or switching a team writes no workspace content — there
/// is no Startup or TeamSwitch wave any more.
#[tauri::command]
pub async fn set_active_team_for_wave<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    source_repos: Vec<SourceRepoSlug>,
) -> Result<(), String> {
    let watcher_state = app
        .try_state::<crate::commands::watch::WatcherState>()
        .ok_or_else(|| "WatcherState not registered".to_string())?;
    // One async transaction owns every team-scoped validation, derive, watch
    // mutation, and ActiveTeamState commit. This
    // Tokio guard may safely span the blocking-pool awaits below.
    let _team_activation_guard = watcher_state.lock_team_activation().await;
    validate_team_activation_profile(
        &app.state::<crate::commands::local_profile::LocalProfileState>(),
        github_user_id,
    )?;
    // SW-OC-26: a team activation is when the profile and synced set settle;
    // let the footprint index re-walk its roots.
    crate::context_query::request_reconcile(&app);

    log::info!(
        target: "content_engine::sync::wave",
        "set_active_team_for_wave: org={} team={} slugs={}",
        org_slug,
        team,
        source_repos.len()
    );

    // Iter-6 (Edge MED-3) → iter-7 (Edge HIGH-1): reject empty/whitespace
    // team AND whitespace-padded team early. Iter-6's `trim().is_empty()`
    // caught `""`, `"   "`, `"\t\n"` but NOT padded-content cases like
    // `"  default  "` — those passed the guard, propagated into ctx
    // verbatim, then `validate_path_segment` downstream returned `None`
    // for every layer path, then propagated into the active-team projection
    // stash and every resolution keyed on it, corrupting downstream fan-out
    // under the assumption the empty set was intentional. Catching both
    // pathologies (empty + padded) at the boundary prevents the silent
    // disable of the wave engine's event-driven path.
    if team.trim().is_empty() {
        return Err("set_active_team_for_wave: team must be non-empty".to_string());
    }
    if team != team.trim() {
        return Err(
            "set_active_team_for_wave: team has leading or trailing whitespace".to_string(),
        );
    }

    // SS-6: snapshot (org_slug, team) before derive_resolver_context
    // consumes them. Committed to ActiveTeamState only after derive
    // succeeds (below) — that keeps a failed derive from polluting the
    // singleton with values the resolver itself rejected.
    let active_team_snapshot = crate::mcp::state::ActiveTeam {
        org_slug: org_slug.clone(),
        team_name: team.clone(),
    };

    let ctx = crate::commands::await_blocking("resolve active team context", move || {
        derive_resolver_context(
            PathBuf::from(codesense_home),
            github_user_id,
            org_slug,
            Some(team),
        )
        .map_err(|e| format!("derive_resolver_context failed: {:?}", e))
    })
    .await?;

    let admission_ctx = ctx.clone();
    crate::commands::await_blocking("validate active context", move || {
        let root = crate::context_repo::path_to_org_root(
            &admission_ctx.codesense_home,
            &admission_ctx.principal(),
            &admission_ctx.org_slug,
        )?;
        if !root.join("org.yml").is_file() {
            return Err("Context is no longer available; refresh the workspace list".into());
        }
        Ok(())
    })
    .await?;

    let coord = {
        let s = app
            .try_state::<Arc<WaveCoordinator>>()
            .ok_or_else(|| "WaveCoordinator not registered".to_string())?;
        Arc::clone(s.inner())
    };

    // SW-EW-7 D1/T4: fold registered epic workspaces into the stashed
    // `ProjectionTarget.source_repos` so content/source-edit waves also fan into
    // open epic dirs (the stash is set once here, not per edit).
    let source_repos: Vec<_> = source_repos
        .into_iter()
        .filter(|slug| !crate::domain::is_epic_workspace_slug(slug))
        .collect();
    // SW-CP-34 (S2): stash the active-team target (ctx + slugs) the remaining
    // reverse machinery and the agent reconcile resolve attached repos from.
    // SW-XS-3: a stash only — it enqueues nothing.
    coord.stash_active_projection_on_team_change(&ctx, &source_repos);

    // SW-TS-1 (AC-3): register the team-agnostic worktree move-watch over
    // the org-root `_team/` tree. Idempotent and intentionally decoupled
    // from the content-layer watches above — it must survive team switches
    // so a story moved into a *non-active* team still relinks its worktree
    // record. SW-XS-3: relinking is index bookkeeping; no worktree is synced.
    let org_root = crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?;
    let watch_app = app.clone();
    let watch_root = org_root.clone();
    let watch_ctx = ctx.clone();
    crate::commands::await_blocking("register context ownership watches", move || {
        let state = watch_app.state::<crate::commands::watch::WatcherState>();
        crate::commands::watch::register_worktree_move_watch(
            &watch_app,
            state.inner(),
            &watch_root,
            &watch_ctx.org_slug,
        );
        crate::commands::scratch_watch::register_scratch_watch(
            &watch_app,
            state.inner(),
            &watch_ctx.codesense_home,
            watch_ctx.user_id,
        );
        Ok(())
    })
    .await?;

    // The context repo's git state (HEAD, refs, index, fetch, merge/rebase state), so a
    // commit, push, fetch or rebase by anyone reaches the nav's status, Changes and History.
    // Asking git where it keeps that state is a few small reads, kept off the async worker.
    let git_dirs = {
        let root = org_root.clone();
        crate::commands::await_blocking("resolve context git dirs", move || {
            Ok(crate::commands::context_git_watch::GitDirs::resolve(&root))
        })
        .await
    };
    match git_dirs {
        Ok(dirs) => {
            let watch_app = app.clone();
            let watch_root = org_root.clone();
            crate::commands::await_blocking("register context git watch", move || {
                let state = watch_app.state::<crate::commands::watch::WatcherState>();
                crate::commands::context_git_watch::register_context_git_watch(
                    &watch_app,
                    state.inner(),
                    &watch_root,
                    dirs,
                );
                Ok(())
            })
            .await?;
        }
        Err(e) => log::warn!(target: "watch", "resolve context git dirs failed: {e}"),
    }

    // SW-WL-4: attach the active team's `context_output` watch on a
    // lifecycle SEPARATE from per-workspace watchers. Same idempotent
    // team-activation seam as the two watches above — the frontend calls
    // this command on first mount AND every team switch, so one call covers
    // launch attach + switch re-target. Decoupled from workspace count: it
    // stays attached when all workspaces close, and the zero-subscriber
    // dispatch skip keeps it free when no workspace is open.
    if let Some(team_name) = ctx.team_name.as_deref() {
        // SW-XS-4: the team content watch only — no main-clone reverse watch.
        let watch_app = app.clone();
        let watch_root = org_root.clone();
        let watched_team = team_name.to_string();
        let watched_org = ctx.org_slug.clone();
        crate::commands::await_blocking("register team content watches", move || {
            let state = watch_app.state::<crate::commands::watch::WatcherState>();
            crate::commands::watch::register_team_activation_watches(
                &watch_app,
                state.inner(),
                &watch_root,
                &watched_org,
                &watched_team,
            );
            Ok(())
        })
        .await?;
        // On the same idempotent team-activation seam, warm the deps cache for every
        // built-in CLI (not just the default), so Codex is launchable — the launch path
        // reads the warmed cache and refuses a cold basename. Off the main thread.
        crate::commands::deps::spawn_builtin_agents_warm(app.clone());

        // SW-WU-14: on the SAME always-on team-activation seam, watch each managed clone's
        // `.git/worktrees/` so a worktree created ANY way — including a manual
        // `git worktree add` with zero CodeSense involvement — surfaces in the panel with no
        // refresh (immediacy Layer B on top of SW-WU-13's authoritative scan). `source_repos`
        // is already the epic-expanded set (above), so `resolve_managed_clone_dirs` yields the
        // IDENTICAL clones the scan enumerates — watch-set ≡ scan-set, and epic-integration
        // worktrees (created off an already-attached clone) are covered by this one watch.
        // Resolution shells out to `git` per attached repo, so it runs on the blocking pool
        // (never the async worker — no team-switch jank); the attach itself is cheap syscalls.
        let clone_dirs = {
            let ctx = ctx.clone();
            let slugs = source_repos.clone();
            crate::commands::await_blocking("resolve_managed_clone_dirs", move || {
                Ok(crate::commands::worktree::resolve_managed_clone_dirs(
                    &ctx, &slugs,
                ))
            })
            .await
        };
        // Only (re)assert the watch set on a SUCCESSFUL resolution. On error (a panic in the
        // blocking task) leave the existing watches in place rather than passing an empty set,
        // which the detach-on-switch swap would read as "this team has no clones" and tear
        // every worktree watch down. An `Ok(empty)` legitimately means no clones and DOES
        // detach; a transient error must not — it self-heals on the next activation.
        match clone_dirs {
            Ok(clone_dirs) => {
                let watch_app = app.clone();
                crate::commands::await_blocking("register managed clone watches", move || {
                    let state = watch_app.state::<crate::commands::watch::WatcherState>();
                    crate::commands::watch::register_managed_clone_worktree_watch(
                        &watch_app,
                        state.inner(),
                        &clone_dirs,
                    );
                    Ok(())
                })
                .await?;
            }
            Err(e) => log::warn!(
                target: "content_engine::sync::wave",
                "resolve_managed_clone_dirs failed: {e} — keeping existing worktree watches this activation"
            ),
        }
    }

    // SS-6 Task 1.0 + review iter-1 (H-3): commit ActiveTeamState
    // LAST — only after every fallible step above has succeeded. Iter-1
    // shipped the commit earlier (right after derive_resolver_context),
    // which left ActiveTeamState pointing at the new team even when a
    // later step returned Err to the frontend. Subsequent
    // context MCP tools would then write to a team the rest of the
    // app was never switched to.
    //
    // Review iter-2 (H-2): iter-1's `if let Some(active_state) = ...`
    // silently dropped the commit when the singleton was unregistered.
    // In production that's a misconfigured app (registered in setup);
    // the wave WOULD be registered but no MCP tool could resolve the
    // active team because `derive_active_context` returns
    // `internal_error`. Behavior was indistinguishable from a working
    // app with a stale team. Now we hard-error so a missing singleton
    // surfaces loudly. (Test harnesses that genuinely run without
    // ActiveTeamState should construct the AppHandle with the state
    // registered.)
    let active_state = app
        .try_state::<crate::mcp::state::ActiveTeamState>()
        .ok_or_else(|| "ActiveTeamState not registered with app".to_string())?;
    let active_org = active_team_snapshot.org_slug.clone();
    let active_team = active_team_snapshot.team_name.clone();
    active_state.update(active_team_snapshot);
    // SLW-PT-19: mirror the active org for the AppHandle-free services that surface or
    // run scratch items (routines, agents), and make them rescan. SLW-PT-20: the
    // active team too, so a listing can honour that team's scratch switch.
    let home = ctx.codesense_home.clone();
    crate::context_repo::scratch_space::set_active_org(&home, Some(&active_org));
    crate::context_repo::scratch_space::set_active_team(&home, Some(&active_team));
    crate::routines::discover::invalidate_discovery_cache(&home);
    crate::live_hub::agent_definitions::membership_changed(&app);

    Ok(())
}

// ---------------------------------------------------------------------------
// SW-TS-5 Task 4 — launch reconcile of the worktree registry
// ---------------------------------------------------------------------------

/// Per-reconcile tally, surfaced in the completion log. `total` counts the
/// enumerated worktree records that survived the ghost reap; `reaped` counts the
/// confirmed-gone worktrees whose record and sync-store rows were dropped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcileSummary {
    total: usize,
    reaped: usize,
}

/// Reconcile the worktree registry against disk: enumerate every known worktree
/// record and reap the confirmed-gone ghosts (the shared gone-vs-unmounted gate),
/// so a record whose worktree was removed out-of-band self-heals. Every record
/// is a registered story worktree, which prepares its context explicitly, so
/// nothing is derived into it here. A failed enumerate is logged and skipped.
pub(crate) fn reconcile_worktrees_with(codesense_home: &Path, user_id: u64) -> ReconcileSummary {
    let records =
        match worktree_index::iter_story_records(codesense_home, &Principal::from(user_id)) {
            Ok(records) => records,
            Err(e) => {
                log::warn!(
                    target: "content_engine::sync::wave",
                    "worktree reconcile: enumerate failed — skipping: {:?}", e
                );
                return ReconcileSummary::default();
            }
        };
    // Silent background path — no toast. Only the survivors are counted here
    // (the reaped paths drive the panel receipt, not this background pass).
    let (records, reaped) = worktree_index::reap_confirmed_gone_records(
        codesense_home,
        &Principal::from(user_id),
        records,
    );
    // SW-XS-3 (K7): the gone-worktree reap used to ride the forward waves — a
    // fan-out that reached a removed worktree dropped its sync-store rows. Those
    // waves are gone, so the launch reconcile reaps the store rows (and the
    // per-path reverse state) of every worktree it just confirmed gone.
    for path in &reaped {
        crate::content_engine::sync::wave::reap_detached_worktree(codesense_home, path);
    }
    // SW-XS-4 (K7): the reverse sweeps also reaped gone branch workspaces, epic dirs
    // and managed clones (store targets with no worktree-index record). They are
    // gone, so the launch reconcile reaps every confirmed-gone store target too.
    let gone_targets =
        crate::content_engine::sync::wave::reap_gone_store_targets(codesense_home, user_id);
    ReconcileSummary {
        total: records.len(),
        reaped: reaped.len() + gone_targets,
    }
}

/// Offload [`reconcile_worktrees_with`] to a blocking task (non-blocking for the
/// caller) and log its tally.
fn spawn_worktree_reconcile(home: PathBuf, user_id: u64) {
    tauri::async_runtime::spawn_blocking(move || {
        let summary = reconcile_worktrees_with(&home, user_id);
        log::debug!(
            target: "content_engine::sync::wave",
            "worktree reconcile: {} surface(s) registered, {} gone worktree(s) reaped",
            summary.total, summary.reaped
        );
    });
}

/// SW-TS-5 Task 4 — fire-and-forget launch reconcile of every known worktree.
/// The frontend calls this once on mount, un-awaited; it reaps ghost records off
/// the UI thread, so boot never waits on it.
#[tauri::command]
pub async fn reconcile_worktrees_on_launch(
    codesense_home: String,
    github_user_id: u64,
) -> Result<(), String> {
    let home = PathBuf::from(codesense_home);
    tauri::async_runtime::spawn_blocking(move || {
        let summary = launch_reconcile_with(
            &home,
            github_user_id,
            &INSTALL_STORE_BACKFILL_STARTED,
            &backfill_install_store,
        );
        log::debug!(
            target: "content_engine::sync::wave",
            "launch worktree reconcile: {} surface(s) registered, {} gone worktree(s) reaped",
            summary.total, summary.reaped
        );
    });
    Ok(())
}

/// Set once the install-store backfill has started in this process.
static INSTALL_STORE_BACKFILL_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The launch-only housekeeping, then the shared [`reconcile_worktrees_with`]:
/// - SW-XS-17: delete the idle-clock sidecar the removed worktree reaper left
///   next to the records.
/// - SW-IS-3a: prime the install store from the JSON lockfiles, at most once per
///   process (`backfill_started`). This used to ride the removed worktree-cap
///   admission gate.
///
/// The backfill is injected so a test can count it. Blocking: callers run it off
/// the UI thread.
fn launch_reconcile_with(
    codesense_home: &Path,
    user_id: u64,
    backfill_started: &std::sync::atomic::AtomicBool,
    backfill: &dyn Fn(&Path, u64),
) -> ReconcileSummary {
    worktree_index::remove_retired_session_activity_file(codesense_home, &Principal::from(user_id));
    if !backfill_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
        backfill(codesense_home, user_id);
    }
    reconcile_worktrees_with(codesense_home, user_id)
}

/// SW-IS-3a: prime the install store from the JSON lockfiles. Marker-gated inside
/// [`backfill_json_once`](crate::content_engine::sync_state_store::SyncStateStore::backfill_json_once),
/// so a later launch is a cheap marker check, and it never overwrites a target the
/// store already holds. Best-effort: a failure leaves those targets store-absent
/// until a later launch retries.
fn backfill_install_store(home: &Path, user_id: u64) {
    let Some(store) = crate::content_engine::lockfile::open_sync_state_store(home) else {
        return;
    };
    if let Err(e) = store.backfill_json_once(home, user_id) {
        log::warn!(
            target: "content_engine::sync::wave",
            "install store backfill failed: {e}; those targets stay store-absent until a later launch retries"
        );
    }
}

/// SLW-FX-3 — event-driven in-session registry reconcile. The frontend fires
/// this on cheap, natural, timer-free in-session events (window focus /
/// visibility, worktree-surface open), so a worktree removed out-of-band drops
/// from the registry without a cold remount.
#[tauri::command]
pub async fn reconcile_stuck_worktrees(
    codesense_home: String,
    github_user_id: u64,
) -> Result<(), String> {
    spawn_worktree_reconcile(PathBuf::from(codesense_home), github_user_id);
    Ok(())
}

/// SW-TS-4 — assemble the worktree→story index record from the establish
/// inputs. `home`, `uid`, and `org_slug` are derived from the *story's own
/// path* (the authoritative `_team/<team>/` location parsed by the same
/// helpers the move-watch trusts), so a frontend can't drift the identity
/// that selects the team to materialize. Only `story_id` and `source_repo`
/// come from the caller — both genuinely frontmatter-sourced.
///
/// `None` is the neutral case (AC-5): the story path isn't an org-scoped
/// `_team` content path, or `source_repo` doesn't parse to a valid
/// `owner/repo` slug. The worktree is still created; it just can't be
/// linked — never persist a half-formed record with a bogus slug. No
/// `AppHandle` — the link assembly is unit-testable without Tauri; its only
/// read is the org registry (`org.yml` / `people.yml` / `teams.yml`) that
/// resolves the slug's provider + host.
fn build_link_record(
    story_source_path: &Path,
    code_repo_path: &Path,
    story_id: &str,
    source_repo: &str,
) -> Option<(PathBuf, u64, WorktreeStoryRecord)> {
    let (codesense_home, user_id, org_slug) = user_org_context_from_path(story_source_path)?;
    // SW-LN-B29: the slug as the story's team configures it (a local / non-GitHub repo keeps
    // its provider + host), so the record and store rows sit under the slug the team's
    // forward fan-out and refocus catch-up query.
    let slug = worktree_index::slug_for_story(
        &codesense_home,
        &Principal::from(user_id),
        &org_slug,
        story_source_path,
        source_repo,
    )?;
    let record =
        WorktreeStoryRecord::new(code_repo_path, org_slug, slug, story_id, story_source_path);
    Some((codesense_home, user_id, record))
}

/// Register durable story ownership and essential wiring without hydrating context.
/// Reopen preserves existing bytes, setup state, baselines, and receipts.
#[tauri::command]
pub async fn establish_worktree_story_link(
    app: tauri::AppHandle,
    code_repo_path: String,
    story_source_path: String,
    story_id: String,
    source_repo: String,
) -> Result<bool, String> {
    // Keep all registration and essential wiring I/O off the command executor.
    crate::commands::async_helpers::await_blocking("establish_worktree_story_link", move || {
        // RI-2-ALLOW: minted on the blocking pool inside `await_blocking` (the
        // SLW-RI-6 AC-1 open-chain offload). The `&BlockingCtx` gates the heavy
        // establishment body below so it can never be called back on a
        // command worker without this proof-of-off-plane.
        crate::commands::async_helpers::with_blocking(|ctx| {
            establish_worktree_story_link_blocking(
                ctx,
                &app,
                &code_repo_path,
                &story_source_path,
                &story_id,
                &source_repo,
            )
        })
    })
    .await
}

/// Register durable story ownership and admit explicit lifecycle operations,
/// preserving existing setup state and receipts without materializing context.
pub(crate) fn register_story_workspace(
    home: &Path,
    principal: &Principal,
    mut incoming: WorktreeStoryRecord,
) -> Result<bool, String> {
    incoming.code_repo_path = incoming
        .code_repo_path
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let identity =
        crate::content_engine::context_lifecycle::local_identity(&incoming.code_repo_path);
    let _lease = crate::content_engine::context_lifecycle::reserve_selected(home, &identity)?;
    let registrations = worktree_index::story_registrations(home, &incoming.code_repo_path)
        .map_err(|e| e.to_string())?;
    for (owner, record) in &registrations {
        if owner != principal
            || record.org_slug != incoming.org_slug
            || record.source_repo != incoming.source_repo
            || record.story_id != incoming.story_id
        {
            return Err("story workspace registration conflicts with its durable owner".into());
        }
    }
    if registrations.len() > 1 {
        return Err("story workspace has ambiguous existing registrations".into());
    }
    if let Some((_, existing)) = registrations.first() {
        incoming.code_repo_path = existing.code_repo_path.clone();
    }
    let root = incoming.code_repo_path.clone();
    let record = worktree_index::update_record_with(home, principal, &root, |current| {
        if let Some(mut current) = current {
            if current.org_slug != incoming.org_slug
                || current.source_repo != incoming.source_repo
                || current.story_id != incoming.story_id
            {
                return Err("story workspace registration changed concurrently".into());
            }
            current.story_source_path = incoming.story_source_path;
            current.intended_team = worktree_index::resolve_team(&current);
            Ok(current)
        } else {
            incoming.setup_complete = false;
            Ok(incoming)
        }
    })?;
    crate::content_engine::sync_state_store::SyncStateStore::open(
        crate::content_engine::sync_state_store::default_store_path(home),
    )
    .map_err(|e| e.to_string())?
    .admit_explicit_context(&identity)
    .map_err(|e| e.to_string())?;
    Ok(record.setup_complete)
}

/// Disk-touching registration and wiring run only on the blocking pool.
fn establish_worktree_story_link_blocking(
    _ctx: &crate::commands::async_helpers::BlockingCtx,
    app: &tauri::AppHandle,
    code_repo_path: &str,
    story_source_path: &str,
    story_id: &str,
    source_repo: &str,
) -> Result<bool, String> {
    let story_path = PathBuf::from(story_source_path);
    let code_path = PathBuf::from(code_repo_path);

    let Some((codesense_home, user_id, record)) =
        build_link_record(&story_path, &code_path, story_id, source_repo)
    else {
        return Err(
            "story workspace registration requires a durable team and source repository".into(),
        );
    };
    let setup_complete =
        register_story_workspace(&codesense_home, &Principal::from(user_id), record)?;
    crate::commands::managed_worktree_config::write(&code_path, &codesense_home);
    let _ = write_story_branch(app, &story_path, &code_path);

    // Publish registration readiness to the existing workspace roster.
    crate::commands::worktree::emit_worktree_changed(app);

    Ok(setup_complete)
}

/// Pure core of [`write_story_branch`] (unit-testable, no I/O): compute the
/// byte-preserving `branch` rewrite, returning `Some(updated)` only when it
/// actually changes the content and `None` when the branch is already recorded
/// (byte-identical) — the idempotency gate the caller skips the write on.
///
/// SW-OR-1b: targets the unified canonical `branch` key (was `dev_branch` under
/// SW-GS-35). Legacy `branch_name`/`dev_branch` lines in older docs are left
/// untouched — only the `branch` line is inserted/updated.
fn story_branch_rewrite(
    format: crate::commands::pipeline_format::PipelineFileFormat,
    content: &str,
    branch: &str,
) -> Option<String> {
    let updated =
        crate::commands::pipeline_format::rewrite_field(format, content, "branch", branch);
    (updated != content).then_some(updated)
}

/// SW-GS-35 / SW-OR-1b: write the worktree's current git branch into the story
/// doc's **canonical `branch`** frontmatter — byte-preservingly — so a *pulled*
/// story is self-describing and a reviewer sees the branch to check out without
/// asking the author. The value then rides the normal publish/pull path (it's
/// plain frontmatter). Shared by the establish/derive wave (its original SW-GS-35
/// caller) and the SW-OR-1b `open_workspace` reconcile hook — one stamp, one
/// field, everywhere.
///
/// Returns `Some(branch)` (the value written) when a write actually happened, and
/// `None` when nothing was written — the caller uses this to report an honest
/// reconcile result.
///
/// Three guarantees baked in:
/// - **Idempotent:** a byte-identical rewrite performs NO write — no mtime bump,
///   no spurious diff, no redundant sync wave on re-establish/re-open ⇒ `None`.
/// - **Fail-safe:** a detached HEAD / unreadable branch (`None`) leaves any prior
///   `branch` intact — a good value is never blanked ⇒ `None`.
/// - **Non-disruptive:** the write is watcher-suppressed (the story doc lives in
///   a watched content dir; an un-suppressed write would enqueue a redundant
///   team-wide fan-out). Best-effort: a read/write failure is logged, never fatal
///   to the caller (the branch stamp is an additive convenience).
pub(crate) fn write_story_branch(
    app: &tauri::AppHandle,
    story_path: &Path,
    code_path: &Path,
) -> Option<String> {
    // Fail-safe: only write when a branch is actually readable.
    let branch = crate::commands::worktree::current_branch_of_worktree(code_path)?;
    let format = crate::commands::pipeline_format::format_of(story_path);
    let content = match std::fs::read_to_string(story_path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                target: "content_engine::sync::wave",
                "branch write-back: read {} failed: {e}",
                story_path.display()
            );
            return None;
        }
    };
    // Idempotency: `None` when the branch is already recorded (byte-identical
    // rewrite) ⇒ no write at all.
    let updated = story_branch_rewrite(format, &content, &branch)?;
    let write = match app.try_state::<crate::commands::watch::WatcherState>() {
        Some(ws) => {
            let handle = ws.dispatcher.suppression_handle();
            write_tmp_fsync_rename_suppressed(story_path, updated.as_bytes(), &handle)
        }
        None => crate::content_engine::write_tmp_fsync_rename(story_path, updated.as_bytes()),
    };
    if let Err(e) = write {
        log::warn!(
            target: "content_engine::sync::wave",
            "branch write-back: write {} failed: {e}",
            story_path.display()
        );
        return None;
    }
    Some(branch)
}

/// The record repointed at a moved story's new location. Pure.
fn relinked_record(record: &WorktreeStoryRecord, new_story_path: &Path) -> WorktreeStoryRecord {
    let mut updated = record.clone();
    updated.story_source_path = new_story_path.to_path_buf();
    // Refresh the display-only cache; the sync input is always re-parsed.
    updated.intended_team = worktree_index::resolve_team(&updated);
    // SW-HV-7 (AC-7): a move/rename does NOT re-derive the closure, so the
    // derive-time team-policy provenance no longer describes the (possibly
    // now-different-team) worktree. Clear the flag so the overlay note can't
    // mislabel a relocated worktree under the destination team's policy; a later
    // reopen re-establishes it honestly via `apply_derive_policy_closure`.
    // (`hydration_policy` override and `hydrated_epics` still ride the clone —
    // that carry is SY-5/AC-6 behavior; only the provenance flag is invalidated.)
    updated.policy_auto_hydrated = false;
    updated
}

/// Parse `{home}/users/{uid}/…` → `(home, uid)`. The inverse of
/// `path_to_user_home`; `None` for a path with no numeric `/users/<uid>/`
/// segment (i.e. not under the codesense user tree). The uid parse reuses
/// `domain::user_path::extract_user_id` (same `/users/` marker, single
/// source of truth); `home` is the prefix before that marker. Pure.
fn user_home_and_id_from_path(path: &Path) -> Option<(PathBuf, u64)> {
    if let Some(location) =
        crate::context_repo::context_attachments::location_for_known_path(path).ok()?
    {
        return Some((location.home, location.principal.as_str().parse().ok()?));
    }
    let s = path.to_str()?;
    let home = PathBuf::from(&s[..s.rfind("/users/")?]);
    let user_id: u64 = crate::domain::user_path::extract_user_id(s)?.parse().ok()?;
    Some((home, user_id))
}

/// Extract the org slug (the `…/orgs/{org_slug}/{team}/{marker}/…` segment,
/// possibly multi-part like `owner/repo`) from a flat story path (SLW-IN-7).
/// Anchors on the first layer-marker after `/orgs/`; the org slug is everything
/// before the team segment that precedes the marker. `None` if the path isn't
/// an org-scoped content path. Pure — used to scope the move reverse-lookup to
/// one org (story ids are unique only within an org; the index is keyed per
/// user, across all orgs).
/// The `(codesense_home, user_id, org_slug)` context of a story path under
/// `{home}/users/{uid}/orgs/{org}/…`. Composes the two path parsers used
/// together at every worktree-linkage call site (establish, move relink,
/// rename rekey). `None` when the path isn't an org-scoped user content path.
fn user_org_context_from_path(path: &Path) -> Option<(PathBuf, u64, String)> {
    let (home, user_id) = user_home_and_id_from_path(path)?;
    let org = org_slug_from_path(path)?;
    Some((home, user_id, org))
}

fn org_slug_from_path(path: &Path) -> Option<String> {
    if let Some(location) =
        crate::context_repo::context_attachments::location_for_known_path(path).ok()?
    {
        return Some(location.attachment.slug);
    }
    use crate::context_repo::LAYER_MARKER_DIRS;
    let after_orgs = path.to_str()?.split_once("/orgs/")?.1;
    let segs: Vec<&str> = after_orgs.split('/').filter(|s| !s.is_empty()).collect();
    let marker_idx = segs.iter().position(|s| LAYER_MARKER_DIRS.contains(s))?;
    if marker_idx == 0 {
        return None; // marker with no team (and thus no org) before it
    }
    // The team is `segs[marker_idx - 1]`; the org slug is everything before it.
    let org = segs[..marker_idx - 1].join("/");
    (!org.is_empty()).then_some(org)
}

/// SW-TS-1 Task 5 core — repoint every worktree record working `story_id`
/// at the story's new location after the story file appears at
/// `new_story_path` (AC-3). Team-agnostic — fires regardless of which team is
/// currently active.
///
/// SW-XS-3 (F10): index bookkeeping only. A story moving between teams no
/// longer re-derives or re-syncs its worktree: nothing is torn down, nothing is
/// materialized, no lockfile changes. The record's `story_source_path` follows
/// the story so `hydrate_story` and the MCP story tools resolve it.
///
/// Reverse lookup is `find_by_story_id` (the index's one enumerate
/// surface); home + user id are parsed from `new_story_path`, which is
/// always under `{home}/users/{uid}/…`. Per-worktree failures are logged
/// and skipped — one bad record never blocks the others.
pub(crate) fn relink_worktrees_for_story(story_id: &str, new_story_path: &Path) {
    // Story ids are unique only WITHIN an org, but the index is keyed per
    // user (across all orgs), so `find_by_story_id` can return a same-id
    // record from a *different* org. Scope to the org the moved story lives
    // in, or we'd rewrite another org's record to point at this path.
    let Some((codesense_home, user_id, event_org)) = user_org_context_from_path(new_story_path)
    else {
        return;
    };
    let records = match worktree_index::find_by_story_id(
        &codesense_home,
        &Principal::from(user_id),
        story_id,
    ) {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                target: "content_engine::sync::wave",
                "worktree move reverse-lookup failed for story {}: {}", story_id, e
            );
            return;
        }
    };
    for record in records {
        // Cross-org guard: only relink worktrees whose story belongs to the
        // same org as the moved file (same story id ≠ same story).
        if record.org_slug != event_org {
            continue;
        }
        // Task 5.2: react only to the story *appearing at a new path*. An
        // in-place edit (the record already points here) is not a move.
        if record.story_source_path == new_story_path {
            continue;
        }
        // AC-8: never relink a torn-down worktree.
        match worktree_index::prune_if_stale(&codesense_home, &Principal::from(user_id), &record) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                log::warn!(
                    target: "content_engine::sync::wave",
                    "worktree prune check failed for {}: {}", record.code_repo_path.display(), e
                );
                continue;
            }
        }
        let updated = relinked_record(&record, new_story_path);
        if let Err(e) =
            worktree_index::write_record(&codesense_home, &Principal::from(user_id), &updated)
        {
            log::error!(
                target: "content_engine::sync::wave",
                "worktree index stale: relink of {} to {} failed: {}",
                updated.code_repo_path.display(), new_story_path.display(), e
            );
        }
    }
}

/// SLW-IN-9 AC-5 — reconcile a story_id RENAME. When the file at `path` now
/// declares `new_id` where it previously declared `old_id` (the id field was
/// edited in place; the file did NOT move), repoint the worktree record's
/// identity old→new. This is the ONE place `record.story_id` is mutated: a
/// move (`relinked_record`) preserves the id and rewrites the path; a rename
/// preserves the path and rewrites the id. Scoped to the event's org (ids are
/// unique per org; the index is keyed per user across orgs) and to the exact
/// renamed file (a same-id record at a *different* path is a distinct story —
/// a legacy pre-AC-1 collision — not this rename). Best-effort + logged: one
/// bad record never blocks the others. No re-derive/wave — a pure id rename
/// changes neither the team nor the on-disk location, so the materialization
/// is unaffected; only the identity linkage needs repointing. The FE viewer
/// state self-reconciles: `syncWorkspaceFrontmatter` matches the workspace by
/// its (unchanged) path and refreshes `ws.storyId` from the retitled tree ref.
pub(crate) fn rekey_worktrees_story_id(old_id: &str, new_id: &str, path: &Path) {
    let Some((codesense_home, user_id, event_org)) = user_org_context_from_path(path) else {
        return;
    };
    let records = match worktree_index::find_by_story_id(
        &codesense_home,
        &Principal::from(user_id),
        old_id,
    ) {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                target: "content_engine::sync::wave",
                "worktree rename reverse-lookup failed for story {} → {}: {}", old_id, new_id, e
            );
            return;
        }
    };
    for record in records {
        // Same id ≠ same story: only repoint records in THIS org whose stored
        // path is the exact file that was renamed.
        if record.org_slug != event_org || record.story_source_path != path {
            continue;
        }
        let mut updated = record.clone();
        updated.story_id = new_id.to_string();
        if let Err(e) =
            worktree_index::write_record(&codesense_home, &Principal::from(user_id), &updated)
        {
            log::error!(
                target: "content_engine::sync::wave",
                "worktree index stale: story_id rename {} → {} for {} failed to persist: {}",
                old_id, new_id, updated.code_repo_path.display(), e
            );
        }
    }
}

/// Read a path's `story_id` from the (already-refreshed) frontmatter
/// cache. Shared by both move-detection call sites — the org-root
/// move-watch here and `build_context_handler` — so "what story is this
/// file?" is resolved one way.
pub(crate) fn story_id_at_path(
    cache: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    path: &Path,
) -> Option<String> {
    // SLW-RI-3: read from an O(1) snapshot.
    crate::install::frontmatter_cache::FrontmatterCache::snapshot(cache)
        .entries
        .get(path)
        .and_then(|e| e.story_id().map(str::to_string))
}

/// Org-root move-watch entry (AC-3): a file changed under some
/// `_team/<team>/content/`. Resolve its `story_id` via the frontmatter
/// cache (refreshing the changed path) and relink any worktree record working
/// that story. The team-agnostic move-watch routes here for teams that
/// have no nearer content handler; the content handler composes the same
/// `relink_worktrees_for_story` call for the teams it already watches.
///
/// **Appearance-only.** Detection fires when a story *arrives* at a team
/// path (`old.is_none()`). A move *out* of all teams (a deletion at the
/// old path, with no arrival under any watched `_team/.../content/`) is not
/// torn down live here — that remediation rides SW-TS-3's §4.6 actual-vs-intent
/// reconciliation (the §4.4 launch reconcile only re-derives known worktrees),
/// consistent with AC-3 ("moving to a team's folder") and AC-5 (no team →
/// neutral).
/// Which team's content tree does an org-root move-watch event target, if any?
/// `None` (skip) when:
/// - the path is a transient atomic-temp sibling (SW-SC-11) — guarding here
///   keeps a temp from polluting the cache under its own key or triggering a
///   spurious `relink_worktrees_for_story`, symmetric with the active-team
///   `build_context_handler` entry guard (the other half of this SW-TS-1 move
///   detection);
/// - the path isn't the flat content layout (`_team/<team>/content/…`, owner/
///   repo empty) — the recursive `_team/` watch also sees install-dir and
///   legacy `_team/<team>/repos/<owner>/<repo>/…` events, which the cache
///   doesn't index for worktree moves.
///
/// Pure so the gate is unit-testable without a live `AppHandle`.
pub(crate) fn move_event_target_team(event_path: &Path) -> Option<String> {
    if crate::commands::watch::is_transient_editor_file(event_path) {
        return None;
    }
    let (team, owner, _repo) =
        crate::install::capture::parse_context_repo_parts(event_path.to_str()?)?;
    owner.is_empty().then_some(team)
}

pub(crate) fn handle_story_move_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event_path: &Path,
) {
    crate::context_query::note_path(app, event_path);
    let Some(team) = move_event_target_team(event_path) else {
        return;
    };
    let Some(cache) = app.try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
    else {
        return;
    };
    let (old, new) = crate::install::frontmatter_cache::FrontmatterCache::refresh_under_lock(
        cache.inner(),
        event_path,
        &team,
    );
    // Perf: a story *appearing* at this path (no prior cache entry) is the
    // move signal; an in-place edit (already cached here) needs no
    // reverse-lookup directory walk. The `story_source_path` equality guard
    // in `relink_worktrees_for_story` is the cold-cache backstop.
    if let Some(old_entry) = &old {
        // SLW-IN-9 AC-5: the path was already cached, so this is an in-place
        // edit — NOT a move. If the edit changed the `story_id` (same path,
        // different id), it is a RENAME: repoint the worktree record's identity
        // old→new. Any other in-place edit is a no-op here.
        if let (Some(old_id), Some(new_id)) = (
            old_entry.story_id(),
            new.as_ref().and_then(|e| e.story_id()),
        ) {
            if old_id != new_id {
                rekey_worktrees_story_id(old_id, new_id, event_path);
            }
        }
        return;
    }
    if let Some(story_id) = story_id_at_path(cache.inner(), event_path) {
        relink_worktrees_for_story(&story_id, event_path);
    }
}

// ---------------------------------------------------------------------------
// Canvas wireframe libraries (SW-SC-5, collapsed to one team tier by SW-SC-8)
//
// The shared canvas uses Excalidraw's native library panel for all
// management; CodeSense persists the whole panel to a single canonical file
// on the active team tier (`_team/{team}/libraries/team.excalidrawlib`, a
// sibling of `install/`, never materialized into source repos). These two
// commands — read + write — are the only seam the frontend canvas uses; no
// projection/wave machinery is involved.
// ---------------------------------------------------------------------------

/// App-relative path to the bundled starter pack (declared in
/// `tauri.conf.json` `bundle.resources`). Resolved at runtime via the
/// Tauri resource resolver with a dev-mode fallback to `src-tauri/`.
const STARTER_PACK_RESOURCE: &str = "resources/wireframes/wireframe-starter.excalidrawlib";
/// Re-seed marker — its presence means "already seeded once", so deleting
/// the seeded shapes never brings them back (delete-sticky). Git-synced with
/// the team bag, so the first member to open seeds for everyone.
const STARTER_PACK_MARKER: &str = ".seeded-wireframe-starter";

use super::paths::resolve_bundled_resource;

/// Seed the bundled starter pack into the active team's library bag, once.
/// Best-effort: a missing bundle, no active team, or a write failure is
/// logged and swallowed — seeding must never block the canvas open (AC-6).
fn seed_starter_pack_best_effort(app: &tauri::AppHandle, ctx: &ResolverContext) {
    // One writable tier now: the active team. The bag + marker are git-synced
    // via the context repo, so the first member to open seeds for everyone.
    let Some(team_libs) = team_libraries_dir(ctx) else {
        return; // no active team / unsafe context — nothing to seed into
    };
    // Cheap fast-path: skip the bundle resolve once the marker exists.
    if team_libs.join(STARTER_PACK_MARKER).exists() {
        return;
    }
    let Some(pack_path) = resolve_bundled_resource(app, STARTER_PACK_RESOURCE) else {
        log::warn!(
            target: "content_engine::commands",
            "canvas starter pack bundle not found at {STARTER_PACK_RESOURCE}; skipping seed"
        );
        return;
    };
    let bytes = match std::fs::read(&pack_path) {
        Ok(b) => b,
        Err(e) => {
            log::warn!(target: "content_engine::commands", "reading starter pack failed: {e}");
            return;
        }
    };
    match seed_library_if_absent(&team_libs, STARTER_PACK_MARKER, &bytes) {
        Ok(true) => log::info!(
            target: "content_engine::commands",
            "seeded canvas starter pack into {}", team_libs.display()
        ),
        Ok(false) => {}
        Err(e) => log::warn!(
            target: "content_engine::commands",
            "seeding canvas starter pack failed: {e}"
        ),
    }
}

/// This org's scratch-space `libraries/` dir — the personal canvas overlay target
/// (AC-4). Scoped by the context's org via the scratch resolver, so another org's
/// private stencils never overlay this one. `None` when this org has no scratch space
/// (exists-gated) — callers then fall back to team-only behavior. Also `None` if it
/// would collapse onto the team dir itself (defensive: splitting the write against the
/// file the read uses as its base would erase every stencil but the delta, AC-7).
fn personal_overlay_dir(ctx: &ResolverContext, team_dir: &Path) -> Option<PathBuf> {
    let dir = crate::context_repo::scratch_space::org_space(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?
    .libraries_dir();
    (dir != team_dir).then_some(dir)
}

/// Tauri params shared by the canvas-library commands. Repo-agnostic — no
/// `code_repo_slug` (libraries ride the team workspace tier only).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasLibraryContext {
    pub codesense_home: String,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
}

impl CanvasLibraryContext {
    fn into_ctx(self) -> Result<ResolverContext, Error> {
        derive_resolver_context(
            PathBuf::from(self.codesense_home),
            self.user_id,
            self.org_slug,
            self.team_name,
        )
    }
}

/// Read the active team's canonical library bag (the whole v2
/// `.excalidrawlib`), seeding the bundled starter into it on first open
/// (idempotent, delete-sticky), then overlay your personal library on top
/// (AC-1): team stencils PLUS your private ones, with the **team bag
/// authoritative** — personal stencils are additive and never shadow a team
/// `id` (the exact inverse of the AC-2 write-split). `None` when there's no
/// active team or neither bag exists — the canvas injects nothing then. No
/// scratch space for this org → today's team bag, byte-identical.
#[tauri::command]
pub async fn read_canvas_library(
    app: tauri::AppHandle,
    params: CanvasLibraryContext,
) -> Result<Option<String>, Error> {
    let ctx = params.into_ctx()?;
    seed_starter_pack_best_effort(&app, &ctx);
    let team = read_team_library(&ctx);
    let personal = team_libraries_dir(&ctx)
        .and_then(|team_dir| personal_overlay_dir(&ctx, &team_dir))
        .and_then(|dir| read_library_at(&dir));
    Ok(read_overlay(team, personal))
}

/// Tauri params for [`write_canvas_library`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteCanvasLibraryParams {
    /// The entire library serialized as a v2 `.excalidrawlib` envelope.
    pub content: String,
    #[serde(flatten)]
    pub context: CanvasLibraryContext,
}

/// Persist the library bag for the active team (SW-SC-8 D3), split-aware.
///
/// When this org has a scratch space, the panel's whole bag is **split**: only
/// the delta (items whose `id` isn't in the shared team base) is written to your
/// scratch `libraries/` bag; the team bag is left
/// untouched (AC-2/AC-5), so your private stencils never leak into the shared,
/// git-synced team file. Otherwise it's today's whole-bag atomic write to the
/// team's unwatched `libraries/` dir (created on first write). An empty bag is a
/// valid write — a native per-item delete must stick, not be re-seeded.
#[tauri::command]
pub async fn write_canvas_library(params: WriteCanvasLibraryParams) -> Result<(), Error> {
    let WriteCanvasLibraryParams { content, context } = params;
    let ctx = context.into_ctx()?;
    let team_dir = team_libraries_dir(&ctx).ok_or(Error::LayerUnbacked(LayerId::TeamProjection))?;

    match personal_overlay_dir(&ctx, &team_dir) {
        Some(personal_dir) => {
            let team_base = read_library_at(&team_dir);
            let delta =
                bag_delta(&content, team_base.as_deref()).map_err(engine_error_to_command_error)?;
            write_team_library_at(&personal_dir, &delta).map_err(engine_error_to_command_error)?;
        }
        None => {
            write_team_library_at(&team_dir, &content).map_err(engine_error_to_command_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod canvas_library_command_tests {
    use super::*;

    fn params_for(
        home: &str,
        org: &str,
        team: Option<&str>,
        content: &str,
    ) -> WriteCanvasLibraryParams {
        WriteCanvasLibraryParams {
            content: content.to_string(),
            context: CanvasLibraryContext {
                codesense_home: home.to_string(),
                user_id: 42,
                org_slug: org.to_string(),
                team_name: team.map(String::from),
            },
        }
    }

    fn params(home: &str, team: Option<&str>, content: &str) -> WriteCanvasLibraryParams {
        params_for(home, "acme/api", team, content)
    }

    /// The overlay is `exists()`-gated on the org's scratch space, so a test must
    /// create it to exercise the overlay. Returns its `libraries/` dir.
    fn provision_personal_team(home: &Path) -> PathBuf {
        crate::context_repo::scratch_space::ensure_org_space(
            home,
            &Principal::from(42u64),
            "acme/api",
        )
        .unwrap()
        .0
        .libraries_dir()
    }

    fn resolver_ctx(home: &Path, org: &str, team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: org.to_string(),
            team_name: team.map(String::from),
        }
    }

    fn item_ids(bag: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(bag).unwrap();
        v["libraryItems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn write_persists_the_whole_bag_to_the_team_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let bag = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"a"}]}"#;

        write_canvas_library(params(&home, Some("default"), bag))
            .await
            .unwrap();

        let dest = tmp
            .path()
            .join("users/42/orgs/acme/api/default/libraries/team.excalidrawlib");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), bag);
    }

    #[tokio::test]
    async fn write_accepts_an_empty_bag_so_a_native_delete_sticks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let empty = r#"{"type":"excalidrawlib","version":2,"libraryItems":[]}"#;
        // Unlike the old add command, an empty bag is a VALID write — deleting
        // the last item must persist, not be rejected.
        write_canvas_library(params(&home, Some("default"), empty))
            .await
            .unwrap();
        let dest = tmp
            .path()
            .join("users/42/orgs/acme/api/default/libraries/team.excalidrawlib");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), empty);
    }

    #[tokio::test]
    async fn write_with_no_active_team_is_layer_unbacked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        assert!(matches!(
            write_canvas_library(params(&home, None, "{}")).await,
            Err(Error::LayerUnbacked(LayerId::TeamProjection))
        ));
    }

    // ---- personal overlay write-split (AC-2/AC-5) ----

    #[tokio::test]
    async fn write_splits_delta_to_personal_leaving_team_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let personal_libs = provision_personal_team(tmp.path());

        // Seed the shared team base with one stencil.
        let team_libs = tmp.path().join("users/42/orgs/acme/api/default/libraries");
        std::fs::create_dir_all(&team_libs).unwrap();
        let team_bag = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1"}]}"#;
        std::fs::write(team_libs.join("team.excalidrawlib"), team_bag).unwrap();

        // The panel serializes the WHOLE merged bag (team + a new personal item).
        let whole =
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1"},{"id":"p1"}]}"#;
        write_canvas_library(params(&home, Some("default"), whole))
            .await
            .unwrap();

        // Team bag byte-identical; personal bag holds only the delta.
        assert_eq!(
            std::fs::read_to_string(team_libs.join("team.excalidrawlib")).unwrap(),
            team_bag,
        );
        assert_eq!(
            item_ids(&std::fs::read_to_string(personal_libs.join("team.excalidrawlib")).unwrap()),
            ["p1"],
        );
    }

    #[tokio::test]
    async fn another_orgs_scratch_stencils_never_overlay_or_receive_writes() {
        // Org A has a scratch space; working in org B is team-only both ways.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let a_libs = provision_personal_team(tmp.path());
        std::fs::create_dir_all(&a_libs).unwrap();
        std::fs::write(
            a_libs.join("team.excalidrawlib"),
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"private"}]}"#,
        )
        .unwrap();

        let whole = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"b"}]}"#;
        write_canvas_library(params_for(&home, "other/org", Some("default"), whole))
            .await
            .unwrap();

        let ctx = resolver_ctx(tmp.path(), "other/org", Some("default"));
        let team_dir = team_libraries_dir(&ctx).unwrap();
        assert!(personal_overlay_dir(&ctx, &team_dir).is_none());
        assert_eq!(
            item_ids(&std::fs::read_to_string(team_dir.join("team.excalidrawlib")).unwrap()),
            ["b"],
            "org B's whole bag lands in its team file"
        );
        assert_eq!(
            item_ids(&std::fs::read_to_string(a_libs.join("team.excalidrawlib")).unwrap()),
            ["private"],
            "org A's scratch stencils are untouched"
        );
    }

    #[tokio::test]
    async fn write_malformed_content_aborts_touching_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let personal_libs = provision_personal_team(tmp.path());
        assert!(
            write_canvas_library(params(&home, Some("default"), "not json"))
                .await
                .is_err()
        );
        assert!(!personal_libs.join("team.excalidrawlib").exists());
    }

    // ---- personal_overlay_dir resolution + AC-7 collapse guard ----

    #[test]
    fn personal_overlay_dir_some_when_provisioned_and_distinct() {
        let tmp = tempfile::TempDir::new().unwrap();
        provision_personal_team(tmp.path());
        let ctx = resolver_ctx(tmp.path(), "acme/api", Some("default"));
        let team_dir = team_libraries_dir(&ctx).unwrap();
        let dir = personal_overlay_dir(&ctx, &team_dir).unwrap();
        assert_ne!(dir, team_dir);
        assert!(dir.ends_with("scratch/orgs/acme/api/libraries"));
    }

    #[test]
    fn personal_overlay_dir_none_when_no_personal_team() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = resolver_ctx(tmp.path(), "acme/api", Some("default"));
        let team_dir = team_libraries_dir(&ctx).unwrap();
        assert!(personal_overlay_dir(&ctx, &team_dir).is_none());
    }

    #[test]
    fn personal_overlay_dir_is_scoped_to_the_contexts_org() {
        let tmp = tempfile::TempDir::new().unwrap();
        provision_personal_team(tmp.path());
        let ctx = resolver_ctx(tmp.path(), "other/org", Some("default"));
        let team_dir = team_libraries_dir(&ctx).unwrap();
        assert!(personal_overlay_dir(&ctx, &team_dir).is_none());
    }
}

// ---------------------------------------------------------------------------
// SW-CP-29: read-only, repo-scoped deep-merge preview for merge-marked files
// ---------------------------------------------------------------------------

/// SW-CP-29: one attached code repo a merge-marked file can be previewed
/// against — the selector's option shape. `code_repo_path` is the on-disk
/// target the wave actually iterates (the base the merge reads). `camelCase`
/// so the FE reads `codeRepoPath` (matches every sibling command struct here).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePreviewRepo {
    pub owner: String,
    pub repo: String,
    pub code_repo_path: String,
}

/// SW-CP-29: list the attached code repos the active team projects into — the
/// candidates for the merge-preview repo selector. Reuses the SAME enumeration
/// the wave fans out over (`enumerate_attached_code_repos_for_team`), so the
/// selector can never offer a target the wave wouldn't write. Read-only; runs on
/// the blocking pool so directory I/O never touches the UI thread.
#[tauri::command]
pub async fn list_merge_preview_repos(
    codesense_home: PathBuf,
    user_id: u64,
    org_slug: String,
    team: String,
) -> Result<Vec<MergePreviewRepo>, String> {
    crate::commands::await_blocking("list_merge_preview_repos", move || {
        let ctx = derive_resolver_context(codesense_home, user_id, org_slug, Some(team))
            .map_err(|e| e.to_string())?;
        let slugs = configured_team_source_repos(&ctx);
        let attached =
            enumerate_attached_code_repos_for_team(&ctx, &slugs).map_err(|e| e.to_string())?;
        Ok(attached
            .into_iter()
            .map(|r| MergePreviewRepo {
                owner: r.slug.owner,
                repo: r.slug.repo,
                code_repo_path: r.code_repo_path.to_string_lossy().into_owned(),
            })
            .collect())
    })
    .await
}

/// What a merge-marked file's preview shows: the merged text a launch in
/// that repo would write, and what Step 2 would do with it there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePreview {
    /// The launch's merged file (its `GeneratedEntry`, serialized as Step 2
    /// writes it).
    pub text: String,
    /// `write` (absent), `unchanged`, `replace` (an unmodified copy CodeSense
    /// wrote), `keepEdited`, `keepUnmanaged` (a file CodeSense didn't write)
    /// or `tracked` (the repo tracks it; never written).
    pub outcome: String,
    /// The plain-language line for `outcome`.
    pub note: String,
}

/// SW-CP-29 / SW-XS-8b (review r1): the read-only, repo-scoped preview for a
/// merge-marked file — exactly the merged file a launch in `code_repo_path`
/// writes (the same `GeneratedEntry` Step 2 gets, merged across the content
/// layers), and the managed-record outcome there (write, replace, keep).
/// Strictly read/compute; runs on the blocking pool.
///
/// Rejects an un-marked path (defensive — the viewer only offers the preview for
/// marked files, but the backend never previews an unmarked file).
#[tauri::command]
pub async fn preview_merge_marked_file(
    codesense_home: PathBuf,
    user_id: u64,
    org_slug: String,
    team: String,
    logical_path: String,
    code_repo_path: PathBuf,
) -> Result<MergePreview, String> {
    crate::commands::await_blocking("preview_merge_marked_file", move || {
        if !is_safe_logical_name(&logical_path) {
            return Err(format!("unsafe logical path: {logical_path}"));
        }
        let ctx = derive_resolver_context(codesense_home, user_id, org_slug, Some(team))
            .map_err(|e| e.to_string())?;
        // Only a repo this team is attached to (the selector's own set).
        let attached =
            enumerate_attached_code_repos_for_team(&ctx, &configured_team_source_repos(&ctx))
                .map_err(|e| e.to_string())?;
        let Some(matched) = attached.iter().find(|r| r.code_repo_path == code_repo_path) else {
            return Err("code_repo_path is not an attached repo for this team".to_string());
        };
        if !code_repo_path.is_dir() {
            return Err("attached repo directory is not present (moved or unmounted)".to_string());
        }
        merge_preview_at(&ctx, &matched.slug, &code_repo_path, &logical_path)
    })
    .await
}

/// [`preview_merge_marked_file`]'s core (tests call it directly).
pub(crate) fn merge_preview_at(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    code_repo_path: &Path,
    logical_path: &str,
) -> Result<MergePreview, String> {
    use crate::shared::config_format::ConfigFormat;
    let target = crate::agents::ManifestTarget {
        context_root: None,
        org: ctx.org_slug.clone(),
        team: ctx.team_name.clone().unwrap_or_default(),
        repo: Some(slug.clone()),
    };
    let entry = crate::agents::manifest_resolve::merged_workspace_file(
        &ctx.codesense_home,
        &ctx.principal(),
        &target,
        logical_path,
    )?;
    let format = ConfigFormat::detect(Path::new(logical_path))
        .ok_or("merge-marked, but not a JSON, YAML or TOML file")?;
    let bytes = format
        .serialize(&entry.content)
        .map_err(|e| format!("cannot write the merge: {e}"))?;
    let text =
        String::from_utf8(bytes.clone()).map_err(|e| format!("merged output is not UTF-8: {e}"))?;
    let root = code_repo_path
        .canonicalize()
        .unwrap_or_else(|_| code_repo_path.to_path_buf());
    let identity = crate::content_engine::context_lifecycle::local_identity(&root);
    let record = crate::agents::prepare::ManagedRecord::load(
        &ctx.codesense_home,
        &ctx.principal(),
        &identity,
    );
    let want = *blake3::hash(&bytes).as_bytes();
    let tracked = crate::agents::prepare::tracked_in(&root, logical_path);
    let dest = root.join(logical_path);
    let (outcome, note) =
        if tracked {
            (
                "tracked",
                "The repo tracks this file, so a launch never writes it.",
            )
        } else {
            match crate::content_engine::atomic_io::hash_file(&dest) {
            Err(_) => ("write", "A launch writes this merged file here (it's absent)."),
            Ok(hash) if hash == want => ("unchanged", "The workspace already has exactly this."),
            Ok(hash) => match record.entries.get(logical_path) {
                Some(managed) if managed.hash.0 == hash => (
                    "replace",
                    "A launch replaces the copy CodeSense wrote here (nobody edited it).",
                ),
                Some(_) => (
                    "keepEdited",
                    "The copy here was edited, so a launch keeps it and reports it.",
                ),
                // An older build's merged copy, not yet folded into the
                // record: the launch folds it first, so it is managed.
                None if crate::agents::prepare::record::interim_hash(
                    &ctx.codesense_home,
                    &root,
                    logical_path,
                ) == Some(hash) =>
                {
                    (
                        "replace",
                        "A launch replaces the copy CodeSense wrote here (nobody edited it).",
                    )
                }
                None => (
                    "keepUnmanaged",
                    "CodeSense didn't write the file here, so a launch keeps it and reports it.",
                ),
            },
        }
        };
    Ok(MergePreview {
        text,
        outcome: outcome.into(),
        note: note.into(),
    })
}

// ---------------------------------------------------------------------------
// Tests (AC-8)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DomainError;
    use std::fs;
    use tempfile::TempDir;

    /// Claude's status hook command.
    fn notify_command() -> &'static str {
        &project_hooks(AgentCli::Claude).unwrap().status_command
    }

    #[test]
    fn explicit_story_registration_alias_and_team_move_preserve_one_owner() {
        let home = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let root = roots.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let alias = roots.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let principal = Principal::from(42);
        let mut record = WorktreeStoryRecord::new(
            &alias,
            "acme/context",
            SourceRepoSlug::new("acme", "repo").unwrap(),
            "S-1",
            home.path()
                .join("users/42/orgs/acme/context/old/content/S-1.md"),
        );
        record.hydrated_epics.push(home.path().join("kept-receipt"));
        worktree_index::write_record(home.path(), &principal, &record).unwrap();
        let mut moved = record.clone();
        moved.code_repo_path = root.clone();
        moved.story_source_path = home
            .path()
            .join("users/42/orgs/acme/context/new/content/S-1.md");
        assert!(
            register_story_workspace(home.path(), &principal, moved).unwrap(),
            "legacy complete record stays complete"
        );
        let registrations = worktree_index::story_registrations(home.path(), &root).unwrap();
        assert_eq!(registrations.len(), 1);
        assert!(worktree_index::read_record(home.path(), &principal, &root)
            .unwrap()
            .is_some());
        assert!(worktree_index::read_record(home.path(), &principal, &alias)
            .unwrap()
            .is_some());
        worktree_index::update_record_with(home.path(), &principal, &root, |record| {
            let mut record = record.unwrap();
            record.setup_complete = false;
            Ok(record)
        })
        .unwrap();
        assert!(
            !worktree_index::read_record(home.path(), &principal, &alias)
                .unwrap()
                .unwrap()
                .setup_complete
        );
        assert_eq!(
            worktree_index::resolve_team(&registrations[0].1).as_deref(),
            Some("new")
        );
        assert_eq!(registrations[0].1.hydrated_epics, record.hydrated_epics);
        let mut conflicting = record;
        conflicting.story_id = "S-2".into();
        assert!(register_story_workspace(home.path(), &principal, conflicting).is_err());
    }

    #[test]
    fn explicit_story_registration_and_reopen_preserve_receipts_without_hydration() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let mut record = WorktreeStoryRecord::new(
            root.path(),
            "acme/context",
            SourceRepoSlug::new("acme", "repo").unwrap(),
            "S-1",
            home.path()
                .join("users/42/orgs/acme/context/team/content/S-1.md"),
        );
        record.setup_complete = false;
        record
            .hydrated_epics
            .push(home.path().join("previous-epic"));
        record.failure_reason = Some("historical receipt".into());
        std::fs::write(root.path().join("edited.md"), "unchanged").unwrap();
        register_story_workspace(home.path(), &principal, record.clone()).unwrap();
        let before = worktree_index::read_record(home.path(), &principal, root.path())
            .unwrap()
            .unwrap();
        let fresh = WorktreeStoryRecord::new(
            root.path(),
            "acme/context",
            record.source_repo,
            "S-1",
            record.story_source_path,
        );
        register_story_workspace(home.path(), &principal, fresh).unwrap();
        let after = worktree_index::read_record(home.path(), &principal, root.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("edited.md")).unwrap(),
            "unchanged"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn delayed_old_profile_team_activation_is_rejected_before_mutation() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let profile = crate::commands::local_profile::LocalProfileState::load(home.path()).unwrap();
        profile.select("41", None).unwrap();
        validate_team_activation_profile(&profile, 41).unwrap();
        profile.select("42", None).unwrap();
        assert!(validate_team_activation_profile(&profile, 41).is_err());
        assert!(validate_team_activation_profile(&profile, 0).is_err());
        validate_team_activation_profile(&profile, 42).unwrap();
    }

    // ---- SW-GS-35 / SW-OR-1b: `branch` write-back idempotency core ---------

    #[test]
    fn story_branch_rewrite_inserts_then_is_idempotent() {
        use crate::commands::pipeline_format::PipelineFileFormat;
        let md = "---\nstatus: todo\nsource_repo: org/repo\n---\n# Title\n";
        // First write inserts → Some(updated).
        let updated = story_branch_rewrite(PipelineFileFormat::Markdown, md, "feat/SW-OR-1b-foo")
            .expect("insert changes content");
        assert!(updated.contains("branch: feat/SW-OR-1b-foo"));
        // Idempotent: same value on the updated doc → None (no write).
        assert!(
            story_branch_rewrite(PipelineFileFormat::Markdown, &updated, "feat/SW-OR-1b-foo")
                .is_none(),
            "same-value rewrite must be a no-op"
        );
    }

    #[test]
    fn story_branch_rewrite_updates_on_branch_change() {
        use crate::commands::pipeline_format::PipelineFileFormat;
        // A different branch is a real change → Some(updated), stale gone.
        let md = "---\nstatus: todo\nbranch: feat/old\n---\n# Title\n";
        let updated = story_branch_rewrite(PipelineFileFormat::Markdown, md, "fix/new")
            .expect("changed branch rewrites");
        assert!(updated.contains("fix/new"));
        assert!(!updated.contains("feat/old"), "stale branch removed");
    }

    #[test]
    fn story_branch_rewrite_leaves_legacy_fields_untouched() {
        use crate::commands::pipeline_format::PipelineFileFormat;
        // SW-OR-1b: the stamp writes ONLY the canonical `branch` line. Legacy
        // `branch_name`/`dev_branch` in an older doc are left verbatim (additive,
        // no migration) — readers prefer `branch` via `story_branch()`.
        let md = "---\nstatus: todo\nbranch_name: feat/legacy-intended\ndev_branch: feat/legacy-observed\n---\n# Title\n";
        let updated = story_branch_rewrite(PipelineFileFormat::Markdown, md, "feat/new-observed")
            .expect("inserting `branch` changes content");
        // `branch` inserted with the live/observed value...
        assert!(updated.contains("branch: feat/new-observed"));
        // ...and BOTH legacy fields survive verbatim (not touched, not migrated).
        assert!(
            updated.contains("branch_name: feat/legacy-intended"),
            "legacy branch_name left untouched"
        );
        assert!(
            updated.contains("dev_branch: feat/legacy-observed"),
            "legacy dev_branch left untouched"
        );
    }

    // ---- SLW-CO-8: epic dev-env parallel fan-out + terminal completion ----

    #[derive(Default)]
    struct RecordingSink(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl crate::commands::worktree::EventSink for RecordingSink {
        fn emit(&self, channel: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((channel.to_string(), payload));
        }
    }
    impl RecordingSink {
        fn events(&self) -> Vec<(String, serde_json::Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    #[test]
    fn slw_co8_terminal_completion_maps_ok_failure_and_refusal() {
        use crate::commands::worktree::RunResult;
        let sink = RecordingSink::default();
        // Clean zero-exit install.
        emit_worktree_run_completed(
            &sink,
            "r-ok",
            &Ok(RunResult {
                results: vec![],
                exit_code: 0,
            }),
        );
        // A failed `npm ci` returns Ok with a non-zero exit today — must be a
        // FAILURE, not a success (AC-4).
        emit_worktree_run_completed(
            &sink,
            "r-nonzero",
            &Ok(RunResult {
                results: vec![],
                exit_code: 1,
            }),
        );
        // A provenance refusal / config error is an Err — failure + surfaced reason.
        emit_worktree_run_completed(&sink, "r-err", &Err("NEEDS_APPROVAL:abc".to_string()));

        let ev = sink.events();
        assert_eq!(ev.len(), 3);
        assert!(ev.iter().all(|(ch, _)| ch == WORKTREE_RUN_COMPLETED_EVENT));
        assert_eq!(ev[0].1["run_id"], "r-ok");
        assert_eq!(ev[0].1["ok"], true);
        assert_eq!(ev[0].1["exit_code"], 0);
        assert!(ev[0].1["error"].is_null());
        assert_eq!(ev[1].1["ok"], false);
        assert_eq!(ev[1].1["exit_code"], 1);
        assert_eq!(ev[2].1["ok"], false);
        assert_eq!(ev[2].1["error"], "NEEDS_APPROVAL:abc");
    }

    #[tokio::test]
    async fn slw_co8_dev_env_fanout_completes_every_run_incl_empty_config() {
        // Empty/absent worktree.yaml → run_setup_for_worktree falls back to an
        // empty config → no-op Ok(exit 0). No git harness needed. The fan-out must
        // still emit exactly one terminal completion PER run so the overlay counts
        // M-of-M correctly (AC-6 empty-config still counted).
        let tmp = TempDir::new().unwrap();
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&org_root).unwrap();
        let mut setups = Vec::new();
        for i in 0..3 {
            let wt = tmp.path().join(format!("wt{i}"));
            let clone = tmp.path().join(format!("clone{i}"));
            fs::create_dir_all(&wt).unwrap();
            fs::create_dir_all(&clone).unwrap();
            setups.push((format!("epic-e-o{i}-r{i}"), wt, clone));
        }
        let sink = Arc::new(RecordingSink::default());
        let registry = crate::commands::worktree::WorktreeRunRegistry::new();
        let authority = crate::commands::worktree::WorktreeConfigAuthority {
            codesense_home: tmp.path().join("home"),
            principal: Principal::from(7u64),
            team: "default".to_owned(),
        };
        run_epic_dev_env_setups(sink.clone(), registry, org_root, authority, setups).await;

        let completed: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|(ch, _)| ch == WORKTREE_RUN_COMPLETED_EVENT)
            .collect();
        assert_eq!(completed.len(), 3, "one terminal completion per run");
        assert!(
            completed.iter().all(|(_, p)| p["ok"] == true),
            "empty-config no-op is a success"
        );
        let ids: std::collections::HashSet<String> = completed
            .iter()
            .map(|(_, p)| p["run_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 3, "each run_id reported exactly once");
    }

    #[test]
    fn slw_co8_completion_guard_emits_failure_on_armed_drop() {
        // Simulates the panic path: the guard drops while still armed (the real
        // emit was never reached) → a synthetic failure completion fires so the
        // FE run always terminalizes.
        let sink = RecordingSink::default();
        {
            let _g = RunCompletionGuard::new(&sink, "r-panic");
        }
        let ev = sink.events();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, WORKTREE_RUN_COMPLETED_EVENT);
        assert_eq!(ev[0].1["run_id"], "r-panic");
        assert_eq!(ev[0].1["ok"], false);

        // Disarmed (normal path) → the guard stays silent; the real emit owns it.
        let sink2 = RecordingSink::default();
        {
            let mut g = RunCompletionGuard::new(&sink2, "r-ok");
            g.disarm();
        }
        assert!(sink2.events().is_empty());
    }

    #[tokio::test]
    async fn slw_co8_dev_env_fanout_empty_setups_is_noop() {
        let tmp = TempDir::new().unwrap();
        let sink = Arc::new(RecordingSink::default());
        let registry = crate::commands::worktree::WorktreeRunRegistry::new();
        let authority = crate::commands::worktree::WorktreeConfigAuthority {
            codesense_home: tmp.path().join("home"),
            principal: Principal::from(7u64),
            team: "default".to_owned(),
        };
        run_epic_dev_env_setups(
            sink.clone(),
            registry,
            tmp.path().to_path_buf(),
            authority,
            Vec::new(),
        )
        .await;
        assert!(sink.events().is_empty());
    }

    // Most tests exercise the pure inner logic by calling private helpers
    // directly (derive_resolver_context, content_layer_root resolution,
    // composite assembly) plus the read/write *Outcome dispatch via the
    // command bodies' `async fn` paths. Tauri's `app: AppHandle` and the
    // watcher dispatcher are required for the two write commands; those
    // helpers are tested in a dedicated block that bypasses the
    // `#[tauri::command]` boundary by extracting the locked rename / write
    // sequence into a free function — see `write_path_writes_atomically`
    // and `move_path_returns_moved`.
    //
    // The full Tauri-wired surface (with AppHandle + state) is covered by
    // the SS-13 e2e contract tests Phase 2 (Task 4.4).

    /// SW-SC-11: the org-root move-watch must skip transient atomic-temp
    /// siblings — same guarantee `build_context_handler` gives the active-team
    /// half of this SW-TS-1 move detection. A `*.tmp.<pid>.<hash>` / `*.tmp.<ext>`
    /// / swap/backup path under a team's content dir must NOT resolve to a team
    /// (so `handle_story_move_event` returns before the cache refresh + re-derive),
    /// while a real story path still does.
    #[test]
    fn move_event_target_team_skips_transient_temps() {
        // Real content stories → resolve to their team.
        assert_eq!(
            move_event_target_team(Path::new("/org/default/content/story.md")).as_deref(),
            Some("default"),
        );
        assert_eq!(
            move_event_target_team(Path::new("/org/alpha/content/canvas.excalidraw")).as_deref(),
            Some("alpha"),
        );

        // Transient siblings under a content dir → None (the fix).
        for transient in [
            "/org/default/content/story.md.tmp.99999.deadbeef",
            "/org/default/content/story.tmp.md",
            "/org/default/content/canvas.excalidraw.tmp.11024.beadfeed",
            "/org/default/content/story.md~",
            "/org/default/content/.story.md.swp",
            "/org/default/content/.#story.md",
        ] {
            assert_eq!(
                move_event_target_team(Path::new(transient)),
                None,
                "{transient} must be skipped by the move-watch guard",
            );
        }

        // Non-content layouts stay skipped (legacy repos / install) — owner set.
        assert_eq!(
            move_event_target_team(Path::new("/org/default/repos/owner/repo/install/x.md")),
            None,
        );
    }

    #[test]
    fn epic_workspace_path_is_safe_none_through_move_watch() {
        // SW-EW-7 AC-4: an `_epic/` workspace path has NO `_team` segment, so the
        // watcher's team-resolution (`parse_context_repo_parts`) returns None and
        // the org-root move-watch safely skips it — no crash, and NOT misrouted as
        // flat content (the `_team` check fails before any flat-vs-legacy branch).
        // The watcher resolves team from the paired stored `context_repo_path`,
        // never from an epic path; the agent-write round-trip needs a reverse
        // watch, which is EW-8's job. This pins "no parser change needed".
        // SW-CP-15 Task 10: epic workspaces root under the per-user home, outside
        // any context-repo clone — still `_team`-free, so still None.
        assert_eq!(
            move_event_target_team(Path::new(
                "/home/dev/.codesense/users/42/_epic/acme/codesense-context/SW-EW/owner/repo/_bmad-output/story.md"
            )),
            None,
        );
    }

    fn ctx(tmp: &Path, team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: tmp.to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: team.map(String::from),
        }
    }

    fn slug() -> SourceRepoSlug {
        SourceRepoSlug::new("alderco", "codesense").unwrap()
    }

    // ----- SW-TS-1: worktree derive caller (pure) -------------------------

    const WT_HOME: &str = "/home/dev/.codesense";

    fn wt_record(code_repo_path: &str, story_source_path: &str) -> WorktreeStoryRecord {
        WorktreeStoryRecord::new(
            PathBuf::from(code_repo_path),
            // org_slug is `<owner>/<repo>` form (validated by derive_resolver_context).
            "acme/codesense-context",
            SourceRepoSlug::new("acme", "codesense").unwrap(),
            "SW-TS-1",
            PathBuf::from(story_source_path),
        )
    }

    fn story_at(team: &str) -> String {
        format!("{WT_HOME}/users/42/orgs/acme/codesense-context/{team}/content/impl/SW-TS-1.md")
    }

    // ----- SW-TS-3 (AC-4b): actual-vs-intent teardown decision -----------

    // ----- SW-EW-7 (T1): shared projection-plan builder ------------------

    // ----- SW-EW-7 (T3): epic inheritance wave (both engines, pure) -------

    // ----- SW-EW-7 (T4 / D1): epic-slug auto-merge into the team fan-out ----

    #[test]
    fn relinked_record_updates_path_and_keeps_identity() {
        use crate::install::hydration_closure::{HydrationPolicy, LIGHT};
        let mut rec = wt_record("/abs/wt", &story_at("alpha"));
        // A prior team-default derive had set the provenance flag; and the
        // worktree carries a per-worktree override.
        rec.policy_auto_hydrated = true;
        rec.hydration_policy = Some(HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        });
        let new_path = story_at("beta");
        let updated = relinked_record(&rec, Path::new(&new_path));

        assert_eq!(updated.story_source_path, PathBuf::from(&new_path));
        assert_eq!(updated.intended_team, Some("beta".to_string())); // refreshed cache
                                                                     // Identity fields preserved across the move.
        assert_eq!(updated.code_repo_path, rec.code_repo_path);
        assert_eq!(updated.source_repo, rec.source_repo);
        assert_eq!(updated.story_id, rec.story_id);
        // SW-HV-7 (AC-7): the move invalidates derive-time team-policy provenance
        // (not recomputed for the destination team) → flag cleared, so the note
        // can't mislabel a relocated worktree. The override still rides the clone.
        assert!(
            !updated.policy_auto_hydrated,
            "move clears the derive-provenance flag"
        );
        assert_eq!(
            updated.hydration_policy,
            Some(HydrationPolicy {
                reference_depth: 1,
                ..LIGHT
            }),
            "override still carried across the move (AC-6)"
        );
    }

    /// SLW-IN-9 AC-5: a story_id rename repoints ONLY the record for the exact
    /// renamed file, in the event's org. A same-id record at a different path
    /// (a distinct story) and a same-id record in another org are untouched.
    #[test]
    fn rekey_repoints_only_the_renamed_record_in_this_org() {
        let home = TempDir::new().unwrap();
        let h = home.path();
        let seed = |code_repo: &str, org: &str, id: &str, story_path: &str| {
            let rec = WorktreeStoryRecord::new(
                PathBuf::from(code_repo),
                org,
                SourceRepoSlug::new("acme", "codesense").unwrap(),
                id,
                PathBuf::from(story_path),
            );
            worktree_index::write_record(h, &Principal::from(42u64), &rec).unwrap();
        };
        let renamed = format!(
            "{}/users/42/orgs/acme/codesense-context/alpha/content/impl/OLD-ID.md",
            h.display()
        );
        // The exact file being renamed, in the event org (must flip to NEW-ID).
        seed(
            "/abs/wt-target",
            "acme/codesense-context",
            "OLD-ID",
            &renamed,
        );
        // Same OLD-ID, DIFFERENT path (distinct story / legacy collision) — untouched.
        let other = format!(
            "{}/users/42/orgs/acme/codesense-context/beta/content/impl/OTHER.md",
            h.display()
        );
        seed("/abs/wt-other", "acme/codesense-context", "OLD-ID", &other);
        // Same OLD-ID, DIFFERENT org — untouched (cross-org guard).
        let cross = format!(
            "{}/users/42/orgs/other-org/codesense-context/alpha/content/impl/OLD-ID.md",
            h.display()
        );
        seed(
            "/abs/wt-cross",
            "other-org/codesense-context",
            "OLD-ID",
            &cross,
        );

        rekey_worktrees_story_id("OLD-ID", "NEW-ID", Path::new(&renamed));

        // Only the exact renamed file's record flips to NEW-ID.
        let after = worktree_index::find_by_story_id(h, &Principal::from(42u64), "NEW-ID").unwrap();
        assert_eq!(after.len(), 1, "exactly one record rekeyed");
        assert_eq!(after[0].code_repo_path, PathBuf::from("/abs/wt-target"));

        // The two guarded records still answer to OLD-ID.
        let still_old =
            worktree_index::find_by_story_id(h, &Principal::from(42u64), "OLD-ID").unwrap();
        let mut paths: Vec<_> = still_old.iter().map(|r| r.code_repo_path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/abs/wt-cross"),
                PathBuf::from("/abs/wt-other"),
            ]
        );
    }

    // ----- SW-TS-5: launch reconcile across worktree surfaces (pure) ------

    /// Persist a worktree record under `home` so `iter_records` can enumerate
    /// it. `code_repo_path` is arbitrary (the index never stats it) — only the
    /// path-hash keying matters, so distinct paths produce distinct records.
    fn persist_wt_record(home: &Path, code_repo_path: &str, team: &str) {
        let story = format!(
            "{}/users/42/orgs/acme/codesense-context/{team}/content/impl/SW-TS-1.md",
            home.display()
        );
        let rec = WorktreeStoryRecord::new(
            PathBuf::from(code_repo_path),
            "acme/codesense-context",
            SourceRepoSlug::new("acme", "codesense").unwrap(),
            "SW-TS-1",
            PathBuf::from(story),
        );
        worktree_index::write_record(home, &Principal::from(42u64), &rec).unwrap();
    }

    /// SW-XS-17: the LAUNCH reconcile deletes the retired idle-clock sidecar the
    /// worktree reaper used to keep next to the records, and touches nothing else: a
    /// live worktree's record survives. The in-session reconcile leaves it alone.
    #[test]
    fn launch_reconcile_deletes_the_retired_idle_clock_file() {
        let home = TempDir::new().unwrap();
        let worktrees = TempDir::new().unwrap();
        let live = worktrees.path().join("wt-live");
        std::fs::create_dir(&live).unwrap();
        persist_wt_record(home.path(), live.to_str().unwrap(), "alpha");
        let index_dir = home.path().join("users").join("42").join("worktrees");
        let idle_clock = index_dir.join(["session-activity", ".state"].concat());
        std::fs::write(&idle_clock, br#"{"/wt/a":1}"#).unwrap();

        reconcile_worktrees_with(home.path(), 42);
        assert!(
            idle_clock.exists(),
            "the in-session reconcile does not touch it"
        );

        let started = std::sync::atomic::AtomicBool::new(false);
        let summary = launch_reconcile_with(home.path(), 42, &started, &|_, _| {});

        assert!(!idle_clock.exists(), "the retired idle clock is removed");
        assert_eq!(
            summary,
            ReconcileSummary {
                total: 1,
                reaped: 0
            }
        );
        assert!(live.exists());
    }

    /// SW-XS-17: the install-store backfill that used to start inside the removed
    /// admission gate now starts from the launch reconcile, exactly once per process
    /// however many times the launch reconcile runs.
    #[test]
    fn launch_reconcile_starts_the_install_store_backfill_exactly_once() {
        let home = TempDir::new().unwrap();
        let started = std::sync::atomic::AtomicBool::new(false);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let count = |h: &Path, uid: u64| {
            assert_eq!((h, uid), (home.path(), 42));
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        };
        launch_reconcile_with(home.path(), 42, &started, &count);
        launch_reconcile_with(home.path(), 42, &started, &count);
        launch_reconcile_with(home.path(), 42, &started, &count);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn reconcile_keeps_live_story_worktrees_and_reaps_confirmed_gone_ones() {
        // The launch reconcile enumerates every record across ALL teams (no
        // viewed-team filter), keeps each live worktree's record, and reaps a
        // record whose worktree was removed while its container remains.
        let home = TempDir::new().unwrap();
        let worktrees = TempDir::new().unwrap();
        let live_a = worktrees.path().join("wt-a");
        let live_b = worktrees.path().join("wt-b");
        let gone = worktrees.path().join("wt-gone");
        std::fs::create_dir(&live_a).unwrap();
        std::fs::create_dir(&live_b).unwrap();
        persist_wt_record(home.path(), live_a.to_str().unwrap(), "alpha");
        persist_wt_record(home.path(), live_b.to_str().unwrap(), "beta");
        persist_wt_record(home.path(), gone.to_str().unwrap(), "alpha");

        let summary = reconcile_worktrees_with(home.path(), 42);

        assert_eq!(
            summary,
            ReconcileSummary {
                total: 2,
                reaped: 1
            }
        );
        let mut remaining: Vec<PathBuf> =
            worktree_index::iter_story_records(home.path(), &Principal::from(42u64))
                .unwrap()
                .into_iter()
                .map(|r| r.code_repo_path)
                .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![live_a, live_b],
            "only the gone record is reaped"
        );
    }

    /// SW-XS-3 (AC4, K7): with the forward waves gone, nothing else reaps the
    /// sync-store rows of a worktree deleted on disk. The launch reconcile drops
    /// them together with the index record; a live worktree's rows survive. On
    /// `main` the store rows of the gone worktree survived the launch reconcile.
    #[test]
    fn launch_reconcile_reaps_store_rows_of_a_deleted_worktree() {
        use crate::content_engine::sync_state_store::{
            default_store_path, SyncStateStore, TargetKey,
        };
        let home = TempDir::new().unwrap();
        let worktrees = TempDir::new().unwrap();
        let live = worktrees.path().join("wt-live");
        let gone = worktrees.path().join("wt-gone");
        std::fs::create_dir(&live).unwrap();
        persist_wt_record(home.path(), live.to_str().unwrap(), "alpha");
        persist_wt_record(home.path(), gone.to_str().unwrap(), "alpha");
        let slug = SourceRepoSlug::new("acme", "codesense").unwrap();
        let store = SyncStateStore::open(default_store_path(home.path())).unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 42,
            org_slug: "acme/codesense-context".to_string(),
            team_name: Some("alpha".to_string()),
        };
        for path in [&live, &gone] {
            store
                .upsert(
                    42,
                    &slug,
                    &Lockfile {
                        schema_version: 3,
                        org_slug: ctx.org_slug.clone(),
                        team_name: ctx.team_name.clone(),
                        code_repo_path: path.clone(),
                        entries: vec![],
                    },
                )
                .unwrap();
        }
        let present = |path: &Path| {
            store
                .read(&TargetKey::from_ctx(&ctx, path))
                .unwrap()
                .is_some()
        };
        assert!(present(&live) && present(&gone), "precondition: both rows");

        let summary = reconcile_worktrees_with(home.path(), 42);

        assert_eq!(
            summary,
            ReconcileSummary {
                total: 1,
                reaped: 1
            }
        );
        assert!(
            !present(&gone),
            "the deleted worktree's store row is reaped"
        );
        assert!(present(&live), "a live worktree's store row is kept");
    }

    /// SW-XS-3 (F10, AC1): a story moving between teams — driven through the REAL
    /// move hook (`handle_story_move_event`) on an app with a registered
    /// coordinator — relinks the worktree record to the new location and enqueues
    /// no wave, writes nothing into the worktree and creates no lockfile row. On
    /// `main` the move enqueued a `WorktreeDerive` wave.
    #[test]
    fn story_move_relinks_the_record_and_writes_nothing_into_the_worktree() {
        let home = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        std::fs::write(worktree.path().join("notes.md"), b"mine").unwrap();
        let wt = worktree.path().to_str().unwrap().to_string();
        persist_wt_record(home.path(), &wt, "alpha");
        let moved = PathBuf::from(format!(
            "{}/users/42/orgs/acme/codesense-context/beta/content/impl/SW-TS-1.md",
            home.path().display()
        ));
        let snapshot = |dir: &Path| {
            let mut entries: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| {
                    let p = e.unwrap().path();
                    let bytes = std::fs::read(&p).unwrap_or_default();
                    (p, bytes)
                })
                .collect();
            entries.sort();
            entries
        };
        let before = snapshot(worktree.path());
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::write(
            &moved,
            "---\nstory_id: SW-TS-1\nstatus: ready\nsource_repo: acme/codesense\n---\n# moved\n",
        )
        .unwrap();
        let coord = Arc::new(WaveCoordinator::new());
        let app = tauri::test::mock_builder()
            .manage(crate::install::frontmatter_cache::FrontmatterCache::shared(
                crate::install::frontmatter_cache::FrontmatterCache::new(),
            ))
            .manage(Arc::clone(&coord))
            .manage(crate::commands::watch::WatcherState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();

        handle_story_move_event(app.handle(), &moved);

        assert_eq!(coord.enqueued_count(), 0, "a story move enqueues no wave");
        assert_eq!(
            coord.entry_attempts(),
            0,
            "no coordinator entry point is reached"
        );
        assert!(!coord.has_wave_in_flight());
        assert_eq!(snapshot(worktree.path()), before, "nothing written");
        let record =
            worktree_index::read_record(home.path(), &Principal::from(42u64), worktree.path())
                .unwrap()
                .expect("record kept");
        assert_eq!(record.story_source_path, moved, "record follows the story");
        assert_eq!(record.intended_team.as_deref(), Some("beta"));
        assert!(
            crate::content_engine::lockfile::open_sync_state_store(home.path())
                .and_then(|store| store
                    .teams_for_materialized_path(
                        "acme/codesense-context",
                        &SourceRepoSlug::new("acme", "codesense").unwrap(),
                        worktree.path(),
                    )
                    .ok())
                .unwrap_or_default()
                .is_empty(),
            "no lockfile/store row was created for the worktree"
        );
    }

    #[test]
    fn reconcile_no_records_is_noop() {
        // AC-5/AC-12: a user with no worktrees reconciles nothing, calmly.
        let home = TempDir::new().unwrap();
        assert_eq!(
            reconcile_worktrees_with(home.path(), 42),
            ReconcileSummary::default()
        );
    }

    // ----- SW-TS-4: establish link record (pure) --------------------------

    #[test]
    fn build_link_record_assembles_from_authoritative_path() {
        // The establish writer derives home/uid/org from the story's own
        // path (the same parse the move-watch trusts), so a frontend can't
        // drift those. Only story_id + source_repo come from the caller.
        let story = story_at("alpha");
        let (home, uid, record) = build_link_record(
            Path::new(&story),
            Path::new("/abs/wt"),
            "SW-TS-4",
            "alderco/codesense",
        )
        .expect("a well-formed context-repo story path links");

        assert_eq!(home, PathBuf::from(WT_HOME));
        assert_eq!(uid, 42);
        assert_eq!(record.code_repo_path, PathBuf::from("/abs/wt"));
        // org_slug derived from `/orgs/<org>/`, never the caller.
        assert_eq!(record.org_slug, "acme/codesense-context");
        assert_eq!(record.story_id, "SW-TS-4");
        assert_eq!(record.story_source_path, PathBuf::from(&story));
        assert_eq!(
            record.source_repo,
            SourceRepoSlug::new("alderco", "codesense").unwrap()
        );
        // intended_team cache populated from the story location (AC-2).
        assert_eq!(record.intended_team, Some("alpha".to_string()));
    }

    /// SW-LN-B29: a story in a LOCAL org names its no-remote repo `local/fixture-api`. The
    /// link record must carry the team's configured `local`-host slug — the one the forward
    /// fan-out and refocus catch-up enumerate the store by — not the GitHub-shaped parse,
    /// which no team lookup ever queries (the worktree would never be found again).
    #[test]
    fn build_link_record_uses_the_teams_local_slug_for_a_local_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let org_root = home.join("users/42/orgs/local/e2e");
        std::fs::create_dir_all(org_root.join("general/content/two-repo-epic")).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            "slug: local/e2e\ndisplay_name: E2E\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(org_root.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            "teams:\n  - name: general\n    source_repos:\n      - owner: local\n        repo: fixture-api\n        provider: local\n        host: local\n",
        )
        .unwrap();
        let story = org_root.join("general/content/two-repo-epic/TR-1-api-health-endpoint.md");

        let (_, _, record) =
            build_link_record(&story, Path::new("/abs/wt"), "TR-1", "local/fixture-api")
                .expect("a local-org story links");
        assert_eq!(
            record.source_repo,
            SourceRepoSlug::with_provider_and_host("local", "fixture-api", "local", Some("local"))
                .unwrap(),
            "the record carries the team's configured local slug"
        );
    }

    #[test]
    fn build_link_record_neutral_for_non_context_path() {
        // Not under /users/<uid>/orgs/<org>/ → unlinkable. The
        // worktree is still created; we just can't form a record (AC-5).
        assert!(build_link_record(
            Path::new("/srv/scratch/notes/SW-TS-4.md"),
            Path::new("/abs/wt"),
            "SW-TS-4",
            "alderco/codesense",
        )
        .is_none());
    }

    #[test]
    fn build_link_record_neutral_for_malformed_source_repo() {
        // A bad `source_repo` yields no record rather than a partial one
        // with an unmaterializable slug — fail safe, never half-link.
        let story = story_at("alpha");
        for bad in ["", "noslash", "owner/", "/repo", "a/b/c"] {
            assert!(
                build_link_record(Path::new(&story), Path::new("/abs/wt"), "SW-TS-4", bad)
                    .is_none(),
                "expected neutral for source_repo {bad:?}"
            );
        }
    }

    #[test]
    fn user_home_and_id_parses_and_rejects() {
        let p = story_at("alpha");
        assert_eq!(
            user_home_and_id_from_path(Path::new(&p)),
            Some((PathBuf::from(WT_HOME), 42))
        );
        // Not under a `/users/<uid>/` tree.
        assert_eq!(
            user_home_and_id_from_path(Path::new("/tmp/whatever.md")),
            None
        );
        // Non-numeric uid.
        assert_eq!(
            user_home_and_id_from_path(Path::new("/h/users/notanum/x.md")),
            None
        );
    }

    #[test]
    fn org_slug_from_path_extracts_multipart_org_and_rejects_non_org_paths() {
        // Multi-part org slug (owner/repo) between /orgs/ and /.
        assert_eq!(
            org_slug_from_path(Path::new(&story_at("alpha"))),
            Some("acme/codesense-context".to_string())
        );
        // Not an org-scoped _team path.
        assert_eq!(
            org_slug_from_path(Path::new("/srv/scratch/notes/SW-TS-1.md")),
            None
        );
        // /orgs/ present but no / segment.
        assert_eq!(
            org_slug_from_path(Path::new("/h/users/42/orgs/acme/repo/file.md")),
            None
        );
    }

    fn write_at(root: &Path, rel: &str, body: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    /// A suppression handle backed by a throwaway dispatcher — enough to
    /// drive `move_subtree_locked` / `move_one_locked` without an
    /// `AppHandle`. The handle keeps the underlying sets alive after the
    /// dispatcher temporary drops (it holds `Arc` clones).
    fn sup_handle() -> SuppressionHandle {
        crate::content_engine::Dispatcher::new(1, 1000, 60_000, 500).suppression_handle()
    }

    #[tokio::test]
    async fn subtree_move_target_absent_moves_all_leaves() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            write_at(&from_root, &format!("{prefix}/{rel}"), b"x");
        }

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await
        .unwrap();

        assert_eq!(outcome, MoveOutcome::Moved { count: 3 });
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            assert!(
                to_root.join(prefix).join(rel).is_file(),
                "dest missing {rel}"
            );
            assert!(
                !from_root.join(prefix).join(rel).exists(),
                "source leaf should be gone: {rel}"
            );
        }
        assert!(
            !from_root.join(prefix).exists(),
            "empty source subtree dirs should be removed"
        );
    }

    #[tokio::test]
    async fn subtree_move_reject_whole_rejects_on_collision_and_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        for rel in ["SKILL.md", "steps/step-01.md"] {
            write_at(&from_root, &format!("{prefix}/{rel}"), b"src");
        }
        write_at(&to_root, &format!("{prefix}/SKILL.md"), b"dst-existing");

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await
        .unwrap();

        assert_eq!(outcome, MoveOutcome::RejectedTargetExists);
        // Nothing moved: source intact, target untouched.
        assert!(from_root.join(prefix).join("SKILL.md").is_file());
        assert!(from_root.join(prefix).join("steps/step-01.md").is_file());
        assert_eq!(
            fs::read(to_root.join(prefix).join("SKILL.md")).unwrap(),
            b"dst-existing"
        );
        assert!(!to_root.join(prefix).join("steps/step-01.md").exists());
    }

    #[tokio::test]
    async fn subtree_move_skip_existing_moves_non_colliding_reports_partial() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        for rel in ["SKILL.md", "steps/step-01.md"] {
            write_at(&from_root, &format!("{prefix}/{rel}"), b"src");
        }
        write_at(&to_root, &format!("{prefix}/SKILL.md"), b"dst-existing");

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::SkipExisting,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            MoveOutcome::MovedPartial {
                moved: 1,
                // The colliding leaf's full logical name, so the workbench
                // conflict dialog can render and replay it.
                skipped: vec![format!("{prefix}/SKILL.md")],
            }
        );
        // Non-colliding leaf moved; colliding leaf left at source, target kept.
        assert!(to_root.join(prefix).join("steps/step-01.md").is_file());
        assert!(!from_root.join(prefix).join("steps/step-01.md").exists());
        assert!(from_root.join(prefix).join("SKILL.md").is_file());
        assert_eq!(
            fs::read(to_root.join(prefix).join("SKILL.md")).unwrap(),
            b"dst-existing"
        );
    }

    // SS-FU-18 AC-4e: a byte-identical collision is auto-resolved (promoted
    // over the matching target, source removed, counted in `moved`) and is
    // NOT surfaced; only the genuinely-differing collision appears in
    // `skipped`. Keeps the promote dialog from rendering noise rows for
    // files that already match upstream.
    #[tokio::test]
    async fn subtree_move_skip_existing_auto_resolves_byte_identical_collision() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        // SKILL.md: identical on both sides (no-op promote).
        // steps/step-01.md: differs (real conflict).
        // references/guide.md: non-colliding (plain move).
        write_at(&from_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        write_at(&to_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        write_at(
            &from_root,
            &format!("{prefix}/steps/step-01.md"),
            b"edited-src",
        );
        write_at(&to_root, &format!("{prefix}/steps/step-01.md"), b"old-dst");
        write_at(
            &from_root,
            &format!("{prefix}/references/guide.md"),
            b"brand-new",
        );

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::SkipExisting,
        )
        .await
        .unwrap();

        // Only the differing leaf is reported skipped; the identical one and
        // the non-colliding one both count as moved.
        assert_eq!(
            outcome,
            MoveOutcome::MovedPartial {
                moved: 2,
                skipped: vec![format!("{prefix}/steps/step-01.md")],
            }
        );
        // Identical collision: target unchanged, redundant source consumed.
        assert_eq!(
            fs::read(to_root.join(prefix).join("SKILL.md")).unwrap(),
            b"same-bytes"
        );
        assert!(!from_root.join(prefix).join("SKILL.md").exists());
        // Differing collision: target kept, source left in place.
        assert_eq!(
            fs::read(to_root.join(prefix).join("steps/step-01.md")).unwrap(),
            b"old-dst"
        );
        assert!(from_root.join(prefix).join("steps/step-01.md").is_file());
        // Non-colliding leaf: moved across.
        assert!(to_root.join(prefix).join("references/guide.md").is_file());
        assert!(!from_root.join(prefix).join("references/guide.md").exists());
    }

    // SS-FU-18 (review F1): a subtree leaf whose target exists as a
    // DIRECTORY (not a regular file) must surface as a conflict — never
    // byte-compared (reading a dir errors with EISDIR and would fail the
    // whole batch) and never auto-resolved as identical. Under SkipExisting
    // it is reported by name; the directory target is left untouched.
    #[tokio::test]
    async fn subtree_move_skip_existing_treats_directory_target_as_conflict() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        // NOTE.md is a regular file in the source, but a DIRECTORY at the target.
        write_at(&from_root, &format!("{prefix}/NOTE.md"), b"src-bytes");
        fs::create_dir_all(to_root.join(prefix).join("NOTE.md")).unwrap();
        // A plain non-colliding leaf so the move still does real work.
        write_at(&from_root, &format!("{prefix}/ok.md"), b"brand-new");

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::SkipExisting,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            MoveOutcome::MovedPartial {
                moved: 1,
                skipped: vec![format!("{prefix}/NOTE.md")],
            },
            "a directory-shaped target is a conflict, not a hard error or an identical no-op"
        );
        // Directory target untouched; source file stays put.
        assert!(to_root.join(prefix).join("NOTE.md").is_dir());
        assert!(from_root.join(prefix).join("NOTE.md").is_file());
        // Non-colliding leaf moved across.
        assert!(to_root.join(prefix).join("ok.md").is_file());
        assert!(!from_root.join(prefix).join("ok.md").exists());
    }

    // SS-FU-18 (review iter-3): under RejectWhole, a subtree whose ONLY
    // collisions are byte-identical SUCCEEDS — identical content is a no-op,
    // never a conflict, so there is nothing for RejectWhole to reject. The
    // redundant identical source is dropped; non-colliding leaves move.
    #[tokio::test]
    async fn subtree_move_reject_whole_succeeds_when_only_collisions_are_identical() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        // SKILL.md: identical on both sides (a no-op collision, not a conflict).
        write_at(&from_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        write_at(&to_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        // guide.md: non-colliding (plain move).
        write_at(
            &from_root,
            &format!("{prefix}/references/guide.md"),
            b"brand-new",
        );

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await
        .unwrap();

        // Both leaves end up on the target side; nothing rejected.
        assert_eq!(outcome, MoveOutcome::Moved { count: 2 });
        // Identical collision: target kept, redundant source dropped.
        assert_eq!(
            fs::read(to_root.join(prefix).join("SKILL.md")).unwrap(),
            b"same-bytes"
        );
        assert!(!from_root.join(prefix).join("SKILL.md").exists());
        // Non-colliding leaf moved across.
        assert!(to_root.join(prefix).join("references/guide.md").is_file());
        assert!(!from_root.join(prefix).join("references/guide.md").exists());
    }

    // SS-FU-18 (review R1): a colliding regular-file target that cannot be
    // read (permission denied) must NOT abort the whole SkipExisting batch —
    // it surfaces as a conflict, like any collision we can't prove
    // byte-identical. Equal length forces the read attempt (past the length
    // short-circuit); differing bytes also make it a conflict under root,
    // where the privileged read would still succeed.
    #[cfg(unix)]
    #[tokio::test]
    async fn subtree_move_skip_existing_unreadable_target_is_conflict_not_abort() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        write_at(&from_root, &format!("{prefix}/LOCKED.md"), b"AAAA");
        write_at(&to_root, &format!("{prefix}/LOCKED.md"), b"BBBB");
        let locked = to_root.join(prefix).join("LOCKED.md");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // A plain non-colliding leaf so the move still does real work.
        write_at(&from_root, &format!("{prefix}/ok.md"), b"brand-new");

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::SkipExisting,
        )
        .await;
        // Restore perms so TempDir cleanup can remove the file.
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o644));

        let outcome = outcome.expect("an unreadable collision must not error the whole batch");
        assert_eq!(
            outcome,
            MoveOutcome::MovedPartial {
                moved: 1,
                skipped: vec![format!("{prefix}/LOCKED.md")],
            }
        );
        assert!(to_root.join(prefix).join("ok.md").is_file());
    }

    // SS-FU-18 AC-2/AC-4b: a single-file move with `force` replaces an
    // existing target with the source bytes and reports `Moved { count: 1 }`.
    #[tokio::test]
    async fn move_one_force_overwrites_existing_target() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let rel = ".claude/skills/bmad-tea/SKILL.md";
        write_at(&from_root, rel, b"my edited version");
        write_at(&to_root, rel, b"stale upstream version");

        let outcome = move_one_locked(
            &from_root.join(rel),
            &to_root.join(rel),
            &to_root,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            rel,
            true,
        )
        .await
        .unwrap();

        assert_eq!(outcome, MoveOutcome::Moved { count: 1 });
        assert_eq!(fs::read(to_root.join(rel)).unwrap(), b"my edited version");
        assert!(!from_root.join(rel).exists(), "source consumed by the move");
    }

    // SS-FU-18 AC-4c: without `force`, a collision still rejects and nothing
    // changes (preserves today's reject-on-collision behavior).
    #[tokio::test]
    async fn move_one_without_force_rejects_existing_target() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let rel = ".claude/skills/bmad-tea/SKILL.md";
        write_at(&from_root, rel, b"my edited version");
        write_at(&to_root, rel, b"stale upstream version");

        let outcome = move_one_locked(
            &from_root.join(rel),
            &to_root.join(rel),
            &to_root,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            rel,
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome, MoveOutcome::RejectedTargetExists);
        assert_eq!(
            fs::read(to_root.join(rel)).unwrap(),
            b"stale upstream version",
            "target untouched on reject"
        );
        assert!(from_root.join(rel).is_file(), "source untouched on reject");
    }

    #[tokio::test]
    async fn subtree_move_rollback_on_failure_restores_source() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            write_at(&from_root, &format!("{prefix}/{rel}"), b"x");
        }
        // Plant a FILE where the `steps` directory must be created, so the
        // `steps/step-01.md` leaf fails (stat or mkdir errors on a file
        // used as a directory) mid-batch and forces a rollback.
        write_at(&to_root, &format!("{prefix}/steps"), b"blocker");

        let result = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await;

        assert!(
            result.is_err(),
            "expected a hard I/O error mid-batch, got {result:?}"
        );
        // All-or-nothing: every source leaf restored, no dest leaf leaked.
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            assert!(
                from_root.join(prefix).join(rel).is_file(),
                "source not restored after rollback: {rel}"
            );
        }
        assert!(!to_root.join(prefix).join("SKILL.md").exists());
        assert!(!to_root.join(prefix).join("references/guide.md").exists());
        // Rollback also removes the empty dest dirs the forward moves
        // created — no ghost dirs left in the destination layer.
        assert!(
            !to_root.join(prefix).join("references").exists(),
            "empty dest dir should be cleaned up on rollback"
        );
    }

    // SS-FU-18: a byte-identical collision must survive a mid-batch rollback
    // intact. Identical leaves are resolved post-success (redundant-source
    // removal), never via an overwrite-move, so a failure before that point
    // leaves BOTH the identical source and its target untouched — "as if the
    // move never started", same as every other leaf. (Regression guard for the
    // overwrite-then-rollback gap the post-success design avoids.)
    #[tokio::test]
    async fn subtree_move_rollback_preserves_byte_identical_target() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        // SKILL.md: byte-identical collision (resolved only on success).
        // references/guide.md: plain movable leaf (moved, then rolled back).
        // steps/step-01.md: movable leaf that FAILS mid-batch (blocked dir).
        write_at(&from_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        write_at(&to_root, &format!("{prefix}/SKILL.md"), b"same-bytes");
        write_at(&from_root, &format!("{prefix}/references/guide.md"), b"g");
        write_at(&from_root, &format!("{prefix}/steps/step-01.md"), b"s");
        // Plant a FILE where the `steps` dir must be created → mkdir fails →
        // mid-batch rollback.
        write_at(&to_root, &format!("{prefix}/steps"), b"blocker");

        let result = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::SkipExisting,
        )
        .await;

        assert!(
            result.is_err(),
            "expected a mid-batch failure, got {result:?}"
        );
        // The identical leaf is intact on BOTH sides — never overwritten, never
        // removed (removal happens only on success, which we never reach).
        assert!(from_root.join(prefix).join("SKILL.md").is_file());
        assert_eq!(
            fs::read(to_root.join(prefix).join("SKILL.md")).unwrap(),
            b"same-bytes",
            "byte-identical target must survive rollback"
        );
        // The plain movable leaf rolled back: restored at source, gone at dest.
        assert!(from_root.join(prefix).join("references/guide.md").is_file());
        assert!(!to_root.join(prefix).join("references/guide.md").exists());
    }

    #[tokio::test]
    async fn subtree_move_missing_prefix_rejects_source_missing() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        fs::create_dir_all(&from_root).unwrap();

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            ".claude/skills/nonexistent",
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await
        .unwrap();

        assert_eq!(outcome, MoveOutcome::RejectedSourceMissing);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subtree_move_with_symlink_is_rejected_whole() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        write_at(&from_root, &format!("{prefix}/SKILL.md"), b"x");
        std::os::unix::fs::symlink("SKILL.md", from_root.join(prefix).join("link.md")).unwrap();

        let outcome = move_subtree_locked(
            &from_root,
            &to_root,
            prefix,
            &sup_handle(),
            LayerId::TeamProjection,
            LayerId::TeamProjection,
            ConflictPolicy::RejectWhole,
        )
        .await
        .unwrap();

        // A symlink in the subtree → reject the whole move, change nothing
        // (no partial split across layers).
        assert_eq!(outcome, MoveOutcome::RejectedSourceMissing);
        assert!(from_root.join(prefix).join("SKILL.md").is_file());
        assert!(!to_root.join(prefix).exists());
    }

    #[tokio::test]
    async fn subtree_move_concurrent_same_target_serializes() {
        let tmp = TempDir::new().unwrap();
        let from_root = tmp.path().join("from");
        let to_root = tmp.path().join("to");
        let prefix = ".claude/skills/bmad-tea";
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            write_at(&from_root, &format!("{prefix}/{rel}"), b"x");
        }
        let handle = sup_handle();

        // Two workspace subtree moves race on the same source/target. Each
        // takes both workspace-root locks (as the outer command does), so
        // they serialize — one moves the whole tree, the other finds the
        // source already gone. Without the lock they would interleave.
        let run = || async {
            let _locks = acquire_workspace_locks(&from_root, &to_root).await;
            move_subtree_locked(
                &from_root,
                &to_root,
                prefix,
                &handle,
                LayerId::TeamProjection,
                LayerId::TeamProjection,
                ConflictPolicy::RejectWhole,
            )
            .await
            .unwrap()
        };
        let (a, b) = tokio::join!(run(), run());

        let one_moved = [&a, &b]
            .iter()
            .filter(|o| matches!(o, MoveOutcome::Moved { count: 3 }))
            .count();
        let one_missing = [&a, &b]
            .iter()
            .filter(|o| matches!(o, MoveOutcome::RejectedSourceMissing))
            .count();
        assert_eq!(
            (one_moved, one_missing),
            (1, 1),
            "expected exactly one clean Moved and one RejectedSourceMissing (no interleave), got {a:?} / {b:?}"
        );
        for rel in ["SKILL.md", "references/guide.md", "steps/step-01.md"] {
            assert!(
                to_root.join(prefix).join(rel).is_file(),
                "dest missing {rel}"
            );
        }
        assert!(!from_root.join(prefix).exists());
    }

    /// Tiny helpers to keep the call-site shape consistent across tests.
    /// `home` is the tempdir path stringified; `team`/`slug_opt` cover the
    /// common variants.
    fn list_layers_params(
        home: &Path,
        team: Option<&str>,
        slug_opt: Option<SourceRepoSlug>,
        writable_only: bool,
    ) -> ListLayersParams {
        ListLayersParams {
            codesense_home: home.to_string_lossy().into_owned(),
            user_id: 42,
            org_slug: "acme/api".into(),
            team_name: team.map(String::from),
            code_repo_slug: slug_opt,
            writable_only,
        }
    }

    fn list_layer_contents_params(
        home: &Path,
        layer_id: LayerId,
        team: Option<&str>,
        slug_opt: Option<SourceRepoSlug>,
    ) -> ListLayerContentsParams {
        ListLayerContentsParams {
            layer_id,
            codesense_home: home.to_string_lossy().into_owned(),
            user_id: 42,
            org_slug: "acme/api".into(),
            team_name: team.map(String::from),
            code_repo_slug: slug_opt,
        }
    }

    fn read_layer_file_params(
        home: &Path,
        layer_id: LayerId,
        logical_name: &str,
        team: Option<&str>,
        slug_opt: Option<SourceRepoSlug>,
    ) -> ReadLayerFileParams {
        ReadLayerFileParams {
            layer_id,
            logical_name: logical_name.into(),
            codesense_home: home.to_string_lossy().into_owned(),
            user_id: 42,
            org_slug: "acme/api".into(),
            team_name: team.map(String::from),
            code_repo_slug: slug_opt,
        }
    }

    fn read_lockfile_params(
        home: &Path,
        team: Option<&str>,
        slug_v: SourceRepoSlug,
    ) -> ReadLockfileForRepoParams {
        ReadLockfileForRepoParams {
            code_repo_slug: slug_v,
            // Tests pass a synthetic stand-in repo path. Review-iter-1
            // added this field so the empty-lockfile fallback carries a
            // meaningful `code_repo_path` field.
            code_repo_path: PathBuf::from("/test/code/repo"),
            codesense_home: home.to_string_lossy().into_owned(),
            user_id: 42,
            org_slug: "acme/api".into(),
            team_name: team.map(String::from),
        }
    }

    fn scan_cwd_params(
        home: &Path,
        team: Option<&str>,
        slug_v: SourceRepoSlug,
        code_repo: &Path,
    ) -> SenseSyncScanCwdParams {
        SenseSyncScanCwdParams {
            code_repo_slug: slug_v,
            code_repo_path: code_repo.to_path_buf(),
            codesense_home: home.to_string_lossy().into_owned(),
            user_id: 42,
            org_slug: "acme/api".into(),
            team_name: team.map(String::from),
        }
    }

    // -----------------------------------------------------------------
    // derive_resolver_context (AC-2, AC-7 InvalidContextSegment)
    // -----------------------------------------------------------------

    #[test]
    fn derive_resolver_context_happy_path() {
        let ctx = derive_resolver_context(
            PathBuf::from("/home/user/.codesense"),
            42,
            "acme/api".into(),
            Some("alpha".into()),
        )
        .unwrap();
        assert_eq!(ctx.org_slug, "acme/api");
        assert_eq!(ctx.team_name.as_deref(), Some("alpha"));
        assert_eq!(ctx.user_id, 42);
    }

    #[test]
    fn derive_resolver_context_rejects_unsafe_org_slug() {
        let err =
            derive_resolver_context(PathBuf::from("/home"), 42, "../etc".into(), None).unwrap_err();
        match err {
            Error::InvalidContextSegment { field, .. } => assert_eq!(field, "orgSlug"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn derive_resolver_context_rejects_unsafe_team_name() {
        let err = derive_resolver_context(
            PathBuf::from("/home"),
            42,
            "acme/api".into(),
            Some("../escape".into()),
        )
        .unwrap_err();
        match err {
            Error::InvalidContextSegment { field, .. } => assert_eq!(field, "teamName"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    /// SLW-IN-12: `derive_resolver_context` is now ALSO the reverse watch-set
    /// builder (`commands/watch.rs::resolve_reverse_dest_prefixes`), replacing the
    /// chain-less `ctx_with_team` swap. It must carry the full parent chain, so
    /// the reverse watch set covers inherited (parent-hop) projections — that
    /// the downstream `query_projection_dests` walk over such a chain then places
    /// the inherited (content- AND config-class) dest dirs in the watch set, and
    /// omits them under the chain-less build, is pinned directly by
    /// `install::story_linker::query_projection_dests_includes_parent_inherited_dests_across_surfaces`.
    /// A parentless team stays length-1 (parity with the pre-IN-12 reverse
    /// behavior).
    #[test]
    fn derive_resolver_context_carries_parent_chain_for_reverse_watch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (home, uid) = (tmp.path(), 42u64);
        let org_root = crate::context_repo::path_to_org_root(
            home,
            &crate::domain::Principal::from(uid),
            "acme/api",
        )
        .unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            "slug: acme/api\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("people.yml"),
            "people:\n  - github_user_id: 1\n    github_username: alice\n",
        )
        .unwrap();
        // alpha inherits from beta.
        std::fs::write(
            org_root.join("teams.yml"),
            "teams:\n  - name: beta\n  - name: alpha\n    parent_team: beta\n",
        )
        .unwrap();

        // Chained team → [self, parent], nearest-first — the inherited hop the
        // reverse watch set now covers (was dropped by the empty-chain swap).
        let chained = derive_resolver_context(
            home.to_path_buf(),
            uid,
            "acme/api".into(),
            Some("alpha".into()),
        )
        .unwrap();
        assert_eq!(
            chained.view().unwrap().team_names(),
            vec!["alpha".to_string(), "beta".to_string()],
            "reverse watch-set ctx must carry the full parent chain"
        );

        // Parentless team → length-1 chain (byte-identical to pre-IN-12).
        let solo = derive_resolver_context(
            home.to_path_buf(),
            uid,
            "acme/api".into(),
            Some("beta".into()),
        )
        .unwrap();
        assert_eq!(solo.view().unwrap().team_names(), vec!["beta".to_string()]);
    }

    // -----------------------------------------------------------------
    // is_content_layer_path (D-14)
    // -----------------------------------------------------------------

    #[test]
    fn content_layer_path_matches_skill_md() {
        assert_eq!(
            is_content_layer_path(Path::new(".claude/skills/sense-sync/SKILL.md")),
            Some(".claude/skills/*/SKILL.md")
        );
    }

    #[test]
    fn content_layer_path_matches_codesense_hook() {
        assert_eq!(
            is_content_layer_path(Path::new(".claude/hooks/codesense-pre-commit")),
            Some(".claude/hooks/codesense-*")
        );
    }

    #[test]
    fn content_layer_path_rejects_unmatched() {
        assert_eq!(is_content_layer_path(Path::new("src/main.rs")), None);
        assert_eq!(is_content_layer_path(Path::new("README.md")), None);
    }

    // -----------------------------------------------------------------
    // list_layers (AC-3 row 1, AC-8 writable_only + backed coverage)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn list_layers_writable_only_returns_all_writable() {
        let tmp = TempDir::new().unwrap();
        let layers = list_layers(list_layers_params(
            tmp.path(),
            Some("alpha"),
            Some(slug()),
            true,
        ))
        .await
        .unwrap();
        // SW-CP-7: every remaining layer is writable, so writable_only
        // filters nothing.
        assert_eq!(layers.len(), PRIORITY_ORDER.len());
        assert!(layers.iter().all(|l| l.writable));
        assert!(!layers
            .iter()
            .any(|l| l.id == LayerId::TeamProjection && !l.writable));
    }

    #[tokio::test]
    async fn list_layers_enumerates_every_priority_layer() {
        let tmp = TempDir::new().unwrap();
        let layers = list_layers(list_layers_params(
            tmp.path(),
            Some("alpha"),
            Some(slug()),
            false,
        ))
        .await
        .unwrap();
        // SW-XS-8: team content is the one layer.
        assert_eq!(layers.len(), PRIORITY_ORDER.len());
        assert_eq!(layers.first().unwrap().id, LayerId::TeamProjection);
        // SW-XS-8: team content is the one layer, with its source path.
        let tp = layers
            .iter()
            .find(|l| l.id == LayerId::TeamProjection)
            .expect("TeamProjection must appear in list_layers");
        assert!(tp.writable);
        assert!(tp.backed);
        assert!(tp.source_path.is_some());
    }

    // -----------------------------------------------------------------
    // list_layer_contents (AC-3 row 2, AC-7 RepoSlugRequired)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn list_layer_contents_team_workspace_empty_layer_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let out = list_layer_contents(list_layer_contents_params(
            tmp.path(),
            LayerId::TeamProjection,
            Some("alpha"),
            None,
        ))
        .await
        .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn list_layer_contents_team_workspace_enumerates_files() {
        let tmp = TempDir::new().unwrap();
        let team_root =
            content_layer_root(LayerId::TeamProjection, &ctx(tmp.path(), Some("alpha"))).unwrap();
        write_at(&team_root, ".claude/skills/foo/SKILL.md", b"x");
        write_at(&team_root, ".claude/hooks/sense-bar", b"y");

        let out = list_layer_contents(list_layer_contents_params(
            tmp.path(),
            LayerId::TeamProjection,
            Some("alpha"),
            None,
        ))
        .await
        .unwrap();
        let names: Vec<_> = out.iter().map(|e| e.logical_name.clone()).collect();
        assert!(names.iter().any(|n| n == ".claude/skills/foo/SKILL.md"));
        assert!(names.iter().any(|n| n == ".claude/hooks/sense-bar"));
    }

    // -----------------------------------------------------------------
    // read_layer_file (AC-3 row 3, AC-7 InvalidLogicalName / FileNotFound /
    //                  LayerUnbacked / RepoSlugRequired)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn read_layer_file_team_workspace_happy_path() {
        let tmp = TempDir::new().unwrap();
        let team_root =
            content_layer_root(LayerId::TeamProjection, &ctx(tmp.path(), Some("alpha"))).unwrap();
        write_at(&team_root, "skills/foo/SKILL.md", b"hello world");

        let bytes = read_layer_file(read_layer_file_params(
            tmp.path(),
            LayerId::TeamProjection,
            "skills/foo/SKILL.md",
            Some("alpha"),
            None,
        ))
        .await
        .unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn read_layer_file_invalid_logical_name() {
        let tmp = TempDir::new().unwrap();
        let err = read_layer_file(read_layer_file_params(
            tmp.path(),
            LayerId::TeamProjection,
            "../escape.md",
            Some("alpha"),
            None,
        ))
        .await
        .unwrap_err();
        assert!(matches!(err, Error::InvalidLogicalName(_)));
    }

    #[tokio::test]
    async fn read_layer_file_file_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = read_layer_file(read_layer_file_params(
            tmp.path(),
            LayerId::TeamProjection,
            "skills/missing.md",
            Some("alpha"),
            None,
        ))
        .await
        .unwrap_err();
        match err {
            Error::FileNotFound { layer, .. } => assert_eq!(layer, LayerId::TeamProjection),
            other => panic!("unexpected: {:?}", other),
        }
    }

    // SW-LT-1: PersonalWorkspace is now backed, so the durable
    // `LayerUnbacked` case is a Team layer with no active team — its root
    // is genuinely unresolvable in that context.
    #[tokio::test]
    async fn read_layer_file_team_workspace_unbacked_without_team() {
        let tmp = TempDir::new().unwrap();
        let err = read_layer_file(read_layer_file_params(
            tmp.path(),
            LayerId::TeamProjection,
            "anything.md",
            None,
            None,
        ))
        .await
        .unwrap_err();
        match err {
            Error::LayerUnbacked(LayerId::TeamProjection) => {}
            other => panic!("unexpected: {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // read_lockfile_for_repo (AC-3 row 5, empty-lockfile fallback)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn read_lockfile_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let lf = read_lockfile_for_repo(read_lockfile_params(tmp.path(), Some("alpha"), slug()))
            .await
            .unwrap();
        assert_eq!(lf.schema_version, content_engine::SCHEMA_VERSION);
        assert_eq!(lf.org_slug, "acme/api");
        assert_eq!(lf.team_name.as_deref(), Some("alpha"));
        assert!(lf.entries.is_empty());
        // Review-iter-1: empty fallback now carries the caller-supplied
        // code_repo_path, not PathBuf::new().
        assert_eq!(lf.code_repo_path, PathBuf::from("/test/code/repo"));
    }

    #[tokio::test]
    async fn read_lockfile_existing_file_round_trips_entries() {
        // Review-iter-1: AC-8 happy-path now writes a populated lockfile
        // with a real MaterializedEntry and asserts it round-trips
        // (previously only one field was asserted).
        //
        // SS-FU-7: `lockfile_path` is keyed by `code_repo_path`. The
        // helper `read_lockfile_params` stubs that field as
        // `/test/code/repo`; the planted lockfile and the Lockfile
        // struct's `code_repo_path` must agree with it so the Tauri
        // command reads the same file the test wrote.
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("alpha"));
        let code_repo = PathBuf::from("/test/code/repo");
        // The store serves only projection entries (a retired install tier's
        // wave entry is drained on read), so round-trip a projection entry — the round-trip mechanism is
        // entry-type-agnostic.
        let entry = content_engine::MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/foo/SKILL.md".into(),
            dest_path: PathBuf::from("/code/repo/.claude/skills/foo/SKILL.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/source/path"),
            last_materialized_hash: [7u8; 32],
            last_materialized_mtime_ns: 123_456_789,
            projection_key: Some(".claude/skills/foo/SKILL.md".into()),
            source_oid: None,
        };
        let lf = Lockfile {
            schema_version: content_engine::SCHEMA_VERSION,
            org_slug: "acme/api".into(),
            team_name: Some("alpha".into()),
            code_repo_path: code_repo.clone(),
            entries: vec![entry.clone()],
        };
        seed_store_lock(&c, &slug(), &lf);

        let got = read_lockfile_for_repo(read_lockfile_params(tmp.path(), Some("alpha"), slug()))
            .await
            .unwrap();
        assert_eq!(got.code_repo_path, code_repo);
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0], entry);
    }

    // -----------------------------------------------------------------
    // sense_sync_scan_cwd (AC-3 row 8 — dirty + untracked + ignore)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn scan_finds_dirty_and_untracked_and_ignores_ds_store() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("alpha"));

        // Set up a code repo with one materialized file (with a stale
        // hash → dirty) and one untracked file (matches a CONTENT_LAYER
        // pattern but no lockfile entry).
        let code_repo = tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        // Review-iter-5 (EH-14): scan_cwd requires a `.git` marker
        // (file or dir) at code_repo_path. Create an empty `.git` dir
        // so the test's synthetic working tree passes the gate.
        fs::create_dir_all(code_repo.join(".git")).unwrap();
        let materialized = code_repo.join(".claude/hooks/codesense-pre-commit");
        fs::create_dir_all(materialized.parent().unwrap()).unwrap();
        fs::write(&materialized, b"new bytes - user edited").unwrap();

        let untracked_file = code_repo.join(".claude/skills/foo/SKILL.md");
        fs::create_dir_all(untracked_file.parent().unwrap()).unwrap();
        fs::write(&untracked_file, b"# foo skill").unwrap();

        // A `.DS_Store` matching IGNORE_GLOBS that should NOT appear in untracked.
        let ds_store = code_repo.join(".claude/skills/foo/.DS_Store");
        fs::write(&ds_store, b"junk").unwrap();

        // Seed a lockfile with materialized's recorded hash = blake3("old bytes")
        // into the store (the sole install-state substrate `scan_cwd` reads).
        let stale_hash = blake3::hash(b"old bytes").into();
        let lf = Lockfile {
            schema_version: content_engine::SCHEMA_VERSION,
            org_slug: "acme/api".into(),
            team_name: Some("alpha".into()),
            code_repo_path: code_repo.clone(),
            entries: vec![content_engine::MaterializedEntry {
                last_materialized_size: None,
                logical_name: ".claude/hooks/codesense-pre-commit".into(),
                dest_path: materialized.clone(),
                source_layer: LayerId::TeamProjection,
                source_path: PathBuf::from("/somewhere"),
                last_materialized_hash: stale_hash,
                last_materialized_mtime_ns: 0,
                projection_key: Some(".claude/hooks/codesense-pre-commit".into()),
                source_oid: None,
            }],
        };
        seed_store_lock(&c, &slug(), &lf);

        let report = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap();

        assert_eq!(report.dirty.len(), 1);
        assert_eq!(
            report.dirty[0].logical_name,
            ".claude/hooks/codesense-pre-commit"
        );

        let untracked_paths: Vec<_> = report
            .untracked
            .iter()
            .map(|u| u.code_repo_relative_path.to_string_lossy().into_owned())
            .collect();
        assert!(
            untracked_paths
                .iter()
                .any(|p| p == ".claude/skills/foo/SKILL.md"),
            "expected untracked SKILL.md, got: {:?}",
            untracked_paths
        );
        assert!(
            !untracked_paths.iter().any(|p| p.contains(".DS_Store")),
            "DS_Store must be ignored, got: {:?}",
            untracked_paths
        );
    }

    #[tokio::test]
    async fn scan_returns_not_a_git_working_tree_when_dot_git_missing() {
        // AC-8 reachable-variant coverage for `Error::NotAGitWorkingTree`.
        // Iter-5 (EH-14) added the variant; iter-7 (Blind-H6) narrowed it
        // to `ErrorKind::NotFound`. This test pins the typed surface for
        // a code_repo_path that exists as a regular dir but contains no
        // `.git` marker (file or dir). Review-iter-8 (Auditor PARTIAL).
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("no-git-repo");
        fs::create_dir_all(&code_repo).unwrap();
        // Deliberately do NOT create `.git`.

        let err = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap_err();
        match err {
            Error::NotAGitWorkingTree(p) => {
                assert_eq!(p, code_repo, "error must carry the rejected path");
            }
            other => panic!("expected Error::NotAGitWorkingTree, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // SS-FU-3: allowlist walk semantics + perf regression
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn scan_returns_empty_untracked_when_dot_claude_missing() {
        // AC-2: when the code repo has no `.claude/` directory the scan
        // must return empty untracked — the allowlist walker has nothing
        // in scope, no matter what other content the repo carries.
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        fs::create_dir_all(code_repo.join(".git")).unwrap();
        write_at(&code_repo, "src/lib.rs", b"// out-of-scope");
        write_at(&code_repo, "node_modules/foo/index.js", b"// noise");

        let report = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap();

        assert!(
            report.untracked.is_empty(),
            "expected empty untracked when .claude/ missing, got: {:?}",
            report.untracked
        );
        assert!(
            report.dirty.is_empty(),
            "expected empty dirty when no lockfile, got: {:?}",
            report.dirty
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_returns_empty_untracked_when_dot_claude_is_symlink() {
        // AC-5b: a symlinked `.claude/` must NOT be descended into. Today's
        // walker starts at `repo_root` and walkdir's default does not follow
        // a child symlink, so scanned content under a symlinked `.claude/`
        // is empty. The SS-FU-3 rewrite walks `.claude/` directly; without
        // the `symlink_metadata` gate, walkdir-v2 would follow the
        // symlinked root and silently flip this behavior. This test pins
        // the gate.
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        fs::create_dir_all(code_repo.join(".git")).unwrap();

        let real_claude = tmp.path().join("real-claude");
        fs::create_dir_all(&real_claude).unwrap();
        // Content that WOULD match if the walker descended.
        write_at(&real_claude, "skills/foo/SKILL.md", b"# foo");
        std::os::unix::fs::symlink(&real_claude, code_repo.join(".claude")).unwrap();

        let report = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap();

        assert!(
            report.untracked.is_empty(),
            "symlinked .claude/ must not be descended into; got: {:?}",
            report.untracked
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_returns_empty_untracked_when_dot_claude_symlink_targets_outside_repo() {
        // AC-5b coverage extension (review iter-1, Edge M1) — the threat
        // model that motivates the `symlink_metadata` gate is "a symlink
        // pointing to an arbitrary absolute location outside the repo".
        // The previous test plants the target inside the same `TempDir`;
        // this variant plants the target under an entirely separate
        // top-level tempdir to prove the gate suppresses traversal
        // regardless of where the symlink points.
        let repo_tmp = TempDir::new().unwrap();
        let outside_tmp = TempDir::new().unwrap();
        let code_repo = repo_tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        fs::create_dir_all(code_repo.join(".git")).unwrap();

        // Plant content under a path the test's repo can never observe
        // by walking its own tree — proves the gate, not just filesystem
        // boundaries, is what's suppressing traversal.
        write_at(outside_tmp.path(), "skills/foo/SKILL.md", b"# outside");
        std::os::unix::fs::symlink(outside_tmp.path(), code_repo.join(".claude")).unwrap();

        let report = sense_sync_scan_cwd(scan_cwd_params(
            repo_tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap();

        assert!(
            report.untracked.is_empty(),
            "symlinked .claude/ targeting outside the repo must not be descended into; got: {:?}",
            report.untracked
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_continues_after_permission_denied_subdir() {
        // AC-7: a permission-denied subdir under `.claude/` must not poison
        // the rest of the scan. Walkdir's error path emits a warn log and
        // continues; sibling content files must still be detected.
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        fs::create_dir_all(code_repo.join(".git")).unwrap();

        write_at(&code_repo, ".claude/skills/visible/SKILL.md", b"# visible");

        let denied = code_repo.join(".claude/skills/denied");
        fs::create_dir_all(&denied).unwrap();
        write_at(&denied, "SKILL.md", b"# hidden");
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

        // Review iter-2 (Blind M1) — bail when chmod 0o000 is ineffective
        // (root user / certain container runtimes / overlayfs). Without
        // this guard the iter-1 negative assertion below false-positives
        // on root-owned CI runners. We probe the actual ACL outcome
        // rather than checking euid so the guard is portable.
        //
        // Review iter-3 (Edge L1 / Blind L1) — consume one entry from
        // the iterator. `read_dir` only does `opendir(3)`; some
        // filesystems (FUSE, overlayfs, certain NFS configs) defer
        // EACCES to `readdir()` instead of returning it at open time.
        // Treating opendir-success as "chmod ineffective" would skip
        // the test on those FSes where chmod actually IS effective.
        // The `.next()` call probes the same code path walkdir takes.
        let chmod_ineffective = std::fs::read_dir(&denied)
            .ok()
            .and_then(|mut it| it.next())
            .map(|r| r.is_ok())
            .unwrap_or(false);
        if chmod_ineffective {
            // Review iter-3 (Edge L2) — surface restore failure loudly
            // instead of silently leaking the tempdir. `.unwrap()` is
            // load-bearing: if the post-bypass chmod can't undo 0o000,
            // TempDir::Drop cannot recurse into `denied/` and the entry
            // leaks until tmpwatch.
            fs::set_permissions(&denied, fs::Permissions::from_mode(0o755))
                .expect("perms-restore on `denied` failed; tempdir would leak");
            eprintln!(
                "skipping scan_continues_after_permission_denied_subdir: \
                 chmod 0o000 was not effective in this environment"
            );
            return;
        }

        let result = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await;

        // Restore perms BEFORE assertions so TempDir cleanup never fails.
        // Review iter-3 (Edge L2) — `.expect()` instead of `.ok()`: a
        // silent restore failure would let TempDir::Drop leave the
        // tempdir partially populated (denied/ unreadable). Loud is
        // better than leaky.
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o755))
            .expect("perms-restore on `denied` failed; tempdir would leak");

        let report = result.unwrap();
        let paths: Vec<_> = report
            .untracked
            .iter()
            .map(|u| u.code_repo_relative_path.to_string_lossy().into_owned())
            .collect();
        assert!(
            paths.iter().any(|p| p == ".claude/skills/visible/SKILL.md"),
            "visible sibling must be detected despite denied sibling; got: {:?}",
            paths
        );
        // Review iter-1 (Edge L4) — also pin that nothing under the
        // denied subtree leaks into untracked. A regression that flipped
        // the directory-entry filter or naively re-listed `denied/` post
        // perm-restore would surface here.
        assert!(
            !paths.iter().any(|p| p.starts_with(".claude/skills/denied")),
            "denied subtree must not leak into untracked; got: {:?}",
            paths
        );
    }

    #[tokio::test]
    async fn scan_skips_large_out_of_scope_subtree_fast() {
        // AC-8: regression test pinning the allowlist optimization. A repo
        // with 2,000 files under `node_modules/` (out-of-scope sentinel)
        // must complete in <200ms. The 2k fixture is the regression-trip
        // mechanism — if someone re-introduces a full-repo walk the scan
        // will stat all of them and blow the threshold. The threshold has
        // ~4× headroom over a clean allowlist walk (single-digit ms
        // expected). If 200ms is flaky in practice, raise to 500ms with a
        // comment; do NOT shrink the 2k fixture.
        let tmp = TempDir::new().unwrap();
        let code_repo = tmp.path().join("code-repo");
        fs::create_dir_all(&code_repo).unwrap();
        fs::create_dir_all(code_repo.join(".git")).unwrap();

        // 50 in-scope content files.
        for i in 0..50 {
            write_at(
                &code_repo,
                &format!(".claude/skills/skill-{:03}/SKILL.md", i),
                b"# in-scope",
            );
        }

        // 2,000 out-of-scope files across 40 pkg dirs × 50 files each.
        for i in 0..2_000usize {
            write_at(
                &code_repo,
                &format!("node_modules/pkg-{:03}/file-{:02}.js", i / 50, i % 50),
                b"// noise",
            );
        }

        let started = std::time::Instant::now();
        let report = sense_sync_scan_cwd(scan_cwd_params(
            tmp.path(),
            Some("alpha"),
            slug(),
            &code_repo,
        ))
        .await
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(
            report.untracked.len(),
            50,
            "expected exactly 50 in-scope files, got: {}",
            report.untracked.len()
        );
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "scan must complete in <200ms (got {:?}). If flaky raise to 500ms with a comment; \
             do NOT shrink the 2k out-of-scope fixture — it is the regression-trip mechanism.",
            elapsed
        );
    }

    // -----------------------------------------------------------------
    // Layer classifier sanity tests. The MoveOutcome / WriteOutcome
    // variant behavioral tests live below the registry tests and drive
    // through `classify_move_precondition` / `classify_write_precondition`.
    // -----------------------------------------------------------------

    #[test]
    fn layer_writability_classifier() {
        // SW-CP-7: every remaining layer is a writable filesystem layer.
        assert!(layer_is_writable(LayerId::TeamProjection));
    }

    // -----------------------------------------------------------------
    // RepoLockRegistry — serialization across two acquisitions.
    //
    // Review-iter-4 (P-9): each test uses a UNIQUE slug so multiple
    // acquire_repo_lock tests can run in parallel without sharing
    // registry entries. The `REPO_LOCKS` static persists for the life
    // of the process, so any test that uses the default `slug()` and
    // acquires a lock leaks that entry to sibling tests; later tests
    // hitting the same slug see the leaked Arc<Mutex<()>>. Distinct
    // slugs sidestep the issue without needing a `clear_for_test`
    // escape hatch (which would itself race with other tests holding
    // locks under the cleared slug).
    // -----------------------------------------------------------------

    #[test]
    fn public_slug_validation_rejects_provider_host_mismatch() {
        let malformed = SourceRepoSlug {
            owner: "acme".into(),
            repo: "api".into(),
            provider: "gitlab".into(),
            host: Some("github.com".into()),
        };
        assert!(validate_repo_slug(&malformed).is_err());
    }

    #[tokio::test]
    async fn repo_lock_registry_serializes_same_repo() {
        let slug_a = SourceRepoSlug::new("repo-lock-test", "serializes-same-repo").unwrap();
        let g1 = acquire_repo_lock(&slug_a).await;
        // try_lock from the same registry on the same slug must fail
        // while g1 is held.
        let mutex = repo_locks().get(&slug_a.remote_key()).unwrap().clone();
        assert!(
            mutex.try_lock().is_err(),
            "second acquisition must block while first is held"
        );
        drop(g1);
        assert!(
            mutex.try_lock().is_ok(),
            "lock must be re-acquirable once first guard drops"
        );
    }

    #[tokio::test]
    async fn repo_lock_registry_does_not_serialize_different_repos() {
        // P-9: unique slugs per test prevent cross-test registry
        // pollution (see module-level comment).
        let slug_a = SourceRepoSlug::new("repo-lock-test", "diff-repos-A").unwrap();
        let slug_b = SourceRepoSlug::new("repo-lock-test", "diff-repos-B").unwrap();
        let _g1 = acquire_repo_lock(&slug_a).await;
        // Holding slug_a's lock must NOT block slug_b's acquisition.
        let _g2 = acquire_repo_lock(&slug_b).await;
    }

    #[tokio::test]
    async fn repo_lock_registry_canonicalizes_legacy_and_explicit_github() {
        let legacy = SourceRepoSlug {
            owner: "Repo-Lock-Test".into(),
            repo: "Canonical-GitHub".into(),
            provider: "github".into(),
            host: None,
        };
        let explicit = SourceRepoSlug::new("repo-lock-test", "canonical-github").unwrap();
        let guard = acquire_repo_lock(&legacy).await;
        let mutex = repo_locks().get(&explicit.remote_key()).unwrap().clone();
        assert!(mutex.try_lock().is_err());
        drop(guard);
    }

    #[tokio::test]
    async fn workspace_lock_registry_serializes_same_key() {
        // A workspace subtree move (no repo slug) must still serialize on
        // a shared key so two concurrent moves can't interleave.
        let key = PathBuf::from("/tmp/ws-lock-test/serializes-same");
        let g1 = acquire_workspace_lock(&key).await;
        let mutex = workspace_locks().get(&key).unwrap().clone();
        assert!(
            mutex.try_lock().is_err(),
            "second acquisition must block while first is held"
        );
        drop(g1);
        assert!(
            mutex.try_lock().is_ok(),
            "lock must be re-acquirable once first guard drops"
        );
    }

    #[tokio::test]
    async fn workspace_lock_registry_does_not_serialize_different_keys() {
        let a = PathBuf::from("/tmp/ws-lock-test/diff-A");
        let b = PathBuf::from("/tmp/ws-lock-test/diff-B");
        let _g1 = acquire_workspace_lock(&a).await;
        // Different workspace keys must not block each other.
        let _g2 = acquire_workspace_lock(&b).await;
    }

    // ---- RF-11 LockPlan parity (AC-4) ----------------------------------

    #[tokio::test]
    async fn lockplan_repo_none_takes_no_guard() {
        // A workspace-only single-file op passes `Repo(None)` and keeps
        // today's no-lock behavior — byte-identical to the old
        // `let _lock = None`.
        let guards = LockPlan::Repo(None).acquire().await;
        assert!(guards.is_empty(), "Repo(None) must acquire nothing");
    }

    #[tokio::test]
    async fn lockplan_repo_some_takes_one_guard_and_serializes() {
        let slug = SourceRepoSlug::new("lockplan-test", "repo-some").unwrap();
        let guards = LockPlan::Repo(Some(&slug)).acquire().await;
        assert_eq!(guards.len(), 1, "Repo(Some) must acquire exactly one lock");
        // The same repo's mutex must now be held (serialization intact).
        let mutex = repo_locks().get(&slug.remote_key()).unwrap().clone();
        assert!(
            mutex.try_lock().is_err(),
            "the repo lock must be held while the guard is alive"
        );
        drop(guards);
        assert!(
            mutex.try_lock().is_ok(),
            "the repo lock must free once the guard drops"
        );
    }

    #[tokio::test]
    async fn lockplan_repo_set_takes_one_guard_per_slug() {
        // Distinct slugs → one guard each, no deadlock. Mirrors the
        // open-coded promote loop.
        let slugs = vec![
            SourceRepoSlug::new("lockplan-test", "set-a").unwrap(),
            SourceRepoSlug::new("lockplan-test", "set-b").unwrap(),
            SourceRepoSlug::new("lockplan-test", "set-c").unwrap(),
        ];
        let guards = LockPlan::RepoSet(&slugs).acquire().await;
        assert_eq!(
            guards.len(),
            slugs.len(),
            "RepoSet must acquire one lock per slug"
        );
    }

    #[tokio::test]
    async fn lockplan_workspace_pair_dedups_identical_roots() {
        // Identical roots must collapse to a single guard — same dedup the
        // old `acquire_workspace_locks` gave, so a same-root pair can't
        // self-deadlock on the non-reentrant mutex.
        let root = PathBuf::from("/tmp/ws-lock-test/lockplan-dedup");
        let guards = LockPlan::WorkspacePair(&root, &root).acquire().await;
        assert_eq!(
            guards.len(),
            1,
            "identical WorkspacePair roots must de-duplicate to one guard"
        );
    }

    #[tokio::test]
    async fn lockplan_repo_set_dedups_duplicate_slugs() {
        // A repeated slug must collapse to a single guard — else the second
        // acquisition of the same non-reentrant repo mutex would self-deadlock.
        // RepoSet owns this dedup so callers can't trip it (mirrors
        // WorkspacePair's identical-root dedup).
        let slug = SourceRepoSlug::new("lockplan-test", "dedup-me").unwrap();
        let slugs = vec![slug.clone(), slug.clone(), slug];
        let guards = LockPlan::RepoSet(&slugs).acquire().await;
        assert_eq!(
            guards.len(),
            1,
            "duplicate slugs in a RepoSet must de-duplicate to one guard"
        );
    }

    #[tokio::test]
    async fn lockplan_repo_set_sorts_regardless_of_caller_order() {
        // The deadlock-safety invariant (one global lock order) is owned by
        // `acquire`, enforced in release, not delegated to the caller: an
        // unsorted slice acquires the same set without deadlock.
        let slugs = vec![
            SourceRepoSlug::new("lockplan-test", "zzz-order").unwrap(),
            SourceRepoSlug::new("lockplan-test", "aaa-order").unwrap(),
            SourceRepoSlug::new("lockplan-test", "mmm-order").unwrap(),
        ];
        let guards = LockPlan::RepoSet(&slugs).acquire().await;
        assert_eq!(guards.len(), 3, "all distinct slugs acquired, no deadlock");
    }

    #[test]
    fn move_outcome_moved_serializes_with_count() {
        let v = serde_json::to_value(MoveOutcome::Moved { count: 3 }).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "moved", "count": 3 }));
    }

    #[test]
    fn move_outcome_moved_partial_serializes_with_skipped_names() {
        let v = serde_json::to_value(MoveOutcome::MovedPartial {
            moved: 2,
            skipped: vec![".claude/skills/bmad-tea/SKILL.md".to_string()],
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "movedPartial",
                "moved": 2,
                "skipped": [".claude/skills/bmad-tea/SKILL.md"]
            })
        );
    }

    #[test]
    fn conflict_policy_deserializes_camelcase_and_defaults_reject_whole() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::RejectWhole);
        let p: ConflictPolicy = serde_json::from_value(serde_json::json!("skipExisting")).unwrap();
        assert_eq!(p, ConflictPolicy::SkipExisting);
    }

    // -----------------------------------------------------------------
    // MoveOutcome behavioral tests (review-iter-1, AC-8)
    //
    // The full move command requires a Tauri AppHandle for the
    // SuppressionHandle, which is unavailable in unit tests. The
    // precondition cascade is extracted into `classify_move_precondition`
    // so 6 of the 8 MoveOutcome variants can be pinned behaviorally
    // without an AppHandle. The remaining 2 (Moved + RejectedCross-
    // Filesystem) need a live watcher and are covered by the workbench
    // integration test track in SS-10.
    // -----------------------------------------------------------------

    #[test]
    fn classify_move_same_layer() {
        let r = classify_move_precondition(LayerId::TeamProjection, LayerId::TeamProjection);
        assert_eq!(r.unwrap(), Some(MoveOutcome::RejectedSameLayer));
    }

    // Review-iter-4 (P-10): replaced the prior iter-1 std::fs smoke
    // test with a real behavioral test that drives
    // `classify_move_fs_preconditions` — the same helper the command
    // body invokes — on a real tempdir with NO source file. Confirms
    // the SS-13 helper returns `MoveOutcome::RejectedSourceMissing`,
    // not just that `std::fs::symlink_metadata` returns NotFound.
    #[test]
    fn classify_move_fs_preconditions_source_missing_when_from_path_absent() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("absent.md");
        let to_path = tmp.path().join("to-side.md");
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, false).unwrap();
        assert_eq!(outcome, Some(MoveOutcome::RejectedSourceMissing));
    }

    // Review-iter-4 (P-10): symlinked source path is also treated as
    // missing — `symlink_metadata` returns `Ok(m)` with `m.is_file()`
    // false. Pins the "symlinks at source are NOT moved" semantics
    // documented inline in the command body.
    #[test]
    #[cfg(unix)]
    fn classify_move_fs_preconditions_source_missing_when_from_path_is_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let real_target = tmp.path().join("target.md");
        fs::write(&real_target, b"data").unwrap();
        let from_path = tmp.path().join("link.md");
        symlink(&real_target, &from_path).unwrap();
        let to_path = tmp.path().join("to-side.md");
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, false).unwrap();
        assert_eq!(outcome, Some(MoveOutcome::RejectedSourceMissing));
    }

    // Review-iter-4 (P-11): AC-8 — `RejectedTargetExists` behavioral
    // coverage. Iter-1 patch claimed the variant was tested; iter-4
    // audit found it wasn't.
    #[test]
    fn classify_move_fs_preconditions_target_exists() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("source.md");
        fs::write(&from_path, b"source bytes").unwrap();
        let to_path = tmp.path().join("dest.md");
        fs::write(&to_path, b"existing dest bytes").unwrap();
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, false).unwrap();
        assert_eq!(outcome, Some(MoveOutcome::RejectedTargetExists));
    }

    // SS-FU-18: `force` skips the target-exists rejection so a "Replace"
    // promote can overwrite an existing file. Source present + target
    // present + force ⇒ None (proceed to the overwriting rename).
    #[test]
    fn classify_move_fs_preconditions_force_bypasses_target_exists() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("source.md");
        fs::write(&from_path, b"source bytes").unwrap();
        let to_path = tmp.path().join("dest.md");
        fs::write(&to_path, b"existing dest bytes").unwrap();
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, true).unwrap();
        assert_eq!(
            outcome, None,
            "force must bypass the target-exists rejection"
        );
    }

    // SS-FU-18: `force` must NEVER bypass the source-missing check — a
    // Replace over an absent source is still a rejected move, not a
    // silent no-op that the caller would mistake for success.
    #[test]
    fn classify_move_fs_preconditions_force_does_not_bypass_source_missing() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("absent.md");
        let to_path = tmp.path().join("dest.md");
        fs::write(&to_path, b"existing dest bytes").unwrap();
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, true).unwrap();
        assert_eq!(outcome, Some(MoveOutcome::RejectedSourceMissing));
    }

    // SS-FU-18 (review F2): `force` overwrites a regular FILE only. A
    // symlink target is a first-class entity — `rename(2)` would silently
    // unlink it — so the precondition rejects rather than clobbering, even
    // with force.
    #[cfg(unix)]
    #[test]
    fn classify_move_fs_preconditions_force_rejects_symlink_target() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("source.md");
        fs::write(&from_path, b"source bytes").unwrap();
        let real = tmp.path().join("real.md");
        fs::write(&real, b"linked bytes").unwrap();
        let to_path = tmp.path().join("dest.md");
        std::os::unix::fs::symlink(&real, &to_path).unwrap();
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, true).unwrap();
        assert_eq!(
            outcome,
            Some(MoveOutcome::RejectedTargetExists),
            "force must not clobber a symlink target"
        );
    }

    // SS-FU-18 (review F1/F2): `force` over a DIRECTORY target rejects
    // cleanly rather than failing the rename with a raw I/O error — the spec
    // requires a dir target be "left rejecting, not silently clobbered".
    #[test]
    fn classify_move_fs_preconditions_force_rejects_directory_target() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("source.md");
        fs::write(&from_path, b"source bytes").unwrap();
        let to_path = tmp.path().join("dest_dir");
        fs::create_dir(&to_path).unwrap();
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, true).unwrap();
        assert_eq!(outcome, Some(MoveOutcome::RejectedTargetExists));
    }

    // Sanity: when both sides are absent we surface SourceMissing first
    // (source check runs before target check). Locks the order documented
    // in the handler comment.
    #[test]
    fn classify_move_fs_preconditions_proceeds_when_source_present_target_absent() {
        let tmp = TempDir::new().unwrap();
        let from_path = tmp.path().join("source.md");
        fs::write(&from_path, b"source bytes").unwrap();
        let to_path = tmp.path().join("dest.md");
        let outcome = classify_move_fs_preconditions(&from_path, &to_path, false).unwrap();
        assert_eq!(
            outcome, None,
            "both checks must pass when source exists and target is absent"
        );
    }

    // Both layers backed ⇒ helper returns None (proceed to FS checks).
    #[test]
    fn classify_move_unbacked_returns_none_when_both_backed() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("alpha"));
        // Team content is backed with a team set.
        let outcome = classify_move_unbacked(LayerId::TeamProjection, LayerId::TeamProjection, &c);
        let from_backed = content_layer_root(LayerId::TeamProjection, &c).is_some();
        let to_backed = content_layer_root(LayerId::TeamProjection, &c).is_some();
        if from_backed && to_backed {
            assert_eq!(outcome, None, "both backed ⇒ no early rejection");
        } else {
            assert!(
                matches!(
                    outcome,
                    Some(MoveOutcome::RejectedSourceUnbacked)
                        | Some(MoveOutcome::RejectedTargetUnbacked)
                ),
                "at least one side unbacked ⇒ expected Rejected*Unbacked, got {:?}",
                outcome
            );
        }
    }

    // -----------------------------------------------------------------
    // WriteOutcome behavioral tests (review-iter-1, AC-8)
    // -----------------------------------------------------------------

    // Review-iter-4 (P-14): AC-8 — `WriteOutcome::RejectedTargetUnbacked`
    // behavioral coverage. SW-LT-1: Personal/Org tiers are now backed, so
    // the durable unbacked target is a Team tier with no active team.
    #[test]
    fn classify_write_unbacked_target_unbacked_when_layer_lacks_root() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), None);
        assert!(content_layer_root(LayerId::TeamProjection, &c).is_none());
        let outcome = classify_write_unbacked(LayerId::TeamProjection, &c);
        assert_eq!(outcome, Some(WriteOutcome::RejectedTargetUnbacked));
    }

    // Sanity: backed layer ⇒ helper returns None (proceed to write).
    #[test]
    fn classify_write_unbacked_returns_none_when_layer_backed() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("alpha"));
        assert!(content_layer_root(LayerId::TeamProjection, &c).is_some());
        let outcome = classify_write_unbacked(LayerId::TeamProjection, &c);
        assert_eq!(outcome, None);
    }

    // -----------------------------------------------------------------
    // _none team-name uniform rejection (review-iter-1)
    // -----------------------------------------------------------------

    #[test]
    fn derive_resolver_context_rejects_underscore_none_team_name() {
        for variant in ["_none", "_None", "_NONE", "_NoNe"] {
            let err = derive_resolver_context(
                PathBuf::from("/home"),
                42,
                "acme/api".into(),
                Some(variant.into()),
            )
            .unwrap_err();
            match err {
                Error::InvalidContextSegment { field, .. } => {
                    assert_eq!(field, "teamName", "variant {:?}", variant);
                }
                other => panic!("variant {:?} produced unexpected: {:?}", variant, other),
            }
        }
    }

    // -----------------------------------------------------------------
    // CONTENT_LAYER_PATTERNS glob compilation pinning (review-iter-1)
    // Catches a typo in a pattern at test time rather than at first
    // sense_sync_scan_cwd call hours into a session.
    // -----------------------------------------------------------------

    #[test]
    fn all_content_layer_patterns_compile() {
        for pat in CONTENT_LAYER_PATTERNS {
            globset::Glob::new(pat).unwrap_or_else(|e| {
                panic!("CONTENT_LAYER_PATTERNS entry {:?} invalid: {}", pat, e)
            });
        }
    }

    // SS-FU-3 invariant — sense_sync_scan_cwd's walker descends only into
    // `.claude/`. Every pattern must live under that prefix or the scanner
    // silently misses matches.
    #[test]
    fn all_content_layer_patterns_under_dot_claude() {
        for pat in CONTENT_LAYER_PATTERNS {
            assert!(
                pat.starts_with(".claude/"),
                "CONTENT_LAYER_PATTERNS entry {:?} is outside .claude/ — \
                 sense_sync_scan_cwd's walker only descends into .claude/. \
                 If you need to scan a different root, update SS-FU-3's allowlist.",
                pat
            );
        }
    }

    // -----------------------------------------------------------------
    // engine_error_to_command_error mapping (review-iter-1)
    // -----------------------------------------------------------------

    #[test]
    fn engine_error_invalid_team_name_maps_to_invalid_context_segment() {
        let err = content_engine::Error::InvalidTeamName {
            reason: "leading dot".into(),
        };
        match engine_error_to_command_error(err) {
            Error::InvalidContextSegment { field, .. } => assert_eq!(field, "teamName"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn engine_error_invalid_source_repo_slug_maps_to_invalid_context_segment() {
        let err = content_engine::Error::InvalidSourceRepoSlug {
            field: "owner",
            reason: "test".into(),
        };
        match engine_error_to_command_error(err) {
            Error::InvalidContextSegment { field, .. } => assert_eq!(field, "owner"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn engine_error_io_maps_to_io_with_engine_op() {
        let err = content_engine::Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test",
        ));
        match engine_error_to_command_error(err) {
            Error::Io { op, .. } => assert_eq!(op, "engine"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    // SS-6 Task 1.3 — classify_conflict_check coverage.
    //
    // The atomic conflict check inside sense_sync_write_to_layer is
    // factored into `classify_conflict_check` so its decision logic
    // (compare hashes, fold NotFound, fold spawn_blocking JoinError)
    // can be exercised without staging a tempdir or holding a Tauri
    // AppHandle. Test inputs are synthetic Result variants.

    #[test]
    fn conflict_check_proceeds_on_matching_hashes() {
        let expected = [7u8; 32];
        let actual = [7u8; 32];
        let result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError> =
            Ok(Ok(actual));
        assert_eq!(
            classify_conflict_check(result, expected),
            ConflictDecision::Proceed
        );
    }

    #[test]
    fn conflict_check_rejects_on_mismatched_hashes() {
        let expected = [0u8; 32];
        let mut actual = [0u8; 32];
        actual[0] = 1; // differ in the first byte
        let result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError> =
            Ok(Ok(actual));
        match classify_conflict_check(result, expected) {
            ConflictDecision::Reject(WriteOutcome::RejectedConflict { layer_current_hash }) => {
                assert_eq!(layer_current_hash, actual);
            }
            other => panic!("expected Reject(RejectedConflict), got {other:?}"),
        }
    }

    #[test]
    fn conflict_check_proceeds_when_layer_file_absent() {
        // NotFound from hash_file → "no prior state to conflict against"
        // → caller falls through to write (which creates the file).
        let expected = [0u8; 32];
        let not_found = content_engine::Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file gone",
        ));
        let result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError> =
            Ok(Err(not_found));
        assert_eq!(
            classify_conflict_check(result, expected),
            ConflictDecision::Proceed
        );
    }

    #[test]
    fn conflict_check_hash_failed_for_other_io_errors() {
        let expected = [0u8; 32];
        let denied = content_engine::Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "EACCES",
        ));
        let result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError> =
            Ok(Err(denied));
        match classify_conflict_check(result, expected) {
            ConflictDecision::HashFailed(msg) => {
                assert!(
                    msg.to_lowercase().contains("eacces") || msg.contains("permission"),
                    "diagnostic should surface the underlying error, got: {msg}"
                );
            }
            other => panic!("expected HashFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn conflict_check_join_failed_surfaces_panic() {
        // Synthesize a JoinError by panicking inside spawn_blocking.
        let result: Result<Result<[u8; 32], content_engine::Error>, tokio::task::JoinError> =
            tokio::task::spawn_blocking::<_, Result<[u8; 32], content_engine::Error>>(|| {
                panic!("test panic to produce JoinError")
            })
            .await;
        let expected = [0u8; 32];
        match classify_conflict_check(result, expected) {
            ConflictDecision::JoinFailed(msg) => {
                assert!(
                    msg.contains("blocking task join error"),
                    "diagnostic prefix should identify the source, got: {msg}"
                );
            }
            other => panic!("expected JoinFailed, got {other:?}"),
        }
    }

    #[test]
    fn write_outcome_rejected_conflict_serializes_with_camelcase_hash() {
        let outcome = WriteOutcome::RejectedConflict {
            layer_current_hash: [1u8; 32],
        };
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(
            json["kind"],
            serde_json::Value::String("rejectedConflict".into())
        );
        // The wire shape must use camelCase `layerCurrentHash` so TS
        // consumers can read it directly. The Rust field is snake_case
        // but the variant carries a #[serde(rename = "layerCurrentHash")]
        // override.
        assert!(
            json.get("layerCurrentHash").is_some(),
            "must serialize as layerCurrentHash (camelCase) on the wire; got: {json}"
        );
        // The hash itself is a JSON array of 32 numbers.
        let hash_array = json["layerCurrentHash"].as_array().expect("hash is array");
        assert_eq!(hash_array.len(), 32);
        assert!(hash_array.iter().all(|v| v.as_u64() == Some(1)));
    }

    // SS-FU-2 AC-4: cross-API parity between domain construction and
    // content_engine path-segment validation. `SourceRepoSlug::new` and
    // `validate_repo_slug` must agree on accept/reject — and now, with
    // whitespace-first ordering in `validate_segment`, on the *reason*
    // string too for whitespace cases. Direct struct-literal construction
    // bypasses `new()`'s validation so we can feed `validate_repo_slug` a
    // slug whose fields contain whitespace, exercising the path
    // `SS-FU-2` was written to guard.
    #[test]
    fn ac4_cross_api_parity_on_whitespace() {
        // Each row: (owner, repo, expected_reject_field). The expected
        // reason for all rows is "leading or trailing whitespace" on both
        // sides of the API (post-SS-FU-2). The field is the first
        // whitespace-bearing field encountered in the validator's order
        // (owner before repo) — `validate_repo_slug` returns "codeRepoSlug.<f>"
        // while `SourceRepoSlug::new` returns the bare `<f>`.
        let cases: [(&str, &str, &str); 5] = [
            (" alderco", "codesense", "owner"),
            ("alderco ", "codesense", "owner"),
            ("\talderco", "codesense", "owner"),
            ("alderco", " codesense", "repo"),
            // SS-FU-2 review iter-1 patch (Blind#2+#3) + iter-3 (Blind#1):
            // both-fields-dirty is the only row that actually pins owner-first
            // short-circuit ordering. Single-field-dirty rows (owner dirty,
            // repo clean — or vice versa) do NOT detect an order flip: under
            // a hypothetical repo-first order, the clean field passes its
            // check and the dirty field is still caught second, producing the
            // same `expected_field` answer. Only this row distinguishes the
            // two orderings — under repo-first it would report `"repo"`.
            (" alderco", " codesense", "owner"),
        ];

        for (owner, repo, expected_field) in cases {
            // Domain side: `SourceRepoSlug::new` must reject with the
            // expected field + whitespace reason.
            let domain_err = SourceRepoSlug::new(owner, repo).expect_err(
                "SourceRepoSlug::new should reject whitespace-padded segments after SS-FU-2",
            );
            match domain_err {
                DomainError::InvalidSourceRepoSegment { field, reason, .. } => {
                    assert_eq!(
                        field, expected_field,
                        "domain rejected wrong field for ({owner:?}, {repo:?})"
                    );
                    assert_eq!(
                        reason, "leading or trailing whitespace",
                        "domain rejected with wrong reason for ({owner:?}, {repo:?})"
                    );
                }
                other => panic!("unexpected domain error variant: {other:?}"),
            }

            // content_engine side: feed the same raw owner/repo to
            // `validate_repo_slug` via a struct-literal that bypasses
            // `new()`. Must also reject with the matching whitespace
            // reason on the corresponding `codeRepoSlug.<field>` path.
            let bypass = SourceRepoSlug {
                owner: owner.to_string(),
                repo: repo.to_string(),
                provider: "github".to_string(),
                host: Some("github.com".to_string()),
            };
            let engine_err = validate_repo_slug(&bypass)
                .expect_err("validate_repo_slug should reject whitespace-padded segments");
            match engine_err {
                Error::InvalidContextSegment { field, reason } => {
                    let expected_engine_field = match expected_field {
                        "owner" => "codeRepoSlug.owner",
                        "repo" => "codeRepoSlug.repo",
                        _ => unreachable!(),
                    };
                    assert_eq!(
                        field, expected_engine_field,
                        "engine rejected wrong field for ({owner:?}, {repo:?})"
                    );
                    assert_eq!(
                        reason, "leading or trailing whitespace",
                        "engine rejected with wrong reason for ({owner:?}, {repo:?})"
                    );
                }
                other => panic!("unexpected engine error variant: {other:?}"),
            }
        }
    }

    // SS-FU-2 review iter-1 patch (Edge#1): pure-whitespace inputs intentionally
    // diverge across the two APIs by reason wording. `SourceRepoSlug::new` returns
    // `EmptyField` (via the `!segment.trim().is_empty()` guard preserving AC-1's
    // carve-out), while `validate_repo_slug` returns `"leading or trailing whitespace"`.
    //
    // SS-FU-2 review iter-2 patch (Blind#1): correct the docstring rationale for
    // the engine-side reason. The order in `validate_path_segment` is empty → NUL →
    // separator → whitespace. For a non-empty all-whitespace input like `" "`, the
    // empty gate does *not* fire (the input has length), so control falls through
    // to the whitespace gate which then reports `"leading or trailing whitespace"`.
    // The whitespace gate fires *because* the earlier empty check did not — not
    // "before" it.
    //
    // SS-FU-2 review iter-2 patch (Edge#1): coverage extended to `\n`, `\r`, `\r\n`,
    // `\x0B`, `\x0C`. All are `char::is_whitespace`, so they trim to empty and
    // exercise the same divergence path as space + tab. NBSP and other Unicode
    // *whitespace* characters are also handled correctly here (not deferred);
    // Unicode *format* characters that are NOT whitespace per `char::is_whitespace`
    // — ZWSP / BOM / WJ — are what's actually deferred per iter-1's deferred-work.md.
    //
    // SS-FU-2 review iter-2 patch (Edge#2): added repo-field rows so the asymmetric-
    // reason contract is locked on the `repo` side too (owner is normally validated
    // first; with a clean owner the repo path fires).
    #[test]
    fn ac4_cross_api_pure_whitespace_diverges_on_reason_but_both_reject() {
        let cases: &[(&str, &str, &str)] = &[
            // (owner_input, repo_input, expected_field_label)
            (" ", "r", "owner"),
            ("  ", "r", "owner"),
            ("\t", "r", "owner"),
            ("\t \t", "r", "owner"),
            ("\n", "r", "owner"),
            ("\r", "r", "owner"),
            ("\r\n", "r", "owner"),
            ("\x0B", "r", "owner"),
            ("\x0C", "r", "owner"),
            // Repo-side coverage (Edge#2): owner clean, repo pure-whitespace.
            ("o", " ", "repo"),
            ("o", "\t", "repo"),
            ("o", "\n", "repo"),
        ];

        for &(owner, repo, expected_field) in cases {
            // Domain rejects as EmptyField on the corresponding field.
            let domain_err = SourceRepoSlug::new(owner, repo)
                .expect_err(&format!("domain must reject ({owner:?}, {repo:?})"));
            match domain_err {
                DomainError::EmptyField { field } => assert_eq!(
                    field, expected_field,
                    "domain rejected wrong field for ({owner:?}, {repo:?})"
                ),
                other => {
                    panic!("({owner:?}, {repo:?}): domain should report EmptyField, got: {other:?}")
                }
            }

            // Engine rejects with the whitespace reason on the corresponding
            // `codeRepoSlug.<field>` path.
            let bypass = SourceRepoSlug {
                owner: owner.to_string(),
                repo: repo.to_string(),
                provider: "github".to_string(),
                host: Some("github.com".to_string()),
            };
            let engine_err = validate_repo_slug(&bypass)
                .expect_err(&format!("engine must reject ({owner:?}, {repo:?})"));
            match engine_err {
                Error::InvalidContextSegment { field, reason } => {
                    let expected_engine_field = match expected_field {
                        "owner" => "codeRepoSlug.owner",
                        "repo" => "codeRepoSlug.repo",
                        _ => unreachable!(),
                    };
                    assert_eq!(
                        field, expected_engine_field,
                        "engine rejected wrong field for ({owner:?}, {repo:?})"
                    );
                    assert_eq!(
                        reason, "leading or trailing whitespace",
                        "({owner:?}, {repo:?}): engine reason drift"
                    );
                }
                other => panic!("({owner:?}, {repo:?}): unexpected engine error: {other:?}"),
            }
        }
    }

    // Belt-and-braces: positive parity — a clean slug must pass both
    // validators. Catches a future regression where one side accidentally
    // tightens past the other.
    #[test]
    fn ac4_cross_api_parity_accepts_clean_slug() {
        let clean = SourceRepoSlug::new("alderco", "codesense")
            .expect("clean slug must pass SourceRepoSlug::new");
        validate_repo_slug(&clean).expect("clean slug must pass validate_repo_slug");
    }

    // ---------- SW-EW-5: ensure_epic_workspace registration ----------

    #[tokio::test]
    async fn ensure_epic_workspace_creates_plain_dir_and_registers() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();

        // SW-EW-15: the 5th arg is now the epic DOC PATH; the id is derived
        // from its `{parent}__{leaf}` (here `epics__SW-EW`).
        let epic_doc = "/ctx/content/epics/SW-EW/epic.md";
        let (handle, _epic_id) =
            ensure_epic_workspace_inner(&home, 42, "acme/api", "default", epic_doc)
                .expect("registration should succeed");

        // AC-1: synthetic slug, parent-disambiguated. AC-5: dir exists, plain.
        assert_eq!(handle.slug, "_epic/epics__SW-EW");
        assert!(handle.registered);
        // SW-CP-15 Task 10: rooted under the per-user home, outside the clone.
        let dir = tmp.path().join("users/42/_epic/acme/api/epics__SW-EW");
        assert_eq!(handle.dir_path, dir.to_string_lossy());
        assert!(dir.is_dir(), "epic dir must exist");
        assert!(
            !dir.join(".git").exists(),
            "must be a plain folder, no .git"
        );

        // AC-2: OUTSIDE the context-repo clone — `org_root` IS the clone's git
        // root, so the epic dir must not be a descendant of it.
        let org_root =
            crate::context_repo::path_to_org_root(tmp.path(), &Principal::from(42u64), "acme/api")
                .unwrap();
        assert!(!dir.starts_with(&org_root));

        // AC-4: the dir enumerates as an attached target for its slug.
        let ctx = derive_resolver_context(
            tmp.path().to_path_buf(),
            42,
            "acme/api".to_string(),
            Some("default".to_string()),
        )
        .unwrap();
        let slug = crate::domain::derive_epic_workspace_slug("epics__SW-EW").unwrap();
        let attached =
            crate::content_engine::enumerate_attached_code_repos_for_slug(&ctx, &slug).unwrap();
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].code_repo_path, dir);
    }

    #[tokio::test]
    async fn ensure_epic_workspace_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let call = || {
            ensure_epic_workspace_inner(
                &home,
                42,
                "acme/api",
                "default",
                "/ctx/content/epics/SW-EW/epic.md",
            )
        };

        let (first, _) = call().unwrap();
        // Re-open: must reuse the dir + the existing lockfile, no error, no dup.
        let (second, _) = call().unwrap();
        assert_eq!(first.dir_path, second.dir_path);
        assert!(second.registered);

        // SW-IS-3c: registration upserts into the store (the sole substrate), not a
        // JSON lockfile — so idempotency is proven by exactly one store target for
        // this slug after two registrations, not a `.lock.json` file count.
        let ctx = derive_resolver_context(
            tmp.path().to_path_buf(),
            42,
            "acme/api".to_string(),
            Some("default".to_string()),
        )
        .unwrap();
        let slug = crate::domain::derive_epic_workspace_slug("epics__SW-EW").unwrap();
        let attached =
            crate::content_engine::enumerate_attached_code_repos_for_slug(&ctx, &slug).unwrap();
        let for_dir = attached
            .iter()
            .filter(|r| r.code_repo_path == Path::new(&first.dir_path))
            .count();
        assert_eq!(
            for_dir, 1,
            "exactly one store target after two registrations"
        );
    }

    #[tokio::test]
    async fn ensure_epic_workspace_rejects_empty_epic_file_path() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        // An empty doc path (e.g. the Ungrouped sentinel) must not register and
        // must not panic.
        let err = ensure_epic_workspace_inner(&home, 42, "acme/api", "default", "").unwrap_err();
        assert!(err.contains("invalid epic id"), "got: {err}");
    }

    // ---------- agent status hooks (per-launch settings merge) ----------

    /// Every hook command registered for `event` in `v`.
    fn hook_commands(v: &serde_json::Value, event: &str) -> Vec<String> {
        v["hooks"][event]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn notify_count(v: &serde_json::Value, event: &str) -> usize {
        hook_commands(v, event)
            .iter()
            .filter(|c| c.contains("codesense-notify"))
            .count()
    }

    #[tokio::test]
    async fn ensure_epic_workspace_carries_the_hand_launch_hooks() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let (handle, _) =
            ensure_epic_workspace_inner(&home, 42, "acme/api", "default", "/ctx/epics/SW-EW/e.md")
                .unwrap();
        let dir = Path::new(&handle.dir_path);
        let local: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        for ev in status_hook_events(AgentCli::Claude) {
            assert_eq!(notify_count(&local, ev), 1, "event {ev} wired once");
        }
        assert!(
            !dir.join(".claude/settings.json").exists(),
            "the shared settings.json is never written"
        );
    }

    #[test]
    fn launch_hooks_wire_every_event_once_for_a_folder_with_no_settings() {
        let tmp = TempDir::new().unwrap();
        let mut v = serde_json::json!({ "permissions": { "allow": ["Read"] } });
        merge_launch_notify_hooks(&mut v, tmp.path(), AgentCli::Claude);
        for ev in status_hook_events(AgentCli::Claude) {
            assert_eq!(notify_count(&v, ev), 1, "event {ev} wired once");
        }
        assert_eq!(v["permissions"]["allow"], serde_json::json!(["Read"]));
        let stop = hook_commands(&v, "Stop");
        // A turn end re-arms the inbox waiter, which tells the agent about new mail.
        assert_eq!(
            stop,
            vec![notify_command().to_string(), inbox_hook(&v, "Stop")]
        );
        assert_eq!(
            hook_commands(&v, "SessionStart"),
            vec![inbox_hook(&v, "SessionStart")]
        );
        // Re-merging is idempotent.
        merge_launch_notify_hooks(&mut v, tmp.path(), AgentCli::Claude);
        for ev in status_hook_events(AgentCli::Claude) {
            assert_eq!(notify_count(&v, ev), 1, "event {ev} still once");
        }
        for ev in claude_inbox_events() {
            assert_eq!(
                hook_commands(&v, ev)
                    .iter()
                    .filter(|c| c.contains(INBOX_ARG))
                    .count(),
                1,
                "{ev}"
            );
        }
        assert!(
            !tmp.path().join(".claude").exists(),
            "the folder is only read"
        );
    }

    /// The events Claude's inbox port arms its waiter at.
    fn claude_inbox_events() -> Vec<&'static str> {
        adapter(AgentCli::Claude).inbox().events()
    }

    /// What every inbox hook runs.
    const INBOX_ARG: &str = codesense_pty_core::agent_cli::INBOX_WAIT_ARG;

    /// The inbox hook `v` arms at `event`.
    fn inbox_hook(v: &serde_json::Value, event: &str) -> String {
        hook_commands(v, event)
            .into_iter()
            .find(|c| c.contains(INBOX_ARG))
            .unwrap_or_else(|| panic!("no inbox hook at {event}"))
    }

    #[test]
    fn the_inbox_waiter_rides_a_claude_launch_and_never_a_folders_own_settings() {
        let tmp = TempDir::new().unwrap();
        // A launch's settings carry its CLI's inbox hooks, armed once whatever re-merges.
        let mut launch = serde_json::json!({});
        merge_launch_notify_hooks(&mut launch, tmp.path(), AgentCli::Claude);
        merge_launch_notify_hooks(&mut launch, tmp.path(), AgentCli::Claude);
        let mut required = serde_json::json!({});
        merge_required_launch_notify_hooks(&mut required, tmp.path(), AgentCli::Claude).unwrap();
        for v in [&launch, &required] {
            for ev in claude_inbox_events() {
                assert_eq!(
                    inbox_hook(v, ev),
                    codesense_pty_core::agent_cli::inbox_hook_command(AgentCli::Claude, ev)
                );
            }
        }
        // The managed worktree's own settings never get it: the waiter runs the helper of
        // the host a launch runs on, which a folder cannot name.
        let mut local = serde_json::json!({});
        merge_local_notify_hooks(&mut local, tmp.path(), AgentCli::Claude);
        assert!(!local.to_string().contains(INBOX_ARG));
        // An agent on a machine gets its inbox hooks from its machine's host.
        let mut remote = serde_json::json!({});
        merge_remote_notify_hooks(&mut remote, AgentCli::Claude).unwrap();
        assert!(!remote.to_string().contains(INBOX_ARG));
        // A CLI with no status hooks whose inbox hooks ride its argv (Codex) gets nothing
        // in its settings: its launch's own preparing adds its checks.
        let mut codex = serde_json::json!({});
        merge_launch_notify_hooks(&mut codex, tmp.path(), AgentCli::Codex);
        merge_required_launch_notify_hooks(&mut codex, tmp.path(), AgentCli::Codex).unwrap();
        merge_remote_notify_hooks(&mut codex, AgentCli::Codex).unwrap();
        assert!(
            codex["hooks"].as_object().is_none_or(|hooks| hooks
                .values()
                .all(|groups| groups.as_array().is_some_and(Vec::is_empty))),
            "{codex}"
        );
    }

    #[test]
    fn launch_hooks_skip_events_the_folder_settings_already_wire() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let group =
            |cmd: &str| serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd }] }]);
        fs::write(
            claude.join("settings.json"),
            serde_json::json!({ "hooks": { "Stop": group(notify_command()) } }).to_string(),
        )
        .unwrap();
        fs::write(
            claude.join("settings.local.json"),
            serde_json::json!({ "hooks": {
                "PermissionRequest": group("x \"$CODESENSE_ACTIVITY_HOST/codesense-notify.sh\" y"),
            } })
            .to_string(),
        )
        .unwrap();
        let mut v = serde_json::json!({});
        merge_launch_notify_hooks(&mut v, tmp.path(), AgentCli::Claude);
        assert_eq!(notify_count(&v, "Stop"), 0, "settings.json already runs it");
        assert_eq!(
            notify_count(&v, "PermissionRequest"),
            0,
            "settings.local.json already runs it"
        );
        for ev in status_hook_events(AgentCli::Claude)
            .filter(|ev| !["Stop", "PermissionRequest"].contains(ev))
        {
            assert_eq!(notify_count(&v, ev), 1, "event {ev}");
            assert_eq!(hook_commands(&v, ev)[0], notify_command(), "event {ev}");
        }
    }

    /// Run the wired hook command the way Claude does (`sh -c`), in a tab of the host
    /// whose activity root is `host`, with `stdin` piped in.
    fn run_hook_command(host: &Path, stdin: &str) -> std::process::Output {
        use std::io::Write;
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(notify_command())
            .env(codesense_pty_core::activity::HOST_ENV, host)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // `sh` may exit before reading stdin (the script-absent case): a broken
        // pipe is expected there, any other write error is not.
        if let Err(e) = child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "stdin write: {e}");
        }
        child.wait_with_output().unwrap()
    }

    #[test]
    fn guarded_hook_command_is_silent_success_when_script_absent() {
        let tmp = TempDir::new().unwrap();
        let out = run_hook_command(tmp.path(), r#"{"hook_event_name":"Stop"}"#);
        assert!(out.status.success(), "exit {:?}", out.status);
        assert!(out.stdout.is_empty(), "stdout {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "stderr {:?}", out.stderr);
    }

    #[test]
    fn guarded_hook_command_runs_script_with_stdin_when_present() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join(codesense_pty_core::notify_hook::NAME);
        fs::write(
            &script,
            "#!/bin/sh\ncat > \"$CODESENSE_ACTIVITY_HOST/got.json\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let payload = r#"{"hook_event_name":"PermissionRequest"}"#;
        let out = run_hook_command(tmp.path(), payload);
        assert!(out.status.success());
        assert_eq!(
            fs::read_to_string(tmp.path().join("got.json")).unwrap(),
            payload,
            "the hook JSON reaches the script on stdin"
        );
        assert!(notify_command().contains(&codesense_pty_core::notify_hook::installed_script()));
    }

    /// The secret-file guard rides next to the status hooks: one `PreToolUse` group scoped to the
    /// reading tools, added once however often the hooks are merged, with every other hook kept.
    #[test]
    fn claude_launch_hooks_carry_one_secret_file_guard_beside_the_status_hooks() {
        let mut settings = serde_json::json!({
            "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "mine.sh" }] }] },
            "permissions": { "deny": ["WebFetch"] }
        });
        for _ in 0..2 {
            merge_remote_notify_hooks(&mut settings, AgentCli::Claude).unwrap();
        }
        let groups = settings["hooks"]["PreToolUse"].as_array().unwrap();
        let guards: Vec<_> = groups
            .iter()
            .filter(|g| g["hooks"][0]["command"].as_str().is_some_and(|c| c.contains("codesense-secret-guard")))
            .collect();
        assert_eq!(guards.len(), 1, "added once: {groups:?}");
        // Claude's own deny rules ride along, once, beside whatever the file already denies.
        let denies = settings["permissions"]["deny"].as_array().unwrap();
        for rule in codesense_pty_core::notify_hook::deny_rules() {
            assert_eq!(
                denies.iter().filter(|d| d.as_str() == Some(rule.as_str())).count(),
                1,
                "{rule}"
            );
        }
        assert!(denies.contains(&serde_json::json!("Read(**/*.pem)")));
        assert!(denies.contains(&serde_json::json!("Read(**/id_rsa*)")));
        assert!(denies.contains(&serde_json::json!("WebFetch")), "the file's own rules stay");
        assert_eq!(guards[0]["matcher"], "Read|Grep|Glob|Bash");
        assert!(groups.iter().any(|g| g["hooks"][0]["command"] == "mine.sh"), "the user's own hook stays");
        assert!(groups.iter().any(|g| g["hooks"][0]["command"].as_str().is_some_and(|c| c.contains("codesense-notify"))));
        // A CLI with no project hooks gets none.
        let mut codex = serde_json::json!({});
        merge_remote_notify_hooks(&mut codex, AgentCli::Codex).unwrap();
        assert_eq!(codex, serde_json::json!({}));
    }

    /// SW-AN-51: the committed `.claude/settings.json` must stay in lockstep with
    /// the writer's command string: the dedup check (`event_runs_notify_hook`) counts any
    /// command naming the installed script, so a drifted one would silently survive every
    /// regen — this test fails loudly instead. Compiled only where the file exists.
    #[cfg(cs_claude_settings)]
    #[test]
    fn committed_claude_settings_hooks_match_writer_command_verbatim() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .join(".claude/settings.json");
        let body =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let settings: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        let hooks = settings
            .get("hooks")
            .and_then(|h| h.as_object())
            .unwrap_or_else(|| panic!("{} missing hooks object", path.display()));
        assert!(!hooks.is_empty(), "{} has no hooks events", path.display());
        let mut saw_notify = false;
        for (event, groups) in hooks {
            let groups = groups.as_array().expect("event value is an array");
            for group in groups {
                let items = group
                    .get("hooks")
                    .and_then(|hs| hs.as_array())
                    .expect("group has hooks array");
                for item in items {
                    let command = item
                        .get("command")
                        .and_then(|c| c.as_str())
                        .expect("hook has command string");
                    if command.contains("codesense-notify") {
                        saw_notify = true;
                        assert_eq!(
                            command,
                            notify_command(),
                            "{} event {event} drifted from writer",
                            path.display()
                        );
                    }
                }
            }
        }
        assert!(
            saw_notify,
            "{} has no codesense-notify hook (nothing to guard)",
            path.display()
        );
    }

    // ----- SW-LT-4: promote-to-team routing -------------------------------

    #[test]
    fn classify_promote_target_rejects_same_team() {
        let chain = vec!["self".to_string(), "org-root".to_string()];
        assert_eq!(
            classify_promote_target(&chain, "self", "self"),
            Some(PromoteOutcome::RejectedSameTeam)
        );
    }

    #[test]
    fn classify_promote_target_rejects_team_outside_chain() {
        // `sibling` is not in self's inheritance chain → not a reachable target.
        let chain = vec!["self".to_string(), "org-root".to_string()];
        assert_eq!(
            classify_promote_target(&chain, "self", "sibling"),
            Some(PromoteOutcome::RejectedTargetNotInChain)
        );
    }

    #[test]
    fn classify_promote_target_allows_chain_member_both_directions() {
        let chain = vec![
            "self".to_string(),
            "platform".to_string(),
            "org-root".to_string(),
        ];
        // Promote up (self → org-root) and back (org-root → self) both pass.
        assert_eq!(classify_promote_target(&chain, "self", "org-root"), None);
        assert_eq!(classify_promote_target(&chain, "org-root", "self"), None);
        assert_eq!(classify_promote_target(&chain, "self", "platform"), None);
    }

    /// SW-IS-3b: seed a lockfile into the STORE (the sole source of truth that
    /// `rewrite_promoted_lockfile_entries` both enumerates from and reads/writes).
    fn seed_store_lock(c: &ResolverContext, s: &SourceRepoSlug, lf: &Lockfile) {
        let store = crate::content_engine::sync_state_store::SyncStateStore::open(
            crate::content_engine::sync_state_store::default_store_path(&c.codesense_home),
        )
        .unwrap();
        store.upsert(c.user_id, s, lf).unwrap();
    }

    /// SW-IS-3b: read a target's persisted state back from the store.
    fn read_store_lock(
        c: &ResolverContext,
        _s: &SourceRepoSlug,
        code_repo_path: &Path,
    ) -> Lockfile {
        let store = crate::content_engine::sync_state_store::SyncStateStore::open(
            crate::content_engine::sync_state_store::default_store_path(&c.codesense_home),
        )
        .unwrap();
        read_through(&store, c, code_repo_path)
            .unwrap()
            .expect("target present in store")
    }

    /// Build a `MaterializedEntry` with a specific logical name + source path
    /// so the rewrite test can assert selective, exact-match rewriting. The store
    /// serves only projection entries (a retired install tier's wave entry is
    /// drained on read), so this fixture is a projection entry — the mechanism under test
    /// (repoint by `(logical_name, source_path)`) is layer-agnostic.
    fn promote_lf_entry(
        logical_name: &str,
        source_path: &Path,
    ) -> crate::content_engine::MaterializedEntry {
        crate::content_engine::MaterializedEntry {
            last_materialized_size: None,
            logical_name: logical_name.to_string(),
            dest_path: PathBuf::from("/abs/wt").join(logical_name),
            source_layer: LayerId::TeamProjection,
            source_path: source_path.to_path_buf(),
            last_materialized_hash: [0u8; 32],
            last_materialized_mtime_ns: 0,
            projection_key: Some(logical_name.to_string()),
            source_oid: None,
        }
    }

    #[test]
    fn rewrite_promoted_lockfile_entries_repoints_only_the_matching_entry() {
        // AC-2b / AC-7: rewriting `source_path` self→org-root touches ONLY the
        // matching (logical_name, old source_path) entry; a sibling entry is
        // left byte-identical (default round-trip unchanged).
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("self"));
        let s = slug();
        let code_repo_path = Path::new("/abs/wt");

        // SLW-UD-8: the team-level install tier is retired (its `install` marker
        // dropped from `LAYER_MARKER_DIRS`), so promote now targets per-repo
        // install content — `team_from_source_path` anchors on `repos`, the first
        // marker before the nested `install`.
        let old_src = PathBuf::from("/ctx/self/repos/o/r/install/skills/x.md");
        let new_src = PathBuf::from("/ctx/org-root/repos/o/r/install/skills/x.md");
        let untouched_src = PathBuf::from("/ctx/self/repos/o/r/install/skills/y.md");

        let mut lf = Lockfile::empty(
            c.org_slug.clone(),
            Some("self".to_string()),
            code_repo_path.to_path_buf(),
        );
        lf.entries.push(promote_lf_entry("skills/x.md", &old_src));
        lf.entries
            .push(promote_lf_entry("skills/y.md", &untouched_src));
        seed_store_lock(&c, &s, &lf);

        let count = rewrite_promoted_lockfile_entries(
            &c,
            std::slice::from_ref(&s),
            "skills/x.md",
            &old_src,
            &new_src,
        )
        .unwrap();
        assert_eq!(count, 1, "exactly one repo's lockfile rewritten");

        let reloaded = read_store_lock(&c, &s, code_repo_path);
        let x = reloaded
            .entries
            .iter()
            .find(|e| e.logical_name == "skills/x.md")
            .unwrap();
        assert_eq!(
            x.source_path, new_src,
            "promoted entry now points at org-root"
        );
        // AC-7 / D1: the OWNING TEAM is now derivable from the new source_path —
        // this is the whole "record the team via source_path, no stored field"
        // design. Assert via the same single-source-of-truth helper the resolver
        // + provenance use, not just raw string equality.
        assert_eq!(
            team_from_source_path(&x.source_path).as_deref(),
            Some("org-root"),
            "team_from_source_path derives the promoted-to team from the new path"
        );
        assert_eq!(
            x.source_layer,
            LayerId::TeamProjection,
            "layer KIND unchanged by promote"
        );
        let y = reloaded
            .entries
            .iter()
            .find(|e| e.logical_name == "skills/y.md")
            .unwrap();
        assert_eq!(y.source_path, untouched_src, "sibling entry untouched");
        assert_eq!(
            team_from_source_path(&y.source_path).as_deref(),
            Some("self"),
            "un-promoted sibling still owned by self (round-trip unchanged)"
        );
    }

    #[test]
    fn rewrite_promoted_lockfile_entries_covers_every_slug_of_a_workspace_file() {
        // AC-2b / F1: a team-content file is slug-agnostic — materialized into
        // EVERY attached repo, so its stale source_path lives in every per-repo
        // lockfile. Promote must rewrite ALL of them, not just the viewed slug.
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("self"));
        let slug_a = SourceRepoSlug::new("alderco", "repo-a").unwrap();
        let slug_b = SourceRepoSlug::new("alderco", "repo-b").unwrap();

        // Same slug-agnostic workspace source_path in both repos' lockfiles.
        let old_src = PathBuf::from("/ctx/self/install/skills/x.md");
        let new_src = PathBuf::from("/ctx/org-root/install/skills/x.md");
        for (slug, wt) in [(&slug_a, "/abs/wt-a"), (&slug_b, "/abs/wt-b")] {
            let wt_path = Path::new(wt);
            let mut lf =
                Lockfile::empty(c.org_slug.clone(), Some("self".to_string()), wt_path.into());
            lf.entries.push(promote_lf_entry("skills/x.md", &old_src));
            seed_store_lock(&c, slug, &lf);
        }

        // Rewrite fanned over BOTH slugs → both lockfiles updated.
        let count = rewrite_promoted_lockfile_entries(
            &c,
            &[slug_a.clone(), slug_b.clone()],
            "skills/x.md",
            &old_src,
            &new_src,
        )
        .unwrap();
        assert_eq!(count, 2, "both repos' lockfiles rewritten");

        for (slug, wt) in [(&slug_a, "/abs/wt-a"), (&slug_b, "/abs/wt-b")] {
            let reloaded = read_store_lock(&c, slug, Path::new(wt));
            assert_eq!(
                reloaded.entries[0].source_path, new_src,
                "workspace entry repointed in {wt}"
            );
        }
    }

    #[test]
    fn rewrite_promoted_lockfile_entries_skips_missing_lockfile() {
        // No lockfile on disk for this (team, slug) → nothing to rewrite, no error.
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("self"));
        let count = rewrite_promoted_lockfile_entries(
            &c,
            std::slice::from_ref(&slug()),
            "skills/x.md",
            Path::new("/ctx/self/install/skills/x.md"),
            Path::new("/ctx/org-root/install/skills/x.md"),
        )
        .unwrap();
        assert_eq!(count, 0);
    }
}

#[cfg(test)]
mod epic_worktree_command_tests {
    use super::*;
    use crate::context_repo::test_fixtures::init_source_clone;
    use std::path::Path;

    const UID: u64 = 42;
    const ORG: &str = "acme/api";
    const TEAM: &str = "default";
    // SW-EW-15: the epic id is now derived from the doc path's `{parent}__{leaf}`.
    // `write_epic_doc` nests the doc under `epics/SW-EW/` so the derived id is
    // this stable value, which assembly + teardown both target.
    const EPIC: &str = "epics__SW-EW";

    /// Register a source clone as a team-attached code repo (the lookup
    /// `resolve_epic_repo_clones` reads).
    fn register(home: &str, slug: &str, clone: &Path) {
        let ctx = derive_resolver_context(
            PathBuf::from(home),
            UID,
            ORG.to_string(),
            Some(TEAM.to_string()),
        )
        .unwrap();
        let s = SourceRepoSlug::parse_owner_repo(slug).unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &s, clone).unwrap();
    }

    /// Write an epic doc with `repos:` (+ optional `branch:`) frontmatter,
    /// returning its path.
    fn write_epic_doc(dir: &Path, repos: &[&str], branch: Option<&str>) -> String {
        let mut fm = String::from("---\ntype: epic\n");
        if let Some(b) = branch {
            fm.push_str(&format!("branch: {b}\n"));
        }
        if !repos.is_empty() {
            fm.push_str("repos:\n");
            for r in repos {
                fm.push_str(&format!("  - {r}\n"));
            }
        }
        fm.push_str("---\n# Epic\n");
        // SW-EW-15: nest under `epics/SW-EW/` so the doc-path-derived id is the
        // stable `epics__SW-EW` (== EPIC) that assembly + teardown both target.
        let epic_dir = dir.join("epics").join("SW-EW");
        std::fs::create_dir_all(&epic_dir).unwrap();
        let p = epic_dir.join("epic.md");
        std::fs::write(&p, fm).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn assemble(
        home: &str,
        epic_doc: &str,
    ) -> impl std::future::Future<Output = Result<AssembleEpicResult, String>> {
        // Drives the AppHandle-free core directly (the inheritance-wave enqueue
        // + dev-env spawn the command wraps around it need a Tauri harness).
        let home = home.to_string();
        let epic_doc = epic_doc.to_string();
        async move {
            assemble_epic_worktrees_core(
                home,
                UID,
                ORG.to_string(),
                TEAM.to_string(),
                EPIC.to_string(),
                epic_doc,
            )
            .await
            .map(|(result, _org_root, _setups)| result)
        }
    }

    fn reconcile(
        home: &str,
        epic_doc: &str,
        in_use: Vec<String>,
    ) -> impl std::future::Future<Output = Result<TeardownEpicResult, String>> {
        // Drives the AppHandle-free core directly (the command wrapper's only extra
        // is the best-effort `worktree-changed` emit, which needs a Tauri harness).
        reconcile_epic_worktrees_core(
            home.to_string(),
            UID,
            ORG.to_string(),
            epic_doc.to_string(),
            in_use,
        )
    }

    /// `{home}/users/{uid}/_epic/{org_slug}/{epic}/{owner}/{repo}`.
    fn wt_path(home: &Path, owner: &str, repo: &str) -> PathBuf {
        // SW-CP-15 Task 10: epic workspaces root under the per-user home
        // (`users/{uid}/_epic/{org_slug}/{id}/`), outside the context-repo clone.
        let ws =
            crate::context_repo::path_to_epic_workspace(home, &Principal::from(UID), ORG, EPIC);
        crate::context_repo::path_to_integration_worktree(
            &ws,
            &SourceRepoSlug::new(owner, repo).unwrap(),
        )
    }

    #[tokio::test]
    async fn assembles_multiple_repos_in_parallel() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone_a = tempfile::tempdir().unwrap();
        let clone_b = tempfile::tempdir().unwrap();
        init_source_clone(clone_a.path(), "main");
        init_source_clone(clone_b.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone_a.path());
        register(&h, "acme/b", clone_b.path());
        let doc = write_epic_doc(docs.path(), &["acme/a", "acme/b"], None);

        let res = assemble(&h, &doc).await.unwrap();

        assert_eq!(res.branch, "integration/epics__SW-EW");
        assert_eq!(res.assembled.len(), 2);
        assert!(res.failures.is_empty());
        // Same-volume tempdirs → no spurious cross-volume warnings (AC-7/D4).
        assert!(res.assembled.iter().all(|w| w.warnings.is_empty()));
        assert!(wt_path(home.path(), "acme", "a")
            .join("README.md")
            .is_file());
        assert!(wt_path(home.path(), "acme", "b")
            .join("README.md")
            .is_file());
    }

    #[tokio::test]
    async fn branch_override_is_honored() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        let doc = write_epic_doc(docs.path(), &["acme/a"], Some("release/custom"));

        let res = assemble(&h, &doc).await.unwrap();
        assert_eq!(res.branch, "release/custom");
        assert_eq!(res.assembled.len(), 1);
        let repo = git2::Repository::open(clone.path()).unwrap();
        assert!(repo.find_reference("refs/heads/release/custom").is_ok());
    }

    #[tokio::test]
    async fn empty_repos_is_noop_success() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let h = home.path().to_string_lossy().into_owned();
        let doc = write_epic_doc(docs.path(), &[], None);

        let res = assemble(&h, &doc).await.unwrap();
        assert!(res.assembled.is_empty());
        assert!(res.failures.is_empty());
        assert_eq!(res.branch, "integration/epics__SW-EW");
    }

    #[tokio::test]
    async fn repo_with_no_local_clone_fails_without_aborting_siblings() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let good = tempfile::tempdir().unwrap();
        init_source_clone(good.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/good", good.path());
        // `acme/missing` is in the epic doc but not registered → no local clone.
        let doc = write_epic_doc(docs.path(), &["acme/good", "acme/missing"], None);

        let res = assemble(&h, &doc).await.unwrap();
        assert_eq!(res.assembled.len(), 1);
        assert_eq!(res.assembled[0].repo, "good");
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].repo, "missing");
        assert!(res.failures[0].error.contains("no local clone"));
    }

    #[tokio::test]
    async fn all_repos_missing_clone_assembles_nothing() {
        // SW-WU-8 AC-2: repos declared but NONE registered → no clone resolves, so
        // nothing is assembled (only failures). The wrapper gates its `worktree-changed`
        // emit on `assembled` being non-empty, so this shape emits nothing — no
        // pointless refetch. `assembled` empty here is that gate's negative branch;
        // `assembles_multiple_repos_in_parallel` covers the positive (emit) branch.
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let h = home.path().to_string_lossy().into_owned();
        let doc = write_epic_doc(docs.path(), &["acme/missing-a", "acme/missing-b"], None);

        let res = assemble(&h, &doc).await.unwrap();
        assert!(res.assembled.is_empty());
        assert_eq!(res.failures.len(), 2);
        assert!(res
            .failures
            .iter()
            .all(|f| f.error.contains("no local clone")));
    }

    #[tokio::test]
    async fn reconcile_removes_one_of_two_and_keeps_other_and_branch() {
        // AC-1/2/3: remove one of two repos → only its worktree is torn down; the
        // still-assigned repo's worktree is untouched; the shared integration
        // branch ref is preserved on both clones.
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone_a = tempfile::tempdir().unwrap();
        let clone_b = tempfile::tempdir().unwrap();
        init_source_clone(clone_a.path(), "main");
        init_source_clone(clone_b.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone_a.path());
        register(&h, "acme/b", clone_b.path());
        let doc = write_epic_doc(docs.path(), &["acme/a", "acme/b"], None);
        assemble(&h, &doc).await.unwrap();
        assert!(wt_path(home.path(), "acme", "a").exists());
        assert!(wt_path(home.path(), "acme", "b").exists());

        // Drop `acme/a` from the epic doc, then reconcile.
        write_epic_doc(docs.path(), &["acme/b"], None);
        let res = reconcile(&h, &doc, vec![]).await.unwrap();

        assert_eq!(res.removed.len(), 1);
        assert!(res.skipped_in_use.is_empty());
        assert!(res.warnings.is_empty());
        assert!(
            !wt_path(home.path(), "acme", "a").exists(),
            "de-assigned worktree removed"
        );
        assert!(
            wt_path(home.path(), "acme", "b").exists(),
            "still-assigned worktree kept"
        );
        for clone in [clone_a.path(), clone_b.path()] {
            let repo = git2::Repository::open(clone).unwrap();
            assert!(repo
                .find_reference("refs/heads/integration/epics__SW-EW")
                .is_ok());
        }
    }

    #[tokio::test]
    async fn reconcile_all_assigned_is_noop() {
        // AC-2/6: every on-disk worktree is still assigned → nothing torn down,
        // empty result. The command wrapper gates its `worktree-changed` emit on a
        // non-empty `removed`, so this shape emits nothing (no pointless refetch).
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        let doc = write_epic_doc(docs.path(), &["acme/a"], None);
        assemble(&h, &doc).await.unwrap();

        // Doc unchanged — `acme/a` still assigned.
        let res = reconcile(&h, &doc, vec![]).await.unwrap();
        assert!(res.removed.is_empty());
        assert!(res.skipped_in_use.is_empty());
        assert!(res.warnings.is_empty());
        assert!(
            wt_path(home.path(), "acme", "a").exists(),
            "assigned worktree untouched"
        );
    }

    #[tokio::test]
    async fn reconcile_prunes_external_drift() {
        // AC-2 drift self-heal: a worktree on disk for a repo not in `repos:` is
        // pruned even though the doc was edited externally (no per-slug signal).
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone_a = tempfile::tempdir().unwrap();
        let clone_b = tempfile::tempdir().unwrap();
        init_source_clone(clone_a.path(), "main");
        init_source_clone(clone_b.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone_a.path());
        register(&h, "acme/b", clone_b.path());
        let doc = write_epic_doc(docs.path(), &["acme/a", "acme/b"], None);
        assemble(&h, &doc).await.unwrap();
        // Externally rewrite the doc to list only `acme/a`.
        write_epic_doc(docs.path(), &["acme/a"], None);

        let res = reconcile(&h, &doc, vec![]).await.unwrap();
        assert_eq!(res.removed.len(), 1);
        assert!(
            !wt_path(home.path(), "acme", "b").exists(),
            "drift worktree pruned"
        );
        assert!(wt_path(home.path(), "acme", "a").exists());
    }

    #[tokio::test]
    async fn reconcile_skips_in_use_deassigned_worktree() {
        // AC-4: a de-assigned worktree with a live cwd at/below it is preserved,
        // reported as skipped-in-use, never deleted (no force path).
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        let doc = write_epic_doc(docs.path(), &["acme/a"], None);
        assemble(&h, &doc).await.unwrap();
        let wt = wt_path(home.path(), "acme", "a");
        write_epic_doc(docs.path(), &[], None); // de-assign acme/a

        let res = reconcile(
            &h,
            &doc,
            vec![wt.join("subdir").to_string_lossy().into_owned()],
        )
        .await
        .unwrap();
        assert_eq!(res.skipped_in_use.len(), 1);
        assert!(res.removed.is_empty());
        assert!(wt.exists(), "in-use worktree must be preserved");
    }

    #[tokio::test]
    async fn reconcile_warns_on_dirty_deassigned_and_preserves() {
        // AC-4: a de-assigned worktree with uncommitted work is preserved + warned
        // (warn-not-fail). The frontmatter change stands (the caller wrote it); only
        // the worktree teardown defers.
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        let doc = write_epic_doc(docs.path(), &["acme/a"], None);
        assemble(&h, &doc).await.unwrap();
        let wt = wt_path(home.path(), "acme", "a");
        std::fs::write(wt.join("wip.md"), b"uncommitted").unwrap();
        write_epic_doc(docs.path(), &[], None); // de-assign acme/a

        let res = reconcile(&h, &doc, vec![]).await.unwrap();
        assert!(res.removed.is_empty());
        assert_eq!(res.warnings.len(), 1);
        assert!(res.warnings[0].contains("uncommitted"));
        assert!(wt.exists(), "dirty worktree must be preserved");
    }

    // ---- Code-review hardening ----

    #[tokio::test]
    async fn rejects_traversal_epic_id() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let doc = write_epic_doc(docs.path(), &[], None);
        // `..` must be rejected before any path math (can't escape `_epic/`).
        // The core takes a pre-resolved id, so a traversal id is still rejected.
        let err = assemble_epic_worktrees_core(
            home.path().to_string_lossy().into_owned(),
            UID,
            ORG.to_string(),
            TEAM.to_string(),
            "..".to_string(),
            doc.clone(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("invalid epic id"), "got: {err}");

        // SW-EW-15: reconcile DERIVES the id from the doc path (the helper
        // sanitizes `.`/`..`/separators), so a traversal id can't be injected;
        // an empty doc path is the rejectable caller bug.
        let err = reconcile_epic_worktrees_core(
            home.path().to_string_lossy().into_owned(),
            UID,
            ORG.to_string(),
            String::new(),
            vec![],
        )
        .await
        .unwrap_err();
        assert!(err.contains("invalid epic id"), "got: {err}");
    }

    #[tokio::test]
    async fn inner_worktrees_are_not_registered_as_content_targets() {
        let home = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        let doc = write_epic_doc(docs.path(), &["acme/a"], None);
        assemble(&h, &doc).await.unwrap();

        // AC-3: assembly registers NO new content target. The source slug
        // still resolves only to its clone, never the inner worktree path,
        // and the worktree dir is never registered under any slug.
        let ctx = derive_resolver_context(
            PathBuf::from(&h),
            UID,
            ORG.to_string(),
            Some(TEAM.to_string()),
        )
        .unwrap();
        let slug = SourceRepoSlug::parse_owner_repo("acme/a").unwrap();
        let attached =
            crate::content_engine::enumerate_attached_code_repos_for_slug(&ctx, &slug).unwrap();
        let wt = wt_path(home.path(), "acme", "a");
        for a in &attached {
            assert_ne!(a.code_repo_path, wt, "inner worktree must not be a target");
        }
        assert!(
            attached.iter().any(|a| a.code_repo_path == clone.path()),
            "the clone itself stays registered"
        );
    }

    #[test]
    fn two_slugs_one_clone_both_resolve() {
        // A monorepo attached under two owner/repo names. Per-slug resolution
        // must return BOTH (the team-wide enumerator dedups by path and would
        // drop the second → spurious "no local clone").
        let home = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let h = home.path().to_string_lossy().into_owned();
        register(&h, "acme/a", clone.path());
        register(&h, "acme/b", clone.path());

        let ctx = derive_resolver_context(
            PathBuf::from(&h),
            UID,
            ORG.to_string(),
            Some(TEAM.to_string()),
        )
        .unwrap();
        let repos = vec![
            SourceRepoSlug::parse_owner_repo("acme/a").unwrap(),
            SourceRepoSlug::parse_owner_repo("acme/b").unwrap(),
        ];
        let (resolved, missing) = resolve_epic_repo_clones(&ctx, &repos).unwrap();
        assert_eq!(resolved.len(), 2, "both slugs resolve to the shared clone");
        assert!(missing.is_empty());
    }

    #[test]
    fn branch_workspace_slug_is_injective_and_readable() {
        // `/` collapses for readability, but the blake3 suffix keeps refs that
        // would otherwise alias (`feat/x` vs the literal `feat-x`) DISTINCT — the
        // dir must be 1:1 with the branch or two reviews collide on one worktree.
        let a = branch_workspace_slug("feat/x");
        let b = branch_workspace_slug("feat-x");
        assert!(a.starts_with("feat-x-"), "readable prefix: {a}");
        assert!(b.starts_with("feat-x-"), "readable prefix: {b}");
        assert_ne!(a, b, "distinct refs must map to distinct dirs");
        // Stable across calls (a re-open must re-derive the same dir).
        assert_eq!(a, branch_workspace_slug("feat/x"));
    }

    #[test]
    fn branch_checkout_reads_the_wire_names_and_defaults_to_review() {
        let parse = |v: &str| serde_json::from_str::<BranchCheckout>(v).unwrap();
        assert_eq!(parse("\"review\""), BranchCheckout::Review);
        assert_eq!(parse("\"existing\""), BranchCheckout::Existing);
        assert_eq!(parse("\"new\""), BranchCheckout::New);
        assert_eq!(
            serde_json::from_str::<Option<BranchCheckout>>("null")
                .unwrap()
                .unwrap_or_default(),
            BranchCheckout::Review
        );
    }

    #[test]
    fn review_checkout_identity_routes_and_isolates_forks() {
        assert_eq!(
            review_checkout_identity("acme/app", Some("acme/app"), Some(7), "feat/x").unwrap(),
            ("feat/x".to_string(), None),
        );
        assert_eq!(
            review_checkout_identity("acme/app", Some("contrib/app"), Some(7), "feat/x").unwrap(),
            ("pull/7/feat/x".to_string(), Some(7)),
        );
        assert!(
            review_checkout_identity("acme/app", Some("contrib/app"), None, "feat/x")
                .unwrap_err()
                .contains("requires a GitHub pull request number")
        );
    }
}

/// `resolve_local_clone_for_slug` resolves a slug to a local clone,
/// attachments first (what the attach tools write) and the install store as a
/// fallback (managed clones + synthetic targets). The gap this closes: an
/// in-place user checkout outside the codesense home has an `attachments.yml`
/// row but no install-store row — a store-only resolver turned it into a
/// spurious `no local clone` refusal.
#[cfg(test)]
mod resolve_local_clone_for_slug_tests {
    use super::*;
    use crate::commands::attachments::upsert_attachment_record;
    use crate::context_repo::test_fixtures::init_source_clone;
    use crate::domain::Attachment;
    use std::path::Path;

    const UID: u64 = 42;
    const ORG: &str = "acme/api";
    const TEAM: &str = "default";

    /// A real git repo at `dir` with `origin = <remote_url>` so its
    /// `remote_key()` matches `SourceRepoSlug::new(owner, repo).remote_key()`.
    fn init_github_clone(dir: &Path, remote_url: &str) {
        init_source_clone(dir, "main");
        git2::Repository::open(dir)
            .unwrap()
            .remote("origin", remote_url)
            .unwrap();
    }

    /// Attach a repo via the production writer `upsert_attachment_record`, so
    /// the test exercises the SAME `attachments.yml` row the attach tools
    /// produce. Does NOT touch the install store.
    fn attach(home: &Path, repo_path: &Path, remote_url: Option<&str>) {
        upsert_attachment_record(
            home,
            UID,
            Attachment {
                repo_path: repo_path.to_string_lossy().into_owned(),
                remote_url: remote_url.map(str::to_owned),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
            None,
            false,
        )
        .unwrap();
    }

    fn ctx_for(home: &Path, team: Option<&str>) -> ResolverContext {
        derive_resolver_context(
            home.to_path_buf(),
            UID,
            ORG.to_string(),
            team.map(String::from),
        )
        .unwrap()
    }

    fn gh_slug(owner: &str, repo: &str) -> SourceRepoSlug {
        SourceRepoSlug::new(owner, repo).unwrap()
    }

    /// Seed `teams.yml` so the team has `local/<folder>` configured (the
    /// shape `SourceRepoSlug::resolve_against` reads for a remote-less story).
    fn write_local_team_config(home: &Path, folder: &str) {
        let org_root =
            crate::context_repo::path_to_org_root(home, &Principal::from(UID), ORG).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            format!("slug: {ORG}\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
        )
        .unwrap();
        std::fs::write(org_root.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            format!(
                "teams:\n  - name: {TEAM}\n    source_repos:\n      - owner: local\n        repo: {folder}\n        provider: local\n        host: local\n"
            ),
        )
        .unwrap();
    }

    // An in-place user checkout outside the codesense home has an
    // attachments.yml row and NO install-store row. The resolver returns the
    // attachment path — a store-only lookup used to return None here and
    // produce a spurious `no local clone` refusal from the launcher. The
    // before/after `is_empty()` check across the call also pins the
    // primary-checkout invariant: resolving must not write into the store.
    #[test]
    fn in_place_github_checkout_resolves_via_attachments_without_store_row() {
        let home = tempfile::tempdir().unwrap();
        let user_checkout = tempfile::tempdir().unwrap();
        init_github_clone(user_checkout.path(), "https://github.com/acme/widget.git");
        attach(
            home.path(),
            user_checkout.path(),
            Some("https://github.com/acme/widget.git"),
        );

        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "widget");

        // Precondition: no install-store row exists for this slug (the
        // managed-surface gate skips user checkouts).
        let before =
            crate::content_engine::enumerate_attached_code_repos_for_slug(&ctx, &slug).unwrap();
        assert!(
            before.is_empty(),
            "no install-store row before resolve, got {before:?}"
        );

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(user_checkout.path()),
            "attachments.yml row must resolve even without an install-store row"
        );

        // Resolving via the attachments branch must not write the primary
        // checkout into the install store. If a future refactor demotes
        // `resolve_local_clone_for_slug` into something that round-trips
        // through `register_attached_code_repo`, this flips red — the
        // "never write to a user's primary checkout" invariant depends on
        // this.
        let after =
            crate::content_engine::enumerate_attached_code_repos_for_slug(&ctx, &slug).unwrap();
        assert!(
            after.is_empty(),
            "install store must stay empty after resolve, got {after:?}"
        );
    }

    // A remote-less local repo at `<tmp>/myfolder` attached with
    // `remote_url: None` resolves under the team's configured `local/myfolder`
    // slug (the slug the attach-a-local-folder flow tracks on a team).
    #[test]
    fn remote_less_local_folder_resolves_under_configured_local_slug() {
        let home = tempfile::tempdir().unwrap();
        let repo_parent = tempfile::tempdir().unwrap();
        let repo_dir = repo_parent.path().join("myfolder");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_source_clone(&repo_dir, "main");
        attach(home.path(), &repo_dir, None);
        write_local_team_config(home.path(), "myfolder");

        let ctx = ctx_for(home.path(), Some(TEAM));
        let configured = configured_team_source_repos(&ctx);
        let slug = SourceRepoSlug::resolve_against("local/myfolder", &configured)
            .expect("the team's configured local slug must match the story's source_repo");

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(repo_dir.as_path()),
            "a remote-less attached folder must resolve under the team's `local/<folder>` slug"
        );
    }

    // A stale attachment row (`repo_path` deleted) with no store row falls
    // through to `None` so the step-2 call site maps it to the typed
    // `no local clone` refusal.
    #[test]
    fn stale_attachment_row_without_store_row_is_none() {
        let home = tempfile::tempdir().unwrap();
        let user_checkout = tempfile::tempdir().unwrap();
        let repo_path = user_checkout.path().to_path_buf();
        init_github_clone(&repo_path, "https://github.com/acme/widget.git");
        attach(
            home.path(),
            &repo_path,
            Some("https://github.com/acme/widget.git"),
        );
        // Delete the attached directory on disk — the row stays in attachments.yml.
        drop(user_checkout);
        assert!(!repo_path.is_dir());

        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "widget");

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert!(
            resolved.is_none(),
            "a stale attachment path must not resolve — the step-2 call site needs \
             None here to produce the typed `no local clone` refusal (got {resolved:?})"
        );
    }

    // No attachment row for the slug, but the install store carries an
    // `install_target` row (seeded via `register_attached_code_repo`) for an
    // existing dir. The resolver returns the store path — managed clones and
    // synthetic epic workspace targets keep resolving.
    #[test]
    fn store_only_registration_still_resolves_as_fallback() {
        let home = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        // No `upsert_attachment_record` — attachments.yml has no row for this slug.
        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "managed");
        crate::content_engine::register_attached_code_repo(&ctx, &slug, clone.path()).unwrap();

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(clone.path()),
            "a store-only registration (managed clone / synthetic target) must still resolve"
        );
    }

    // Attachment row and store row point at DIFFERENT existing dirs for the
    // same slug — the attachment path wins. The user's explicit attach is the
    // right answer and matches the UI's `list_attachments_by_remote` read.
    #[test]
    fn attachment_wins_when_both_sources_exist_at_different_paths() {
        let home = tempfile::tempdir().unwrap();
        let attached = tempfile::tempdir().unwrap();
        let store_only = tempfile::tempdir().unwrap();
        init_github_clone(attached.path(), "https://github.com/acme/widget.git");
        init_source_clone(store_only.path(), "main");

        attach(
            home.path(),
            attached.path(),
            Some("https://github.com/acme/widget.git"),
        );
        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "widget");
        crate::content_engine::register_attached_code_repo(&ctx, &slug, store_only.path()).unwrap();

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(attached.path()),
            "the attachment path must win over a differing store row"
        );
    }

    // `resolve_epic_repo_clones` sees an in-place attached clone (no store
    // row) in `resolved`, not `missing` — epic assembly uses the same
    // resolver, so it stops surfacing a spurious `no local clone` failure.
    #[test]
    fn resolve_epic_repo_clones_sees_attached_in_place_clone_without_store_row() {
        let home = tempfile::tempdir().unwrap();
        let user_checkout = tempfile::tempdir().unwrap();
        init_github_clone(user_checkout.path(), "https://github.com/acme/widget.git");
        attach(
            home.path(),
            user_checkout.path(),
            Some("https://github.com/acme/widget.git"),
        );

        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "widget");
        let (resolved, missing) =
            resolve_epic_repo_clones(&ctx, std::slice::from_ref(&slug)).unwrap();
        assert!(missing.is_empty(), "attached clone must not be missing");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].clone_path.as_path(), user_checkout.path());
    }

    // When `attachments.yml` is absent entirely but the store has a row, the
    // resolver still falls through to the store (no panic / no false-missing
    // from the attachments reader). Complements the attachment-row-missing
    // case with the absent-file edge case.
    #[test]
    fn missing_attachments_file_falls_through_to_store() {
        let home = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "nofile");
        crate::content_engine::register_attached_code_repo(&ctx, &slug, clone.path()).unwrap();

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(resolved.as_deref(), Some(clone.path()));
    }

    // Lock the canonical-remote-key invariant: an attachment written with
    // non-canonical casing or a trailing `.git` must resolve under the
    // slug's normalized `remote_key`. If `canonical_remote_key`'s rules
    // drift away from `SourceRepoSlug::remote_key`'s, this flips red.
    #[test]
    fn attachment_with_mixed_case_and_dot_git_still_resolves() {
        let home = tempfile::tempdir().unwrap();
        let user_checkout = tempfile::tempdir().unwrap();
        init_github_clone(user_checkout.path(), "HTTPS://GitHub.com/Acme/Widget");
        attach(
            home.path(),
            user_checkout.path(),
            Some("HTTPS://GitHub.com/Acme/Widget"),
        );

        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "widget");

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(user_checkout.path()),
            "mixed-case URL (no `.git`) must still key under the canonical slug"
        );
    }

    // A malformed `attachments.yml` must not turn a store-only slug into a
    // `no local clone` refusal. The composite resolver logs and falls
    // through to the install store — the only new failure mode this diff
    // could have introduced for managed clones is pinned here.
    #[test]
    fn malformed_attachments_file_falls_through_to_store() {
        let home = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let ctx = ctx_for(home.path(), Some(TEAM));
        let slug = gh_slug("acme", "managed");
        crate::content_engine::register_attached_code_repo(&ctx, &slug, clone.path()).unwrap();

        // Write a syntactically-broken attachments.yml at the exact path the
        // reader expects, so `read_attachments` errors.
        let att_path =
            crate::context_repo::path_to_user_attachments_file(home.path(), &Principal::from(UID));
        std::fs::create_dir_all(att_path.parent().unwrap()).unwrap();
        std::fs::write(&att_path, b":::not valid yaml:::").unwrap();

        let resolved = resolve_local_clone_for_slug(&ctx, &slug).unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(clone.path()),
            "a corrupt attachments.yml must not shadow a store row"
        );
    }
}

#[cfg(test)]
mod inherited_promote_tests {
    use super::*;
    #[tokio::test]
    async fn promoting_a_private_parent_file_down_answers_as_missing() {
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        let run = |name: &str| {
            promote_entry(
                PromoteParams {
                    layer: LayerId::TeamProjection,
                    logical_name: name.into(),
                    code_repo_slug: None,
                    codesense_home: home.path().to_string_lossy().into_owned(),
                    user_id: 1,
                    org_slug: "acme/ctx".into(),
                    team_name: Some("child".into()),
                    from_team: "parent".into(),
                    to_team: "child".into(),
                    force: true,
                },
                || {
                    Ok(crate::content_engine::Dispatcher::new(1, 1000, 60_000, 500)
                        .suppression_handle())
                },
            )
        };
        let child_before = std::fs::read(root.join("child/content/secret.md")).unwrap();
        let denied = run("secret.md").await.unwrap();
        let missing = run("missing.md").await.unwrap();
        assert!(matches!(denied, PromoteOutcome::RejectedSourceMissing));
        assert_eq!(
            serde_json::to_value(denied).unwrap(),
            serde_json::to_value(missing).unwrap()
        );
        assert!(root.join("parent/content/secret.md").exists());
        assert_eq!(
            std::fs::read(root.join("child/content/secret.md")).unwrap(),
            child_before
        );
        assert!(!root.join("child/content/missing.md").exists());
        let shared_bytes = std::fs::read(root.join("parent/content/shared.md")).unwrap();
        assert!(matches!(
            run("shared.md").await.unwrap(),
            PromoteOutcome::Promoted { .. }
        ));
        assert!(!root.join("parent/content/shared.md").exists());
        assert_eq!(
            std::fs::read(root.join("child/content/shared.md")).unwrap(),
            shared_bytes
        );
    }
}
