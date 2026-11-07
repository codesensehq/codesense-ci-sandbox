// SPDX-License-Identifier: MIT
//! SW-TS-1: the team-independent worktree→story index.
//!
//! A worktree derives its team from the story it belongs to. The link
//! "this worktree path is working this story" lives here — in a record
//! keyed by `code_repo_path_hash`, **one level above team** so it's
//! resolvable *before* the team is known:
//!
//! ```text
//! {codesense_home}/users/{user_id}/worktrees/{code_repo_path_hash}.json
//! ```
//!
//! It cannot live in the per-path lockfile: that file's path embeds
//! `{team}` (`content_engine::lockfile::lockfile_path`), so storing the
//! team-deriving link there would be circular (architecture.md §4.1, D1).
//! It also lives outside every git repo (honours the
//! no-`.codesense`-in-repos rule, SS-FU-9) — it is a sibling of
//! `users/{uid}/installs/`.
//!
//! **Single home for every index operation.** This module owns the
//! record type, the hash-keyed path, and read / iter / find / write /
//! prune. The establish flow (SW-TS-4), the live-move trigger (SW-TS-1
//! Task 5), the stale-record self-heal (Task 6), and the launch
//! reconcile (SW-TS-3 §4.4) are all *consumers* — none re-walks the
//! directory or re-defines the record.
//!
//! **`story_source_path` is authoritative; `intended_team` is a cache.**
//! Team is always re-parsed from `story_source_path` at derive time via
//! [`resolve_team`]; `intended_team` is stored only for cheap display and
//! is never the sync input (SW-TS-1 AC-1, single source of truth).

// The read/resolve/fallback/prune surface is exercised by tests now; its
// production consumers (the C-layer derive caller + the move-watch) land in
// the SW-TS-1 follow-up. Same posture as `FrontmatterCache::by_team` —
// public API awaiting its caller. Remove when the caller is wired.
#![allow(dead_code)]

pub(crate) mod remote;

use std::path::{Path, PathBuf};

use codesense_fs_core::gate::path_within_any_root;
use serde::{Deserialize, Serialize};

use crate::content_engine::lockfile::code_repo_path_hash;
use crate::content_engine::{write_tmp_fsync_rename, Error};
use crate::context_repo::path_to_user_home;
use crate::domain::Principal;
use crate::domain::SourceRepoSlug;

use super::capture::parse_context_repo_parts;
use super::frontmatter_cache::FrontmatterCache;

/// Schema version stamped on every record. Bumped if the on-disk shape
/// changes incompatibly. Unsupported versions are skipped with a warning;
/// additive fields on the supported version are retained on rewrite.
pub const SCHEMA_VERSION: u32 = 1;

/// `{codesense_home}/users/{user_id}/worktrees/` — the index directory.
const WORKTREES_INDEX_DIR: &str = "worktrees";

/// Extension of each per-worktree record file.
const RECORD_EXT: &str = "json";

/// Sync state of a worktree's materialization, as understood by the
/// index (the *intent*; the lockfile remains the authoritative *fact* of
/// what's on disk — architecture.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SyncStatus {
    /// Materialization in flight (a wave is enqueued/running).
    Syncing,
    /// Last wave completed successfully.
    Synced,
    /// Last wave failed; awaiting retry.
    Failed,
    /// Linked but not yet synced — the default for a fresh record.
    #[default]
    Pending,
}

/// RF-perf-24 (AC-5): who triggered the most recent reverse-sync (dest→content-layer
/// promote) of a worktree. Stamped alongside [`WorktreeStoryRecord::last_reverse_sync_at`]
/// at the single wave-completion seam so the worktree roster can carry honest
/// provenance ("Synced by agent · 2m ago"). Wire form is lowercase (`auto`/`user`/
/// `agent`) to match the FE DTO the panel reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SyncInitiator {
    /// An ambient/automatic reverse pass (the sampled sweep, the on-become-live
    /// catch-up, or an override-flip reconcile). The default for a fresh record.
    #[default]
    Auto,
    /// A human explicitly requested the reverse pass. Kept so records stamped
    /// by the former worktrees-panel "Sync now" button still deserialize.
    User,
    /// An agent asked CodeSense to flush its worktree over the MCP
    /// `reverse_sync_worktree` tool.
    Agent,
}

/// One worktree's link to the story it is working, plus transient sync
/// state. Persisted at
/// `{home}/users/{uid}/worktrees/{code_repo_path_hash}.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeStoryRecord {
    /// Always [`SCHEMA_VERSION`] for live records.
    pub schema_version: u32,
    /// Absolute path of the worktree (code repo) this record describes.
    /// The record filename is `code_repo_path_hash(code_repo_path)`.
    pub code_repo_path: PathBuf,
    /// Org slug the story belongs to.
    pub org_slug: String,
    /// Source repo the worktree is a checkout of. Required by the derive
    /// caller: materialization resolves `TeamRepo` via the slug and the
    /// single-repo wave scope is an
    /// [`AttachedCodeRepo { slug, code_repo_path }`]. The establish writer
    /// (SW-TS-4) knows it; the branch fallback reads it from the story's
    /// frontmatter (`CacheEntry::source_repo()`).
    pub source_repo: SourceRepoSlug,
    /// The story id this worktree is working (e.g. `SW-TS-1`).
    pub story_id: String,
    /// Absolute path to the story file in the content layer. **The
    /// authoritative source for the derived team** — always re-parsed via
    /// [`resolve_team`], never read from [`Self::intended_team`].
    pub story_source_path: PathBuf,
    /// Cache of `resolve_team(story_source_path)` for display only. Never
    /// trusted as the sync input (AC-1). `None` when the story isn't
    /// under a `_team/<team>/` path (the neutral case, AC-5).
    #[serde(default)]
    pub intended_team: Option<String>,
    /// Index-side sync state. The lockfile is authoritative for "what's
    /// materialized"; this is the intent/UX state.
    #[serde(default)]
    pub status: SyncStatus,
    /// The id of the *latest* wave to touch this surface — set on both the
    /// pre-run `Syncing` stamp and the post-run `Synced`/`Failed` finalize.
    /// Process-local (`NEXT_WAVE_ID` resets each launch), so it is a *display /
    /// correlation* value, not a cross-session comparator. Combined with
    /// [`Self::status`] it answers "interrupted": a record left at
    /// [`SyncStatus::Syncing`] across a launch is a wave that started but never
    /// recorded completion ⇒ a hard exit mid-sync, healed by the launch
    /// reconcile. `None` only before a surface's first wave.
    #[serde(default)]
    pub pending_wave: Option<u64>,
    /// SW-SY-5: does this worktree hydrate stories **lazily** (pull-on-demand)?
    /// v1 sets this `true` only for codesense-**derived** worktrees (the derive
    /// seam seeds [`Self::hydrated_epics`] with the story's own epic); every
    /// hand-attached repo leaves it `false` = eager (byte-identical to today).
    /// `#[serde(default)]` ⇒ a legacy record with no field reads back `false`,
    /// the safe hydrate-everything migration default (AC-3/AC-4).
    #[serde(default)]
    pub lazy: bool,
    /// SW-SY-5: absolute epic-dir paths this worktree has hydrated. A story
    /// whose epic dir is NOT in this set is lazily excluded from the forward
    /// materialize UNLESS exempt (a tracked lockfile row, or a dest already on
    /// disk). Only consulted when [`Self::lazy`]. Grows **monotonically** — the
    /// derive seed plus each `hydrate_story` / stranded-edit-heal — the
    /// eager-convergence ratchet (OQ-2: no dehydrate in v1). `#[serde(default)]`
    /// ⇒ legacy/eager records read back empty.
    #[serde(default)]
    pub hydrated_epics: Vec<PathBuf>,
    /// SW-HV-6: this worktree's hydration **scope policy** override. `None` (the
    /// `#[serde(default)]` legacy/absent case) falls through to the team default
    /// and finally [`crate::install::hydration_closure::LIGHT`]. Persisted here (a
    /// machine-local record) exactly like [`Self::lazy`]/[`Self::hydrated_epics`];
    /// the *team* default lives in the git-shared team content root. Cleared
    /// back to `None` by `set_worktree_hydration_policy(null)` (AC-2). Survives
    /// move/rename because the record is cloned wholesale on those paths.
    #[serde(default)]
    pub hydration_policy: Option<crate::install::hydration_closure::HydrationPolicy>,
    /// SW-HV-7 (AC-7): set at the derive seam when a **team-default** policy (no
    /// per-worktree override) auto-hydrated the story's closure **beyond** the
    /// seed epic. This is the honest provenance signal the HV-1 overlay's
    /// first-open note reads — a manual pull-in (`hydrate_epic`) or an adopt
    /// grows `hydrated_epics` but NEVER sets this, so the note can distinguish
    /// "the team policy did this at derive" from "the user pulled it in." Always
    /// recomputed per derive (never a stale latch): `false` under LIGHT, under a
    /// present override, or when the closure added nothing past the seed.
    #[serde(default)]
    pub policy_auto_hydrated: bool,
    /// SW-FR-1 (AC-1): plain-language reason the last sync **failed**, set alongside
    /// [`SyncStatus::Failed`] (the first `WaveResult.errors` message, or a synthetic
    /// reason for a deferred / interrupted / enqueue-miss finalize). Persisted so a
    /// Failed surface still explains itself after an app restart — the transient
    /// `sync-wave-completed` error array is gone by then. Cleared to `None` on
    /// [`SyncStatus::Synced`]. `#[serde(default)]` ⇒ legacy records read back `None`.
    #[serde(default)]
    pub failure_reason: Option<String>,
    /// SW-XS-4: RETIRED. SW-FR-1's auto-retry streak was written but never
    /// scheduled (the derive/retry chain is gone with automatic sync). Kept for one
    /// release as read-and-drop so a record an older client wrote still parses
    /// under `deny_unknown_fields`; never written back (`skip_serializing`).
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    pub retry_attempts: u32,
    /// SW-XS-4: RETIRED with [`Self::retry_attempts`] — read-and-drop for one release.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    pub next_retry_at: Option<u64>,
    /// RF-perf-24 (AC-5): unix-epoch **milliseconds** of the last reverse-sync that
    /// actually promoted ≥1 dest edit back into the content layer, or `None` before
    /// this worktree has ever reverse-synced. Stamped at the single wave-completion
    /// seam ([`stamp_reverse_sync`]). Additive + `#[serde(default)]` ⇒ legacy records
    /// read back `None` with NO schema bump (same posture as every field above).
    #[serde(default)]
    pub last_reverse_sync_at: Option<u64>,
    /// RF-perf-24 (AC-5): who triggered the [`Self::last_reverse_sync_at`] promote —
    /// `auto` / `user` / `agent`. `None` before the first reverse-sync. Surfaced on
    /// the managed-worktree roster so unsynced work is never a silent question.
    #[serde(default)]
    pub last_reverse_sync_by: Option<SyncInitiator>,
    /// SW-WT-12 (DW-WT12-2): `false` ONLY while a headless (MCP) fresh-create's
    /// `setup:` is pending — the record is written before setup so the hydration
    /// barrier can read it, then flipped `true` once setup succeeds. Everywhere
    /// else (interactive/establish records, and legacy records absent the field via
    /// the `true` default) it is `true`. The MCP re-dispatch adopts a worktree only
    /// when it is BOTH `Synced` and setup-complete, so a crash mid-setup leaves a
    /// `Synced`-but-not-complete record that re-runs setup instead of adopting a
    /// bare tree. `#[serde(default = "setup_complete_default")]` (→ `true`) keeps
    /// pre-existing records — written under the old "record ⇒ setup done" ordering
    /// — adoptable with no schema bump.
    #[serde(default = "setup_complete_default")]
    pub setup_complete: bool,
    /// SV-CDE-7: whether line-attribution capture was working at the last agent launch
    /// here. `None` before any launch (or for a record an older client wrote).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<crate::attribution::AttributionState>,
    /// Preserve fields written by newer clients when updating this record.
    #[serde(flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}

/// SW-WT-12 (DW-WT12-2): default for [`WorktreeStoryRecord::setup_complete`] — a
/// record with no field predates the marker and was written post-setup, so it is
/// complete. Only a live MCP fresh-create explicitly writes `false`.
fn setup_complete_default() -> bool {
    true
}

impl WorktreeStoryRecord {
    /// Build a fresh record for `code_repo_path` working `story_id` at
    /// `story_source_path`. Stamps the schema version, caches the parsed
    /// `intended_team`, and starts in [`SyncStatus::Pending`] with no
    /// outstanding wave.
    pub fn new(
        code_repo_path: impl Into<PathBuf>,
        org_slug: impl Into<String>,
        source_repo: SourceRepoSlug,
        story_id: impl Into<String>,
        story_source_path: impl Into<PathBuf>,
    ) -> Self {
        let story_source_path = story_source_path.into();
        let intended_team = team_from_path(&story_source_path);
        Self {
            schema_version: SCHEMA_VERSION,
            code_repo_path: code_repo_path.into(),
            org_slug: org_slug.into(),
            source_repo,
            story_id: story_id.into(),
            story_source_path,
            intended_team,
            status: SyncStatus::Pending,
            pending_wave: None,
            // SW-SY-5: a freshly-built record is eager by default; the derive
            // seam (`establish_worktree_story_link`) flips `lazy` + seeds
            // `hydrated_epics` for codesense-derived worktrees.
            lazy: false,
            hydrated_epics: Vec::new(),
            // SW-HV-6: no per-worktree policy override on a fresh record — falls
            // through to team default → LIGHT.
            hydration_policy: None,
            // SW-HV-7: no derive-time policy auto-hydration until the seam runs.
            policy_auto_hydrated: false,
            // SW-FR-1: a fresh record has never failed. SW-XS-4: the retry
            // fields are retired (read-and-drop).
            failure_reason: None,
            retry_attempts: 0,
            next_retry_at: None,
            // RF-perf-24: never reverse-synced until a wave promotes a dest edit.
            last_reverse_sync_at: None,
            last_reverse_sync_by: None,
            // SW-WT-12 (DW-WT12-2): default complete; the MCP fresh-create flips
            // this to `false` before setup and back to `true` on setup success.
            setup_complete: true,
            attribution: None,
            extras: Default::default(),
        }
    }
}

fn extend_hydrated_epics(epics: &mut Vec<PathBuf>, epic: PathBuf) -> bool {
    if epics.contains(&epic) {
        return false;
    }
    epics.push(epic);
    true
}

/// The derived team for a record — **re-parsed from the authoritative
/// `story_source_path` every call** (AC-1). `None` ⇒ the story isn't
/// under a `_team/<team>/` path ⇒ neutral (nothing synced, AC-5).
///
/// Never consult [`WorktreeStoryRecord::intended_team`] for sync
/// decisions; that field is a display cache and may lag the path.
pub fn resolve_team(record: &WorktreeStoryRecord) -> Option<String> {
    team_from_path(&record.story_source_path)
}

/// Parse a frontmatter `source_repo` string (`"owner/repo"`) into a
/// validated [`SourceRepoSlug`]. Returns `None` for an empty, malformed,
/// or invalid value (e.g. missing `/`, traversal segments) — the caller
/// fails safe to neutral rather than persisting an unmaterializable slug.
pub fn slug_from_source_repo(source_repo: &str) -> Option<SourceRepoSlug> {
    let (owner, repo) = source_repo.split_once('/')?;
    SourceRepoSlug::new(owner, repo).ok()
}

/// SW-LN-B29: the source repos configured for the team whose content holds
/// `story_source_path` (its `teams.yml` entry). Empty when the path names no team or
/// the org registry doesn't load — callers then fall back to the bare slug parse.
pub fn team_source_repos_for_story(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    story_source_path: &Path,
) -> Vec<SourceRepoSlug> {
    let Some(team) = team_from_path(story_source_path) else {
        return Vec::new();
    };
    let org_root = match crate::context_repo::path_to_org_root(codesense_home, principal, org_slug)
    {
        Ok(root) => root,
        Err(error) => {
            log::warn!("{error}");
            return Vec::new();
        }
    };
    let Some(org) = crate::domain::load_org(org_slug.to_string(), &org_root, None).org else {
        log::warn!(
            target: "worktree_index",
            "team source repos unavailable (org {org_slug} registry did not load); \
             the story's source_repo falls back to an owner/repo parse"
        );
        return Vec::new();
    };
    org.team(&team)
        .map(|t| t.source_repos.clone())
        .unwrap_or_default()
}

/// SW-LN-B29: a story's `source_repo` as its TEAM configures it — provider and host
/// included. [`slug_from_source_repo`] reads `owner/repo` as a GitHub slug; for a repo
/// with no remote (`local`) or on another forge that slug never matches the team's, so
/// the worktree's record and store rows would sit under a slug no team fan-out or
/// refocus lookup ever queries. Resolves against the team's `teams.yml` first
/// ([`SourceRepoSlug::resolve_against`]), falling back to the GitHub parse.
pub fn slug_for_story(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    story_source_path: &Path,
    source_repo: &str,
) -> Option<SourceRepoSlug> {
    let configured =
        team_source_repos_for_story(codesense_home, principal, org_slug, story_source_path);
    SourceRepoSlug::resolve_against(source_repo, &configured)
}

/// `_team/<team>/…` → `Some(team)`, else `None`. Reuses the canonical
/// path parser so the index agrees with the watcher and capture on what
/// "a team path" means.
fn team_from_path(story_source_path: &Path) -> Option<String> {
    let s = story_source_path.to_str()?;
    parse_context_repo_parts(s).map(|(team, _, _)| team)
}

/// `{codesense_home}/users/{user_id}/worktrees/`.
fn worktrees_index_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(WORKTREES_INDEX_DIR)
}

/// SW-XS-17: the idle-clock sidecar the removed worktree reaper kept next to the
/// records (SW-XS-3 K5). Nothing reads or writes it any more; the launch reconcile
/// deletes a leftover copy via [`remove_retired_session_activity_file`].
const RETIRED_SESSION_ACTIVITY_FILE: &str = "session-activity.state";

/// SW-XS-17: delete the retired idle-clock sidecar, if present. Best-effort: a
/// missing file is the normal case, and any other failure is logged and left for
/// the next launch.
pub(crate) fn remove_retired_session_activity_file(codesense_home: &Path, principal: &Principal) {
    let path = worktrees_index_root(codesense_home, principal).join(RETIRED_SESSION_ACTIVITY_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!(
            target: "install::worktree_index",
            "could not remove the retired idle clock {}: {e}", path.display()
        ),
    }
}

/// Absolute path of the record file for `code_repo_path`. Reuses
/// [`code_repo_path_hash`] so a worktree maps to its record exactly as it
/// maps to its lockfile dir. Requires an absolute `code_repo_path`
/// (enforced by the hash helper).
fn record_path(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<PathBuf, Error> {
    let hash = code_repo_path_hash(code_repo_path)?;
    Ok(worktrees_index_root(codesense_home, principal).join(format!("{hash}.{RECORD_EXT}")))
}

/// Read the version before decoding either persisted record kind.
#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// Parse either kind of index record; additive fields round-trip unchanged.
fn parse_record(bytes: &[u8]) -> Result<WorktreeRecord, Error> {
    let invalid = |e: serde_json::Error| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid worktree-index JSON: {e}"),
        ))
    };
    let probe: SchemaProbe = serde_json::from_slice(bytes).map_err(invalid)?;
    if probe.schema_version != SCHEMA_VERSION {
        return Err(Error::UnsupportedSchemaVersion {
            found: probe.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    serde_json::from_slice(bytes).map_err(invalid)
}

/// Read the record for `code_repo_path`. `Ok(None)` when no record
/// exists (a missing file is a valid state, not an error — the neutral /
/// fallback path).
pub fn read_record(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<Option<WorktreeStoryRecord>, Error> {
    let path = record_path(codesense_home, principal, code_repo_path)?;
    if let Some(record) = read_record_at(&path)? {
        return Ok(Some(record));
    }
    // Registration stores physical identity, while IPC callers may retain a
    // symlink/firmlink spelling. Reuse the indexed alias lookup on direct misses.
    let mut registrations = story_registrations(codesense_home, code_repo_path)?
        .into_iter()
        .filter(|(owner, _)| owner == principal);
    let record = registrations.next().map(|(_, record)| record);
    if registrations.next().is_some() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ambiguous story registrations for workspace",
        )));
    }
    Ok(record)
}

/// Direct reads preserve errors so callers cannot mistake a skipped file for
/// a missing record and overwrite it. Enumeration handles these errors per file.
fn read_record_at(path: &Path) -> Result<Option<WorktreeStoryRecord>, Error> {
    Ok(match read_index_record_at(path)? {
        Some(WorktreeRecord::Story(record)) => Some(record),
        _ => None,
    })
}

fn read_index_record_at(path: &Path) -> Result<Option<WorktreeRecord>, Error> {
    read_index_record_with_warning(path, |path, error| {
        log::warn!("Skipping worktree-index record {}: {error}", path.display());
    })
}

fn read_index_record_with_warning(
    path: &Path,
    warn: impl FnOnce(&Path, &Error),
) -> Result<Option<WorktreeRecord>, Error> {
    let result = match std::fs::read(path) {
        Ok(bytes) => parse_record(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => Err(Error::Io(error)),
    };
    match result {
        Ok(record) => Ok(Some(record)),
        Err(error) => {
            if should_warn_record(path) {
                warn(path, &error);
            }
            Err(error)
        }
    }
}

/// Repeated path lookups may rescan the index; diagnose each file revision once.
fn should_warn_record(path: &Path) -> bool {
    type Revision = Option<(Option<std::time::SystemTime>, u64)>;
    static WARNED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Revision>>,
    > = std::sync::OnceLock::new();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let revision = std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.modified().ok(), metadata.len()));
    let mut warned = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if warned.get(&key) == Some(&revision) {
        return false;
    }
    warned.insert(key, revision);
    true
}

/// SW-WT-12 (S1): delete the index record for `code_repo_path` (idempotent — a
/// missing record is `Ok`). Used to roll back a headless fresh-create when
/// post-hydration setup fails: dropping the record makes `find_story_worktree`
/// miss, so a retry re-enters `ensure_story_worktree`, adopts the (idempotent)
/// worktree, re-hydrates, and re-runs setup — the same clean-rollback property
/// the former write-record-last ordering gave.
pub fn remove_record(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<(), Error> {
    remove_record_file(codesense_home, principal, code_repo_path)
}

fn record_writes() -> &'static std::sync::Mutex<()> {
    static WRITES: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    WRITES.get_or_init(Default::default)
}

/// Read-modify-write without losing a concurrent registration or setup update.
pub(crate) fn update_record_with(
    home: &Path,
    principal: &Principal,
    root: &Path,
    update: impl FnOnce(Option<WorktreeStoryRecord>) -> Result<WorktreeStoryRecord, String>,
) -> Result<WorktreeStoryRecord, String> {
    let _guard = record_writes().lock().unwrap_or_else(|e| e.into_inner());
    let current = read_record(home, principal, root).map_err(|e| e.to_string())?;
    let record = update(current)?;
    write_record_unlocked(home, principal, &record).map_err(|e| e.to_string())?;
    Ok(record)
}

/// Serialize setup executions for one physical root, including alias callers.
pub(crate) async fn setup_lock(root: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    type Locks = std::sync::Mutex<
        std::collections::HashMap<PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >;
    static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let lock = {
        let mut locks = LOCKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        let entry = locks.entry(root).or_default();
        if let Some(lock) = entry.upgrade() {
            lock
        } else {
            let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            *entry = std::sync::Arc::downgrade(&lock);
            lock
        }
    };
    lock.lock_owned().await
}

/// Write `record` atomically — **the one write path** for the index
/// (Task 2.3: fallback backfill and Task 5 move-updates both go through
/// here, never a second writer). Forces the schema version and creates
/// parent dirs as needed (via [`write_tmp_fsync_rename`]).
pub fn write_record(
    codesense_home: &Path,
    principal: &Principal,
    record: &WorktreeStoryRecord,
) -> Result<(), Error> {
    let _guard = record_writes().lock().unwrap_or_else(|e| e.into_inner());
    write_record_unlocked(codesense_home, principal, record)
}

fn write_record_unlocked(
    codesense_home: &Path,
    principal: &Principal,
    record: &WorktreeStoryRecord,
) -> Result<(), Error> {
    let path = record_path(codesense_home, principal, &record.code_repo_path)?;
    let _ = read_index_record_at(&path)?;
    let mut canonical = record.clone();
    canonical.schema_version = SCHEMA_VERSION;
    let bytes = serde_json::to_vec_pretty(&canonical).map_err(|e| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize worktree-index record: {e}"),
        ))
    })?;
    write_tmp_fsync_rename(&path, &bytes)?;
    remember_story_root(codesense_home, principal, &record.code_repo_path);
    Ok(())
}

/// SW-TS-3 (AC-2/4): update only the transient sync state of an existing
/// record — `status` and the latest `wave_id` (stored in `pending_wave`) —
/// leaving the story link untouched. Read-modify-write through the one
/// [`write_record`] writer so the `_team`/`source_repo`/`story_*` fields are
/// never disturbed.
///
/// `Ok(false)` when no record exists for `code_repo_path` (the neutral case:
/// a wave for a worktree that was never linked, or whose record was pruned
/// mid-flight) — the caller treats it as a no-op, never an error. This is the
/// single seam that brings the otherwise-inert `status` / `pending_wave`
/// fields to life from the wave engine (`sync/wave.rs`).
///
/// **Ordering note.** This does NOT guard against an out-of-order (superseded)
/// wave: `wave_id` resets to 1 each process (`NEXT_WAVE_ID`), so a guard
/// against the *persisted* `pending_wave` would wrongly reject every wave
/// after a restart and wedge the surface. The out-of-order guard is
/// session-scoped and lives in the wave engine (`record_worktree_sync_status`),
/// where wave ids are comparable; this function is an unconditional writer.
pub fn set_sync_state(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
    wave_id: u64,
    status: SyncStatus,
) -> Result<bool, Error> {
    let Some(mut record) = read_record(codesense_home, principal, code_repo_path)? else {
        return Ok(false);
    };
    record.status = status;
    record.pending_wave = Some(wave_id);
    write_record(codesense_home, principal, &record)?;
    Ok(true)
}

/// RF-perf-24 (AC-5): stamp the last-reverse-sync provenance (`now_ms` + `initiator`)
/// on an existing record, read-modify-write through the one [`write_record`] writer —
/// every other field (status, the story link) stays untouched. Called
/// at the single wave-completion seam whenever a reverse pass promoted ≥1 dest edit,
/// so the roster can say "Synced by {agent|user} · <ago>". `now_ms` is
/// injected (no clock in the index) so the stamp is deterministic in tests. `Ok(false)`
/// when no record exists (a raw-attached repo with no story link, or a path pruned
/// mid-flight) — a no-op, never an error.
pub fn stamp_reverse_sync(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
    initiator: SyncInitiator,
    now_ms: u64,
) -> Result<bool, Error> {
    let Some(mut record) = read_record(codesense_home, principal, code_repo_path)? else {
        return Ok(false);
    };
    record.last_reverse_sync_at = Some(now_ms);
    record.last_reverse_sync_by = Some(initiator);
    write_record(codesense_home, principal, &record)?;
    Ok(true)
}

/// SW-FX-66 (D1.1 / AC-2): stamp ONLY the `pending_wave` id of an existing
/// record, leaving `status` (and every other field) untouched. Called at wave
/// ENQUEUE time so a `WorktreeDerive` lost or superseded *before* it reaches
/// Hook 1 (the `Syncing` transition in [`set_sync_state`]) still leaves
/// `pending_wave: Some(id)` — distinguishable from a never-enqueued record
/// (`None`).
///
/// **Only stamps a still-`Pending` record — compare-and-set on `status`.** This
/// is both correctness and intent:
/// - *Correctness (no lost update).* Hook 1's [`set_sync_state`] writes the same
///   record file under the wave run body's per-target keyed lock; this
///   enqueue-time writer runs on the enqueue thread and holds no such lock. If
///   the record has already advanced past `Pending` (a wave reached Hook 1 and
///   owns `pending_wave`), a lock-free whole-record write here could revert that
///   live `Syncing`/`Synced` back to the stale snapshot we read. Refusing to
///   write once `status != Pending` removes that clobber: a record that has
///   started a wave already carries a `pending_wave`, so the breadcrumb is moot.
/// - *Intent (do NOT flip to `Syncing`).* The in-session sweep re-drives only
///   `Pending`/`Failed` and skips a *live* `Syncing`; flipping a stranded record
///   to `Syncing` at enqueue would make the sweep skip it. We only ever set
///   `pending_wave`, never `status`.
///
/// A still-`Pending` record has no running wave writing it (Hook 1 is what
/// leaves `Pending`), so the write is safe; the residual "read `Pending`, a
/// superseded prior wave's Hook 1 writes `Syncing`, we write `Pending`" window
/// is self-correcting — the superseding wave (the reason we're enqueuing) runs
/// and re-stamps `Syncing`, and any transient `Pending` is swept meanwhile.
///
/// `Ok(true)` when stamped, `Ok(false)` when there is no record OR the record
/// has already advanced past `Pending` (nothing to stamp).
pub fn assign_pending_wave(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
    wave_id: u64,
) -> Result<bool, Error> {
    let Some(mut record) = read_record(codesense_home, principal, code_repo_path)? else {
        return Ok(false);
    };
    // Compare-and-set: never rewrite a record a wave has already advanced.
    if record.status != SyncStatus::Pending {
        return Ok(false);
    }
    record.pending_wave = Some(wave_id);
    write_record(codesense_home, principal, &record)?;
    Ok(true)
}

/// Every record under `users/{uid}/worktrees/`. The single directory
/// walk — Task 5 (move reverse-lookup), Task 6 (prune), and SW-TS-3's
/// launch reconcile all build on this rather than re-walking.
///
/// A missing index dir yields an empty list (no worktrees linked yet). A
/// single malformed or vanished *record file* is skipped, not fatal — the
/// index is self-healing and one bad file must not blind the reconcile. A
/// directory-level iteration error (the dir itself became unreadable
/// mid-walk) does propagate — that's a whole-index fault, not one bad file.
/// Both kinds share the hash-keyed store and its write lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorktreeRecord {
    Context(ContextWorktreeRecord),
    Story(WorktreeStoryRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ContextWorktreeOwner {
    Agent { handle: String, name: String },
    Person,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWorktreeRecord {
    #[serde(default)]
    pub owner: Option<ContextWorktreeOwner>,
    pub schema_version: u32,
    pub kind: String,
    pub id: String,
    pub org: String,
    pub team: String,
    pub name: String,
    pub branch: String,
    pub base_sha: String,
    pub root: PathBuf,
    pub launch_cwd: PathBuf,
    pub created_at: u64,
    pub last_used_at: u64,
    /// Preserve fields written by newer clients when updating this record.
    #[serde(flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}

pub fn write_context_record(
    home: &Path,
    principal: &Principal,
    record: &ContextWorktreeRecord,
) -> Result<(), Error> {
    let _guard = record_writes().lock().unwrap_or_else(|e| e.into_inner());
    let path = record_path(home, principal, &record.root)?;
    let _ = read_index_record_at(&path)?;
    let bytes =
        serde_json::to_vec_pretty(record).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    write_tmp_fsync_rename(&path, &bytes)
}

pub fn iter_records(home: &Path, principal: &Principal) -> Result<Vec<WorktreeRecord>, Error> {
    let root = worktrees_index_root(home, principal);
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };
    Ok(entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(RECORD_EXT) {
                return None;
            }
            read_index_record_at(&path).ok().flatten()
        })
        .collect())
}

pub fn iter_story_records(
    home: &Path,
    principal: &Principal,
) -> Result<Vec<WorktreeStoryRecord>, Error> {
    Ok(iter_records(home, principal)?
        .into_iter()
        .filter_map(|r| match r {
            WorktreeRecord::Story(r) => Some(r),
            WorktreeRecord::Context(_) => None,
        })
        .collect())
}

pub fn context_records(
    home: &Path,
    principal: &Principal,
) -> Result<Vec<ContextWorktreeRecord>, Error> {
    let mut rows: Vec<_> = iter_records(home, principal)?
        .into_iter()
        .filter_map(|r| match r {
            WorktreeRecord::Context(r) => Some(r),
            WorktreeRecord::Story(_) => None,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.last_used_at
            .cmp(&a.last_used_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

/// Resolve a context checkout without creating or changing one.
pub fn resolve_context_record(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: Option<&str>,
    selected: &str,
) -> Result<ContextWorktreeRecord, String> {
    if selected.eq_ignore_ascii_case("clone") || selected.eq_ignore_ascii_case("private") {
        return Err(format!(
            "'{selected}' is a reserved source; address a worktree with that name by id"
        ));
    }
    let rows = context_records(home, principal).map_err(|e| e.to_string())?;
    let by_id = rows.iter().find(|r| r.id == selected);
    let mut names = rows.iter().filter(|r| {
        r.org == org
            && team.is_none_or(|t| r.team == t)
            && r.name.to_lowercase() == selected.to_lowercase()
    });
    let first_name = names.next();
    if by_id.is_none() && names.next().is_some() {
        return Err(format!(
            "Ambiguous context worktree name '{selected}'. Use an id: {}.",
            rows.iter()
                .filter(|r| r.org == org
                    && team.is_none_or(|t| r.team == t)
                    && r.name.to_lowercase() == selected.to_lowercase())
                .map(|r| format!("{} ({}:{})", r.id, r.org, r.team))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let row = by_id.or(first_name).ok_or_else(|| {
        format!(
            "{} has no worktree named '{selected}'. Its worktrees: {}.",
            team.unwrap_or(org),
            rows.iter()
                .filter(|r| r.org == org && team.is_none_or(|t| r.team == t))
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    if row.org != org || team.is_some_and(|team| row.team != team) {
        return Err(format!(
            "{} is a worktree of {}:{}, not {}:{}",
            row.name,
            row.org,
            row.team,
            org,
            team.unwrap_or("*")
        ));
    }
    crate::commands::workspace_open::validate_context_worktree(row)?;
    Ok(row.clone())
}

pub fn context_for_cwd(home: &Path, cwd: &Path) -> Option<ContextWorktreeRecord> {
    let users = std::fs::read_dir(home.join(crate::context_repo::USERS_DIR)).ok()?;
    for user in users.flatten() {
        let Ok(uid) = user.file_name().to_string_lossy().parse::<u64>() else {
            continue;
        };
        let Ok(rows) = context_records(home, &Principal::from(uid)) else {
            continue;
        };
        if let Some(row) = rows.into_iter().find(|r| r.launch_cwd == cwd) {
            return Some(row);
        }
    }
    None
}

/// Canonical story identities are indexed once per namespace. Revalidate the
/// durable record on every lookup; writes refresh this index before returning.
/// This protects historical external roots and aliases without an index scan per
/// watcher event or receiver admission.
type StoryRoots = std::collections::HashMap<PathBuf, Vec<(Principal, PathBuf)>>;
type StoryIndexStamp = Vec<(PathBuf, Option<std::time::SystemTime>)>;
type StoryIndex = (StoryIndexStamp, StoryRoots);
fn story_index_stamp(home: &Path) -> Result<StoryIndexStamp, Error> {
    let users = home.join(crate::context_repo::USERS_DIR);
    let mut stamp = vec![(
        users.clone(),
        std::fs::metadata(&users).and_then(|m| m.modified()).ok(),
    )];
    match std::fs::read_dir(users) {
        Ok(entries) => {
            for entry in entries {
                let root = entry.map_err(Error::Io)?.path().join(WORKTREES_INDEX_DIR);
                stamp.push((
                    root.clone(),
                    std::fs::metadata(root).and_then(|m| m.modified()).ok(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io(e)),
    }
    stamp.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(stamp)
}
fn story_roots_cache() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, StoryIndex>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, StoryIndex>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub(crate) fn story_registrations(
    home: &Path,
    root: &Path,
) -> Result<Vec<(Principal, WorktreeStoryRecord)>, Error> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut cache = story_roots_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let stamp = story_index_stamp(&home)?;
    let retargeted = cache.get(&home).is_some_and(|(_, roots)| {
        roots.iter().any(|(canonical, owners)| {
            owners
                .iter()
                .any(|(_, path)| path.canonicalize().unwrap_or_else(|_| path.clone()) != *canonical)
        })
    });
    if retargeted
        || cache
            .get(&home)
            .is_none_or(|(previous, _)| previous != &stamp)
    {
        let mut roots = StoryRoots::new();
        let users = home.join(crate::context_repo::USERS_DIR);
        match std::fs::read_dir(users) {
            Ok(users) => {
                for user in users {
                    let user = user.map_err(Error::Io)?;
                    let Ok(principal) = Principal::parse_local(&user.file_name().to_string_lossy())
                    else {
                        continue;
                    };
                    let directory = worktrees_index_root(&home, &principal);
                    let entries = match std::fs::read_dir(&directory) {
                        Ok(entries) => entries,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(Error::Io(e)),
                    };
                    for entry in entries {
                        let path = entry.map_err(Error::Io)?.path();
                        if path.extension().and_then(|s| s.to_str()) != Some(RECORD_EXT) {
                            continue;
                        }
                        let Some(record) = read_record_at(&path).ok().flatten() else {
                            continue;
                        };
                        let canonical = record
                            .code_repo_path
                            .canonicalize()
                            .unwrap_or_else(|_| record.code_repo_path.clone());
                        roots
                            .entry(canonical)
                            .or_default()
                            .push((principal.clone(), record.code_repo_path));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
        cache.insert(home.clone(), (stamp, roots));
    }
    let candidates = cache
        .get(&home)
        .and_then(|(_, roots)| roots.get(&root))
        .cloned()
        .unwrap_or_default();
    drop(cache);
    let mut registrations = Vec::new();
    for (principal, path) in candidates {
        if let Some(record) = read_record_at(&record_path(&home, &principal, &path)?)
            .ok()
            .flatten()
        {
            if record
                .code_repo_path
                .canonicalize()
                .unwrap_or_else(|_| record.code_repo_path.clone())
                == root
            {
                registrations.push((principal, record));
            }
        }
    }
    Ok(registrations)
}

fn remember_story_root(home: &Path, principal: &Principal, root: &Path) {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut cache = story_roots_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // An uninitialized namespace will read this record on its first lookup.
    if let Some((_, roots)) = cache.get_mut(&home) {
        let owners = roots.entry(canonical).or_default();
        let owner = (principal.clone(), root.to_path_buf());
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
}

/// Every registered worktree root across ALL users — the `code_repo_path` of
/// each record under `{codesense_home}/users/*/worktrees/*.json`.
///
/// The allow-list source for the worktree editor's path sandbox
/// (`commands::files::validate_worktree_path`, SLW-WDE-6): a path is a valid
/// worktree path iff it resolves inside one of these roots. Returns the
/// **recorded** set, never "any git worktree on disk," so the gate stays an
/// allow-list (SLW-WDE-6 AC-2/RD-2).
///
/// **uid-agnostic on purpose.** The sandbox runs synchronously inside the
/// `read_file`/`list_directory` blocking closures and has no active-uid/auth
/// context; walking every user's `worktrees/` dir keeps it a sync, drop-in
/// gate (and correct across multi-account) instead of threading the
/// async auth-derived uid onto the hot read path. Missing dirs yield an empty
/// set; a vanished/corrupt record is skipped, not fatal — the same
/// fault-isolation posture as [`iter_records`].
pub fn worktree_roots(codesense_home: &Path) -> Vec<PathBuf> {
    all_records(codesense_home)
        .into_iter()
        .map(|record| record.code_repo_path)
        .collect()
}

/// Every worktree record across ALL users (`{codesense_home}/users/*/worktrees/*.json`),
/// with the same uid-agnostic, skip-a-bad-record posture as [`worktree_roots`]. The usage
/// report reads it once per report to tie a session's working folder to its story.
pub fn all_records(codesense_home: &Path) -> Vec<WorktreeStoryRecord> {
    let users_dir = codesense_home.join(crate::context_repo::USERS_DIR);
    let user_entries = match std::fs::read_dir(&users_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut records_out = Vec::new();
    for user in user_entries.flatten() {
        let worktrees_dir = user.path().join(WORKTREES_INDEX_DIR);
        let records = match std::fs::read_dir(&worktrees_dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in records.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(RECORD_EXT) {
                continue;
            }
            // Skip a raced/corrupt record rather than blinding the whole sweep.
            if let Ok(Some(record)) = read_record_at(&path) {
                records_out.push(record);
            }
        }
    }
    records_out
}

/// SLW-FX-13: the forward-materialize safety invariant — is `path` a
/// **codesense-managed surface** it is safe to materialize into?
///
/// codesense materializes ONLY into managed surfaces; it must NEVER write
/// `_bmad-output/` (or install-layer `.claude/skills`, hooks, MCP registration)
/// into a user's primary source checkout. A managed surface is EITHER:
///
/// 1. **a registered worktree** — a `code_repo_path` recorded in the worktree
///    index ([`worktree_roots`]). A linked worktree for an *attached* repo is
///    created OUTSIDE `{codesense_home}` (default `../{repo}-wt/{slug}`,
///    `commands::worktree::DEFAULT_WORKTREE_DIR`), so the registry — NOT a
///    path-prefix or a literal `worktrees/` component — is the authoritative
///    oracle; OR
/// 2. **the machine-local managed tree** under `{codesense_home}/users/`
///    (managed clones `…/sources/`, context repos `…/orgs/{slug}/`, epic
///    workspaces `…/_epic/…`, and internal worktrees).
///
/// Cheap-first (SLW-WDE-6 §E / SLW-FX-13 AC-9): the pure `users/` prefix test
/// runs first; only a path outside it pays the [`worktree_roots`] filesystem
/// scan. Callers MUST resolve this at most once per repo/wave, never per file.
pub(crate) fn is_codesense_managed_target(path: &Path, codesense_home: &Path) -> bool {
    // (b) machine-local managed tree — pure, no I/O. Covers managed clones,
    // context/epic trees, and internal worktrees (all under `users/`).
    let users_root = codesense_home.join(crate::context_repo::USERS_DIR);
    if path_within_any_root(path, std::slice::from_ref(&users_root)) {
        return true;
    }
    // (a) registered worktree — authoritative for attached-repo worktrees that
    // live outside `{codesense_home}`. Pays a filesystem scan, hence last.
    path_within_any_root(path, &worktree_roots(codesense_home))
}

/// The **same managed-surface boundary** as [`is_codesense_managed_target`] — the
/// `users/` tree plus every registered worktree root — materialized as one flat root
/// set for callers that gate MANY paths in a single pass (resolve the scan ONCE, then
/// each path is a pure [`path_within_any_root`] test). `is_codesense_managed_target`
/// stays the right choice for a single path (it short-circuits before the scan); this is
/// its batch form (e.g. `commands::worktree::discover_worktrees`). `users/` is listed
/// first so the common in-`users/` hit short-circuits `path_within_any_root`'s `.any()`.
/// Keep the two definitions in lockstep: a new managed surface added to one belongs here
/// too.
pub(crate) fn managed_surface_roots(codesense_home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![codesense_home.join(crate::context_repo::USERS_DIR)];
    roots.extend(worktree_roots(codesense_home));
    roots
}

/// [`is_codesense_managed_target`] with `codesense_home` canonicalized first, so
/// the pure `users/` prefix test doesn't false-miss when home is symlinked (e.g.
/// macOS `/var` → `/private/var`) against an already-canonicalized `path`. Use
/// this whenever `path` is canonical (post-`canonicalize`). Extracted so the
/// epic-path resolver (`resolve_epic_path_in`) and the RF-7 skill-cwd resolver
/// (`resolve_managed_team_content_root`) share ONE symlink-safe check.
pub(crate) fn is_codesense_managed_target_canon(path: &Path, codesense_home: &Path) -> bool {
    let home_canon = codesense_home.canonicalize();
    let home_ref = home_canon.as_deref().unwrap_or(codesense_home);
    is_codesense_managed_target(path, home_ref)
}

/// SR-15 (Task 1 — measure-first gate): an always-on frequency tally for the
/// `story_id` reverse lookup, plus gated per-call timing, so a live story-move burst
/// can PROVE whether the O(records) disk enumerate is hot enough on the watcher thread
/// to justify an index — before one is built. Mirrors the `frontmatter_perf` /
/// `residual_metrics` shape and shares the ONE RF-perf-0 timing gate
/// ([`crate::install::story_linker::sync_timing_enabled`]) — no second switch. The
/// `Relaxed` add is negligible next to the disk walk it measures, so the frequency
/// tally is always live; the clock is read only when that shared gate is armed.
mod lookup_perf {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// The tally as a value so tests instantiate their own instance and never race the
    /// process-global [`COUNTERS`] that other tests bump (the `frontmatter_perf` /
    /// `residual_metrics` discipline).
    pub(super) struct LookupPerf {
        calls: AtomicU64,
        records_scanned: AtomicU64,
    }

    impl LookupPerf {
        pub(super) const fn new() -> Self {
            Self {
                calls: AtomicU64::new(0),
                records_scanned: AtomicU64::new(0),
            }
        }

        /// One completed lookup enumerated `records` record files.
        pub(super) fn note(&self, records: usize) {
            self.calls.fetch_add(1, Relaxed);
            self.records_scanned.fetch_add(records as u64, Relaxed);
        }

        /// `(calls, records_scanned)` — `records_scanned / calls` is the mean N an
        /// index would make O(1). Test/capture readout only (production observes cost
        /// via the gated per-call `debug!` and frequency by counting the tally).
        #[cfg(test)]
        pub(super) fn snapshot(&self) -> (u64, u64) {
            (self.calls.load(Relaxed), self.records_scanned.load(Relaxed))
        }
    }

    /// Process-global tally; production bumps this through [`find_by_story_id`]'s
    /// injected-counter seam ([`super::find_by_story_id_tallied`]).
    pub(super) static COUNTERS: LookupPerf = LookupPerf::new();

    /// `Some(Instant)` only when the shared RF-perf-0 gate is armed — one gate, every
    /// path; no clock is read on a normal prod build.
    pub(super) fn start_timer() -> Option<std::time::Instant> {
        crate::install::story_linker::sync_timing_enabled().then(std::time::Instant::now)
    }
}

/// All worktrees currently linked to `story_id`. Drives the live-move
/// reverse lookup (Task 5.3): when a story moves teams, re-derive every
/// worktree pointing at it. Normally 0 or 1, but not constrained to one.
///
/// SR-15: the enumerate is O(records) disk I/O on the watcher thread; it is tallied and
/// (gated) timed via [`lookup_perf`] so a live capture can decide whether an index is
/// warranted before one is built. The tally counts *completed enumerates* — the empty-id
/// guard and any `iter_records` error return before it, so `calls` is scans performed (not
/// total invocations) and `records_scanned` is records successfully parsed by
/// [`iter_records`] (raced/corrupt files it skips are not counted).
pub fn find_by_story_id(
    codesense_home: &Path,
    principal: &Principal,
    story_id: &str,
) -> Result<Vec<WorktreeStoryRecord>, Error> {
    find_by_story_id_tallied(codesense_home, principal, story_id, &lookup_perf::COUNTERS)
}

/// [`find_by_story_id`] with the perf counter injected, so a test can pin the production
/// enumerate → count → `note` wiring (that it passes `scanned_count`, not `matches.len()`,
/// on the success path) against a LOCAL counter — no race on the process-global
/// [`lookup_perf::COUNTERS`] that parallel tests also bump. Production passes `&COUNTERS`.
fn find_by_story_id_tallied(
    codesense_home: &Path,
    principal: &Principal,
    story_id: &str,
    perf: &lookup_perf::LookupPerf,
) -> Result<Vec<WorktreeStoryRecord>, Error> {
    // An empty query must never bucket-match every empty-id record (a
    // hand-edited record or empty frontmatter `story_id` could hold one).
    if story_id.is_empty() {
        return Ok(Vec::new());
    }
    // SR-15 (Task 1): gated clock around the disk enumerate (the measured cost); the
    // frequency tally below is always-on.
    let timer = lookup_perf::start_timer();
    let scanned = iter_story_records(codesense_home, principal)?;
    let scanned_count = scanned.len();
    let matches: Vec<WorktreeStoryRecord> = scanned
        .into_iter()
        .filter(|r| r.story_id == story_id)
        .collect();
    perf.note(scanned_count);
    if let Some(started) = timer {
        log::debug!(
            target: crate::install::story_linker::SYNC_TIMING_TARGET,
            "worktree_find_by_story_id records={} matched={} elapsed_us={}",
            scanned_count,
            matches.len(),
            started.elapsed().as_micros(),
        );
    }
    Ok(matches)
}

/// Extract a candidate `story_id` from a branch name following the
/// `<prefix>/<story-id>-<slug>` convention (e.g.
/// `feat/SW-TS-1-sync-follows-the-story` → `SW-TS-1`).
///
/// A story id is the leading run of `[A-Z0-9]` segments up to and
/// including the first all-numeric segment (the story number) — e.g.
/// `SW-TS-1`, `SS-FU-19`, `T-11`. Everything after is the lowercase slug
/// and is ignored. Returns `None` if the post-prefix text doesn't start
/// with that shape.
///
/// This is only a *candidate*: the caller validates it against the
/// frontmatter cache ([`backfill_from_branch`]), so an over- or
/// under-greedy parse fails safe to neutral rather than mis-syncing.
pub fn story_id_from_branch(branch: &str) -> Option<String> {
    // Strip the branch-type prefix (everything up to the last '/').
    let tail = branch.rsplit_once('/').map_or(branch, |(_, rest)| rest);
    let mut id_segments: Vec<&str> = Vec::new();
    for segment in tail.split('-') {
        let is_id_segment = !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !is_id_segment {
            break;
        }
        id_segments.push(segment);
        if segment.bytes().all(|b| b.is_ascii_digit()) {
            // Reached the story number — the id ends here.
            return Some(id_segments.join("-"));
        }
    }
    None
}

/// Branch-name fallback (architecture.md §4.1) for a worktree with no
/// index record: parse the branch → `story_id`, resolve it to a story
/// file via the frontmatter cache, and **backfill** a record (through the
/// one writer, Task 2.3) so the fallback runs once and the primary path
/// serves thereafter.
///
/// Returns `Ok(None)` (neutral, AC-5) when the branch yields no story id,
/// the id matches no indexed story, or the id is ambiguous (2+ matches).
pub fn backfill_from_branch(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
    org_slug: &str,
    branch: &str,
    cache: &FrontmatterCache,
) -> Result<Option<WorktreeStoryRecord>, Error> {
    let Some(story_id) = story_id_from_branch(branch) else {
        return Ok(None);
    };
    // Unique match only — 0 (absent) and 2+ (ambiguous) both → neutral, via the
    // shared `single_by_story_id` arity gate (`.ok()` collapses both to a skip).
    // The slug comes from the story's own frontmatter (`source_repo`); a
    // missing/unparseable slug fails safe to neutral rather than writing a
    // record the derive caller can't materialize.
    let Ok(entry) = cache.single_by_story_id(&story_id) else {
        return Ok(None);
    };
    let Some(source_repo) = slug_for_story(
        codesense_home,
        principal,
        org_slug,
        &entry.path,
        entry.source_repo(),
    ) else {
        return Ok(None);
    };
    let record = WorktreeStoryRecord::new(
        code_repo_path,
        org_slug,
        source_repo,
        story_id,
        entry.path.clone(),
    );
    write_record(codesense_home, principal, &record)?;
    Ok(Some(record))
}

/// Self-heal (AC-8): if `record.code_repo_path` is **confirmed gone**,
/// remove its record file and return `true`. A live path returns `false`
/// (untouched). A transient I/O error (e.g. an unmounted volume) is
/// surfaced as `Err` and the record is **not** pruned — a flaky mount
/// must never delete a live record. Teardown itself is out of scope
/// (SS-FU-17); this only stops leaked records from driving a sync.
pub fn prune_if_stale(
    codesense_home: &Path,
    principal: &Principal,
    record: &WorktreeStoryRecord,
) -> Result<bool, Error> {
    prune_record_for_path(codesense_home, principal, &record.code_repo_path)
}

/// [`prune_if_stale`] keyed by path instead of a whole record — for callers that
/// have removed a worktree and hold only its path (SLW-WL-2: the manual/auto remove
/// funnel prunes the record so `list_managed_worktrees` and the teardown poll stop
/// seeing a worktree that no longer exists). Same "only prune a confirmed-gone path,
/// never on a transient I/O error" contract as [`prune_if_stale`].
///
/// NOTE: this gates on `try_exists() == Ok(false)` ALONE, which is correct for the
/// remove funnel (the caller just successfully git-pruned the tree, so the path is
/// gone by our own action). It is NOT safe for a blind reconcile scan of records we
/// did not remove — a transiently-unmounted volume also reads `Ok(false)` and would
/// be reaped. Reconcile callers must use [`reap_confirmed_gone_records`] /
/// [`path_confirmed_gone`], which add the `parent().is_dir()` unmount guard.
pub fn prune_record_for_path(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<bool, Error> {
    match code_repo_path.try_exists() {
        Ok(true) => Ok(false),
        Ok(false) => remove_record_file(codesense_home, principal, code_repo_path).map(|()| true),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Delete the on-disk record file for `code_repo_path` (a NotFound is success — the
/// record is already gone). The shared removal core of [`prune_record_for_path`] and
/// [`reap_confirmed_gone_records`], so the two never diverge on how a record file is
/// unlinked. Does NOT decide *whether* to remove — the caller owns the gone-gate.
fn remove_record_file(
    codesense_home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<(), Error> {
    let path = record_path(codesense_home, principal, code_repo_path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// SLW-WL-9 (AC-1b): is `code_repo_path` a **confirmed-gone** worktree — safe to reap
/// from the registry — as opposed to a merely-unreachable one (transient I/O, or a
/// whole-volume unmount that must self-heal on remount)? This is the SS-FU-17 /
/// RF-perf-20 gone-vs-unmounted gate, defined ONCE here for every reconcile caller:
///
///   * `try_exists() == Ok(false)` — the path DEFINITIVELY does not exist (ENOENT).
///     A permission / I/O error is `Err(_)` → `false` (never reap on a transient);
///     a non-dir-in-place is `Ok(true)` → `false`.
///   * parent still a directory — THIS one worktree dir was removed (git worktree
///     remove / external `rm` / crash), NOT a whole-volume unmount that takes the
///     parent too (which stays tracked → self-heals on remount).
///
/// Pure (two stats, no mutation) so it is unit-testable without any registry I/O.
/// `wave.rs`'s `try_reap_gone_worktree` holds a parallel copy of this predicate in
/// the sync layer (coupled to lockfile resolution there); converging the two is a
/// future cross-layer cleanup, deliberately out of scope here.
///
/// `try_exists()` follows symlinks, so a `code_repo_path` that is a symlink to a
/// now-absent target reads gone and is reaped — matching RF-perf-20's stance
/// (re-attach rebuilds the record; codesense worktrees are real git dirs, never
/// symlinks, so this is not a live path).
pub fn path_confirmed_gone(code_repo_path: &Path) -> bool {
    let definitely_absent = matches!(code_repo_path.try_exists(), Ok(false));
    let container_present = code_repo_path.parent().map(|p| p.is_dir()).unwrap_or(false);
    definitely_absent && container_present
}

/// SLW-WL-9 (AC-1b): reconcile a set of already-enumerated records against disk,
/// removing the registry record file for every one whose path is
/// [`path_confirmed_gone`]. Consumes `records` and returns
/// `(survivors, reaped_paths)` — the records whose worktree still exists, and the
/// `code_repo_path`s that were reaped (so the panel can report `reaped: N` and prune
/// any per-path reverse state). Folding the survivor split in here (rather than
/// returning only the reaped paths and making every caller re-filter) keeps this the
/// SINGLE authority on which records survive a reconcile — the shared reap for
/// `list_managed_worktrees` (user Refresh) and `reconcile_worktrees_with` (background
/// self-heal), so a background out-of-band `git worktree remove` disappears by the
/// next list/refresh, never lingering as a ghost row until app restart.
///
/// Only the record FILE is removed here (the index layer's own state); reverse-sync
/// lockfile/state for a gone path is RF-perf-20's sweep responsibility
/// (`try_reap_gone_worktree`) and self-heals independently. A record whose file can't
/// be unlinked is logged and KEPT as a survivor (retried next pass) — one unlinkable
/// record never blocks reaping the rest, and a still-live worktree is never touched.
pub fn reap_confirmed_gone_records(
    codesense_home: &Path,
    principal: &Principal,
    records: Vec<WorktreeStoryRecord>,
) -> (Vec<WorktreeStoryRecord>, Vec<PathBuf>) {
    let mut survivors = Vec::with_capacity(records.len());
    let mut reaped = Vec::new();
    for record in records {
        if path_confirmed_gone(&record.code_repo_path) {
            match remove_record_file(codesense_home, principal, &record.code_repo_path) {
                Ok(()) => {
                    reaped.push(record.code_repo_path);
                    continue;
                }
                Err(e) => log::warn!(
                    target: "install::worktree_index",
                    "reap_confirmed_gone_records: failed to drop stale record for {} — keeping it: {:?}",
                    record.code_repo_path.display(), e
                ),
            }
        }
        survivors.push(record);
    }
    (survivors, reaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const UID: u64 = 42;

    /// An absolute story path under `_team/<team>/content/…`.
    fn story_path(team: &str, name: &str) -> PathBuf {
        PathBuf::from(format!(
            "/srv/ctx/users/42/orgs/acme/codesense-context/{team}/content/impl/{name}.md"
        ))
    }

    fn slug() -> SourceRepoSlug {
        SourceRepoSlug::new("acme", "codesense").unwrap()
    }

    fn sample(code_repo_path: &Path, team: &str, story_id: &str) -> WorktreeStoryRecord {
        WorktreeStoryRecord::new(
            code_repo_path.to_path_buf(),
            "acme",
            slug(),
            story_id,
            story_path(team, story_id),
        )
    }

    #[test]
    fn write_then_read_roundtrips() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");

        write_record(home.path(), &Principal::from(UID), &rec).unwrap();
        let read = read_record(home.path(), &Principal::from(UID), wt.path()).unwrap();

        assert_eq!(read, Some(rec));
    }

    #[test]
    fn stamp_reverse_sync_records_timestamp_and_initiator_preserving_link() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();
        // A fresh record has never reverse-synced.
        assert_eq!(rec.last_reverse_sync_at, None);
        assert_eq!(rec.last_reverse_sync_by, None);

        assert!(stamp_reverse_sync(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            SyncInitiator::Agent,
            123_456,
        )
        .unwrap());

        let after = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .unwrap();
        assert_eq!(after.last_reverse_sync_at, Some(123_456));
        assert_eq!(after.last_reverse_sync_by, Some(SyncInitiator::Agent));
        // The story link + sync state are untouched — only the reverse-sync stamp moved.
        assert_eq!(after.story_id, rec.story_id);
        assert_eq!(after.status, rec.status);
    }

    #[test]
    fn stamp_reverse_sync_is_noop_when_no_record() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        // No record written — a raw-attached repo. The stamp is a no-op, never an error.
        let stamped = stamp_reverse_sync(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            SyncInitiator::Agent,
            123_456,
        )
        .unwrap();
        assert!(!stamped);
    }

    #[test]
    fn legacy_v1_record_without_reverse_sync_fields_reads_back_none() {
        // RF-perf-24 (AC-8 / R1): the two new fields are additive `#[serde(default)]`, so a
        // record written by an OLDER build (no `last_reverse_sync_*` keys) still parses under
        // the SAME `SCHEMA_VERSION` — the fields default to `None`. This is why the schema
        // version is NOT bumped (matching every prior additive field on this record); a bump
        // would trip the strict-equality gate and reject exactly these legacy records.
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");

        // Serialize, then strip the two RF-perf-24 keys to mimic an older record on disk.
        let mut value = serde_json::to_value(&rec).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("last_reverse_sync_at");
        obj.remove("last_reverse_sync_by");
        assert_eq!(obj["schema_version"], serde_json::json!(SCHEMA_VERSION));

        // Write the legacy bytes to the exact hash-keyed record path and read them back.
        let hash = code_repo_path_hash(wt.path()).unwrap();
        let path = home
            .path()
            .join("users")
            .join(UID.to_string())
            .join("worktrees")
            .join(format!("{hash}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let read = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .expect("legacy record must still parse — no schema bump");
        assert_eq!(read.last_reverse_sync_at, None);
        assert_eq!(read.last_reverse_sync_by, None);
    }

    #[test]
    fn record_lands_at_hash_keyed_path_under_user_worktrees() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(wt.path(), "alpha", "SW-TS-1"),
        )
        .unwrap();

        let hash = code_repo_path_hash(wt.path()).unwrap();
        let expected = home
            .path()
            .join("users")
            .join("42")
            .join("worktrees")
            .join(format!("{hash}.json"));
        assert!(expected.is_file(), "record not at expected hash-keyed path");
    }

    #[test]
    fn set_sync_state_updates_status_and_wave_id_preserving_link() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        // Pre-run stamp: Syncing + the latest wave id.
        assert!(set_sync_state(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            7,
            SyncStatus::Syncing
        )
        .unwrap());
        let after = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .unwrap();
        assert_eq!(after.status, SyncStatus::Syncing);
        assert_eq!(after.pending_wave, Some(7));
        // The story link is untouched — only the transient fields moved.
        assert_eq!(after.story_id, rec.story_id);
        assert_eq!(after.story_source_path, rec.story_source_path);
        assert_eq!(after.source_repo, rec.source_repo);

        // Post-run finalize: Synced, latest wave id carried for display.
        assert!(set_sync_state(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            7,
            SyncStatus::Synced
        )
        .unwrap());
        let done = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .unwrap();
        assert_eq!(done.status, SyncStatus::Synced);
        assert_eq!(done.pending_wave, Some(7));
    }

    #[test]
    fn set_sync_state_on_absent_record_is_noop_false() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        assert!(!set_sync_state(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            1,
            SyncStatus::Synced
        )
        .unwrap());
    }

    // ---- SW-FR-1: failure reason + bounded retry backoff --------------------

    #[test]
    fn fr1_fields_roundtrip_and_legacy_records_default() {
        // Full round-trip with the FR-1 reason populated.
        let mut rec = sample(Path::new("/tmp/wt"), "alpha", "SW-FR-1");
        rec.status = SyncStatus::Failed;
        rec.failure_reason = Some("nope".to_string());
        let json = serde_json::to_string(&rec).unwrap();
        let back: WorktreeStoryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);

        // A legacy record (written before FR-1) has no reason field.
        // `#[serde(default)]` must let it deserialize to the safe default —
        // NOT fail under `deny_unknown_fields`.
        let mut value = serde_json::to_value(&rec).unwrap();
        value.as_object_mut().unwrap().remove("failure_reason");
        let legacy: WorktreeStoryRecord = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.failure_reason, None);
    }

    /// SW-XS-4: the retired retry fields are read-and-drop for one release. A record
    /// an older client wrote WITH them still parses under `deny_unknown_fields`, and
    /// a re-write never emits them again. Fails if either attribute is dropped: a
    /// missing `default`/field rejects the old record; a missing `skip_serializing`
    /// writes them back.
    #[test]
    fn xs4_retired_retry_fields_are_read_and_dropped() {
        let rec = sample(Path::new("/tmp/wt"), "alpha", "SW-XS-4");
        let mut value = serde_json::to_value(&rec).unwrap();
        let obj = value.as_object_mut().unwrap();
        assert!(
            !obj.contains_key("retry_attempts") && !obj.contains_key("next_retry_at"),
            "the retired retry fields are never written: {obj:?}"
        );
        obj.insert("retry_attempts".into(), serde_json::json!(3));
        obj.insert("next_retry_at".into(), serde_json::json!(123_456));
        let old: WorktreeStoryRecord =
            serde_json::from_value(value).expect("an older client's record still parses");
        let rewritten = serde_json::to_value(&old).unwrap();
        assert!(rewritten.get("retry_attempts").is_none());
        assert!(rewritten.get("next_retry_at").is_none());
    }

    #[test]
    fn assign_pending_wave_stamps_id_without_flipping_status() {
        // SW-FX-66 (D1.1 / AC-2): the enqueue-time stamp records the wave id but
        // MUST leave `status: Pending`, so a wave lost before Hook 1 stays
        // retryable rather than looking in flight.
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-FX-66");
        assert_eq!(
            rec.status,
            SyncStatus::Pending,
            "fresh record starts Pending"
        );
        assert_eq!(rec.pending_wave, None, "fresh record has no wave id");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        assert!(assign_pending_wave(home.path(), &Principal::from(UID), wt.path(), 42).unwrap());
        let after = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            after.pending_wave,
            Some(42),
            "wave id is stamped at enqueue"
        );
        assert_eq!(
            after.status,
            SyncStatus::Pending,
            "status stays Pending — enqueue must not flip Syncing (sweep would skip it)"
        );
        // The story link is untouched.
        assert_eq!(after.story_id, rec.story_id);
        assert_eq!(after.story_source_path, rec.story_source_path);
    }

    #[test]
    fn assign_pending_wave_on_absent_record_is_noop_false() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        assert!(!assign_pending_wave(home.path(), &Principal::from(UID), wt.path(), 1).unwrap());
    }

    #[test]
    fn assign_pending_wave_refuses_to_regress_an_advanced_record() {
        // SW-FX-66: the enqueue-time stamp is lock-free and races Hook 1's
        // `set_sync_state` on the same file. Compare-and-set on `status ==
        // Pending` prevents a stale whole-record write from reverting a live
        // `Syncing`/`Synced` back to `Pending` (lost update). An already-advanced
        // record already carries a `pending_wave`, so the stamp is moot.
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(wt.path(), "alpha", "SW-FX-66"),
        )
        .unwrap();

        // A wave reached Hook 1: status Syncing, pending_wave 9.
        assert!(set_sync_state(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            9,
            SyncStatus::Syncing
        )
        .unwrap());

        // A racing enqueue stamp for a superseding wave must NOT clobber it.
        assert!(
            !assign_pending_wave(home.path(), &Principal::from(UID), wt.path(), 12).unwrap(),
            "stamp on a non-Pending record is a no-op (compare-and-set)"
        );
        let after = read_record(home.path(), &Principal::from(UID), wt.path())
            .unwrap()
            .unwrap();
        assert_eq!(after.status, SyncStatus::Syncing, "status not regressed");
        assert_eq!(after.pending_wave, Some(9), "live wave id preserved");
    }

    #[test]
    fn read_absent_record_is_none_not_error() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_team_reparses_authoritative_path_and_ignores_stale_cache() {
        // story actually lives under _team/right/ but the cached
        // intended_team lies "wrong" — resolve_team must trust the path.
        let mut rec = WorktreeStoryRecord::new(
            PathBuf::from("/abs/wt"),
            "acme",
            slug(),
            "SW-TS-1",
            story_path("right", "SW-TS-1"),
        );
        rec.intended_team = Some("wrong".to_string());

        assert_eq!(resolve_team(&rec), Some("right".to_string()));
    }

    #[test]
    fn resolve_team_is_none_for_non_team_path_neutral_case() {
        let rec = WorktreeStoryRecord::new(
            PathBuf::from("/abs/wt"),
            "acme",
            slug(),
            "LOOSE-1",
            PathBuf::from("/srv/scratch/notes/LOOSE-1.md"),
        );
        assert_eq!(resolve_team(&rec), None);
    }

    #[test]
    fn iter_and_find_by_story_id() {
        let home = TempDir::new().unwrap();
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(a.path(), "alpha", "SW-TS-1"),
        )
        .unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(b.path(), "beta", "SW-TS-2"),
        )
        .unwrap();

        let all = iter_story_records(home.path(), &Principal::from(UID)).unwrap();
        assert_eq!(all.len(), 2);

        let found = find_by_story_id(home.path(), &Principal::from(UID), "SW-TS-2").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].story_id, "SW-TS-2");
        assert_eq!(found[0].code_repo_path, b.path());
    }

    #[test]
    fn sr15_lookup_tally_counts_calls_and_records_scanned() {
        // A LOCAL instance — never the process-global `lookup_perf::COUNTERS` — so
        // concurrent tests that call `find_by_story_id` can't race this exact
        // assertion (the `frontmatter_perf` / `residual_metrics` discipline).
        let c = super::lookup_perf::LookupPerf::new();
        c.note(50);
        c.note(30);
        assert_eq!(
            c.snapshot(),
            (2, 80),
            "two lookups enumerating 50 then 30 records → 2 calls, 80 record reads"
        );
    }

    #[test]
    fn sr15_find_by_story_id_tallies_scanned_count_on_the_real_path() {
        // Pin the PRODUCTION enumerate → count → note wiring against a LOCAL counter (no
        // race on the process-global `COUNTERS` that parallel tests also bump). Three
        // records on disk, one matching: a correct wiring notes `scanned_count` (3), so a
        // regression to `note(matches.len())` (1) — or a dropped/gate-scoped note — fails.
        let home = TempDir::new().unwrap();
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let c = TempDir::new().unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(a.path(), "alpha", "SR-15-M"),
        )
        .unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(b.path(), "beta", "SR-15-X"),
        )
        .unwrap();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(c.path(), "gamma", "SR-15-Y"),
        )
        .unwrap();

        let perf = super::lookup_perf::LookupPerf::new();
        let found =
            super::find_by_story_id_tallied(home.path(), &Principal::from(UID), "SR-15-M", &perf)
                .unwrap();

        assert_eq!(found.len(), 1, "one record matches SR-15-M");
        assert_eq!(
            perf.snapshot(),
            (1, 3),
            "one enumerate scanned all 3 records — records_scanned is the scan size, not the match count"
        );
    }

    #[test]
    fn sr15_start_timer_is_none_when_gate_off() {
        // No logger in the test binary → SYNC_TIMING_TARGET is below Debug → the shared
        // RF-perf-0 gate is off → no clock is read. Proves the inert-when-off invariant
        // (mirrors `rf_perf9_start_timer_is_none_when_gate_off`).
        assert!(
            !crate::install::story_linker::sync_timing_enabled(),
            "precondition: the shared timing gate must be off with no logger armed"
        );
        assert!(
            super::lookup_perf::start_timer().is_none(),
            "start_timer must not read the clock when the shared timing gate is off"
        );
    }

    /// SR-15 (Task 1 / AC-1): `#[ignore]`d micro-capture of the `find_by_story_id` disk
    /// enumerate cost across N ∈ {10, 50, 200} records (epic-15 target ≈ 50), so the gate
    /// reads the per-call cost and its O(N) scaling directly instead of a single-N point.
    /// Machine- and disk-dependent → `#[ignore]`d, mirroring
    /// `rf_perf9_measure_frontmatter_lookups`. This is the deterministic per-call half of
    /// the gate; the live burst *frequency* rides SR-14's capture harness. REPS re-reads
    /// one warm directory, so the reported number is a **page-cache-warm** floor — a cold
    /// watcher-thread read costs more, but even a generous cold multiple stays well under a
    /// user-visible budget on a background thread. Run:
    /// `cargo test -p codesense --lib sr15_measure_find_by_story_id --release -- --ignored --nocapture`.
    #[test]
    #[ignore = "perf bench — run with --release --ignored --nocapture"]
    fn sr15_measure_find_by_story_id() {
        use std::time::Instant;
        const REPS: u32 = 200;

        println!(
            "=== SR-15 find_by_story_id (disk enumerate — per-call vs N records; page-cache-warm) ==="
        );
        // Snapshot the always-on global tally around the run so the bench also reports the
        // frequency half — the readout path for the prod-path `note()` — mirroring
        // `rf_perf9_measure_frontmatter_lookups`'s tally-delta line.
        let before = super::lookup_perf::COUNTERS.snapshot();
        let mut sink = 0usize;
        for &n in &[10usize, 50, 200] {
            let home = TempDir::new().unwrap();
            // One record matches `target`; the rest are distinct. The enumerate reads all
            // N record files regardless of matches — the cost under measure is that walk.
            // Paths are hermetic (under the temp home) and need not exist on disk — a
            // record is keyed by a hash of the `code_repo_path` string, not a real dir.
            let target = "SR-15-TARGET";
            for i in 0..n {
                let path = home.path().join(format!("wt-{i}"));
                let story = if i == 0 {
                    target.to_string()
                } else {
                    format!("SR-15-OTHER-{i}")
                };
                write_record(
                    home.path(),
                    &Principal::from(UID),
                    &sample(&path, "acme", &story),
                )
                .unwrap();
            }

            let t = Instant::now();
            for _ in 0..REPS {
                sink += find_by_story_id(home.path(), &Principal::from(UID), target)
                    .unwrap()
                    .len();
            }
            let per_call_us = t.elapsed().as_micros() as f64 / REPS as f64;
            println!(
                "N={n:5} records | find_by_story_id = {per_call_us:8.1} us/call (warm; reads all N files)"
            );
        }
        let after = super::lookup_perf::COUNTERS.snapshot();
        println!(
            "always-on tally delta: calls={} records_scanned={} (sink={sink})",
            after.0 - before.0,
            after.1 - before.1,
        );
    }

    #[test]
    fn iter_on_missing_index_dir_is_empty() {
        let home = TempDir::new().unwrap();
        assert!(iter_story_records(home.path(), &Principal::from(UID))
            .unwrap()
            .is_empty());
    }

    // -- SLW-WDE-6: worktree_roots (the editor path-sandbox allow-list source) --

    #[test]
    fn worktree_roots_collects_code_repo_paths_across_users() {
        let home = TempDir::new().unwrap();
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        // Two worktrees registered under two DIFFERENT users — the scan is
        // uid-agnostic, so both roots must surface without an active uid.
        write_record(
            home.path(),
            &Principal::from(42u64),
            &sample(a.path(), "alpha", "SW-TS-1"),
        )
        .unwrap();
        write_record(
            home.path(),
            &Principal::from(99u64),
            &sample(b.path(), "beta", "SW-TS-2"),
        )
        .unwrap();

        let roots = worktree_roots(home.path());
        assert_eq!(roots.len(), 2, "both users' roots collected; got {roots:?}");
        assert!(roots.iter().any(|r| r == a.path()));
        assert!(roots.iter().any(|r| r == b.path()));
    }

    #[test]
    fn worktree_roots_on_missing_index_is_empty() {
        let home = TempDir::new().unwrap();
        assert!(worktree_roots(home.path()).is_empty());
    }

    #[test]
    fn worktree_roots_skips_corrupt_record_not_fatal() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        write_record(
            home.path(),
            &Principal::from(42u64),
            &sample(wt.path(), "alpha", "SW-TS-1"),
        )
        .unwrap();
        // A malformed record alongside the good one must be skipped, not blind
        // the whole sweep (same fault-isolation posture as `iter_records`).
        let dir = home.path().join("users").join("42").join("worktrees");
        std::fs::write(dir.join("garbage.json"), b"{ not valid json").unwrap();

        assert_eq!(worktree_roots(home.path()), vec![wt.path().to_path_buf()]);
    }

    #[test]
    fn prune_removes_record_when_path_confirmed_gone() {
        let home = TempDir::new().unwrap();
        let gone = TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        let rec = sample(&gone_path, "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        // Make the worktree path actually disappear.
        drop(gone);

        assert!(prune_if_stale(home.path(), &Principal::from(UID), &rec).unwrap());
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), &gone_path).unwrap(),
            None
        );
    }

    #[test]
    fn prune_record_for_path_removes_confirmed_gone_record_by_path() {
        // SLW-WL-2: the path-keyed entry the manual/auto remove funnel uses — same
        // contract as `prune_if_stale`, keyed by path instead of a whole record.
        let home = TempDir::new().unwrap();
        let gone = TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        write_record(
            home.path(),
            &Principal::from(UID),
            &sample(&gone_path, "alpha", "SW-TS-1"),
        )
        .unwrap();
        drop(gone);

        assert!(prune_record_for_path(home.path(), &Principal::from(UID), &gone_path).unwrap());
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), &gone_path).unwrap(),
            None
        );
    }

    #[test]
    fn prune_leaves_record_when_path_live() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        assert!(!prune_if_stale(home.path(), &Principal::from(UID), &rec).unwrap());
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            Some(rec)
        );
    }

    #[test]
    fn path_confirmed_gone_true_only_for_removed_dir_with_present_parent() {
        // SLW-WL-9 (AC-1b): the gone-vs-unmounted gate.
        let parent = TempDir::new().unwrap();
        // Live worktree dir → not gone.
        let live = parent.path().join("wt-live");
        std::fs::create_dir(&live).unwrap();
        assert!(!path_confirmed_gone(&live));
        // Removed worktree dir, parent (container) still present → gone.
        let gone = parent.path().join("wt-gone");
        assert!(!gone.exists());
        assert!(path_confirmed_gone(&gone));
        // Whole container also gone (unmount analogue) → NOT gone (must self-heal on
        // remount, never reap).
        let unmounted = parent.path().join("missing-volume").join("wt");
        assert!(!path_confirmed_gone(&unmounted));
    }

    #[test]
    fn reap_confirmed_gone_records_drops_only_the_ghosts() {
        // SLW-WL-9 (AC-1b): the shared reconcile reap — a gone worktree's record is
        // removed; a live one is kept; the reaped paths are returned for the caller.
        let home = TempDir::new().unwrap();
        let live = TempDir::new().unwrap();
        let gone = TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        let live_rec = sample(live.path(), "alpha", "SW-TS-1");
        let gone_rec = sample(&gone_path, "beta", "SW-TS-2");
        write_record(home.path(), &Principal::from(UID), &live_rec).unwrap();
        write_record(home.path(), &Principal::from(UID), &gone_rec).unwrap();

        // The gone worktree disappears out-of-band (terminal `git worktree remove`).
        drop(gone);

        let (survivors, reaped) = reap_confirmed_gone_records(
            home.path(),
            &Principal::from(UID),
            vec![live_rec.clone(), gone_rec.clone()],
        );

        assert_eq!(reaped, vec![gone_path.clone()]);
        assert_eq!(survivors, vec![live_rec.clone()]);
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), &gone_path).unwrap(),
            None
        );
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), live.path()).unwrap(),
            Some(live_rec)
        );
    }

    #[test]
    fn reap_confirmed_gone_records_is_noop_when_all_live() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let rec = sample(wt.path(), "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        let (survivors, reaped) =
            reap_confirmed_gone_records(home.path(), &Principal::from(UID), vec![rec.clone()]);
        assert!(reaped.is_empty());
        assert_eq!(survivors, vec![rec.clone()]);
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            Some(rec)
        );
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let mut rec = sample(wt.path(), "alpha", "SW-TS-1");
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        // Hand-write a record with a future schema version.
        rec.schema_version = 999;
        let path = record_path(home.path(), &Principal::from(UID), wt.path()).unwrap();
        // write_record would force it back to 1, so serialize directly.
        std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();

        let err = parse_record(&std::fs::read(&path).unwrap()).unwrap_err();
        assert!(read_record(home.path(), &Principal::from(UID), wt.path()).is_err());
        assert!(matches!(
            err,
            Error::UnsupportedSchemaVersion { found: 999, .. }
        ));
    }

    // ----- Task 2: branch-name fallback + backfill -------------------------

    /// Build a temp org context-repo holding one story file under
    /// `_team/{team}/content/`, then index it with the real scanner.
    fn org_with_story(team: &str, story_id: &str) -> (TempDir, PathBuf, FrontmatterCache) {
        let org = TempDir::new().unwrap();
        let content = org.path().join(team).join("content");
        std::fs::create_dir_all(&content).unwrap();
        let file = content.join(format!("{story_id}-some-slug.md"));
        std::fs::write(
            &file,
            format!("---\nstatus: todo\nstory_id: {story_id}\nsource_repo: acme/codesense\n---\n# Story"),
        )
        .unwrap();
        let cache = FrontmatterCache::build_from_scan(org.path());
        (org, file, cache)
    }

    #[test]
    fn story_id_from_branch_parses_convention() {
        assert_eq!(
            story_id_from_branch("feat/SW-TS-1-sync-follows-the-story"),
            Some("SW-TS-1".to_string())
        );
        assert_eq!(
            story_id_from_branch("feat/SS-FU-19-watcher-cascade"),
            Some("SS-FU-19".to_string())
        );
        assert_eq!(
            story_id_from_branch("fix/T-11-perf"),
            Some("T-11".to_string())
        );
        // nested prefix uses the last '/' segment
        assert_eq!(
            story_id_from_branch("user/feat/SW-TS-1-x"),
            Some("SW-TS-1".to_string())
        );
        // bare id, no slug
        assert_eq!(
            story_id_from_branch("feat/SW-TS-1"),
            Some("SW-TS-1".to_string())
        );
        // not the convention
        assert_eq!(story_id_from_branch("main"), None);
        assert_eq!(story_id_from_branch("feat/lowercase-slug-only"), None);
    }

    #[test]
    fn backfill_from_branch_writes_record_and_derives_team() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let (_org, story_file, cache) = org_with_story("alpha", "SW-TS-1");

        let rec = backfill_from_branch(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            "acme",
            "feat/SW-TS-1-sync-follows-the-story",
            &cache,
        )
        .unwrap()
        .expect("fallback should resolve the indexed story");

        assert_eq!(rec.story_id, "SW-TS-1");
        assert_eq!(rec.story_source_path, story_file);
        // Slug carried from the story's `source_repo` frontmatter.
        assert_eq!(
            rec.source_repo,
            SourceRepoSlug::new("acme", "codesense").unwrap()
        );
        assert_eq!(resolve_team(&rec), Some("alpha".to_string()));
        // Backfilled through the one writer — now readable on the primary path.
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            Some(rec)
        );
    }

    /// SW-LN-B29: adopting a branch worktree in a LOCAL org records the story's repo under the
    /// team's configured `local`-host slug (from `teams.yml`), not the GitHub-shaped parse no
    /// team fan-out or refocus lookup ever queries.
    #[test]
    fn backfill_from_branch_uses_the_teams_local_slug() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let org_root = home.path().join(format!("users/{UID}/orgs/local/e2e"));
        let content = org_root.join("general/content/two-repo-epic");
        std::fs::create_dir_all(&content).unwrap();
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
        std::fs::write(
            content.join("TR-1-api-health-endpoint.md"),
            "---\nstatus: todo\nstory_id: TR-1\nsource_repo: local/fixture-api\n---\n# TR-1",
        )
        .unwrap();
        let cache = FrontmatterCache::build_from_scan(&org_root);

        let rec = backfill_from_branch(
            home.path(),
            &Principal::from(UID),
            wt.path(),
            "local/e2e",
            "feat/TR-1-api-health-endpoint",
            &cache,
        )
        .unwrap()
        .expect("the indexed local story backfills");
        assert_eq!(
            rec.source_repo,
            SourceRepoSlug::with_provider_and_host("local", "fixture-api", "local", Some("local"))
                .unwrap()
        );
    }

    #[test]
    fn backfill_unmatched_or_non_convention_branch_is_neutral() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let (_org, _f, cache) = org_with_story("alpha", "SW-TS-1");

        // Non-convention branch.
        assert_eq!(
            backfill_from_branch(
                home.path(),
                &Principal::from(UID),
                wt.path(),
                "acme",
                "main",
                &cache
            )
            .unwrap(),
            None
        );
        // Convention but unindexed story id.
        assert_eq!(
            backfill_from_branch(
                home.path(),
                &Principal::from(UID),
                wt.path(),
                "acme",
                "feat/SW-TS-9-ghost",
                &cache
            )
            .unwrap(),
            None
        );
        // Nothing was persisted.
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            None
        );
    }

    #[test]
    fn backfill_ambiguous_story_id_is_neutral() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        // Two teams declaring the same story_id → ambiguous → neutral.
        let org = TempDir::new().unwrap();
        for team in ["alpha", "beta"] {
            let content = org.path().join(team).join("content");
            std::fs::create_dir_all(&content).unwrap();
            std::fs::write(
                content.join("SW-TS-1-x.md"),
                "---\nstatus: todo\nstory_id: SW-TS-1\nsource_repo: acme/codesense\n---\n# S",
            )
            .unwrap();
        }
        let cache = FrontmatterCache::build_from_scan(org.path());
        assert_eq!(cache.by_story_id("SW-TS-1").len(), 2);

        assert_eq!(
            backfill_from_branch(
                home.path(),
                &Principal::from(UID),
                wt.path(),
                "acme",
                "feat/SW-TS-1-x",
                &cache
            )
            .unwrap(),
            None
        );
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            None
        );
    }

    #[test]
    fn hydration_policy_defaults_absent_and_round_trips() {
        use crate::install::hydration_closure::{HydrationPolicy, LIGHT};
        let mut rec = sample(Path::new("/abs/wt"), "alpha", "X-1");
        // Fresh record has no override (falls through to team default → LIGHT).
        assert_eq!(rec.hydration_policy, None);

        // A legacy record (pre-HV-6) simply omits the key; `#[serde(default)]`
        // reads it back `None` even under `deny_unknown_fields` (only UNKNOWN
        // fields are rejected — an absent KNOWN field defaults).
        let json = serde_json::to_string(&rec).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("hydration_policy");
        let legacy: WorktreeStoryRecord = serde_json::from_value(v).unwrap();
        assert_eq!(legacy.hydration_policy, None);

        // A record carrying an override round-trips exactly.
        rec.hydration_policy = Some(HydrationPolicy {
            reference_depth: 2,
            ..LIGHT
        });
        let back: WorktreeStoryRecord =
            serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
    }

    // ----- Review round 1 fixes -------------------------------------------

    #[test]
    fn forward_incompatible_record_reports_unsupported_schema_not_parse_error() {
        // A future record: bumped version AND an extra unknown field.
        // The schema probe must surface UnsupportedSchemaVersion, not let
        // deny_unknown_fields collapse it to Io(InvalidData).
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let path = record_path(home.path(), &Principal::from(UID), wt.path()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"schema_version":2,"code_repo_path":"/abs/wt","org_slug":"acme","story_id":"X-1","story_source_path":"/p","brand_new_field":true}"#,
        )
        .unwrap();

        let err = parse_record(&std::fs::read(&path).unwrap()).unwrap_err();
        assert!(read_record(home.path(), &Principal::from(UID), wt.path()).is_err());
        assert!(
            matches!(
                err,
                Error::UnsupportedSchemaVersion {
                    found: 2,
                    expected: 1
                }
            ),
            "expected UnsupportedSchemaVersion, got {err:?}"
        );
    }

    #[test]
    fn slug_from_source_repo_parses_and_rejects() {
        assert_eq!(
            slug_from_source_repo("acme/codesense"),
            Some(SourceRepoSlug::new("acme", "codesense").unwrap())
        );
        // No slash, empty, and traversal segments all fail safe to None.
        assert_eq!(slug_from_source_repo("nostroke"), None);
        assert_eq!(slug_from_source_repo(""), None);
        assert_eq!(slug_from_source_repo("acme/.."), None);
    }

    #[test]
    fn backfill_neutral_when_source_repo_frontmatter_missing() {
        // A story whose frontmatter omits `source_repo` yields no slug for
        // the record (the cache also orphan-skips such files), so the
        // fallback stays neutral and persists nothing.
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let org = TempDir::new().unwrap();
        let content = org.path().join("alpha").join("content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(
            content.join("SW-TS-1-x.md"),
            "---\nstatus: todo\nstory_id: SW-TS-1\n---\n# S",
        )
        .unwrap();
        let cache = FrontmatterCache::build_from_scan(org.path());

        assert_eq!(
            backfill_from_branch(
                home.path(),
                &Principal::from(UID),
                wt.path(),
                "acme",
                "feat/SW-TS-1-x",
                &cache
            )
            .unwrap(),
            None
        );
        assert_eq!(
            read_record(home.path(), &Principal::from(UID), wt.path()).unwrap(),
            None
        );
    }

    #[test]
    fn find_by_story_id_empty_query_matches_nothing() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        // A record whose story_id is empty (hand-constructed edge).
        let mut rec = sample(wt.path(), "alpha", "SW-TS-1");
        rec.story_id = String::new();
        write_record(home.path(), &Principal::from(UID), &rec).unwrap();

        assert!(find_by_story_id(home.path(), &Principal::from(UID), "")
            .unwrap()
            .is_empty());
    }

    // ----- SLW-IN-8: flatten backfill of the worktree index (AC-1/2/5) -------

    /// Create a live org root on disk (seed an `org.yml` so
    /// `enumerate_all_org_roots` recognizes it per RF-31) and return it.
    fn make_org_root(home: &Path, uid: u64) -> PathBuf {
        let org_root = home
            .join("users")
            .join(uid.to_string())
            .join("orgs")
            .join("acme")
            .join("codesense-context");
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(org_root.join("org.yml"), "").unwrap();
        org_root
    }

    /// A record whose `story_source_path` sits under the given org root at
    /// `{prefix}/default/content/impl/{story}.md` (prefix e.g. `_team` or "").
    fn record_at(
        org_root: &Path,
        code_repo: &Path,
        story: &str,
        team_container: &str,
    ) -> WorktreeStoryRecord {
        let mut p = org_root.to_path_buf();
        if !team_container.is_empty() {
            p = p.join(team_container);
        }
        let story_path = p
            .join("default")
            .join("content")
            .join("impl")
            .join(format!("{story}.md"));
        WorktreeStoryRecord::new(code_repo.to_path_buf(), "acme", slug(), story, story_path)
    }

    #[test]
    fn remove_record_deletes_and_is_idempotent() {
        // SW-WT-12 (S1): rollback primitive — removing the record makes a retry
        // re-enter ensure_story_worktree; a missing record removes cleanly (Ok).
        let home = TempDir::new().unwrap();
        let org_root = make_org_root(home.path(), 42);
        let wt = TempDir::new().unwrap();
        let rec = record_at(&org_root, wt.path(), "SW-WT-12-x", "_team");
        write_record(home.path(), &Principal::from(42u64), &rec).unwrap();
        assert!(read_record(home.path(), &Principal::from(42u64), wt.path())
            .unwrap()
            .is_some());

        remove_record(home.path(), &Principal::from(42u64), wt.path()).unwrap();
        assert!(
            read_record(home.path(), &Principal::from(42u64), wt.path())
                .unwrap()
                .is_none(),
            "record removed so a retry re-enters ensure_story_worktree"
        );
        // Idempotent: removing an already-absent record is Ok.
        remove_record(home.path(), &Principal::from(42u64), wt.path()).unwrap();
    }

    #[test]
    fn setup_complete_defaults_true_for_legacy_records() {
        // DW-WT12-2 (migration): a record serialized before the field existed must
        // read back `setup_complete == true` — it was written under the old
        // "record ⇒ setup done" ordering, so it stays adoptable (no schema bump).
        let org_root = make_org_root(&std::env::temp_dir(), 42);
        let wt = TempDir::new().unwrap();
        let rec = record_at(&org_root, wt.path(), "SW-WT-12-legacy", "_team");
        let mut json = serde_json::to_value(&rec).unwrap();
        json.as_object_mut().unwrap().remove("setup_complete");
        let parsed: WorktreeStoryRecord = serde_json::from_value(json).unwrap();
        assert!(
            parsed.setup_complete,
            "a legacy record with no setup_complete field must default to complete"
        );
    }

    #[test]
    fn setup_complete_new_true_and_false_roundtrips() {
        // DW-WT12-2: `new()` is complete by default; only a live MCP fresh-create
        // writes `false`, and that survives a serde round-trip so the crash-window
        // (Synced-but-not-complete) is durable across a restart.
        let org_root = make_org_root(&std::env::temp_dir(), 42);
        let wt = TempDir::new().unwrap();
        let rec = record_at(&org_root, wt.path(), "SW-WT-12-new", "_team");
        assert!(rec.setup_complete, "new() records are complete by default");

        let mut incomplete = rec.clone();
        incomplete.setup_complete = false;
        let parsed: WorktreeStoryRecord =
            serde_json::from_str(&serde_json::to_string(&incomplete).unwrap()).unwrap();
        assert!(
            !parsed.setup_complete,
            "an explicit false marker round-trips"
        );
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn context_and_story_records_share_store_without_fabricated_story_identity() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let principal = Principal::from(7);
        let root = home.join("context-worktrees/team/sprint-1");
        let row = ContextWorktreeRecord {
            owner: None,
            schema_version: SCHEMA_VERSION,
            kind: "context".into(),
            id: "cw_one".into(),
            org: "acme".into(),
            team: "team".into(),
            name: "Sprint 1".into(),
            branch: "team/sprint-1".into(),
            base_sha: "abc".into(),
            launch_cwd: root.join("team/content"),
            root,
            created_at: 1,
            last_used_at: 2,
            extras: Default::default(),
        };
        write_context_record(home, &principal, &row).unwrap();
        assert_eq!(
            context_records(home, &principal).unwrap(),
            vec![row.clone()]
        );
        assert!(iter_story_records(home, &principal).unwrap().is_empty());
        assert!(
            matches!(&iter_records(home, &principal).unwrap()[0], WorktreeRecord::Context(r) if r == &row)
        );
        let bytes =
            std::fs::read_to_string(record_path(home, &principal, &row.root).unwrap()).unwrap();
        assert!(!bytes.contains("source_repo"));
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use serde_json::json;

    fn story(home: &Path, principal: &Principal) -> WorktreeStoryRecord {
        let row = WorktreeStoryRecord::new(
            home.join("story-checkout"),
            "acme",
            SourceRepoSlug::new("acme", "code").unwrap(),
            "X-1",
            home.join("team/content/X-1.md"),
        );
        write_record(home, principal, &row).unwrap();
        row
    }

    #[test]
    fn direct_writers_preserve_skipped_existing_records() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let row = story(home.path(), &principal);
        let path = record_path(home.path(), &principal, &row.code_repo_path).unwrap();
        let mut newer = serde_json::to_value(&row).unwrap();
        newer["schema_version"] = json!(2);
        let bytes = serde_json::to_vec_pretty(&newer).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(write_record(home.path(), &principal, &row).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            update_record_with(home.path(), &principal, &row.code_repo_path, |_| Ok(
                row.clone()
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn context_write_preserves_unsupported_worktree_record() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let root = home.path().join("context");
        let row: ContextWorktreeRecord = serde_json::from_value(json!({
            "schema_version": SCHEMA_VERSION, "kind": "context", "id": "cw_one",
            "org": "acme", "team": "team", "name": "Sprint", "branch": "team/sprint",
            "base_sha": "abc", "root": root, "launch_cwd": root.join("team/content"),
            "created_at": 1, "last_used_at": 2
        }))
        .unwrap();
        write_context_record(home.path(), &principal, &row).unwrap();
        let path = record_path(home.path(), &principal, &root).unwrap();
        let mut newer = serde_json::to_value(&row).unwrap();
        newer["schema_version"] = json!(2);
        let bytes = serde_json::to_vec_pretty(&newer).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(write_context_record(home.path(), &principal, &row).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn reregister_preserves_unsupported_worktree_record() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let home_path = home.path().canonicalize().unwrap();
        let row = story(&home_path, &principal);
        std::fs::create_dir_all(&row.code_repo_path).unwrap();
        let path = record_path(home.path(), &principal, &row.code_repo_path).unwrap();
        let mut newer = serde_json::to_value(&row).unwrap();
        newer["schema_version"] = json!(2);
        newer["newer_metadata"] = json!({"keep": true});
        let bytes = serde_json::to_vec_pretty(&newer).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(crate::commands::content_engine::register_story_workspace(
            home.path(),
            &principal,
            row
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn skipped_record_warnings_repeat_only_after_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"{broken").unwrap();
        let mut warnings = Vec::new();
        for _ in 0..3 {
            let _ = read_index_record_with_warning(&path, |file, e| {
                warnings.push(format!("{}: {e}", file.display()))
            });
        }
        assert_eq!(warnings.len(), 1, "unchanged records warn once");
        std::fs::write(&path, b"{changed broken json").unwrap();
        for _ in 0..3 {
            let _ = read_index_record_with_warning(&path, |file, e| {
                warnings.push(format!("{}: {e}", file.display()))
            });
        }
        assert_eq!(warnings.len(), 2, "changed records warn again");
        assert!(warnings.iter().all(|warning| warning.contains("bad.json")));
        let other = dir.path().join("other.json");
        std::fs::write(&other, b"{broken").unwrap();
        let _ = read_index_record_with_warning(&other, |file, e| {
            warnings.push(format!("{}: {e}", file.display()))
        });
        assert_eq!(warnings.len(), 3, "each file has its own warning");
    }

    #[test]
    fn newer_story_fields_load_and_survive_sync_state_rewrite() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let row = story(home.path(), &principal);
        let path = record_path(home.path(), &principal, &row.code_repo_path).unwrap();
        let mut value = serde_json::to_value(&row).unwrap();
        value["newer_feature"] = json!({"nested": [1, "keep", true]});
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            read_record(home.path(), &principal, &row.code_repo_path)
                .unwrap()
                .unwrap()
                .story_id,
            "X-1"
        );
        assert_eq!(
            iter_story_records(home.path(), &principal).unwrap().len(),
            1
        );
        assert!(set_sync_state(
            home.path(),
            &principal,
            &row.code_repo_path,
            7,
            SyncStatus::Synced
        )
        .unwrap());
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(after["newer_feature"], value["newer_feature"]);
    }

    #[test]
    fn newer_context_fields_load_and_survive_rewrite() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let value = json!({
            "schema_version": SCHEMA_VERSION, "kind": "context", "id": "cw_one",
            "org": "acme", "team": "team", "name": "Sprint", "branch": "team/sprint",
            "base_sha": "abc", "root": home.path().join("context"),
            "launch_cwd": home.path().join("context/team/content"),
            "created_at": 1, "last_used_at": 2,
            "newer_feature": {"keep": true}
        });
        let path = record_path(home.path(), &principal, &home.path().join("context")).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let mut rows = context_records(home.path(), &principal).unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].last_used_at = 3;
        write_context_record(home.path(), &principal, &rows[0]).unwrap();
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(after["newer_feature"], value["newer_feature"]);
        assert_eq!(after["last_used_at"], 3);
    }

    #[test]
    fn corrupt_record_does_not_block_story_lookup_or_other_records() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42);
        let row = story(home.path(), &principal);
        let bad = worktrees_index_root(home.path(), &principal).join("corrupt.json");
        std::fs::write(&bad, b"{broken").unwrap();
        assert_eq!(
            story_registrations(home.path(), &row.code_repo_path)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            iter_story_records(home.path(), &principal).unwrap(),
            vec![row]
        );
        assert_eq!(std::fs::read(bad).unwrap(), b"{broken");
    }
}
