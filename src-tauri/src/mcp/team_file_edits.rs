// SPDX-License-Identifier: MIT
//! Which of the active team's workspaces hold team-file edits not
//! yet back in the team — `get_sync_status.unpublished`.
//!
//! - **Local** workspaces (this Mac's install targets and story worktrees for
//!   the active org and team) are counted on every call: a stat walk that
//!   reads a file only when its stat changed. A workspace whose folder is gone
//!   drops out.
//! - **Machine** workspaces (this account's preparation receipts for the
//!   team) are counted only when asked (`target`): ONE read-only manifest call
//!   to a connected machine. Otherwise the last count from this session is
//!   reported with its time, and a disconnected machine is never dialled.
//!
//! Counts only read: the remote destination is resolved receipt-only (no
//! preparation reconcile); the last counts live in memory. Explicit removal
//! re-counts first, clears the missing path's stale marker on the machine,
//! then forgets its desktop preparation receipts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::agents::prepare::edits::{self, CountRequest, EditsState, WorkspaceEdits};
use crate::agents::prepare::Destination;
use crate::commands::route_to_context::ActiveContext;
use crate::content_engine::ResolverContext;

/// A machine workspace: `(owner, machine, handle)`.
type RemoteKey = (u64, String, String, String);

/// The last count of each machine workspace this session (in memory only).
fn remote_counts() -> &'static Mutex<BTreeMap<RemoteKey, WorkspaceEdits>> {
    static COUNTS: std::sync::OnceLock<Mutex<BTreeMap<RemoteKey, WorkspaceEdits>>> =
        std::sync::OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The local workspaces of `ctx`'s org and team: its install targets on this
/// Mac and its story worktrees, deduplicated by real path. A folder that no
/// longer exists is left out. The install store is only read (never created,
/// migrated or quarantined).
pub(crate) fn local_workspaces(ctx: &ResolverContext) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(store) = crate::content_engine::sync_state_store::SyncStateStore::open_existing(
        crate::content_engine::sync_state_store::default_store_path(&ctx.codesense_home),
    ) {
        if let Ok(targets) = store.local_targets(Some(ctx.user_id)) {
            paths.extend(
                targets
                    .into_iter()
                    .filter(|(org, team, _)| *org == ctx.org_slug && *team == ctx.team_name)
                    .map(|(_, _, path)| path),
            );
        }
    }
    if let Ok(records) =
        crate::install::worktree_index::iter_story_records(&ctx.codesense_home, &ctx.principal())
    {
        paths.extend(
            records
                .into_iter()
                .filter(|record| {
                    record.org_slug == ctx.org_slug
                        && crate::install::worktree_index::resolve_team(record) == ctx.team_name
                })
                .map(|record| record.code_repo_path),
        );
    }
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir() && seen.insert(path.clone()))
        .collect()
}

/// Count one local workspace through its own registration (its org, team
/// and repo), as a publish would resolve it.
pub(crate) fn count_local(ctx: &ResolverContext, root: &Path) -> WorkspaceEdits {
    count_local_for(ctx, root, &Default::default())
}
fn count_local_for(
    ctx: &ResolverContext,
    root: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> WorkspaceEdits {
    let (own, target) = match crate::commands::watch::workspace_target(ctx, root) {
        Ok(found) => found,
        Err(reason) => return WorkspaceEdits::unknown(reason),
    };
    let legacy = || crate::agents::prepare::legacy_rows_local_read_only(&own, root);
    edits::count_for(
        &CountRequest {
            home: &own.codesense_home,
            principal: &own.principal(),
            destination: Destination::Local { root },
            target: &target,
            legacy: Some(&legacy),
        },
        scope,
    )
}

/// A machine workspace prepared for the active team.
#[derive(Debug, Clone)]
struct RemotePlace {
    machine_id: String,
    handle: String,
    cwd: String,
}

/// The machine workspaces prepared for `ctx`'s team, from this account's
/// preparation receipts. A count kept for a workspace no receipt names any
/// more is forgotten. Receipts that can't be read are an error (never read
/// as "no machine workspaces", and nothing is forgotten then).
fn remote_places(ctx: &ResolverContext) -> Result<Vec<RemotePlace>, String> {
    let places = super::remote_workspace_target::prepared_places_everywhere(
        &ctx.codesense_home,
        &ctx.principal(),
        ctx.user_id,
    )?;
    let live: BTreeSet<(u64, String, String)> = places
        .iter()
        .map(|place| (ctx.user_id, place.machine_id.clone(), place.handle.clone()))
        .collect();
    remote_counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|key, _| {
            key.0 != ctx.user_id || live.contains(&(key.0, key.1.clone(), key.2.clone()))
        });
    let mut seen = BTreeSet::new();
    Ok(places
        .into_iter()
        .filter(|place| {
            let (org, team) = crate::commands::pty_layout::parse_team_key(&place.team_key);
            org == ctx.org_slug && team == ctx.team_name.as_deref()
        })
        .filter(|place| seen.insert((place.machine_id.clone(), place.handle.clone())))
        .map(|place| RemotePlace {
            machine_id: place.machine_id,
            handle: place.handle,
            cwd: place.cwd,
        })
        .collect())
}

/// Whether `machine` has an admitted remote connection now (reads the
/// connection manager's snapshot; never dials; never This Mac).
fn connected(app: &tauri::AppHandle, machine: &str) -> bool {
    // A machine workspace is never on This Mac: a receipt naming it is not
    // counted remotely (and resolving it might start the local sidecar).
    if machine == crate::commands::machines::LOCAL_MACHINE_ID {
        return false;
    }
    crate::commands::pty_backend::resolve_terminal_backend(app, machine).is_ok()
}

/// Count one machine workspace now: ONE read-only manifest call. The
/// destination is resolved receipt-only, so nothing is dispatched to the
/// machine before that call.
pub(crate) async fn count_remote(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    machine_id: &str,
    handle: &str,
) -> WorkspaceEdits {
    count_remote_context(app, ambient, machine_id, handle, None).await
}

async fn count_remote_context(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    machine_id: &str,
    handle: &str,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
) -> WorkspaceEdits {
    count_remote_context_for(
        app,
        ambient,
        machine_id,
        handle,
        context_root,
        Default::default(),
    )
    .await
}

async fn count_remote_context_for(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    machine_id: &str,
    handle: &str,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> WorkspaceEdits {
    let resolved = match super::remote_workspace_target::resolve_for_count(
        app.clone(),
        ambient,
        machine_id,
        handle,
    )
    .await
    {
        Ok(resolved) => super::remote_destination::from_resolved(resolved),
        Err(reason) => return WorkspaceEdits::unknown(reason),
    };
    let ctx = match crate::commands::content_engine::derive_resolver_context(
        resolved.context.codesense_home.clone(),
        resolved.context.user_id,
        resolved.context.org_slug.clone(),
        resolved.context.team_name.clone(),
    ) {
        Ok(ctx) => ctx,
        Err(error) => return WorkspaceEdits::unknown(error.to_string()),
    };
    let Some(team) = ctx.team_name.clone() else {
        return WorkspaceEdits::unknown("the workspace has no team");
    };
    let target = crate::agents::ManifestTarget {
        context_root,
        org: ctx.org_slug.clone(),
        team,
        repo: Some(resolved.source_repo.clone()),
    };
    let io = resolved.io;
    let slug = resolved.source_repo;
    crate::commands::await_blocking("count_team_file_edits", move || {
        let legacy =
            || match crate::content_engine::destination::DestinationIo::identity(io.as_ref()) {
                crate::content_engine::destination::DestinationIdentity::Remote(remote) => {
                    crate::agents::prepare::legacy_rows_remote_read_only(&ctx, &slug, remote)
                }
                _ => Ok(Vec::new()),
            };
        Ok(edits::count_for(
            &CountRequest {
                home: &ctx.codesense_home,
                principal: &ctx.principal(),
                destination: Destination::Remote { io: io.as_ref() },
                target: &target,
                legacy: Some(&legacy),
            },
            &scope,
        ))
    })
    .await
    .unwrap_or_else(WorkspaceEdits::unknown)
}

/// What to report for a machine workspace after an attempt to count it:
/// a fresh count is kept (a file the machine can't date gets the time this
/// session first saw it edited). An attempt that couldn't count reports ITS
/// state and reason, with the last count's files and time beside it (never
/// as a fresh count).
fn remember(key: RemoteKey, mut fresh: WorkspaceEdits) -> WorkspaceEdits {
    let mut counts = remote_counts().lock().unwrap_or_else(|e| e.into_inner());
    if fresh.state != EditsState::Checked {
        if let Some(last) = counts.get(&key) {
            fresh.counts = last.counts;
            fresh.checked_at_ms = last.checked_at_ms;
            fresh.last_changed_at_ms = last.last_changed_at_ms;
            if fresh.state != EditsState::Missing {
                fresh.files = last.files.clone();
            }
        }
        if fresh.state == EditsState::Missing
            || counts
                .get(&key)
                .is_some_and(|last| last.state == EditsState::Missing)
        {
            counts.insert(key, fresh.clone());
        }
        return fresh;
    }
    let before = counts.get(&key);
    for file in &mut fresh.files {
        if file.changed_at_ms.is_none() {
            file.changed_at_ms = before
                .and_then(|b| b.files.iter().find(|f| f.path == file.path))
                .and_then(|f| f.changed_at_ms)
                .or(Some(fresh.checked_at_ms));
        }
    }
    fresh.last_changed_at_ms = fresh.files.iter().filter_map(|f| f.changed_at_ms).max();
    counts.insert(key, fresh.clone());
    fresh
}

/// A machine workspace not counted on this call: its last count from this
/// session (with the machine's state when it is offline), else nothing.
fn not_counted(key: &RemoteKey, online: bool) -> Option<WorkspaceEdits> {
    let last = remote_counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned();
    match (last, online) {
        (Some(last), true) => Some(last),
        (Some(last), false) => Some(WorkspaceEdits {
            state: EditsState::Offline,
            reason: Some("the machine is not connected; this is its last count".into()),
            ..last
        }),
        (None, true) => None,
        (None, false) => Some(WorkspaceEdits::not_counted_now(
            EditsState::Offline,
            "the machine is not connected",
        )),
    }
}

/// One workspace row of `unpublished`.
fn row(target: Value, name: String, edits: Option<&WorkspaceEdits>, extra: Value) -> Value {
    let mut row = json!({ "target": target, "name": name });
    let map = row.as_object_mut().expect("object");
    match edits {
        Some(edits) => {
            if let Value::Object(fields) = serde_json::to_value(edits).unwrap_or(Value::Null) {
                map.extend(fields);
            }
        }
        None => {
            map.insert("state".into(), json!("notChecked"));
        }
    }
    if let Value::Object(fields) = extra {
        map.extend(fields);
    }
    row
}

fn display_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// What to count now, besides the local workspaces (always counted).
pub(crate) enum Refresh {
    /// Nothing on a machine: machine workspaces report their last count.
    None,
    /// This local workspace must be one of the team's (an error otherwise).
    Local(PathBuf),
    /// This machine workspace: ONE read-only manifest call when the machine
    /// is connected (never dialled).
    Remote { machine_id: String, handle: String },
}

/// `get_sync_status.unpublished` for the active org and team: every local
/// workspace counted now, every machine workspace with its last count (or
/// counted now when it is `refresh`'s target).
pub(crate) async fn unpublished(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    refresh: Refresh,
) -> Result<Value, String> {
    let mut value =
        unpublished_with(
            ambient,
            refresh,
            |machine| connected(app, machine),
            |ambient, machine, handle| async move {
                count_remote(app, ambient, &machine, &handle).await
            },
        )
        .await?;
    let labels = crate::commands::machines::machine_labels(app).await;
    if let Some(rows) = value["workspaces"].as_array_mut() {
        for row in rows {
            if let Some(machine) = row["target"]["machineId"].as_str() {
                let label = labels
                    .get(machine)
                    .map(String::as_str)
                    .unwrap_or(machine)
                    .to_string();
                row["machineName"] = json!(label);
                if row["state"] == "missing" {
                    row["reason"] = json!(format!("folder no longer exists on {label}"));
                }
            }
        }
    }
    Ok(value)
}

/// One request shared by the app and MCP removal entry points.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ForgetMissingTarget {
    pub machine_id: String,
    pub workspace_handle: String,
}

pub(crate) async fn forget_missing_workspace(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    target: ForgetMissingTarget,
) -> Result<Value, String> {
    let cleanup_app = app.clone();
    let cleanup_ctx = ambient.clone();
    let cleanup_target = target.clone();
    forget_missing_with(
        ambient,
        target,
        |machine| connected(app, machine),
        |ctx, machine, handle| async move { count_remote(app, ctx, &machine, &handle).await },
        move || async move {
            let resolved = super::remote_workspace_target::resolve_for_count(
                cleanup_app,
                cleanup_ctx,
                &cleanup_target.machine_id,
                &cleanup_target.workspace_handle,
            )
            .await?;
            let client = resolved.client.clone();
            let target = resolved.target.clone();
            let validate = resolved.validate.clone();
            let destination = super::remote_destination::from_resolved(resolved);
            crate::commands::await_blocking("forget_missing_machine_workspace", move || {
                forget_missing_record(
                    &destination.context.codesense_home,
                    &destination.context.principal(),
                    crate::content_engine::destination::DestinationIo::identity(destination.io.as_ref()),
                    || {
                        validate()?;
                        crate::terminal::session_server::SessionServerClient::forget_missing_workspace(
                            &client,
                            codesense_pty_core::remote_workspace::ForgetMissingWorkspace { target },
                        )?;
                        validate()?;
                        Ok(())
                    },
                )
            })
            .await
        },
    )
    .await
}

/// Serialize marker cleanup and record retirement with this destination's
/// preparation, so a new launch cannot inherit the deleted folder's baseline.
pub(crate) fn forget_missing_record(
    home: &Path,
    principal: &crate::domain::Principal,
    identity: &crate::content_engine::destination::DestinationIdentity,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let lock = crate::agents::prepare::step_lock(identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    cleanup()?;
    crate::agents::prepare::ManagedRecord::remove(home, principal, identity);
    if crate::agents::prepare::ManagedRecord::exists(home, principal, identity) {
        return Err("couldn't forget the missing workspace's copied-file record".into());
    }
    Ok(())
}

fn refused_count(edits: WorkspaceEdits, reason: &str) -> Value {
    json!({"status":"refused", "state":edits.state, "reason":reason, "edits":edits})
}

async fn forget_missing_with<C, R, F, D, G>(
    ambient: ActiveContext,
    target: ForgetMissingTarget,
    connected: C,
    count: R,
    cleanup: D,
) -> Result<Value, String>
where
    C: Fn(&str) -> bool,
    R: Fn(ActiveContext, String, String) -> F,
    F: std::future::Future<Output = WorkspaceEdits>,
    D: FnOnce() -> G,
    G: std::future::Future<Output = Result<(), String>>,
{
    let ctx = crate::commands::content_engine::derive_resolver_context(
        ambient.codesense_home.clone(),
        ambient.user_id,
        ambient.org_slug.clone(),
        ambient.team_name.clone(),
    )
    .map_err(|e| e.to_string())?;
    let places = remote_places(&ctx)?;
    check_target(
        &Refresh::Remote {
            machine_id: target.machine_id.clone(),
            handle: target.workspace_handle.clone(),
        },
        &[],
        &places,
    )?;
    let key = (
        ctx.user_id,
        target.machine_id.clone(),
        target.workspace_handle.clone(),
        crate::context_repo::caller_scope::CallerScope::Human
            .cache_key()
            .to_string(),
    );
    let refuse = |state: EditsState, reason: &str| json!({"status":"refused", "state":state, "reason":reason});
    if !connected(&target.machine_id) {
        return Ok(refuse(EditsState::Offline, "the machine is not connected"));
    }
    let fresh = count(
        ambient.clone(),
        target.machine_id.clone(),
        target.workspace_handle.clone(),
    )
    .await;
    let state = fresh.state;
    if !connected(&target.machine_id) {
        return Ok(refuse(EditsState::Offline, "the machine is not connected"));
    }
    let found = remember(key.clone(), fresh);
    if state != EditsState::Missing {
        return Ok(refused_count(found, "the workspace is not missing"));
    }
    if !connected(&target.machine_id) {
        return Ok(refuse(EditsState::Offline, "the machine is not connected"));
    }
    let receipt_ctx = ctx.clone();
    let receipt_target = target.clone();
    let receipts =
        crate::commands::await_blocking("capture_missing_workspace_receipts", move || {
            super::remote_workspace_target::receipts_to_forget(
                &receipt_ctx.codesense_home,
                &receipt_ctx.principal(),
                receipt_ctx.user_id,
                &receipt_target.machine_id,
                &receipt_target.workspace_handle,
            )
        })
        .await?;
    if let Err(error) = cleanup().await {
        if crate::content_engine::destination::DestinationBatchError::names_outdated_server(&error)
        {
            return Ok(
                json!({"status":"refused", "state":"missing", "reason":"updateTerminalServer"}),
            );
        }
        if !connected(&target.machine_id) {
            return Ok(refuse(EditsState::Offline, "the machine is not connected"));
        }
        let fresh = count(
            ambient,
            target.machine_id.clone(),
            target.workspace_handle.clone(),
        )
        .await;
        let state = if fresh.state == EditsState::Missing {
            EditsState::Unknown
        } else {
            fresh.state
        };
        let mut found = remember(key, fresh);
        found.state = state;
        return Ok(refused_count(found, "couldn't confirm the folder is gone"));
    }
    crate::commands::await_blocking("forget_missing_workspace_receipts", move || {
        super::remote_workspace_target::forget_receipts(receipts)
    })
    .await?;
    remote_counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    Ok(json!({"status":"removed"}))
}

/// The local half of a listing: every local workspace counted, and the
/// machine workspaces' receipts.
#[derive(Clone)]
struct LocalSnapshot {
    /// When its counting began (sharing) and ended (reuse age).
    started: std::time::Instant,
    at: std::time::Instant,
    rows: Vec<(PathBuf, WorkspaceEdits)>,
    places: Result<Vec<RemotePlace>, String>,
}

type ListingKey = (PathBuf, u64, String, Option<String>, Option<String>, String);
type ListingSlot = std::sync::Arc<tokio::sync::Mutex<Option<LocalSnapshot>>>;

/// One local listing at a time per home, user, org and team: concurrent
/// callers for the same team wait and share it; other teams don't wait.
fn local_snapshot(key: &ListingKey) -> ListingSlot {
    static SLOTS: std::sync::OnceLock<Mutex<BTreeMap<ListingKey, ListingSlot>>> =
        std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_default()
        .clone()
}

/// A machine count reuses a local listing this young instead of counting
/// every local workspace again.
const REUSE_LOCAL_FOR_REMOTE: std::time::Duration = std::time::Duration::from_secs(30);

/// The local listing for `ctx`: shared with a listing that finished while
/// this one waited (coalesced), or a recent one when only a machine
/// workspace is being counted; else counted now. `fresh` says which.
async fn local_listing(
    ctx: &ResolverContext,
    remote_only: bool,
) -> Result<(LocalSnapshot, bool), String> {
    local_listing_for(ctx, remote_only, &Default::default()).await
}

async fn local_listing_for(
    ctx: &ResolverContext,
    remote_only: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(LocalSnapshot, bool), String> {
    let key = (
        ctx.codesense_home.clone(),
        ctx.user_id,
        ctx.org_slug.clone(),
        ctx.team_name.clone(),
        ctx.context_root.as_ref().map(|r| r.id.clone()),
        scope.cache_key().to_string(),
    );
    let scope = scope.clone();
    let asked_at = std::time::Instant::now();
    let slot = local_snapshot(&key);
    let mut held = slot.lock().await;
    if let Some(snapshot) = held.as_ref() {
        // Shared only when it STARTED after this caller asked: one that was
        // already counting may have missed what changed since.
        if snapshot.started >= asked_at {
            return Ok((snapshot.clone(), true));
        }
        if remote_only && snapshot.at.elapsed() < REUSE_LOCAL_FOR_REMOTE {
            // The local rows are reused; the receipts are read again (cheap,
            // no network), so a workspace prepared just now is known.
            let mut reused = snapshot.clone();
            let places_ctx = ctx.clone();
            reused.places =
                crate::commands::await_blocking("unpublished_remote_places", move || {
                    Ok(remote_places(&places_ctx))
                })
                .await?;
            return Ok((reused, false));
        }
    }
    let started = std::time::Instant::now();
    let local_ctx = ctx.clone();
    let (rows, places) = crate::commands::await_blocking("unpublished_workspaces", move || {
        let rows: Vec<(PathBuf, WorkspaceEdits)> = local_workspaces(&local_ctx)
            .into_iter()
            .map(|root| {
                let edits = count_local_for(&local_ctx, &root, &scope);
                (root, edits)
            })
            .collect();
        Ok((rows, remote_places(&local_ctx)))
    })
    .await?;
    let snapshot = LocalSnapshot {
        started,
        at: std::time::Instant::now(),
        rows,
        places,
    };
    *held = Some(snapshot.clone());
    Ok((snapshot, true))
}

async fn unpublished_with_context<C, R, F>(
    ambient: ActiveContext,
    refresh: Refresh,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
    connected: C,
    count: R,
) -> Result<Value, String>
where
    C: Fn(&str) -> bool,
    R: Fn(ActiveContext, String, String) -> F,
    F: std::future::Future<Output = WorkspaceEdits>,
{
    unpublished_with_context_for(
        ambient,
        refresh,
        context_root,
        connected,
        count,
        &Default::default(),
    )
    .await
}

async fn unpublished_with_context_for<C, R, F>(
    ambient: ActiveContext,
    refresh: Refresh,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
    connected: C,
    count: R,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String>
where
    C: Fn(&str) -> bool,
    R: Fn(ActiveContext, String, String) -> F,
    F: std::future::Future<Output = WorkspaceEdits>,
{
    let mut ctx = crate::commands::content_engine::derive_resolver_context(
        ambient.codesense_home.clone(),
        ambient.user_id,
        ambient.org_slug.clone(),
        ambient.team_name.clone(),
    )
    .map_err(|e| e.to_string())?;
    ctx.context_root = context_root;
    let owner = ctx.user_id;
    let remote_only = matches!(refresh, Refresh::Remote { .. });
    let (snapshot, local_fresh) = local_listing_for(&ctx, remote_only, scope).await?;
    let local_rows = snapshot.rows;
    let (places, remote_error) = match snapshot.places {
        Ok(places) => (places, None),
        // A remote target can't be checked against receipts that can't be
        // read: that is its answer.
        Err(error) if remote_only => {
            return Err(format!("the machine workspaces can't be listed: {error}"))
        }
        Err(error) => (Vec::new(), Some(error)),
    };
    check_target(&refresh, &local_rows, &places)?;
    let mut remote = Vec::new();
    for place in places {
        let key: RemoteKey = (
            owner,
            place.machine_id.clone(),
            place.handle.clone(),
            scope.cache_key().to_string(),
        );
        let mut online = connected(&place.machine_id);
        let wanted = matches!(&refresh, Refresh::Remote { machine_id, handle }
            if *machine_id == place.machine_id && *handle == place.handle);
        let (mut edits, mut fresh) = if wanted && online {
            let counted = count(
                ambient.clone(),
                place.machine_id.clone(),
                place.handle.clone(),
            )
            .await;
            let fresh = counted.state == EditsState::Checked;
            (Some(remember(key.clone(), counted)), fresh)
        } else {
            (not_counted(&key, online), false)
        };
        if online && !connected(&place.machine_id) {
            online = false;
            edits = not_counted(&key, false);
            fresh = false;
        }
        remote.push(RemoteRow {
            place,
            online,
            edits,
            fresh,
        });
    }
    let mut value = assemble(&local_rows, local_fresh, remote);
    if let Some(error) = remote_error {
        value["remoteError"] = json!(format!("the machine workspaces can't be listed: {error}"));
    }
    Ok(value)
}

/// One machine workspace's row: its count (none yet), whether its machine
/// is connected, and whether the count was made on this call.
struct RemoteRow {
    place: RemotePlace,
    online: bool,
    edits: Option<WorkspaceEdits>,
    fresh: bool,
}

/// A `target` must be one of the team's workspaces.
fn check_target(
    refresh: &Refresh,
    local_rows: &[(PathBuf, WorkspaceEdits)],
    places: &[RemotePlace],
) -> Result<(), String> {
    match refresh {
        Refresh::None => Ok(()),
        Refresh::Local(path) => {
            let real = path
                .canonicalize()
                .map_err(|e| format!("{} is unavailable: {e}", path.display()))?;
            if local_rows.iter().any(|(root, _)| *root == real) {
                Ok(())
            } else {
                Err(format!(
                    "{} is not one of this team's workspaces on this Mac",
                    path.display()
                ))
            }
        }
        Refresh::Remote { machine_id, handle } => {
            if places
                .iter()
                .any(|p| p.machine_id == *machine_id && p.handle == *handle)
            {
                Ok(())
            } else {
                Err(format!(
                    "{handle} on {machine_id} is not a workspace prepared for this team"
                ))
            }
        }
    }
}

/// The `unpublished` value: every workspace with edits, or whose count is
/// not a clean fresh one; a workspace counted clean is only in
/// `cleanWorkspaces`. `total` sums the listed workspaces' edits. Every
/// machine workspace, clean or not, is also in `machineWorkspaces` (so a
/// caller can count it again). `fresh`: counted on this call (else the last
/// count, as old as its `checkedAtMs`).
fn assemble(
    local_rows: &[(PathBuf, WorkspaceEdits)],
    local_fresh: bool,
    remote: Vec<RemoteRow>,
) -> Value {
    let mut rows = Vec::new();
    let mut machines = Vec::new();
    let mut total = 0usize;
    let mut clean = 0usize;
    let is_clean =
        |edits: &WorkspaceEdits| edits.state == EditsState::Checked && edits.counts.total() == 0;
    for (root, edits) in local_rows {
        if is_clean(edits) {
            clean += 1;
            continue;
        }
        total += edits.counts.total();
        let path = root.to_string_lossy().into_owned();
        rows.push(row(
            json!({"kind": "local", "codeRepoPath": path}),
            display_name(&path),
            Some(edits),
            json!({ "fresh": local_fresh }),
        ));
    }
    for RemoteRow {
        place,
        online,
        edits,
        fresh,
    } in remote
    {
        let target = json!({"kind": "remote", "machineId": place.machine_id, "workspaceHandle": place.handle});
        machines.push(json!({
            "target": target,
            "name": display_name(&place.cwd),
            "machineOnline": online,
            "checkedAtMs": edits.as_ref().map(|e| e.checked_at_ms),
        }));
        if let Some(edits) = &edits {
            if is_clean(edits) {
                clean += 1;
                continue;
            }
            if edits.state != EditsState::Missing {
                total += edits.files.len();
            }
        }
        let mut edits = edits;
        if let Some(edits) = &mut edits {
            if edits.state == EditsState::Missing {
                edits.reason = Some(format!("folder no longer exists on {}", place.machine_id));
                edits.files.clear();
            }
        }
        rows.push(row(
            target,
            display_name(&place.cwd),
            edits.as_ref(),
            json!({ "machineOnline": online, "cwd": place.cwd, "fresh": fresh }),
        ));
    }
    json!({
        "total": total,
        "cleanWorkspaces": clean,
        "workspaces": rows,
        "machineWorkspaces": machines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::prepare::edits::{EditCounts, EditDestination, EditKind, EditedFile};
    use crate::agents::prepare::step3::tests::{write, Fx};
    use crate::agents::resolve::tests::{ORG, TEAM};
    use crate::domain::SourceRepoSlug;

    fn ctx_for(home: &Path, team: &str) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 1,
            org_slug: ORG.to_string(),
            team_name: Some(team.to_string()),
        }
    }

    fn file(path: &str, kind: EditKind) -> EditedFile {
        EditedFile {
            path: path.into(),
            kind,
            destination: EditDestination::Private,
            changed_at_ms: None,
            reason: None,
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
            team_path: PathBuf::from("/team").join(path),
            discardable: false,
        }
    }

    fn remote_row(place: RemotePlace, online: bool, edits: Option<WorkspaceEdits>) -> RemoteRow {
        RemoteRow {
            place,
            online,
            edits,
            fresh: false,
        }
    }

    fn checked(files: Vec<EditedFile>, at: u64) -> WorkspaceEdits {
        WorkspaceEdits {
            contexts: Vec::new(),
            state: EditsState::Checked,
            reason: None,
            counts: EditCounts {
                changed: files.len(),
                ..Default::default()
            },
            files,
            last_changed_at_ms: None,
            checked_at_ms: at,
            hashed: 0,
        }
    }

    fn place(handle: &str, cwd: &str) -> RemotePlace {
        RemotePlace {
            machine_id: "devbox".into(),
            handle: handle.into(),
            cwd: cwd.into(),
        }
    }

    #[test]
    fn missing_is_remembered_without_files_or_totals_and_offline_overrides_it() {
        let key: RemoteKey = (
            434343,
            "devbox".into(),
            "missing-fixture".into(),
            crate::context_repo::caller_scope::CallerScope::Human
                .cache_key()
                .to_string(),
        );
        remember(
            key.clone(),
            checked(vec![file("a.md", EditKind::Changed)], 1),
        );
        let missing = remember(
            key.clone(),
            WorkspaceEdits::not_counted_now(EditsState::Missing, "gone"),
        );
        assert_eq!(missing.counts.total(), 1);
        assert!(missing.files.is_empty());
        assert_eq!(not_counted(&key, true).unwrap().state, EditsState::Missing);
        let value = assemble(
            &[],
            true,
            vec![remote_row(
                place("missing-fixture", "/work/gone"),
                true,
                Some(missing),
            )],
        );
        assert_eq!(value["total"], 0);
        assert_eq!(value["workspaces"][0]["state"], "missing");
        assert_eq!(value["workspaces"][0]["files"], json!([]));
        assert_eq!(not_counted(&key, false).unwrap().state, EditsState::Offline);
        let offline = assemble(
            &[],
            true,
            vec![remote_row(
                place("missing-fixture", "/work/gone"),
                false,
                not_counted(&key, false),
            )],
        );
        assert_eq!(
            offline["total"], 0,
            "lost edits never return to the count after disconnection"
        );

        let restored = remember(
            key.clone(),
            checked(vec![file("a.md", EditKind::Changed)], 2),
        );
        assert_eq!(restored.state, EditsState::Checked);
        remote_counts().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn remove_recounts_refuses_or_forgets_only_the_selected_receipts() {
        let home = tempfile::tempdir().unwrap();
        let owner = 434344;
        let principal = crate::domain::Principal::from(owner);
        let root = crate::context_repo::path_to_user_home(home.path(), &principal)
            .join("remote-workspace-intents-v1");
        let receipt = |id: &str, machine: &str, path: &str| {
            crate::mcp::remote_workspace_target::write_test_receipt(
                &root,
                id,
                machine,
                &format!("{ORG}:{TEAM}"),
                path,
                SourceRepoSlug::new("acme", "api").unwrap(),
                &format!("/work/{path}"),
                owner,
                "server",
                Some("story"),
            )
        };
        let handle = receipt("remove-1", "devbox", "gone");
        assert_eq!(receipt("remove-2", "devbox", "gone"), handle);
        // A byte-identical duplicate still has to be removed from disk.
        let path = std::fs::read_dir(&root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::copy(path, root.join(format!("{}.json", "f".repeat(64)))).unwrap();
        let kept = receipt("keep-1", "devbox", "kept");
        let other_machine = receipt("keep-2", "another", "gone");
        let ambient = ActiveContext {
            codesense_home: home.path().into(),
            user_id: owner,
            org_slug: ORG.into(),
            team_name: Some(TEAM.into()),
        };
        let target = || ForgetMissingTarget {
            machine_id: "devbox".into(),
            workspace_handle: handle.clone(),
        };
        let before = std::fs::read_dir(&root).unwrap().count();
        let offline = forget_missing_with(
            ambient.clone(),
            target(),
            |_| false,
            |_, _, _| async { panic!("offline must not be dialled") },
            || async { panic!("offline must not clean up") },
        )
        .await
        .unwrap();
        assert_eq!(offline["state"], "offline");
        let refused = forget_missing_with(
            ambient.clone(),
            target(),
            |_| true,
            |_, _, _| async { checked(vec![file("a.md", EditKind::Changed)], 2) },
            || async { panic!("restored workspace must not clean up") },
        )
        .await
        .unwrap();
        assert_eq!(refused["status"], "refused");
        assert_eq!(refused["state"], "checked");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
        let old = forget_missing_with(
            ambient.clone(),
            target(),
            |_| true,
            |_, _, _| async { WorkspaceEdits::not_counted_now(EditsState::Missing, "gone") },
            || async { Err("[terminalServerOutdated] update".into()) },
        )
        .await
        .unwrap();
        assert_eq!(old["reason"], "updateTerminalServer");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
        let result = forget_missing_with(
            ambient,
            target(),
            |_| true,
            |_, _, _| async { WorkspaceEdits::not_counted_now(EditsState::Missing, "gone") },
            || async { Ok(()) },
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "removed");
        assert!(not_counted(
            &(
                owner,
                "devbox".into(),
                handle.clone(),
                crate::context_repo::caller_scope::CallerScope::Human
                    .cache_key()
                    .to_string()
            ),
            true
        )
        .is_none());
        let places = crate::mcp::remote_workspace_target::prepared_places_everywhere(
            home.path(),
            &principal,
            owner,
        )
        .unwrap();
        assert_eq!(places.len(), 2);
        assert!(places
            .iter()
            .any(|p| p.handle == kept && p.machine_id == "devbox"));
        assert!(places
            .iter()
            .any(|p| p.handle == other_machine && p.machine_id == "another"));
        assert!(!places
            .iter()
            .any(|p| p.handle == handle && p.machine_id == "devbox"));
    }

    #[tokio::test]
    async fn ui_command_and_mcp_tool_payloads_share_the_removal_result() {
        let mut outcomes = Vec::new();
        for from_ui in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let owner = 434345;
            let principal = crate::domain::Principal::from(owner);
            let root = crate::context_repo::path_to_user_home(home.path(), &principal)
                .join("remote-workspace-intents-v1");
            let handle = crate::mcp::remote_workspace_target::write_test_receipt(
                &root,
                "parity",
                "devbox",
                &format!("{ORG}:{TEAM}"),
                "gone",
                SourceRepoSlug::new("acme", "api").unwrap(),
                "/work/parity",
                owner,
                "server",
                Some("story"),
            );
            let wire_target = json!({"machineId":"devbox", "workspaceHandle":handle});
            let target: ForgetMissingTarget = if from_ui {
                let command_args = json!({"target":wire_target});
                serde_json::from_value(command_args["target"].clone()).unwrap()
            } else {
                serde_json::from_value(wire_target).unwrap()
            };
            let ambient = ActiveContext {
                codesense_home: home.path().into(),
                user_id: owner,
                org_slug: ORG.into(),
                team_name: Some(TEAM.into()),
            };
            let outcome = forget_missing_with(
                ambient,
                target,
                |_| true,
                |_, _, _| async { WorkspaceEdits::not_counted_now(EditsState::Missing, "gone") },
                || async { Ok(()) },
            )
            .await
            .unwrap();
            assert!(
                crate::mcp::remote_workspace_target::prepared_places_everywhere(
                    home.path(),
                    &principal,
                    owner
                )
                .unwrap()
                .is_empty()
            );
            outcomes.push(outcome);
        }
        assert_eq!(outcomes[0], json!({"status":"removed"}));
        assert_eq!(outcomes[0], outcomes[1]);
    }

    #[tokio::test]
    async fn removal_keeps_a_new_preparation_receipt_reserved_during_cleanup() {
        let home = tempfile::tempdir().unwrap();
        let owner = 434346;
        let principal = crate::domain::Principal::from(owner);
        let root = crate::context_repo::path_to_user_home(home.path(), &principal)
            .join("remote-workspace-intents-v1");
        let receipt = |id: &str| {
            crate::mcp::remote_workspace_target::write_test_receipt(
                &root,
                id,
                "devbox",
                &format!("{ORG}:{TEAM}"),
                "gone",
                SourceRepoSlug::new("acme", "api").unwrap(),
                "/work/concurrent",
                owner,
                "server",
                Some("story"),
            )
        };
        let handle = receipt("old");
        let ambient = ActiveContext {
            codesense_home: home.path().into(),
            user_id: owner,
            org_slug: ORG.into(),
            team_name: Some(TEAM.into()),
        };
        let result = forget_missing_with(
            ambient,
            ForgetMissingTarget {
                machine_id: "devbox".into(),
                workspace_handle: handle.clone(),
            },
            |_| true,
            |_, _, _| async { WorkspaceEdits::not_counted_now(EditsState::Missing, "gone") },
            || async {
                receipt("new-during-cleanup");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "removed");
        let retained = crate::mcp::remote_workspace_target::read_receipts(
            root,
            owner,
            Some("devbox".into()),
            Some(handle),
            None,
        )
        .unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].params.request_id, "new-during-cleanup");
    }

    /// The local workspaces are the active team's — install targets and
    /// story worktrees, each once — and a folder that is gone drops out;
    /// each is counted through its own registration. The install store is
    /// never created by looking.
    #[test]
    fn local_workspaces_are_the_teams_and_are_counted() {
        let fx = Fx::new();
        let ctx = ctx_for(fx.home.path(), TEAM);
        let store = crate::content_engine::sync_state_store::default_store_path(fx.home.path());
        assert!(local_workspaces(&ctx).is_empty());
        assert!(!store.exists(), "looking never creates the install store");

        fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
        write(&fx.root, "CLAUDE.md", b"edited here");
        let slug = SourceRepoSlug::new("acme", "api").unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &slug, &fx.root).unwrap();
        let other_team = tempfile::tempdir().unwrap();
        crate::content_engine::register_attached_code_repo(
            &ctx_for(fx.home.path(), "platform"),
            &slug,
            other_team.path(),
        )
        .unwrap();
        let gone = tempfile::tempdir().unwrap();
        let gone_path = gone.path().to_path_buf();
        crate::content_engine::register_attached_code_repo(&ctx, &slug, &gone_path).unwrap();
        drop(gone);
        // Story worktrees: this team's is listed, another team's is not.
        let story = |dir: &Path, team: &str, id: &str| {
            crate::install::worktree_index::WorktreeStoryRecord::new(
                dir,
                ORG,
                slug.clone(),
                id,
                format!("/x/users/1/orgs/{ORG}/codesense-context/{team}/content/{id}.md"),
            )
        };
        let mine = tempfile::tempdir().unwrap();
        let theirs = tempfile::tempdir().unwrap();
        let principal = ctx.principal();
        for record in [
            story(mine.path(), TEAM, "S-1"),
            story(theirs.path(), "platform", "S-2"),
            // The same folder as an install target: listed once.
            story(&fx.root, TEAM, "S-3"),
        ] {
            crate::install::worktree_index::write_record(fx.home.path(), &principal, &record)
                .unwrap();
        }

        let mut expected = vec![fx.root.clone(), mine.path().canonicalize().unwrap()];
        expected.sort();
        let mut found = local_workspaces(&ctx);
        found.sort();
        assert_eq!(found, expected);
        let edits = count_local(&ctx, &fx.root);
        assert_eq!(edits.state, EditsState::Checked, "{edits:?}");
        let listed: Vec<(&str, EditKind)> = edits
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.kind))
            .collect();
        assert_eq!(listed, vec![("CLAUDE.md", EditKind::Changed)]);
    }

    /// A fresh machine count is kept; a file the machine can't date keeps
    /// the time it was first seen edited (by path, whatever its kind). An
    /// attempt that couldn't count reports ITS state, beside the last
    /// count's files and time — never as a fresh count.
    #[test]
    fn a_machine_count_reports_the_attempts_state_beside_the_last_count() {
        let key: RemoteKey = (
            7,
            "devbox".into(),
            "remote-workspace-remember".into(),
            crate::context_repo::caller_scope::CallerScope::Human
                .cache_key()
                .to_string(),
        );
        let first = remember(
            key.clone(),
            checked(vec![file("a.md", EditKind::Changed)], 1_000),
        );
        assert_eq!(first.files[0].changed_at_ms, Some(1_000));
        let mut second_count = checked(
            vec![
                file("a.md", EditKind::Conflict),
                file("b.md", EditKind::Changed),
            ],
            2_000,
        );
        second_count.contexts = vec!["cw_sprint".into()];
        let second = remember(key.clone(), second_count);
        assert_eq!(second.files[0].changed_at_ms, Some(1_000), "first seen");
        assert_eq!(second.files[1].changed_at_ms, Some(2_000));
        assert_eq!(second.last_changed_at_ms, Some(2_000));
        for state in [EditsState::UnfinishedCopy, EditsState::Unknown] {
            let failed = remember(
                key.clone(),
                WorkspaceEdits::not_counted_now(state, "not now"),
            );
            assert_eq!(failed.state, state, "the attempt's state is reported");
            assert_eq!(failed.reason.as_deref(), Some("not now"));
            assert_eq!(failed.checked_at_ms, 2_000, "beside the last count's time");
            assert_eq!(failed.files.len(), 2);
        }
        let offline = not_counted(&key, false).unwrap();
        assert_eq!(offline.state, EditsState::Offline);
        assert_eq!(offline.files.len(), 2, "its last count");
        assert_eq!(offline.contexts, vec!["cw_sprint"], "its recorded sources");
        assert_eq!(not_counted(&key, true).unwrap().state, EditsState::Checked);
        let never: RemoteKey = (
            7,
            "devbox".into(),
            "remote-workspace-never".into(),
            crate::context_repo::caller_scope::CallerScope::Human
                .cache_key()
                .to_string(),
        );
        assert!(not_counted(&never, true).is_none(), "not checked yet");
        assert_eq!(
            not_counted(&never, false).unwrap().state,
            EditsState::Offline
        );
    }

    /// `assemble`: clean fresh counts are only counted; edited, unknown and
    /// not-yet-checked workspaces are listed; `total` sums local and machine
    /// edits.
    #[test]
    fn the_listing_holds_what_needs_attention_and_totals_it() {
        let local =
            |name: &str, edits: WorkspaceEdits| (PathBuf::from(format!("/w/{name}")), edits);
        let two = checked(
            vec![
                file("a.md", EditKind::Changed),
                file("b.md", EditKind::Changed),
            ],
            5,
        );
        let rows = vec![
            local("clean", checked(Vec::new(), 5)),
            local("edited", two.clone()),
            local("broken", WorkspaceEdits::unknown("record unreadable")),
        ];
        let remote = vec![
            remote_row(
                place("h-clean", "/m/clean"),
                true,
                Some(checked(Vec::new(), 5)),
            ),
            remote_row(place("h-edited", "/m/edited"), true, Some(two)),
            remote_row(place("h-never", "/m/never"), true, None),
        ];
        let value = assemble(&rows, true, remote);
        // Every machine workspace, the clean one too, can be counted again.
        assert_eq!(value["machineWorkspaces"].as_array().unwrap().len(), 3);
        assert_eq!(value["workspaces"][0]["fresh"], true);
        assert_eq!(value["total"], 4);
        assert_eq!(value["cleanWorkspaces"], 2);
        let names: Vec<&str> = value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["edited", "broken", "edited", "never"]);
        let rows = value["workspaces"].as_array().unwrap();
        assert_eq!(rows[1]["state"], "unknown");
        assert_eq!(rows[1]["reason"], "record unreadable");
        assert_eq!(rows[2]["target"]["kind"], "remote");
        assert_eq!(rows[2]["machineOnline"], true);
        assert_eq!(rows[3]["state"], "notChecked");
    }

    /// A machine workspace prepared for the team is listed from its receipt
    /// alone; another team's is not. A target must be one of the team's.
    #[test]
    fn machine_workspaces_come_from_the_teams_receipts_and_targets_are_checked() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(1u64);
        let root = crate::context_repo::path_to_user_home(home.path(), &principal)
            .join("remote-workspace-intents-v1");
        let receipt = |id: &str, team: &str, ws: &str| {
            crate::mcp::remote_workspace_target::write_test_receipt(
                &root,
                id,
                "devbox",
                &format!("{ORG}:{team}"),
                ws,
                SourceRepoSlug::new("acme", "api").unwrap(),
                &format!("/home/dev/work/{ws}"),
                1,
                "server",
                Some("story"),
            )
        };
        let handle = receipt("req-1", TEAM, "ws-1");
        let other = receipt("req-2", "platform", "ws-2");
        let ctx = ctx_for(home.path(), TEAM);
        let places = remote_places(&ctx).unwrap();
        assert_eq!(places.len(), 1, "only this team's workspace");
        assert_eq!(places[0].handle, handle);
        let remote_target = |handle: &str| Refresh::Remote {
            machine_id: "devbox".into(),
            handle: handle.into(),
        };
        assert!(check_target(&remote_target(&handle), &[], &places).is_ok());
        assert!(check_target(&remote_target(&other), &[], &places).is_err());
        let ws = tempfile::tempdir().unwrap();
        let listed = vec![(ws.path().canonicalize().unwrap(), checked(Vec::new(), 1))];
        assert!(check_target(&Refresh::Local(ws.path().into()), &listed, &[]).is_ok());
        let stranger = tempfile::tempdir().unwrap();
        assert!(check_target(&Refresh::Local(stranger.path().into()), &listed, &[]).is_err());
        let value = assemble(
            &[],
            true,
            places
                .into_iter()
                .map(|p| remote_row(p, false, None))
                .collect(),
        );
        assert_eq!(value["workspaces"][0]["name"], "ws-1");
        assert_eq!(value["workspaces"][0]["machineOnline"], false);
    }

    /// A forgotten workspace's last count is dropped when its receipt goes.
    #[test]
    fn a_count_for_a_workspace_no_receipt_names_is_forgotten() {
        let home = tempfile::tempdir().unwrap();
        let ctx = ResolverContext {
            user_id: 4242,
            ..ctx_for(home.path(), TEAM)
        };
        let key: RemoteKey = (
            4242,
            "devbox".into(),
            "remote-workspace-gone".into(),
            crate::context_repo::caller_scope::CallerScope::Human
                .cache_key()
                .to_string(),
        );
        remember(
            key.clone(),
            checked(vec![file("a.md", EditKind::Changed)], 1),
        );
        assert!(not_counted(&key, true).is_some());
        // Receipts that can't be read forget nothing.
        let root = crate::context_repo::path_to_user_home(home.path(), &ctx.principal())
            .join("remote-workspace-intents-v1");
        std::fs::create_dir_all(&root).unwrap();
        let bad = root.join(format!("{}.json", "a".repeat(64)));
        std::fs::write(&bad, b"not a receipt").unwrap();
        assert!(remote_places(&ctx).is_err());
        assert!(not_counted(&key, true).is_some(), "kept");
        std::fs::remove_file(&bad).unwrap();
        remote_places(&ctx).unwrap();
        assert!(not_counted(&key, true).is_none());
    }

    /// End to end through `unpublished` (the real listing, count and
    /// assembly): with the machine not connected, asking to count its
    /// workspace never calls the machine; the row says offline with its last
    /// count. Connected, it is counted once and marked fresh, and the local
    /// workspaces are not counted again for it.
    #[tokio::test]
    async fn an_offline_machine_is_never_asked_and_a_connected_one_once() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(1u64);
        let root = crate::context_repo::path_to_user_home(home.path(), &principal)
            .join("remote-workspace-intents-v1");
        let handle = crate::mcp::remote_workspace_target::write_test_receipt(
            &root,
            "req-e2e",
            "devbox",
            &format!("{ORG}:{TEAM}"),
            "ws-e2e",
            SourceRepoSlug::new("acme", "api").unwrap(),
            "/home/dev/work/ws-e2e",
            1,
            "server",
            Some("story"),
        );
        let ambient = ActiveContext {
            codesense_home: home.path().to_path_buf(),
            user_id: 1,
            org_slug: ORG.to_string(),
            team_name: Some(TEAM.to_string()),
        };
        let asked = Refresh::Remote {
            machine_id: "devbox".into(),
            handle: handle.clone(),
        };
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let count = |_: ActiveContext, _: String, _: String| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { checked(vec![file("a.md", EditKind::Changed)], edits::now_ms()) }
        };
        let offline = unpublished_with(ambient.clone(), asked, |_| false, count)
            .await
            .unwrap();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "never dialled"
        );
        assert_eq!(offline["workspaces"][0]["state"], "offline");
        assert_eq!(offline["workspaces"][0]["machineOnline"], false);
        let asked = Refresh::Remote {
            machine_id: "devbox".into(),
            handle: handle.clone(),
        };
        let online = unpublished_with(ambient.clone(), asked, |_| true, count)
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(online["workspaces"][0]["state"], "checked");
        assert_eq!(online["workspaces"][0]["fresh"], true);
        assert_eq!(online["total"], 1);
        // Not asked: the last count, not fresh.
        let again = unpublished_with(ambient, Refresh::None, |_| true, count)
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(again["workspaces"][0]["fresh"], false);
    }

    /// Review: a listing already counting when a caller asks is not shared
    /// with it (it may have missed what changed since); one that started
    /// after is. A machine-only count reuses the local rows but reads the
    /// receipts again, so a workspace prepared just now is known.
    #[tokio::test]
    async fn a_listing_is_shared_only_when_it_started_after_the_ask() {
        let home = tempfile::tempdir().unwrap();
        let ctx = ctx_for(home.path(), TEAM);
        let (first, fresh) = local_listing(&ctx, false).await.unwrap();
        assert!(fresh);
        let (second, fresh) = local_listing(&ctx, false).await.unwrap();
        assert!(fresh);
        assert!(second.started > first.started, "counted again, not shared");
        // A machine workspace prepared after the last listing.
        let principal = crate::domain::Principal::from(1u64);
        let root = crate::context_repo::path_to_user_home(home.path(), &principal)
            .join("remote-workspace-intents-v1");
        let handle = crate::mcp::remote_workspace_target::write_test_receipt(
            &root,
            "req-new",
            "devbox",
            &format!("{ORG}:{TEAM}"),
            "ws-new",
            SourceRepoSlug::new("acme", "api").unwrap(),
            "/home/dev/work/ws-new",
            1,
            "server",
            Some("story"),
        );
        let (reused, fresh) = local_listing(&ctx, true).await.unwrap();
        assert!(!fresh, "local rows reused");
        assert_eq!(reused.started, second.started);
        let places = reused.places.unwrap();
        assert!(
            places.iter().any(|p| p.handle == handle),
            "receipts read again"
        );
    }
}

async fn unpublished_with<C, R, F>(
    ambient: ActiveContext,
    refresh: Refresh,
    connected: C,
    count: R,
) -> Result<Value, String>
where
    C: Fn(&str) -> bool,
    R: Fn(ActiveContext, String, String) -> F,
    F: std::future::Future<Output = WorkspaceEdits>,
{
    unpublished_with_context(ambient, refresh, None, connected, count).await
}

pub(crate) async fn unpublished_context(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    refresh: Refresh,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
) -> Result<Value, String> {
    if context_root.is_none() {
        return unpublished(app, ambient, refresh).await;
    }
    let root = context_root.clone();
    unpublished_with_context(
        ambient,
        refresh,
        context_root,
        |m| connected(app, m),
        |ambient, machine, handle| {
            let root = root.clone();
            async move { count_remote_context(app, ambient, &machine, &handle, root).await }
        },
    )
    .await
}

pub(crate) async fn unpublished_context_for(
    app: &tauri::AppHandle,
    ambient: ActiveContext,
    refresh: Refresh,
    context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return unpublished_context(app, ambient, refresh, context_root).await;
    }
    let root = context_root.clone();
    let remote_scope = scope.clone();
    unpublished_with_context_for(ambient, refresh, context_root, |machine| connected(app, machine), |ambient, machine, handle| {
        let root = root.clone(); let scope = remote_scope.clone();
        async move { count_remote_context_for(app, ambient, &machine, &handle, root, scope).await }
    }, &scope).await
}
