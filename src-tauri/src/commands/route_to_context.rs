// SPDX-License-Identifier: MIT
//! SLW-WD-4 — the app-side (Tauri command) bridge to WD-3's route/un-route
//! verbs.
//!
//! WD-3 (`install/story_linker.rs`) shipped `route_file_to_context` /
//! `unroute_file_from_context` as `pub(crate)` cores, exposed to *agents* via
//! MCP `#[tool]` wrappers (`mcp/handler.rs`). But the React overlay reaches Rust
//! through `@/tauri/commands`, never MCP — so a per-file "→ context" button in
//! the source-diff overlay (WD-4 AC-1) had no way to invoke the verb.
//!
//! This module is the **single orchestration home** for both callers (WD-4
//! RD-A): the MCP tools delegate to [`run_route_for`] / [`run_unroute`], and the
//! frontend calls the [`route_file_to_context`] / [`unroute_file_from_context`]
//! `#[tauri::command]`s directly. The orchestration — derive active context →
//! resolver context → per-repo lock → `spawn_blocking` the WD-3 core — is
//! written ONCE here; neither caller re-implements any route/adopt/exclude
//! logic (that stays behind WD-3's core).
//!
//! [`ActiveContext`] + [`derive_active_context`] were hoisted here from
//! `mcp/handler.rs` (their only prior home) so both the MCP wrappers and this
//! command share one race-safe deriver; the MCP layer keeps a thin
//! `ErrorData`-mapping shim.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::content_engine::layers::LayerId;
use crate::content_engine::ResolverContext;
use crate::domain::{Principal, SourceRepoSlug};
use crate::mcp::state::ActiveTeamState;

/// Resolved active context for the content-engine command surface. Carries the
/// four fields `derive_resolver_context` consumes so callers forward without
/// re-deriving from app singletons.
///
/// `team_name` is `Option<String>` to match the resolver's signature (which
/// accepts a `None` no-team install context). In practice these flows always
/// run with a team selected — [`derive_active_context`] errors if the frontend
/// hasn't set one yet, so callers never see `team_name = None` here.
#[derive(Debug, Clone)]
pub(crate) struct ActiveContext {
    pub codesense_home: PathBuf,
    pub user_id: u64,
    pub org_slug: String,
    pub team_name: Option<String>,
}

impl ActiveContext {
    /// SW-FG-1: the CodeSense-native identity key for this context's user — the
    /// single conversion point from the raw GitHub `user_id`, so path-scope
    /// callers use `ctx.principal()` rather than `Principal::from(ctx.user_id)`.
    pub(crate) fn principal(&self) -> Principal {
        Principal::from(self.user_id)
    }

    /// Build a context for a concrete, selected team. Both derivers
    /// ([`derive_active_context`] and the any-team [`classify_resolution`])
    /// produce the same shape — `team_name` is always `Some` here — so the
    /// struct literal lives in one place.
    pub(crate) fn for_team(
        codesense_home: PathBuf,
        user_id: u64,
        org_slug: String,
        team: String,
    ) -> Self {
        Self {
            codesense_home,
            user_id,
            org_slug,
            team_name: Some(team),
        }
    }

    /// SW-AU-37: the active team's `content/` dir, when a team is selected. `None` with
    /// no active team.
    pub(crate) fn team_content_dir(&self) -> Option<PathBuf> {
        self.team_name.as_ref().and_then(|team| {
            let org_root = crate::context_repo::path_to_org_root(
                &self.codesense_home,
                &self.principal(),
                &self.org_slug,
            )
            .ok()?;
            Some(crate::context_repo::path_to_team_content_dir(
                &org_root, team,
            ))
        })
    }

    /// SW-AU-35: this context as a [`ResolverContext`] carrying its ambient
    /// identity. Inherited reads resolve the current filtered view on demand.
    pub(crate) fn to_minimal_resolver_context(&self) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: self.codesense_home.clone(),
            user_id: self.user_id,
            org_slug: self.org_slug.clone(),
            team_name: self.team_name.clone(),
        }
    }
}

/// Resolve the active context from ActiveTeamState, the selected local profile,
/// and the data home. Missing profile/team/home errors remain plain strings
/// for retained Tauri callers and the MCP invalid-params adapter.
///
/// Capture the selected team and local principal under the same activation
/// transaction used by profile selection. Identical org/team names in two
/// profiles cannot make a mixed snapshot appear current.
pub(crate) async fn derive_active_context<R: tauri::Runtime>(
    app: &AppHandle<R>,
) -> Result<ActiveContext, String> {
    let watcher = app
        .try_state::<crate::commands::watch::WatcherState>()
        .ok_or_else(|| "WatcherState not registered".to_string())?;
    let _activation = watcher.lock_team_activation().await;
    let active_team_state = app
        .try_state::<ActiveTeamState>()
        .ok_or_else(|| "ActiveTeamState not registered".to_string())?;
    let active_team = active_team_state.get().ok_or_else(|| {
        "no active team selected — open codesense and pick a team first".to_string()
    })?;
    let (codesense_home, user_id) = resolve_ambient(app).await?;
    Ok(ActiveContext::for_team(
        codesense_home,
        user_id,
        active_team.org_slug,
        active_team.team_name,
    ))
}

/// Resolve the local profile's checked numeric namespace and the data home.
/// This performs no credential lookup. Active-team callers hold the activation
/// transaction while combining these facts with the selected team.
pub(crate) async fn resolve_ambient<R: tauri::Runtime>(
    app: &AppHandle<R>,
) -> Result<(PathBuf, u64), String> {
    let codesense_home = crate::commands::paths::codesense_home_dir().ok_or_else(|| {
        "codesense home directory unavailable (HOME/USERPROFILE unset?)".to_string()
    })?;

    let user_id = crate::commands::local_profile::active(app)?
        .principal
        .numeric_id()?;

    Ok((codesense_home, user_id))
}

/// SW-AU-24 — the typed outcome of resolving an **arbitrary** `(org, team)` the
/// user may target. A Rust-internal seam consumed by the agent-authoring tool
/// suite (SW-AU-18/19/20), which owns the FE-serialized envelope and maps these
/// onto it (`Unauthorized → unauthorizedTeam`, `NotSynced → notSyncedLocally`).
#[derive(Debug, Clone)]
#[allow(dead_code)] // `Ok`'s context is read by the SW-AU-18/19/20 consumers.
pub(crate) enum ContextResolution {
    /// The team is a locally-synced member team — resolved to its context. The
    /// shape matches [`derive_active_context`] exactly; the active team is
    /// **untouched** in the process.
    Ok(ActiveContext),
    /// `(org, team)` is not a non-archived team in any synced org's `teams.yml`
    /// (absent, archived, the org was never cloned, its registry failed to load,
    /// or the `org` slug is malformed/unsafe). NOT a permission check — it means
    /// "not confirmable from local data"; membership is bounded to locally-synced
    /// orgs (you cannot enumerate what was never cloned). A broken-on-disk
    /// registry is `log::warn!`'d in `team_sync_states` so it stays diagnosable.
    /// The caller already holds the `(org, team)` it asked for, so it isn't echoed.
    Unauthorized,
    /// The team IS listed in a synced org's `teams.yml`, but its content dir is
    /// absent locally — the only reachable "member but not synced" case.
    /// Auto-sync-on-target is a follow-up.
    NotSynced,
}

/// SW-AU-24 — one org+team the signed-in user can target locally, with whether
/// its content is present (`synced`). The unit of both [`enumerate_orgs_teams`]
/// (the discovery surface) and the membership check inside [`derive_context_for`]
/// — so the archived-exclusion + synced-flag rule lives in exactly one place
/// ([`team_sync_states`]). Serialized for the MCP/FE surface AU-18 builds on top.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrgTeamEntry {
    pub org: String,
    pub team: String,
    pub synced: bool,
    /// The team's declared starter page (`teams.yml` `landing_page:`), relative
    /// to its content folder; absent ⇒ the README/index fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starter_page: Option<String>,
}

/// The non-archived teams of one org with each team's local-sync state. Pure of
/// the active team and of app state — reads only `{org_root}/teams.yml` (via
/// [`load_org`]) and stat's each team's content dir. Reused by both any-team
/// entry points below; runs inside their `await_blocking` closures (the caller
/// keeps this off the async runtime — never the UI thread).
///
/// `synced` is content-dir **presence** (`path_to_team_content_dir(...).exists()`)
/// — the honest "can I read/write this team's content locally?" boundary — not
/// the worktree-level `SyncStatus`; a team can be present here yet have a stale
/// worktree, and that distinction isn't this seam's concern.
fn team_sync_states(org_slug: &str, org_root: &Path) -> Vec<OrgTeamEntry> {
    let loaded = crate::domain::load_org(org_slug.to_string(), org_root, None);
    let Some(org) = loaded.org else {
        // Org not cloned, OR its registry (org/people/teams.yml) failed to load
        // or compose (a bad row, a duplicate team) → we cannot confirm any member
        // here, so the org contributes nothing and its teams read as `Unauthorized`.
        // Surface a broken-on-disk registry so it's diagnosable rather than silently
        // indistinguishable from "never cloned".
        if !loaded.file_errors.is_empty() {
            log::warn!(
                target: "commands::route_to_context",
                "team_sync_states: org {org_slug:?} registry is unavailable; a later request will reload it: {:?}",
                loaded.file_errors
            );
        }
        return Vec::new();
    };
    org.teams
        .into_iter()
        .filter(|t| !t.archived)
        .map(|t| {
            // `.is_dir()` (not `.exists()`): a stray file or dangling symlink at the
            // content path is not a usable content dir — report it not-synced so a
            // downstream writer never targets a non-directory.
            let synced = crate::context_repo::path_to_team_content_dir(org_root, &t.name).is_dir();
            OrgTeamEntry {
                org: org_slug.to_string(),
                team: t.name,
                synced,
                starter_page: t.landing_page,
            }
        })
        .collect()
}

/// Pure classification of a single-org team list into a [`ContextResolution`].
/// Split out from the FS/auth wiring so the outcome logic is unit-tested without
/// an `AppHandle`. `states` is [`team_sync_states`] for `org` (archived already
/// excluded, so an archived/absent team falls through to `Unauthorized`).
fn classify_resolution(
    states: Vec<OrgTeamEntry>,
    codesense_home: PathBuf,
    user_id: u64,
    org: String,
    team: String,
) -> ContextResolution {
    match states.into_iter().find(|e| e.team == team) {
        None => ContextResolution::Unauthorized,
        Some(e) if !e.synced => ContextResolution::NotSynced,
        Some(_) => {
            ContextResolution::Ok(ActiveContext::for_team(codesense_home, user_id, org, team))
        }
    }
}

/// The pure any-team resolution core: validate the caller's `org`, compute its
/// root, read its teams, and classify. Split from [`derive_context_for`] so the
/// whole resolve — **including the traversal guard** — is unit-tested without an
/// `AppHandle`. Does filesystem reads, so it runs inside the caller's
/// `await_blocking`.
pub(crate) fn resolve_local(
    codesense_home: PathBuf,
    user_id: u64,
    org: String,
    team: String,
) -> ContextResolution {
    // Trust boundary: `org` is a caller-supplied arg (the AU-18/19/20 tools),
    // and `path_to_org_root` is pure `join` with no sanitization. Gate it with
    // the SAME validator `discover_local_orgs_impl` applies, so a `..`/absolute
    // or non-`owner/repo` slug can't escape the orgs dir into an arbitrary
    // `teams.yml`. A malformed slug is simply not targetable → `Unauthorized`.
    // (`team` needs no such gate — it only ever string-matches an on-disk team
    // name that already passed `is_slug_safe` at parse; it never joins a path.)
    if crate::context_repo::org_slug::validate_org_slug(&org).is_some() {
        return ContextResolution::Unauthorized;
    }
    let org_root = match crate::context_repo::path_to_org_root(
        &codesense_home,
        &Principal::from(user_id),
        &org,
    ) {
        Ok(root) => root,
        Err(error) => {
            log::warn!("{error}");
            return ContextResolution::Unauthorized;
        }
    };
    let states = team_sync_states(&org, &org_root);
    classify_resolution(states, codesense_home, user_id, org, team)
}

/// SW-AU-24 — resolve an **arbitrary** `(org, team)` the user belongs to, WITHOUT
/// switching the app's active team. Mirrors [`derive_active_context`]'s signature
/// (`&AppHandle` for the ambient home/auth reads only) but reads `org`/`team`
/// from the args instead of [`ActiveTeamState`] — so resolving team B while team
/// A is active leaves A active. Returns a typed [`ContextResolution`]
/// (`Ok`/`Unauthorized`/`NotSynced`); membership + sync are bounded to
/// locally-synced data (the honest limit — you cannot enumerate what was never
/// cloned).
///
/// The FS work (read `teams.yml`, stat the content dir) runs on the blocking
/// pool via [`await_blocking`] — never on the async runtime worker, so a resolve
/// cannot stall the app.
///
/// Foundational seam consumed by SW-AU-18/19/20.
pub(crate) async fn derive_context_for<R: tauri::Runtime>(
    app: &AppHandle<R>,
    org: &str,
    team: &str,
) -> Result<ContextResolution, String> {
    let (codesense_home, user_id) = resolve_ambient(app).await?;
    let (org, team) = (org.to_string(), team.to_string());
    crate::commands::async_helpers::await_blocking("derive_context_for", move || {
        Ok(resolve_local(codesense_home, user_id, org, team))
    })
    .await
}

/// SW-AU-24 — the discovery surface: every org+team the signed-in user can target
/// locally, each flagged `synced`. Bounded to locally-synced orgs — the set is
/// `discover_local_orgs_impl` (the clone scan) × each org's non-archived
/// `teams.yml` teams. An org that was never cloned cannot appear (no data source
/// for it); an archived team is omitted; a `teams.yml` team with no content dir
/// appears with `synced = false`.
///
/// The FS scan runs on the blocking pool via [`await_blocking`].
///
/// Foundational seam consumed by SW-AU-18 (`enumerate_orgs_teams` MCP tool).
pub(crate) async fn enumerate_orgs_teams(app: &AppHandle) -> Result<Vec<OrgTeamEntry>, String> {
    let (codesense_home, user_id) = resolve_ambient(app).await?;
    crate::commands::async_helpers::await_blocking("enumerate_orgs_teams", move || {
        let orgs = crate::commands::org::discover_local_orgs_impl(
            &codesense_home,
            &Principal::from(user_id),
        )?;
        let mut out = Vec::new();
        for summary in orgs {
            out.extend(team_sync_states(
                &summary.slug,
                Path::new(&summary.org_root),
            ));
        }
        Ok(out)
    })
    .await
}

/// SW-AU-35 — the whole **synced footprint** as a resolved [`ActiveContext`] per
/// `(org, team)` for an already-known home and user, decoupled from the UI active
/// team (SW-AU-24 resolvers, no `ActiveTeamState`): whichever team is on screen,
/// every synced team's automations are in scope.
///
/// Bounded to **locally-synced** teams (content dir present) — the honest limit; a
/// never-cloned org/team can't appear. Reuses the same sync cores as
/// [`enumerate_orgs_teams`] (`discover_local_orgs_impl` × [`team_sync_states`]),
/// so the archived-exclusion + synced-flag rule stays in one place. An unsynced
/// team is dropped here (not surfaced as an entry) — callers fire only where a
/// team's content is readable. It does filesystem reads, so async callers keep it
/// on the blocking pool.
pub(crate) fn synced_contexts_in(
    codesense_home: &Path,
    user_id: u64,
) -> Result<Vec<ActiveContext>, String> {
    let orgs =
        crate::commands::org::discover_local_orgs_impl(codesense_home, &Principal::from(user_id))?;
    let entries = orgs
        .into_iter()
        .flat_map(|summary| team_sync_states(&summary.slug, Path::new(&summary.org_root)))
        .collect();
    Ok(synced_contexts_from(entries, codesense_home, user_id))
}

/// The pure selection core of [`synced_contexts_in`]: keep the **synced** teams and
/// build one [`ActiveContext`] each. Split from the FS/auth wiring so the
/// synced-only filter + per-team context shape are unit-tested without an
/// `AppHandle`. An unsynced team is dropped (a caller can't read its content), so
/// the firing footprint is presence-bounded.
fn synced_contexts_from(
    entries: Vec<OrgTeamEntry>,
    codesense_home: &Path,
    user_id: u64,
) -> Vec<ActiveContext> {
    entries
        .into_iter()
        .filter(|e| e.synced)
        .map(|e| ActiveContext::for_team(codesense_home.to_path_buf(), user_id, e.org, e.team))
        .collect()
}

/// SLW-WD-3: what a route did. SW-XS-8b: a route is an explicit publish of
/// one workspace file to the team's content on the preparation engine
/// ([`crate::agents::prepare::publish::route`]); only the team tier is
/// routable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteOutcome {
    Routed,
    AlreadyRouted,
    Refused,
    Collision {
        source_hash: [u8; 32],
        dest_hash: [u8; 32],
    },
    AlreadyTracked,
    NotProjectable,
}

impl From<crate::agents::prepare::publish::RouteResult> for RouteOutcome {
    fn from(result: crate::agents::prepare::publish::RouteResult) -> Self {
        use crate::agents::prepare::publish::RouteResult as R;
        match result {
            R::Routed => Self::Routed,
            R::AlreadyRouted => Self::AlreadyRouted,
            R::Refused => Self::Refused,
            R::Collision {
                source_hash,
                dest_hash,
            } => Self::Collision {
                source_hash,
                dest_hash,
            },
            R::AlreadyTracked => Self::AlreadyTracked,
            R::NotProjectable => Self::NotProjectable,
        }
    }
}

/// SLW-WD-4 — the serialized route outcome handed across the Tauri boundary and
/// (via the MCP shim) to agents. Mirrors WD-3's `RouteOutcome` discriminators
/// byte-for-byte as an internally-tagged `{ "outcome": … }` object so both the
/// FE wrapper and the MCP tool agree on the shape.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum RouteOutcomeDto {
    Routed,
    AlreadyRouted,
    Refused,
    AlreadyTracked,
    NotProjectable,
    Collision {
        #[serde(rename = "sourceHash")]
        source_hash: String,
        #[serde(rename = "destHash")]
        dest_hash: String,
    },
}

impl From<&RouteOutcome> for RouteOutcomeDto {
    fn from(outcome: &RouteOutcome) -> Self {
        match outcome {
            RouteOutcome::Routed => Self::Routed,
            RouteOutcome::AlreadyRouted => Self::AlreadyRouted,
            RouteOutcome::Refused => Self::Refused,
            RouteOutcome::AlreadyTracked => Self::AlreadyTracked,
            RouteOutcome::NotProjectable => Self::NotProjectable,
            RouteOutcome::Collision {
                source_hash,
                dest_hash,
            } => {
                let hex = |b: &[u8; 32]| blake3::Hash::from_bytes(*b).to_hex().to_string();
                Self::Collision {
                    source_hash: hex(source_hash),
                    dest_hash: hex(dest_hash),
                }
            }
        }
    }
}

/// Error from the shared route orchestration. Preserves the ONE distinction the
/// MCP tools need for byte-parity with the pre-hoist behavior (RD-A): a
/// resolver-context validation failure carries the typed engine error so the MCP
/// shim can re-attach `engine_err_to_mcp`'s JSON payload (skill kind-matching),
/// while every other failure is a plain message (mapped to `invalid_params`,
/// exactly as before). `Display` flattens both for the Tauri command's `String`
/// boundary (the FE surfaces the message via `toast.error`).
#[derive(Debug)]
pub(crate) enum RouteError {
    /// A reachable precondition / core-refusal failure → `invalid_params` (as
    /// before): the plain messages the skill surfaces conversationally.
    Message(String),
    /// A should-never-happen infra failure (missing `WaveCoordinator`, a
    /// `spawn_blocking` join panic) → `internal_error`, matching the pre-hoist
    /// error class (byte-parity, RD-A).
    Internal(String),
    /// A resolver-context validation failure — carries the command-surface
    /// engine error so the MCP shim can reproduce `engine_err_to_mcp`'s typed
    /// payload (same type `derive_resolver_context` returns).
    Resolver(crate::commands::content_engine::Error),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::Message(m) | RouteError::Internal(m) => f.write_str(m),
            RouteError::Resolver(e) => write!(f, "{e}"),
        }
    }
}

/// The lock-free half of a route: derive the resolver context only.
async fn derive_ctx(app: &AppHandle) -> Result<ResolverContext, RouteError> {
    let actx = derive_active_context(app)
        .await
        .map_err(RouteError::Message)?;
    crate::commands::content_engine::derive_resolver_context(
        actx.codesense_home.clone(),
        actx.user_id,
        actx.org_slug,
        actx.team_name,
    )
    .map_err(RouteError::Resolver)
}

/// Shared orchestration for a route (the MCP tool and the Tauri command run
/// this one path). SW-XS-8b: on the preparation engine, under its
/// per-destination lock; the wave coordinator and the legacy lease are gone.
pub(crate) async fn run_route_for(
    app: &AppHandle,
    code_repo_path: PathBuf,
    slug: SourceRepoSlug,
    logical_name: String,
    layer: LayerId,
    caller: crate::context_repo::caller_scope::CallerScope,
) -> Result<RouteOutcome, RouteError> {
    // Team content is the only layer (SW-XS-8c): an old caller's
    // `TeamWorkspace` deserializes to it, and `TeamRepo` is refused at the
    // parameter boundary.
    let LayerId::TeamProjection = layer;
    let ctx = derive_ctx(app).await?;
    tokio::task::spawn_blocking(move || {
        let target = crate::agents::ManifestTarget {
            context_root: None,
            org: ctx.org_slug.clone(),
            team: ctx
                .team_name
                .clone()
                .ok_or("route_file_to_context: no active team")?,
            repo: Some(slug),
        };
        let legacy = || crate::agents::prepare::legacy_rows_local(&ctx, &code_repo_path);
        crate::agents::prepare::publish::route_for(
            &ctx.codesense_home,
            &ctx.principal(),
            &code_repo_path,
            &target,
            &logical_name,
            &uuid::Uuid::new_v4().to_string(),
            Some(&legacy),
            &caller,
        )
        .map(RouteOutcome::from)
    })
    .await
    .map_err(|e| RouteError::Internal(format!("route join error: {e}")))?
    .map_err(|e| RouteError::Message(format!("route_file_to_context failed: {e}")))
}

/// Resolve `story_id` → its epic dir, scoped to the destination's slug and the
/// active team so an id shared across teams/repos never resolves ambiguously.
/// The `verb` argument (e.g. `"hydrate_story"`) prefixes error messages.
pub(crate) fn resolve_story_epic_dir(
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    ctx: &ResolverContext,
    slug_str: &str,
    story_id: &str,
    verb: &str,
) -> Result<PathBuf, RouteError> {
    resolve_seed_paths(cache, ctx, slug_str, story_id, verb).map(|(_story, epic)| epic)
}

/// SW-HV-6 — resolve `story_id` → `(story file path, epic dir)`, scoped to this
/// worktree's slug + active team (same unambiguous match as
/// [`resolve_story_epic_dir`], which now delegates here). The closure resolver
/// needs the story **path** as the reference-walk seed, not just its epic dir.
pub(crate) fn resolve_seed_paths(
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    ctx: &ResolverContext,
    slug_str: &str,
    story_id: &str,
    verb: &str,
) -> Result<(PathBuf, PathBuf), RouteError> {
    let matches: Vec<&crate::install::frontmatter_cache::CacheEntry> = cache
        .by_story_id(story_id)
        .into_iter()
        .filter(|e| {
            e.source_repo() == slug_str
                && ctx.team_name.as_deref().is_none_or(|t| {
                    crate::context_repo::inheritance::real_content_owner(&e.path)
                        .is_some_and(|(_, owner)| owner == t)
                })
        })
        .collect();
    let entry = match matches.as_slice() {
        [entry] => *entry,
        [] => {
            return Err(RouteError::Message(format!(
                "{verb}: no story '{story_id}' found for {slug_str} in the active team"
            )))
        }
        _ => {
            return Err(RouteError::Message(format!(
                "{verb}: story id '{story_id}' is ambiguous ({} matches) — cannot resolve",
                matches.len()
            )))
        }
    };
    let epic = entry
        .path
        .parent()
        .ok_or_else(|| {
            RouteError::Message(format!("{verb}: story '{story_id}' has no parent epic dir"))
        })
        .map(Path::to_path_buf)?;
    Ok((entry.path.clone(), epic))
}

/// Enumerate every regular file under `dir` (absolute paths). The single unit of
/// hydration granularity: an epic dir hydrates whole.
pub(crate) fn collect_epic_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// SW-HV-6 — resolve a policy's closure for `story_id`: snapshot the cache,
/// resolve the seed story's `(path, epic dir)`, then run the freshness-gated
/// [`resolve_hydration_closure`]. Returns the closure plus the seed epic dir (the
/// hydrate outcome reports it).
fn resolve_closure_for(
    cache: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    ctx: &ResolverContext,
    slug_str: &str,
    story_id: &str,
    verb: &str,
    policy: &crate::install::hydration_closure::HydrationPolicy,
) -> Result<(crate::install::hydration_closure::ClosureResult, PathBuf), RouteError> {
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::install::hydration_closure as hc;
    let (seed_story_path, seed_epic_dir) = {
        let snapshot = FrontmatterCache::snapshot(cache);
        resolve_seed_paths(&snapshot, ctx, slug_str, story_id, verb)?
    };
    let dir_team = hc::team_of(&seed_epic_dir);
    let closure =
        hc::resolve_hydration_closure(cache, &seed_epic_dir, &seed_story_path, &dir_team, policy);
    Ok((closure, seed_epic_dir))
}

/// Shared orchestration for an un-route — mirrors [`run_route_for`] but calls WD-3's
/// inverse core (no `layer`; un-route targets the one routed entry).
pub(crate) async fn run_unroute(
    app: &AppHandle,
    code_repo_path: PathBuf,
    _slug: SourceRepoSlug,
    logical_name: String,
) -> Result<(), RouteError> {
    let ctx = derive_ctx(app).await?;
    tokio::task::spawn_blocking(move || {
        // The workspace's own registration (its org, team and repo), as the
        // reverse-sync preview resolves it — never just the viewer's team.
        let root = code_repo_path
            .canonicalize()
            .map_err(|e| format!("{} is unavailable: {e}", code_repo_path.display()))?;
        let (ctx, target) = crate::commands::watch::workspace_target(&ctx, &root)?;
        crate::agents::prepare::publish::unroute(
            &ctx.codesense_home,
            &ctx.principal(),
            &code_repo_path,
            &target,
            &logical_name,
        )
    })
    .await
    .map_err(|e| RouteError::Internal(format!("unroute join error: {e}")))?
    .map_err(|e| RouteError::Message(format!("unroute_file_from_context failed: {e}")))
}

/// SLW-WD-4 (AC-1/AC-3) — route a worktree file into the team content layer and
/// hide it from git. The app-side entry the source-diff overlay's "→ context"
/// action calls. `code_repo_slug` is the FE's `"owner/repo"` string (parsed
/// here); `layer` defaults to team content, the only layer (an old caller's
/// `TeamWorkspace` loads as team content; `TeamRepo` is refused, SW-XS-8c).
#[tauri::command]
pub async fn route_file_to_context(
    app: AppHandle,
    code_repo_path: String,
    code_repo_slug: String,
    logical_name: String,
    layer: Option<LayerId>,
    page_path: Option<String>,
    frame_id: Option<String>,
) -> Result<RouteOutcomeDto, String> {
    let slug = SourceRepoSlug::parse_owner_repo(&code_repo_slug)
        .ok_or_else(|| format!("invalid code_repo_slug (expected owner/repo): {code_repo_slug}"))?;
    let caller = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let outcome = run_route_for(
        &app,
        PathBuf::from(code_repo_path),
        slug,
        logical_name,
        layer.unwrap_or(LayerId::TeamProjection),
        caller,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(RouteOutcomeDto::from(&outcome))
}

/// SLW-WD-4 (AC-4) — the inverse of [`route_file_to_context`]: un-route a
/// previously-routed file back to an ordinary untracked source file. Backs the
/// overlay's Undo affordance.
#[tauri::command]
pub async fn unroute_file_from_context(
    app: AppHandle,
    code_repo_path: String,
    code_repo_slug: String,
    logical_name: String,
) -> Result<(), String> {
    let slug = SourceRepoSlug::parse_owner_repo(&code_repo_slug)
        .ok_or_else(|| format!("invalid code_repo_slug (expected owner/repo): {code_repo_slug}"))?;
    run_unroute(&app, PathBuf::from(code_repo_path), slug, logical_name)
        .await
        .map_err(|e| e.to_string())
}

/// A story's hydration set: its epic (or closure) directories and their files.
pub(crate) struct RemoteStoryFiles {
    pub epic_dir: PathBuf,
    pub epic_dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// SW-XS-1: resolve what `hydrate_story` hydrates, without touching the
/// destination or taking any admission lease (shared by the legacy and the
/// selected-engine paths).
pub(crate) fn remote_story_files(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    story_id: &str,
    cache: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    policy: Option<crate::install::hydration_closure::HydrationPolicy>,
) -> Result<RemoteStoryFiles, RouteError> {
    remote_story_files_for(ctx, slug, story_id, cache, policy, &Default::default())
}

pub(crate) fn remote_story_files_for(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    story_id: &str,
    cache: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    policy: Option<crate::install::hydration_closure::HydrationPolicy>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RemoteStoryFiles, RouteError> {
    let chosen_cache;
    let cache = if let Some(root) = &ctx.context_root {
        chosen_cache = crate::install::frontmatter_cache::FrontmatterCache::shared(
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(&root.root),
        );
        &chosen_cache
    } else {
        cache
    };
    let scoped_cache;
    let cache = if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        cache
    } else {
        let snapshot = crate::install::frontmatter_cache::FrontmatterCache::snapshot(cache);
        let mut filtered = crate::install::frontmatter_cache::FrontmatterCache::new();
        for (path, entry) in &snapshot.entries {
            if scope.allows(path) {
                filtered.upsert_entry(path.clone(), entry.clone());
            }
        }
        scoped_cache = crate::install::frontmatter_cache::FrontmatterCache::shared(filtered);
        &scoped_cache
    };
    let (epic_dir, epic_dirs) = if let Some(policy) = policy.filter(|policy| !policy.is_light()) {
        let (closure, seed) = resolve_closure_for(
            cache,
            ctx,
            &slug.to_string(),
            story_id,
            "hydrate_story",
            &policy,
        )?;
        (seed, closure.epic_dirs())
    } else {
        let snapshot = crate::install::frontmatter_cache::FrontmatterCache::snapshot(cache);
        let epic =
            resolve_story_epic_dir(&snapshot, ctx, &slug.to_string(), story_id, "hydrate_story")?;
        (epic.clone(), vec![epic])
    };
    let view = ctx.view();
    let files: Vec<PathBuf> = epic_dirs
        .iter()
        .flat_map(|epic| collect_epic_files(epic))
        .filter(|path| {
            scope.allows(path) && view.as_ref().is_none_or(|v| v.read_path(path).is_ok())
        })
        .collect();
    if files.is_empty() {
        return Err(RouteError::Message(
            "hydrate_story: epic has no files to hydrate".into(),
        ));
    }
    Ok(RemoteStoryFiles {
        epic_dir,
        epic_dirs,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dto_json(outcome: RouteOutcome) -> serde_json::Value {
        serde_json::to_value(RouteOutcomeDto::from(&outcome)).unwrap()
    }

    // The DTO is the single source of truth both the Tauri command and the MCP
    // tool serialize through (WD-4 RD-A). These lock the `{ "outcome": … }`
    // discriminators byte-for-byte against WD-3's `RouteOutcome` so the FE
    // wrapper and the skill can never drift from the backend.
    #[test]
    fn unit_outcomes_serialize_to_camelcase_discriminators() {
        assert_eq!(
            dto_json(RouteOutcome::Routed),
            serde_json::json!({"outcome":"routed"})
        );
        assert_eq!(
            dto_json(RouteOutcome::AlreadyRouted),
            serde_json::json!({"outcome":"alreadyRouted"})
        );
        assert_eq!(
            dto_json(RouteOutcome::Refused),
            serde_json::json!({"outcome":"refused"})
        );
        assert_eq!(
            dto_json(RouteOutcome::AlreadyTracked),
            serde_json::json!({"outcome":"alreadyTracked"})
        );
        assert_eq!(
            dto_json(RouteOutcome::NotProjectable),
            serde_json::json!({"outcome":"notProjectable"})
        );
    }

    #[test]
    fn collision_serializes_outcome_plus_camelcase_hex_hashes() {
        let source_hash = [0u8; 32];
        let dest_hash = [0xffu8; 32];
        let v = dto_json(RouteOutcome::Collision {
            source_hash,
            dest_hash,
        });
        assert_eq!(v["outcome"], "collision");
        assert_eq!(
            v["sourceHash"],
            blake3::Hash::from_bytes(source_hash).to_hex().to_string()
        );
        assert_eq!(
            v["destHash"],
            blake3::Hash::from_bytes(dest_hash).to_hex().to_string()
        );
        // camelCase keys only — never the raw snake_case field names.
        assert!(v.get("source_hash").is_none());
        assert!(v.get("dest_hash").is_none());
    }

    #[test]
    fn slug_parse_rejects_non_owner_repo() {
        assert!(SourceRepoSlug::parse_owner_repo("no-slash").is_none());
        assert!(SourceRepoSlug::parse_owner_repo("owner/repo").is_some());
    }

    // ===== SW-AU-24: any-team context resolver ============================

    const AU24_PEOPLE_YML: &str = r#"
people:
  - github_user_id: 1
    github_username: alice
"#;
    // `backend` synced (content dir written below), `frontend` a member but not
    // synced (no content dir), `platform` archived (must be invisible).
    const AU24_TEAMS_YML: &str = r#"
teams:
  - name: backend
  - name: frontend
  - name: platform
    archived: true
"#;

    /// Write a synced org fixture into `org_root` (its `org.yml` slug = `slug`, so
    /// the fixture is valid for both single-segment labels the unit tests use and
    /// the `owner/repo` slugs `resolve_local` validates); create the content dir
    /// only for each team in `synced_teams` so `team_sync_states` reports them
    /// present.
    fn write_au24_org(org_root: &Path, slug: &str, synced_teams: &[&str]) {
        std::fs::create_dir_all(org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            format!("slug: {slug}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
        )
        .unwrap();
        std::fs::write(org_root.join("people.yml"), AU24_PEOPLE_YML).unwrap();
        std::fs::write(org_root.join("teams.yml"), AU24_TEAMS_YML).unwrap();
        for team in synced_teams {
            std::fs::create_dir_all(crate::context_repo::path_to_team_content_dir(
                org_root, team,
            ))
            .unwrap();
        }
    }

    #[test]
    fn team_sync_states_flags_synced_and_omits_archived() {
        let td = TempDir::new().unwrap();
        let org_root = td.path().join("alpha-corp");
        write_au24_org(&org_root, "alpha-corp", &["backend"]);

        let states = team_sync_states("alpha-corp", &org_root);

        // `platform` is archived → never listed; `backend`/`frontend` are the
        // only entries, with sync state reflecting their content dir.
        assert_eq!(
            states,
            vec![
                OrgTeamEntry {
                    org: "alpha-corp".into(),
                    team: "backend".into(),
                    synced: true,
                    starter_page: None,
                },
                OrgTeamEntry {
                    org: "alpha-corp".into(),
                    team: "frontend".into(),
                    synced: false,
                    starter_page: None,
                },
            ]
        );
    }

    #[test]
    fn team_sync_states_missing_org_is_empty() {
        // Org never cloned (no dir / `teams.yml`) → no confirmable members.
        let td = TempDir::new().unwrap();
        let states = team_sync_states("nope", &td.path().join("nope"));
        assert!(states.is_empty());
    }

    #[test]
    fn classify_resolves_synced_member_to_ok_context() {
        let td = TempDir::new().unwrap();
        let org_root = td.path().join("alpha-corp");
        write_au24_org(&org_root, "alpha-corp", &["backend"]);
        let states = team_sync_states("alpha-corp", &org_root);

        let home = PathBuf::from("/home");
        let res = classify_resolution(
            states,
            home.clone(),
            7u64,
            "alpha-corp".into(),
            "backend".into(),
        );

        match res {
            ContextResolution::Ok(ctx) => {
                assert_eq!(ctx.codesense_home, home);
                assert_eq!(ctx.user_id, 7u64);
                assert_eq!(ctx.org_slug, "alpha-corp");
                assert_eq!(ctx.team_name.as_deref(), Some("backend"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn classify_member_without_content_dir_is_not_synced() {
        let td = TempDir::new().unwrap();
        let org_root = td.path().join("alpha-corp");
        write_au24_org(&org_root, "alpha-corp", &[]); // no content dirs
        let states = team_sync_states("alpha-corp", &org_root);

        let res = classify_resolution(
            states,
            PathBuf::from("/home"),
            7u64,
            "alpha-corp".into(),
            "frontend".into(),
        );
        assert!(
            matches!(res, ContextResolution::NotSynced),
            "member team with no content dir → NotSynced",
        );
    }

    #[test]
    fn classify_absent_and_archived_teams_are_unauthorized() {
        let td = TempDir::new().unwrap();
        let org_root = td.path().join("alpha-corp");
        write_au24_org(&org_root, "alpha-corp", &["backend"]);
        let states = team_sync_states("alpha-corp", &org_root);

        // A team that was never listed.
        let absent = classify_resolution(
            states.clone(),
            PathBuf::from("/home"),
            7u64,
            "alpha-corp".into(),
            "ghost".into(),
        );
        assert!(matches!(absent, ContextResolution::Unauthorized));

        // `platform` is archived, so `team_sync_states` excluded it → it falls
        // through to Unauthorized (a retired membership is not targetable).
        let archived = classify_resolution(
            states,
            PathBuf::from("/home"),
            7u64,
            "alpha-corp".into(),
            "platform".into(),
        );
        assert!(matches!(archived, ContextResolution::Unauthorized));
    }

    #[test]
    fn org_team_entry_serializes_camelcase() {
        let v = serde_json::to_value(OrgTeamEntry {
            org: "alpha-corp".into(),
            team: "backend".into(),
            synced: true,
            starter_page: None,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"org":"alpha-corp","team":"backend","synced":true})
        );
        let v = serde_json::to_value(OrgTeamEntry {
            org: "alpha-corp".into(),
            team: "backend".into(),
            synced: true,
            starter_page: Some("guide/start.md".into()),
        })
        .unwrap();
        assert_eq!(v["starterPage"], "guide/start.md");
    }

    #[test]
    fn ac4_resolved_context_feeds_write_to_the_resolved_team_only() {
        // AC-4: the resolved 4-field context supplies `(org_root, team)` to the
        // explicit-arg write path, targeting the RESOLVED (non-active) team's
        // content dir — not whatever team happens to be active. Fixture lives at
        // the real `path_to_org_root` layout so the resolved context maps to disk.
        let home = TempDir::new().unwrap();
        let uid = 42u64;
        let org = "alpha-corp";
        let org_root =
            crate::context_repo::path_to_org_root(home.path(), &Principal::from(uid), org).unwrap();
        write_au24_org(&org_root, "alpha-corp", &["backend", "frontend"]);

        // Resolve `frontend` while `backend` is the (notional) active team.
        let states = team_sync_states(org, &org_root);
        let ctx = match classify_resolution(
            states,
            home.path().to_path_buf(),
            uid,
            org.into(),
            "frontend".into(),
        ) {
            ContextResolution::Ok(ctx) => ctx,
            other => panic!("expected Ok, got {other:?}"),
        };

        // A caller feeds the resolved context into the existing explicit-arg
        // writer (the seam AU-19 reuses), recomputing org_root from the 4 fields.
        let write_root = crate::context_repo::path_to_org_root(
            &ctx.codesense_home,
            &ctx.principal(),
            &ctx.org_slug,
        )
        .unwrap();
        let team = ctx.team_name.as_deref().unwrap();
        crate::commands::config_editor::write_team_config_file(
            &write_root,
            team,
            crate::context_repo::CONFIG_DIR,
            "au24.yml",
            "automations: []\n".into(),
        )
        .unwrap();

        // Round-trip: the bytes land under the resolved team's config dir…
        let written = write_root
            .join(team)
            .join(crate::context_repo::CONFIG_DIR)
            .join("au24.yml");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "automations: []\n"
        );
        // …and never under the other (active) team.
        let other_team = write_root
            .join("backend")
            .join(crate::context_repo::CONFIG_DIR)
            .join("au24.yml");
        assert!(
            !other_team.exists(),
            "write must target only the resolved team, not the active one"
        );
    }

    #[test]
    fn resolve_local_rejects_unsafe_or_malformed_org_slug() {
        // The trust-boundary guard: a caller-supplied `org` that isn't a safe
        // `owner/repo` slug never touches the filesystem — it's Unauthorized,
        // closing the `path_to_org_root` traversal vector.
        let home = TempDir::new().unwrap();
        for bad in ["../../etc", "..", "/abs", "a/b/c", "alpha-corp", ""] {
            assert!(
                matches!(
                    resolve_local(
                        home.path().to_path_buf(),
                        1u64,
                        bad.into(),
                        "backend".into()
                    ),
                    ContextResolution::Unauthorized
                ),
                "org slug {bad:?} must resolve to Unauthorized without a filesystem read",
            );
        }
    }

    #[test]
    fn resolve_local_guard_blocks_traversal_that_would_otherwise_resolve() {
        // Pin the GUARD, not a coincidentally-missing path: plant a fully valid,
        // synced org exactly where `org = "../evil"` lands, so WITHOUT the slug
        // guard `resolve_local` would return Ok(backend). With the guard it must
        // be Unauthorized — proving the guard (not an absent target) closes it.
        let home = TempDir::new().unwrap();
        let user_home = crate::context_repo::path_to_user_home(home.path(), &Principal::from(1u64));
        // `orgs` must exist so the `..` in `orgs/../evil` resolves at the OS level;
        // the payload org lives at the resolved target `users/1/evil`.
        std::fs::create_dir_all(user_home.join("orgs")).unwrap();
        write_au24_org(&user_home.join("evil"), "acme/widgets", &["backend"]);

        assert!(
            matches!(
                resolve_local(home.path().to_path_buf(), 1u64, "../evil".into(), "backend".into()),
                ContextResolution::Unauthorized
            ),
            "the org-slug guard must reject the traversal even though a valid org exists at the target",
        );
    }

    #[test]
    fn resolve_local_resolves_a_valid_nested_slug_off_disk() {
        // Happy path through the guard: a valid `owner/repo` slug backed by an
        // on-disk fixture resolves to that team's context.
        let home = TempDir::new().unwrap();
        let org = "acme/widgets";
        let org_root =
            crate::context_repo::path_to_org_root(home.path(), &Principal::from(9u64), org)
                .unwrap();
        write_au24_org(&org_root, org, &["backend"]);

        match resolve_local(
            home.path().to_path_buf(),
            9u64,
            org.into(),
            "backend".into(),
        ) {
            ContextResolution::Ok(ctx) => {
                assert_eq!(ctx.org_slug, org);
                assert_eq!(ctx.team_name.as_deref(), Some("backend"));
                assert_eq!(ctx.user_id, 9u64);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        // A team absent from this org still classifies Unauthorized post-guard.
        assert!(matches!(
            resolve_local(home.path().to_path_buf(), 9u64, org.into(), "ghost".into()),
            ContextResolution::Unauthorized
        ));
    }

    // SW-AU-35: the firing footprint keeps only synced teams and stamps each with a
    // full context — the fan-out fires where content is readable, regardless of the
    // UI active team.
    #[test]
    fn synced_contexts_from_keeps_only_synced_teams_across_orgs() {
        let entry = |org: &str, team: &str, synced: bool| OrgTeamEntry {
            org: org.to_string(),
            team: team.to_string(),
            synced,
            starter_page: None,
        };
        let entries = vec![
            entry("acme/api", "backend", true),
            entry("acme/api", "frontend", false), // not synced → dropped
            entry("other/org", "platform", true), // a different org still included
        ];
        let home = Path::new("/home/x");
        let out = synced_contexts_from(entries, home, 42);

        let got: Vec<(String, Option<String>)> = out
            .iter()
            .map(|c| (c.org_slug.clone(), c.team_name.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("acme/api".into(), Some("backend".into())),
                ("other/org".into(), Some("platform".into())),
            ],
            "only synced teams survive, spanning orgs; each carries its own (org, team)"
        );
        // The ambient facts are stamped verbatim on every context.
        assert!(out
            .iter()
            .all(|c| c.user_id == 42 && c.codesense_home == home));
    }

    #[test]
    fn team_sync_states_content_path_that_is_a_file_is_not_synced() {
        // A stray file (not a directory) at the content path must read as
        // not-synced — `.is_dir()`, not `.exists()` — so a writer never targets it.
        let td = TempDir::new().unwrap();
        let org_root = td.path().join("alpha-corp");
        write_au24_org(&org_root, "alpha-corp", &[]);
        let content = crate::context_repo::path_to_team_content_dir(&org_root, "backend");
        std::fs::create_dir_all(content.parent().unwrap()).unwrap();
        std::fs::write(&content, "not a dir").unwrap();

        let states = team_sync_states("alpha-corp", &org_root);
        let backend = states.iter().find(|e| e.team == "backend").unwrap();
        assert!(
            !backend.synced,
            "a file at the content path is not a synced dir"
        );
    }
}
