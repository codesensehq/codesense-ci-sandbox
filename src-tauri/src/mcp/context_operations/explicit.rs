// SPDX-License-Identifier: MIT
//! Step 3 — the explicit MCP operations, on the one engine.
//!
//! `hydrate_worktree`, `hydrate_story`, `preview_reverse_sync_worktree`,
//! `reverse_sync_worktree` and `list_hydrated` take one target, local
//! (`{kind:"local", codeRepoPath, sessionId}`) or remote (`{kind:"remote",
//! machineId, workspaceHandle}`), and run identically on both:
//! - a hydrate is [`crate::agents::prepare::step3::hydrate`]: one diff, one
//!   batched copy, the exclude block in the same operation, every write
//!   recorded in the workspace's managed record (origin `step3:<id>`);
//! - a publish is [`crate::agents::prepare::publish`]: one manifest, one
//!   streamed read of the changed files, one transactional team write.
//!
//! Every operation has an `operationId` (the caller's, or a new one), emits
//! `agent-prepare-progress` events carrying it, ends in one terminal state,
//! is cancellable (`cancel_context_operation`) and keeps its result for
//! `context_operation_status` (calling again with the same id returns it).
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rmcp::model::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use super::local_admission::{AppFacts, LocalAdmission, LocalFacts};
use super::{AppTarget, WorkspaceTarget};
use crate::agents::manifest_resolve::ManifestTarget;
use crate::agents::prepare::{self, publish, step3, Destination, PrepareProgress};
use crate::commands::route_to_context::ActiveContext;
use crate::content_engine::destination::{DestinationIdentity, DestinationIo};
use crate::domain::Principal;

fn invalid(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(error.to_string(), None)
}

// ---------------------------------------------------------------- params

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HydrateParams {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    /// The workspace: `{kind:"local", codeRepoPath, sessionId}` or
    /// `{kind:"remote", machineId, workspaceHandle}`.
    pub target: WorkspaceTarget,
    /// Exact workspace-relative files to hydrate (`docs/guide.md`).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Context sets: workspace-relative folders (`.claude/skills`, `docs`).
    /// With neither paths nor sets, the runtime defaults (CLAUDE.md,
    /// `.claude/{commands,hooks,agents,skills}`, AGENTS.md, `.agents/skills`).
    #[serde(default)]
    pub sets: Vec<String>,
    /// Also remove managed files this workspace no longer wants (only ones
    /// nobody edited). Without it a hydrate only adds and refreshes.
    #[serde(default)]
    pub prune: bool,
    /// Replace files the workspace holds differently (edited, or not copied
    /// by CodeSense) with the team's version: the "Restore pinned files"
    /// recovery. Files the repo tracks are never touched.
    #[serde(default)]
    pub overwrite_modified: bool,
    /// A UUID naming this operation; calling again with it returns the kept
    /// result instead of running again. A new one is made when absent.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HydrateStoryParams {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub target: WorkspaceTarget,
    pub story_id: String,
    /// The closure policy: {referenceDepth, ancestryLevels, siblingBreadth,
    /// maxMembers}. Absent: the story's own epic dir.
    #[serde(default)]
    pub policy: Option<crate::install::hydration_closure::HydrationPolicy>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PublishParams {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub target: WorkspaceTarget,
    /// Only these workspace files or folders (absent: every managed file
    /// and every new file under the workspace's declared context).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Also publish new files under these folders.
    #[serde(default)]
    pub sets: Vec<String>,
    /// Where a NEW file goes: `team` (default), `repo` (this repo's tier),
    /// `personal`, or a tier label from a preview (`team:org/team`).
    /// Edited files always go back to the team file they came from.
    #[serde(default)]
    pub to: Option<String>,
    /// Also remove team files whose copy was deleted in the workspace (only
    /// while the team still holds what was copied). Off by default: a
    /// publish never deletes team content unless asked.
    #[serde(default)]
    pub include_deletes: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewParams {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub target: WorkspaceTarget,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub sets: Vec<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Preview with removals included (as `reverse_sync_worktree
    /// {includeDeletes:true}` would do them).
    #[serde(default)]
    pub include_deletes: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ListHydratedParams {
    pub target: WorkspaceTarget,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationParams {
    /// The id an operation returned (or the `requestId` of a selected one).
    pub operation_id: String,
}

/// What to run against one target.
pub(crate) enum Op {
    Hydrate {
        paths: Vec<String>,
        sets: Vec<String>,
        prune: bool,
        overwrite_modified: bool,
    },
    Story {
        story_id: String,
        policy: Option<crate::install::hydration_closure::HydrationPolicy>,
    },
    Publish {
        paths: Vec<String>,
        sets: Vec<String>,
        to: Option<String>,
        include_deletes: bool,
        expect: BTreeMap<String, publish::Expect>,
    },
    Preview {
        paths: Vec<String>,
        sets: Vec<String>,
        to: Option<String>,
        include_deletes: bool,
    },
    ListHydrated,
    /// "Discard my edits": replace exactly the named
    /// files with the team's version. Each path must name one file — a
    /// managed record entry, or exactly one wanted file — never a folder or
    /// prefix, and is taken verbatim (no trimming).
    Discard {
        paths: Vec<String>,
        /// What each file was when the person looked (the app's
        /// list: its workspace hash, or a new file's stat): a file rewritten
        /// since is not discarded.
        expect: BTreeMap<String, publish::Expect>,
    },
}

impl Op {
    fn kind(&self) -> &'static str {
        match self {
            Op::Hydrate { .. } | Op::Discard { .. } => "hydrate",
            Op::Story { .. } => "hydrateStory",
            Op::Publish { .. } => "publish",
            Op::Preview { .. } => "preview",
            Op::ListHydrated => "listHydrated",
        }
    }
    /// Whether it writes (and so takes the destination lock and an id).
    fn mutating(&self) -> bool {
        matches!(
            self,
            Op::Hydrate { .. } | Op::Story { .. } | Op::Publish { .. } | Op::Discard { .. }
        )
    }
}

/// The files a "Discard my edits" may replace, checked before
/// anything is written. Every path is taken exactly as given: empty paths,
/// surrounding whitespace and trailing `/` are refused, never repaired. A path
/// is accepted only when it names exactly one file — an entry in the
/// workspace's managed record, or the one wanted file whose destination is
/// that path — and refused when it is a folder (anything is wanted under it).
pub(crate) fn discard_selection(
    paths: &[String],
    wanted: &[String],
    managed: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Err("name the files to replace; Discard my edits never runs unscoped".into());
    }
    let mut accepted = Vec::new();
    for path in paths {
        if path.is_empty() || path.trim() != path {
            return Err(format!("{path:?} is not an exact file path"));
        }
        if path.ends_with('/') {
            return Err(format!("{path} is a folder; name the files in it"));
        }
        let under = format!("{path}/");
        if wanted.iter().any(|w| w.starts_with(&under)) {
            return Err(format!("{path} is a folder; name the files in it"));
        }
        let exact = wanted.iter().filter(|w| *w == path).count() == 1;
        if !exact && !managed(path) {
            return Err(format!("{path} is not a file CodeSense copied here"));
        }
        if !accepted.contains(path) {
            accepted.push(path.clone());
        }
    }
    Ok(accepted)
}

/// `blake3:<hex>` (the selected forms' hashes).
pub(crate) fn parse_hash(value: &str) -> Result<[u8; 32], String> {
    blake3::Hash::from_hex(value.trim_start_matches("blake3:"))
        .map(|hash| *hash.as_bytes())
        .map_err(|_| format!("{value:?} is not a blake3:<64 hex> hash"))
}

/// The workspace path a selected `ref` (`doc:relative/path`) names.
pub(crate) fn doc_path(reference: &str) -> Result<String, String> {
    reference
        .strip_prefix("doc:")
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{reference:?} is not a doc:<path> reference"))
}

// ---------------------------------------------------------------- registry

/// What `context_operation_status` and `get_sync_status` report of one
/// operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationRecord {
    pub operation_id: String,
    pub kind: String,
    /// `running`, `ready`, `failed` or `cancelled`.
    pub state: String,
    pub target: Value,
    /// Architecture: source authority of this retained operation, independent of readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<crate::commands::dashboard_launch::LaunchTeam>,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Default)]
struct Registry {
    running: HashMap<String, (Arc<AtomicBool>, OperationRecord)>,
    recent: VecDeque<OperationRecord>,
}

const RECENT: usize = 50;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn record_dir(home: &Path, principal: &Principal) -> PathBuf {
    crate::context_repo::path_to_user_home(home, principal)
        .join("worktrees")
        .join("operations-v1")
}

fn stored(home: &Path, principal: &Principal, id: &str) -> Option<OperationRecord> {
    let bytes = std::fs::read(record_dir(home, principal).join(format!("{id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

enum Begin {
    Started(Arc<AtomicBool>),
    Running,
    Done(OperationRecord),
    /// The id names a different operation (kind or target).
    Mismatch(String),
}

fn same_operation(record: &OperationRecord, kind: &str, target: &Value) -> Result<(), String> {
    if record.kind != kind || &record.target != target {
        return Err(format!(
            "operationId {} already names a {} on another target or of another kind; use a new id",
            record.operation_id, record.kind
        ));
    }
    Ok(())
}

#[cfg(test)]
fn begin(home: &Path, principal: &Principal, id: &str, kind: &str, target: Value) -> Begin {
    begin_with_authority(home, principal, id, kind, target, None)
}

fn begin_with_authority(
    home: &Path,
    principal: &Principal,
    id: &str,
    kind: &str,
    target: Value,
    authority: Option<crate::commands::dashboard_launch::LaunchTeam>,
) -> Begin {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, running)) = registry.running.get(id) {
        return match same_operation(running, kind, &target) {
            Ok(()) => Begin::Running,
            Err(why) => Begin::Mismatch(why),
        };
    }
    if let Some(done) = stored(home, principal, id) {
        return match same_operation(&done, kind, &target) {
            Ok(()) => Begin::Done(done),
            Err(why) => Begin::Mismatch(why),
        };
    }
    let cancel = Arc::new(AtomicBool::new(false));
    registry.running.insert(
        id.to_string(),
        (
            cancel.clone(),
            OperationRecord {
                operation_id: id.to_string(),
                kind: kind.to_string(),
                state: "running".into(),
                target,
                authority,
                started_at_ms: now_ms(),
                finished_at_ms: None,
                result: None,
            },
        ),
    );
    Begin::Started(cancel)
}

fn finish(home: &Path, principal: &Principal, id: &str, state: &str, result: &Value) {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let Some((_, mut record)) = registry.running.remove(id) else {
        return;
    };
    record.state = state.to_string();
    record.finished_at_ms = Some(now_ms());
    record.result = Some(result.clone());
    let dir = record_dir(home, principal);
    if let Ok(bytes) = serde_json::to_vec(&record) {
        if let Err(error) =
            crate::content_engine::write_tmp_fsync_rename(&dir.join(format!("{id}.json")), &bytes)
        {
            log::warn!(target: "agents", "operation {id}'s result was not kept: {error}");
        }
    }
    registry.recent.push_front(record);
    registry.recent.truncate(RECENT);
    drop(registry);
    prune_kept(&dir);
}

/// Kept results older than this are removed (a retry comes within minutes).
const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
/// And at most this many are kept.
const KEEP_AT_MOST: usize = 500;

/// Bound `operations-v1`: drop results past [`KEEP_FOR`], then the oldest
/// beyond [`KEEP_AT_MOST`].
fn prune_kept(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > KEEP_FOR {
            let _ = std::fs::remove_file(&path);
        } else {
            kept.push((modified, path));
        }
    }
    if kept.len() > KEEP_AT_MOST {
        kept.sort();
        for (_, path) in kept.iter().take(kept.len() - KEEP_AT_MOST) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Ask a running operation to stop. `false` when it is not running here.
pub(crate) fn cancel(id: &str) -> bool {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    match registry.running.get(id) {
        Some((flag, _)) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

fn admits_record(
    home: &Path,
    principal: &Principal,
    record: &OperationRecord,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> bool {
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return true;
    }
    record
        .authority
        .as_ref()
        .is_some_and(|team| scope.target_team(home, principal, &team.org, &team.team))
}

pub(crate) fn status_for(
    home: &Path,
    principal: &Principal,
    id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Option<OperationRecord> {
    status(home, principal, id).filter(|record| admits_record(home, principal, record, scope))
}

/// One operation's state: running here, or kept on disk.
pub(crate) fn status(home: &Path, principal: &Principal, id: &str) -> Option<OperationRecord> {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, record)) = registry.running.get(id) {
        return Some(record.clone());
    }
    drop(registry);
    stored(home, principal, id)
}

/// The operations running now and the most recent finished ones.
pub(crate) fn recent(home: &Path, principal: &Principal, limit: usize) -> Vec<OperationRecord> {
    recent_for(home, principal, limit, &Default::default())
}

pub(crate) fn recent_for(
    home: &Path,
    principal: &Principal,
    limit: usize,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Vec<OperationRecord> {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<OperationRecord> = registry
        .running
        .values()
        .map(|(_, record)| record.clone())
        .collect();
    drop(registry);
    let mut done: Vec<OperationRecord> = std::fs::read_dir(record_dir(home, principal))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<OperationRecord>(&bytes).ok())
        .collect();
    out.retain(|record| admits_record(home, principal, record, scope));
    done.retain(|record| admits_record(home, principal, record, scope));
    done.sort_by(|a, b| b.finished_at_ms.cmp(&a.finished_at_ms));
    out.extend(done);
    out.truncate(limit);
    // Results are summaries in this list; `context_operation_status` has the
    // full one.
    for record in &mut out {
        if let Some(result) = &mut record.result {
            if let Some(map) = result.as_object_mut() {
                map.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "status" | "note" | "written" | "published" | "conflicts" | "failed"
                    )
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- binding

/// One admitted target, ready to run against.
struct Bound {
    caller_scope: crate::context_repo::caller_scope::CallerScope,
    ctx: crate::content_engine::ResolverContext,
    slug: crate::domain::SourceRepoSlug,
    place: Place,
    target_json: Value,
    /// Who asked (for progress): the calling agent session, when known.
    agent: String,
    workspace: String,
}

enum Place {
    Local {
        admission: Box<LocalAdmission>,
        facts: Arc<dyn LocalFacts>,
    },
    /// A local workspace the app asked for (no session): admitted
    /// while it is one of `team`'s workspaces on this Mac and its repository
    /// still resolves to the bound one.
    LocalApp {
        root: PathBuf,
        team: crate::content_engine::ResolverContext,
    },
    Remote {
        io: Arc<dyn DestinationIo>,
        team_key: String,
        workspace_id: String,
    },
}

impl Bound {
    fn principal(&self) -> Principal {
        Principal::from(self.ctx.user_id)
    }
    fn manifest_target(&self) -> Result<ManifestTarget, String> {
        Ok(ManifestTarget {
            context_root: self.ctx.context_root.clone(),
            org: self.ctx.org_slug.clone(),
            team: self
                .ctx
                .team_name
                .clone()
                .ok_or("the workspace has no team to resolve context through")?,
            repo: Some(self.slug.clone()),
        })
    }
    fn identity(&self) -> DestinationIdentity {
        match &self.place {
            Place::Local { admission, .. } => {
                crate::content_engine::context_lifecycle::local_identity(&admission.root)
            }
            Place::LocalApp { root, .. } => {
                crate::content_engine::context_lifecycle::local_identity(root)
            }
            Place::Remote { io, .. } => io.identity().clone(),
        }
    }
    fn validate(&self) -> Result<(), String> {
        match &self.place {
            Place::Local { admission, facts } => admission.validate(facts.as_ref()),
            Place::LocalApp { root, team } => {
                if !crate::mcp::team_file_edits::local_workspaces(team).contains(root) {
                    return Err("the workspace is no longer one of this team's on this Mac".into());
                }
                if super::local_repository(&self.ctx, root).map_err(|e| e.message.to_string())?
                    != self.slug
                {
                    return Err("the workspace's repository changed".into());
                }
                Ok(())
            }
            Place::Remote { io, .. } => io.validate_authority(),
        }
    }
    fn destination(&self) -> Destination<'_> {
        match &self.place {
            Place::Local { admission, .. } => Destination::Local {
                root: &admission.root,
            },
            Place::LocalApp { root, .. } => Destination::Local { root },
            Place::Remote { io, .. } => Destination::Remote { io: io.as_ref() },
        }
    }
    fn legacy(&self) -> Result<prepare::LegacyRows, String> {
        match &self.place {
            Place::Local { admission, .. } => {
                prepare::legacy_rows_local(&self.ctx, &admission.root)
            }
            Place::LocalApp { root, .. } => prepare::legacy_rows_local(&self.ctx, root),
            Place::Remote { io, .. } => match io.identity() {
                DestinationIdentity::Remote(remote) => {
                    prepare::legacy_rows_remote(&self.ctx, &self.slug, remote)
                }
                _ => Ok(Vec::new()),
            },
        }
    }
}

fn bound_local(admission: LocalAdmission, facts: Arc<dyn LocalFacts>, agent: String) -> Bound {
    Bound {
        caller_scope: Default::default(),
        ctx: admission.ctx.clone(),
        slug: admission.slug().clone(),
        target_json: json!({"kind":"local","codeRepoPath":admission.root}),
        agent,
        workspace: admission.root.display().to_string(),
        place: Place::Local {
            admission: Box::new(admission),
            facts,
        },
    }
}

/// Run `op` on a local admission, as `run` does after binding (tests).
#[cfg(test)]
pub(super) fn execute_local(
    admission: LocalAdmission,
    facts: Arc<dyn LocalFacts>,
    op: Op,
    id: &str,
) -> Result<(Value, &'static str), String> {
    let bound = bound_local(admission, facts, "test".into());
    execute(
        &|_| {},
        &bound,
        op,
        id,
        &AtomicBool::new(false),
        None,
        &AtomicBool::new(false),
    )
}

/// As [`execute_local`], collecting the progress events it emits.
#[cfg(test)]
pub(super) fn execute_local_events(
    admission: LocalAdmission,
    facts: Arc<dyn LocalFacts>,
    op: Op,
    id: &str,
) -> (Result<(Value, &'static str), String>, Vec<PrepareProgress>) {
    let bound = bound_local(admission, facts, "test".into());
    let events = Mutex::new(Vec::new());
    let result = execute(
        &|p: &PrepareProgress| events.lock().unwrap().push(p.clone()),
        &bound,
        op,
        id,
        &AtomicBool::new(false),
        None,
        &AtomicBool::new(false),
    );
    (result, events.into_inner().unwrap())
}

async fn bind(
    app: &tauri::AppHandle,
    target: WorkspaceTarget,
    caller: Option<super::super::remote_authority::RemoteAuthority>,
    context: Option<super::ContextSelection>,
    connection_session: Option<String>,
) -> Result<Bound, ErrorData> {
    let remote_team = caller
        .as_ref()
        .and_then(|authority| authority.team_key().split_once(':'))
        .map(
            |(org, team)| crate::commands::dashboard_launch::LaunchTeam {
                org: org.into(),
                team: team.into(),
            },
        );
    let caller_scope = crate::context_repo::caller_scope::for_session(
        app,
        connection_session
            .as_deref()
            .or_else(|| caller.as_ref().and_then(|c| c.caller_session())),
        remote_team,
    )
    .await;
    let session = target.context_session(connection_session.or_else(|| {
        caller
            .as_ref()
            .and_then(|c| c.caller_session().map(str::to_string))
    }));
    let mut bound = match target {
        WorkspaceTarget::Remote {
            machine_id,
            workspace_handle,
        } => {
            let ambient = if let Some(caller) = &caller {
                caller.context().await?
            } else {
                let (codesense_home, user_id) =
                    crate::commands::route_to_context::resolve_ambient(app)
                        .await
                        .map_err(invalid)?;
                ActiveContext {
                    codesense_home,
                    user_id,
                    org_slug: String::new(),
                    team_name: None,
                }
            };
            let resolved = super::super::remote_destination::resolve_selected(
                app.clone(),
                ambient,
                &machine_id,
                &workspace_handle,
                caller,
                false,
            )
            .await
            .map_err(invalid)?;
            let active = resolved.context.clone();
            let ctx = crate::commands::content_engine::derive_resolver_context(
                active.codesense_home,
                active.user_id,
                active.org_slug,
                active.team_name,
            )
            .map_err(invalid)?;
            Ok(Bound {
                caller_scope: Default::default(),
                ctx,
                slug: resolved.source_repo,
                target_json: json!({"kind":"remote","machineId":machine_id,"workspaceHandle":workspace_handle}),
                agent: "explicit".into(),
                workspace: workspace_handle.clone(),
                place: Place::Remote {
                    io: resolved.io,
                    team_key: resolved.team_key,
                    workspace_id: resolved.workspace_id,
                },
            })
        }
        WorkspaceTarget::Local {
            code_repo_path,
            session_id,
        } => {
            if caller.is_some() {
                return Err(invalid("remote caller requires a remote target"));
            }
            let facts: Arc<dyn LocalFacts> = Arc::new(AppFacts(app.clone()));
            let agent = session_id.clone();
            tokio::task::spawn_blocking(move || {
                let admission =
                    LocalAdmission::capture(facts.as_ref(), &code_repo_path, session_id)
                        .map_err(invalid)?;
                Ok::<_, ErrorData>(bound_local(admission, facts, agent))
            })
            .await
            .map_err(invalid)?
        }
    }?;
    bound.caller_scope = caller_scope;
    let launcher = session.and_then(|session| {
        app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .and_then(|state| state.launcher_context(&session))
    });
    super::bind_context(&mut bound.ctx, context, launcher.as_deref()).map_err(invalid)?;
    bound.target_json["context"] = json!(bound
        .ctx
        .context_root
        .as_ref()
        .map_or("clone", |r| r.id.as_str()));
    Ok(bound)
}

/// A local workspace the app asked for, admitted: it must be one
/// of `team`'s workspaces on this Mac and resolve to a repository.
fn bound_app_local(
    team: crate::content_engine::ResolverContext,
    requested: &Path,
) -> Result<Bound, String> {
    let root = std::fs::canonicalize(requested)
        .map_err(|e| format!("{} is unavailable: {e}", requested.display()))?;
    if !crate::mcp::team_file_edits::local_workspaces(&team).contains(&root) {
        return Err(format!(
            "{} is not one of this team's workspaces on this Mac",
            root.display()
        ));
    }
    let (ctx, manifest) = crate::commands::watch::workspace_target(&team, &root)?;
    let slug = manifest
        .repo
        .ok_or("the workspace has no repository identity")?;
    Ok(Bound {
        caller_scope: Default::default(),
        ctx,
        slug,
        target_json: json!({"kind":"local","codeRepoPath":root,"requestedBy":"app"}),
        agent: "app".into(),
        workspace: root.display().to_string(),
        place: Place::LocalApp { root, team },
    })
}

/// Run `op` on a local workspace the app asked for, as `run_app` does after
/// binding (tests).
#[cfg(test)]
pub(crate) fn execute_app_local(
    team: crate::content_engine::ResolverContext,
    root: &Path,
    op: Op,
    id: &str,
) -> Result<(Value, &'static str), String> {
    let bound = bound_app_local(team, root)?;
    execute(
        &|_| {},
        &bound,
        op,
        id,
        &AtomicBool::new(false),
        None,
        &AtomicBool::new(false),
    )
}

/// Bind a workspace the app asked for. A local one must be one of
/// the active team's workspaces on this Mac (`local_workspaces`) with a
/// repository that resolves (`workspace_target` → `local_repository`); a
/// machine one is admitted by its durable receipt (the viewer need not have
/// it open). The app is the requester: progress and the operation record say
/// so (`agent: "app"`, `requestedBy: "app"`); no agent session is borrowed.
async fn bind_app(app: &tauri::AppHandle, target: AppTarget) -> Result<Bound, ErrorData> {
    let ambient = crate::commands::route_to_context::derive_active_context(app)
        .await
        .map_err(invalid)?;
    match target {
        AppTarget::Local { code_repo_path } => {
            let team = crate::commands::content_engine::derive_resolver_context(
                ambient.codesense_home,
                ambient.user_id,
                ambient.org_slug,
                ambient.team_name,
            )
            .map_err(invalid)?;
            tokio::task::spawn_blocking(move || {
                bound_app_local(team, &code_repo_path).map_err(invalid)
            })
            .await
            .map_err(invalid)?
        }
        AppTarget::Remote {
            machine_id,
            workspace_handle,
        } => {
            let (active_org, active_team) = (ambient.org_slug.clone(), ambient.team_name.clone());
            let resolved = super::super::remote_workspace_target::resolve_durable(
                app.clone(),
                ambient,
                &machine_id,
                &workspace_handle,
            )
            .await
            .map(super::super::remote_destination::from_resolved)
            .map_err(invalid)?;
            // As for a local workspace: only one of the active team's.
            let (org, team) = crate::commands::pty_layout::parse_team_key(&resolved.team_key);
            if org != active_org || team != active_team.as_deref() {
                return Err(invalid(
                    "that machine workspace was prepared for another team",
                ));
            }
            let active = resolved.context.clone();
            let ctx = crate::commands::content_engine::derive_resolver_context(
                active.codesense_home,
                active.user_id,
                active.org_slug,
                active.team_name,
            )
            .map_err(invalid)?;
            Ok(Bound {
                caller_scope: Default::default(),
                ctx,
                slug: resolved.source_repo,
                target_json: json!({"kind":"remote","machineId":machine_id,"workspaceHandle":workspace_handle,"requestedBy":"app"}),
                agent: "app".into(),
                workspace: workspace_handle.clone(),
                place: Place::Remote {
                    io: resolved.io,
                    team_key: resolved.team_key,
                    workspace_id: resolved.workspace_id,
                },
            })
        }
    }
}

// ---------------------------------------------------------------- running

fn report_json(id: &str, report: &prepare::PrepareReport) -> Value {
    let summary = prepare::progress::PrepareSummary::of(report);
    let mut value = serde_json::to_value(&summary).unwrap_or_else(|_| json!({}));
    if let Some(map) = value.as_object_mut() {
        map.insert("operationId".into(), json!(id));
        map.insert(
            "removed".into(),
            json!(report.removed.iter().take(50).collect::<Vec<_>>()),
        );
        map.insert(
            "writtenPaths".into(),
            json!(report.written.iter().take(50).collect::<Vec<_>>()),
        );
        map.insert("note".into(), json!(report.startup_note("team content")));
    }
    value
}

fn state_of_prepare(report: &prepare::PrepareReport) -> &'static str {
    match report.status {
        prepare::PrepareStatus::Ready | prepare::PrepareStatus::Skipped => "ready",
        prepare::PrepareStatus::Failed => "failed",
        prepare::PrepareStatus::Cancelled => "cancelled",
    }
}

/// The epic dir as `list_hydrated` reports it: its path inside its content
/// tier (or as given when outside every tier).
fn epic_label(
    roots: &[(String, crate::context_repo::inheritance::ContentRoot)],
    epic: &Path,
) -> String {
    roots
        .iter()
        .find_map(|(_, root)| root.relative_directory(epic))
        .unwrap_or_else(|| epic.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

/// The progress event every operation ends with exactly once.
fn terminal_failed(bound: &Bound, id: &str, error: &str) -> PrepareProgress {
    PrepareProgress {
        workspace: bound.workspace.clone(),
        agent: bound.agent.clone(),
        phase: prepare::PreparePhase::Failed,
        files: 0,
        bytes: 0,
        total: 0,
        note: Some(error.to_string()),
        summary: None,
        operation_id: Some(id.to_string()),
        operation_kind: None,
    }
}

fn is_terminal(phase: prepare::PreparePhase) -> bool {
    matches!(
        phase,
        prepare::PreparePhase::Ready
            | prepare::PreparePhase::Failed
            | prepare::PreparePhase::Cancelled
    )
}

/// Run `op` against `bound`. Blocking: call inside `spawn_blocking`.
///
/// A mutating operation emits exactly one terminal progress event
/// (`ready`, `failed` or `cancelled`) whatever path it ends on: an error
/// before, inside or after the engine run emits `failed` unless the run
/// already ended.
fn execute(
    sink: &dyn Fn(&PrepareProgress),
    bound: &Bound,
    op: Op,
    id: &str,
    cancel: &AtomicBool,
    cache: Option<&crate::install::frontmatter_cache::SharedFrontmatterCache>,
    ended: &AtomicBool,
) -> Result<(Value, &'static str), String> {
    let mutating = op.mutating();
    let kind = op.kind();
    let emit = |progress: &PrepareProgress| {
        if is_terminal(progress.phase) && ended.swap(true, Ordering::SeqCst) {
            // A second terminal event would contradict the first.
            return;
        }
        let mut stamped = progress.clone();
        stamped.operation_kind = Some(kind.to_string());
        sink(&stamped);
    };
    let result = execute_inner(bound, op, id, cancel, cache, &emit);
    if let Err(error) = &result {
        if mutating {
            emit(&terminal_failed(bound, id, error));
        }
    }
    result
}

fn execute_inner(
    bound: &Bound,
    op: Op,
    id: &str,
    cancel: &AtomicBool,
    cache: Option<&crate::install::frontmatter_cache::SharedFrontmatterCache>,
    emit: &dyn Fn(&PrepareProgress),
) -> Result<(Value, &'static str), String> {
    bound.validate()?;
    let home = bound.ctx.codesense_home.clone();
    let principal = bound.principal();
    let target = bound.manifest_target()?;
    let cancelled = || cancel.load(Ordering::SeqCst);
    let validate = || bound.validate();
    let legacy = || bound.legacy();
    let run_hydrate = |want: &prepare::WantSet, prune: bool, overwrite: bool| {
        step3::hydrate(step3::Hydrate {
            home: &home,
            principal: &principal,
            destination: bound.destination(),
            want,
            operation: id,
            workspace: bound.workspace.clone(),
            agent: bound.agent.clone(),
            prune,
            overwrite_modified: overwrite,
            progress: emit,
            validate: &validate,
            cancel: Some(&cancelled),
            legacy: &legacy,
        })
    };
    let publish_request = |paths: &[String],
                           sets: &[String],
                           to: Option<&str>,
                           include_deletes: bool,
                           expect: &BTreeMap<String, publish::Expect>|
     -> Result<publish::PublishReport, String> {
        let request = publish::PublishRequest {
            home: &home,
            principal: &principal,
            destination: bound.destination(),
            target: &target,
            paths,
            sets,
            to,
            expect,
            include_deletes,
            legacy: Some(&legacy),
            operation: id,
            workspace: bound.workspace.clone(),
            agent: bound.agent.clone(),
            progress: emit,
            validate: &validate,
            cancel: Some(&cancelled),
        };
        let visible = if matches!(
            bound.caller_scope,
            crate::context_repo::caller_scope::CallerScope::Human
        ) {
            None
        } else {
            Some(publish::preview_for(&request, &bound.caller_scope)?)
        };
        let mut report = publish::publish(&request)?;
        if let Some(visible) = visible {
            report.retain_readback(&visible, &bound.caller_scope);
        }
        Ok(report)
    };
    let (mut out, state) = match op {
        Op::Hydrate {
            paths,
            sets,
            prune,
            overwrite_modified,
        } => {
            let want = step3::want_for_selection_for(
                &home,
                &principal,
                &target,
                &paths,
                &sets,
                &bound.caller_scope,
            )?;
            let report = run_hydrate(&want, prune, overwrite_modified)?;
            let mut value = report_json(id, &report);
            value["warnings"] = json!(want.warnings);
            (value, state_of_prepare(&report))
        }
        Op::Discard { paths, expect } => {
            // Syntax first (no folders, no trimming), before any resolution.
            discard_selection(&paths, &[], &|_| true)?;
            let mut want = step3::want_for_selection_for(
                &home,
                &principal,
                &target,
                &paths,
                &[],
                &bound.caller_scope,
            )?;
            let identity = match bound.destination() {
                Destination::Local { root } => {
                    crate::content_engine::context_lifecycle::local_identity(root)
                }
                Destination::Remote { io } => io.identity().clone(),
            };
            let record = prepare::ManagedRecord::load(&home, &principal, &identity);
            let wanted: Vec<String> = want.files.iter().map(|f| f.dest.clone()).collect();
            // A file CodeSense never copied here (a new file) has no
            // team version to put back; discarding it deletes it.
            let (copied, new): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| {
                record.entries.contains_key(p)
                    || wanted
                        .iter()
                        .any(|w| w == p || w.starts_with(&format!("{p}/")))
            });
            // Checked before anything is written: a copied file must be one
            // exact managed or wanted file; a new one is checked (and refused
            // as a whole) by discard_new_files before it deletes anything.
            let accepted = if copied.is_empty() {
                Vec::new()
            } else {
                discard_selection(&copied, &wanted, &|p| record.entries.contains_key(p))?
            };
            // The same request for the check and the delete.
            macro_rules! new_request {
                ($paths:expr, $expect:expr) => {
                    publish::PublishRequest {
                        home: &home,
                        principal: &principal,
                        destination: bound.destination(),
                        target: &target,
                        paths: $paths,
                        sets: &[],
                        to: None,
                        expect: $expect,
                        include_deletes: false,
                        legacy: Some(&legacy),
                        operation: id,
                        workspace: bound.workspace.clone(),
                        agent: bound.agent.clone(),
                        progress: emit,
                        validate: &validate,
                        cancel: Some(&cancelled),
                    }
                };
            }
            let no_expect: BTreeMap<String, publish::Expect> = expect
                .iter()
                .filter(|(path, _)| new.contains(path))
                .map(|(path, seen)| (path.clone(), seen.clone()))
                .collect();
            if !new.is_empty() {
                // Refused as a whole before anything is written.
                publish::check_new_files(&new_request!(&new, &no_expect))?;
            }
            // A copied file rewritten since the person looked keeps its edit.
            let pinned: Vec<String> = accepted
                .iter()
                .filter(|p| expect.get(*p).is_some_and(|e| e.workspace.is_some()))
                .cloned()
                .collect();
            let now = publish::workspace_hashes(bound.destination(), &pinned)?;
            let moved: Vec<String> = pinned
                .into_iter()
                .filter(|p| {
                    now.get(p).copied().flatten() != expect.get(p).and_then(|e| e.workspace)
                })
                .collect();
            let accepted: Vec<String> = accepted
                .into_iter()
                .filter(|p| !moved.contains(p))
                .collect();
            let mut state = "ready";
            let mut value = json!({ "operationId": id });
            // The team's versions go back first: a failure there deletes nothing.
            if !accepted.is_empty() {
                // Exactly the accepted files, nothing else the selection resolved.
                want.files.retain(|f| accepted.contains(&f.dest));
                // A managed file the team no longer has can't be replaced: say so.
                let not_in_team: Vec<&String> = accepted
                    .iter()
                    .filter(|p| !want.files.iter().any(|f| &f.dest == *p))
                    .collect();
                let report = run_hydrate(&want, false, true)?;
                value = report_json(id, &report);
                value["warnings"] = json!(want.warnings);
                value["notInTeam"] = json!(not_in_team);
                state = state_of_prepare(&report);
            }
            if !moved.is_empty() {
                state = "failed";
                let mut failed: Vec<Value> =
                    value["failed"].as_array().cloned().unwrap_or_default();
                failed.extend(
                    moved
                        .iter()
                        .map(|p| json!([p, "it changed since it was looked at"])),
                );
                value["failedCount"] = json!(failed.len());
                value["failed"] = json!(failed);
            }
            if !new.is_empty() {
                // Checked above; what changed since (or a machine that dropped)
                // is a failed file, never an error that loses the restore's report.
                let discarded = publish::discard_new_files(&new_request!(&new, &no_expect))
                    .unwrap_or_else(|why| publish::DiscardedNew {
                        failed: new.iter().map(|p| (p.clone(), why.clone())).collect(),
                        ..Default::default()
                    });
                if !discarded.failed.is_empty() {
                    state = "failed";
                }
                let mut failed: Vec<Value> =
                    value["failed"].as_array().cloned().unwrap_or_default();
                failed.extend(discarded.failed.iter().map(|(p, why)| json!([p, why])));
                value["failed"] = json!(failed);
                value["failedCount"] = json!(failed.len());
                value["removedNew"] = json!(discarded.removed);
            }
            value["status"] = json!(state);
            (value, state)
        }
        Op::Story { story_id, policy } => {
            let cache = cache.ok_or("the story index is unavailable")?;
            let story = crate::commands::route_to_context::remote_story_files_for(
                &bound.ctx,
                &bound.slug,
                &story_id,
                cache,
                policy,
                &bound.caller_scope,
            )
            .map_err(|e| e.to_string())?;
            let (want, outside) = step3::want_for_files_for(
                &home,
                &principal,
                &target,
                &story.files,
                &bound.caller_scope,
            );
            let report = run_hydrate(&want, false, false)?;
            let mut value = report_json(id, &report);
            // Only a hydrate that landed records its epics; recording is
            // best-effort after the terminal event (never a second one).
            if report.status == prepare::PrepareStatus::Ready {
                let roots = step3::labelled_roots(&home, &principal, &target);
                if let Err(error) = step3::record_hydrated_epics(
                    &home,
                    &principal,
                    &bound.identity(),
                    story.epic_dirs.iter().map(|epic| epic_label(&roots, epic)),
                ) {
                    log::warn!(target: "agents", "hydrate_story {id}: epics not recorded: {error}");
                    value["warnings"] = json!([format!(
                        "the hydrated epics were not recorded for list_hydrated: {error}"
                    )]);
                }
            }
            value["story"] = story_scope_json(&story_id, &story, outside);
            (value, state_of_prepare(&report))
        }
        Op::Publish {
            paths,
            sets,
            to,
            include_deletes,
            expect,
        } => {
            let report = publish_request(&paths, &sets, to.as_deref(), include_deletes, &expect)?;
            let state = match report.status {
                publish::PublishStatus::Published
                | publish::PublishStatus::PublishedWithConflicts
                | publish::PublishStatus::NothingToPublish => "ready",
                publish::PublishStatus::Cancelled => "cancelled",
                _ => "failed",
            };
            let mut value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
            value["operationId"] = json!(id);
            value["published"] = json!(report.entries.len());
            emit(&PrepareProgress {
                workspace: bound.workspace.clone(),
                agent: bound.agent.clone(),
                phase: match state {
                    "ready" => prepare::PreparePhase::Ready,
                    "cancelled" => prepare::PreparePhase::Cancelled,
                    _ => prepare::PreparePhase::Failed,
                },
                files: report.entries.len() as u64,
                bytes: report.entries.iter().map(|e| e.bytes).sum(),
                total: (report.entries.len() + report.conflicts.len()) as u64,
                note: Some(match report.status {
                    publish::PublishStatus::Cancelled => format!(
                        "Publish cancelled before team content was written; {} change(s) not published.",
                        report.entries.len() + report.conflicts.len()
                    ),
                    _ => format!(
                        "Published {} change(s); {} conflict(s) not written.",
                        report.entries.len(),
                        report.conflicts.len()
                    ),
                }),
                summary: None,
                operation_id: Some(id.to_string()),
                operation_kind: None,
            });
            (value, state)
        }
        Op::Preview {
            paths,
            sets,
            to,
            include_deletes,
        } => {
            let report = publish::preview_for(
                &publish::PublishRequest {
                    home: &home,
                    principal: &principal,
                    destination: bound.destination(),
                    target: &target,
                    paths: &paths,
                    sets: &sets,
                    to: to.as_deref(),
                    expect: &BTreeMap::new(),
                    include_deletes,
                    legacy: Some(&legacy),
                    operation: id,
                    workspace: bound.workspace.clone(),
                    agent: bound.agent.clone(),
                    progress: &|_| {},
                    validate: &validate,
                    cancel: None,
                },
                &bound.caller_scope,
            )?;
            (
                serde_json::to_value(&report).map_err(|e| e.to_string())?,
                "ready",
            )
        }
        Op::ListHydrated => {
            let record = prepare::ManagedRecord::load(&home, &principal, &bound.identity());
            let mut by_origin: BTreeMap<String, usize> = BTreeMap::new();
            for entry in record.entries.values() {
                let origin = entry.origin.split(':').next().unwrap_or("").to_string();
                *by_origin.entry(origin).or_default() += 1;
            }
            let shown: Vec<Value> = record
                .entries
                .iter()
                .take(500)
                .map(|(path, entry)| {
                    json!({"path": path, "origin": entry.origin, "sourceLayer": entry.source_layer})
                })
                .collect();
            (
                json!({
                    "hydratedEpics": record.hydrated_epics,
                    "managedFiles": record.entries.len(),
                    "byOrigin": by_origin,
                    "files": shown,
                    "truncated": record.entries.len() > 500,
                    "prefixes": record.prefixes,
                    "lastLaunchStatus": record.last_status,
                }),
                "ready",
            )
        }
    };
    if let Some(map) = out.as_object_mut() {
        map.insert("target".into(), bound.target_json.clone());
        if let Place::Remote {
            team_key,
            workspace_id,
            ..
        } = &bound.place
        {
            map.insert("teamKey".into(), json!(team_key));
            map.insert("workspaceId".into(), json!(workspace_id));
        }
    }
    Ok((out, state))
}

/// The one entry for every explicit operation.
pub(crate) async fn run(
    app: tauri::AppHandle,
    target: WorkspaceTarget,
    context: Option<super::ContextSelection>,
    op: Op,
    operation_id: Option<String>,
    caller: Option<super::super::remote_authority::RemoteAuthority>,
    connection_session: Option<String>,
) -> Result<Value, ErrorData> {
    let id = match operation_id {
        Some(id) => uuid::Uuid::parse_str(&id)
            .map_err(|_| invalid("operationId must be a UUID"))?
            .to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let coordinator = app
        .try_state::<Arc<crate::content_engine::sync::wave::WaveCoordinator>>()
        .ok_or_else(|| invalid("context coordinator unavailable"))?
        .inner()
        .clone();
    let bound = bind(&app, target, caller, context, connection_session).await?;
    run_bound(app, bound, op, id, coordinator).await
}

/// Run `op` on a workspace the app asked for ([`bind_app`]): the
/// same engine, lock, progress and record as an agent's explicit operation.
pub(crate) async fn run_app(
    app: tauri::AppHandle,
    target: AppTarget,
    op: Op,
    operation_id: Option<String>,
) -> Result<Value, ErrorData> {
    let id = match operation_id {
        Some(id) => uuid::Uuid::parse_str(&id)
            .map_err(|_| invalid("operationId must be a UUID"))?
            .to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let coordinator = app
        .try_state::<Arc<crate::content_engine::sync::wave::WaveCoordinator>>()
        .ok_or_else(|| invalid("context coordinator unavailable"))?
        .inner()
        .clone();
    let bound = bind_app(&app, target).await?;
    run_bound(app, bound, op, id, coordinator).await
}

async fn run_bound(
    app: tauri::AppHandle,
    bound: Bound,
    op: Op,
    id: String,
    coordinator: Arc<crate::content_engine::sync::wave::WaveCoordinator>,
) -> Result<Value, ErrorData> {
    let cache = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .map(|state| state.inner().clone());
    let home = bound.ctx.codesense_home.clone();
    let principal = bound.principal();
    let mutating = op.mutating();
    let cancel = if mutating {
        match begin_with_authority(
            &home,
            &principal,
            &id,
            op.kind(),
            bound.target_json.clone(),
            match &bound.caller_scope {
                crate::context_repo::caller_scope::CallerScope::Team(view) => {
                    Some(crate::commands::dashboard_launch::LaunchTeam {
                        org: view.org().into(),
                        team: view.reader_team().into(),
                    })
                }
                crate::context_repo::caller_scope::CallerScope::Unknown => None,
                crate::context_repo::caller_scope::CallerScope::Human => {
                    bound.ctx.team_name.as_ref().map(|team| {
                        crate::commands::dashboard_launch::LaunchTeam {
                            org: bound.ctx.org_slug.clone(),
                            team: team.clone(),
                        }
                    })
                }
            },
        ) {
            Begin::Started(cancel) => cancel,
            Begin::Running => {
                return Ok(json!({"operationId": id, "status": "running"}));
            }
            Begin::Done(record) => {
                if !admits_record(&home, &principal, &record, &bound.caller_scope) {
                    return Err(invalid(crate::context_repo::caller_scope::MISSING));
                }

                return Ok(record
                    .result
                    .unwrap_or_else(|| json!({"operationId": id, "status": record.state})));
            }
            Begin::Mismatch(why) => return Err(invalid(why)),
        }
    } else {
        Arc::new(AtomicBool::new(false))
    };
    // The destination lock the launch path holds (Step 2), across the whole
    // operation: one writer per machine workspace at a time.
    let guard = match (&bound.place, mutating) {
        (Place::Remote { io, .. }, true) => match io.identity() {
            DestinationIdentity::Remote(remote) => Some(
                coordinator
                    .acquire_remote_destination_lock(&bound.ctx, &bound.slug, remote)
                    .await,
            ),
            _ => None,
        },
        _ => None,
    };
    let task_id = id.clone();
    let failed_event = terminal_failed(&bound, &id, "the operation stopped unexpectedly");
    let panic_app = app.clone();
    let ended = Arc::new(AtomicBool::new(false));
    let task_ended = ended.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let sink = |progress: &PrepareProgress| {
            let _ = app.emit(prepare::PREPARE_PROGRESS_EVENT, progress);
        };
        execute(
            &sink,
            &bound,
            op,
            &task_id,
            &cancel,
            cache.as_ref(),
            &task_ended,
        )
    })
    .await;
    let outcome = joined_outcome(joined, &ended, &|| {
        let _ = panic_app.emit(prepare::PREPARE_PROGRESS_EVENT, &failed_event);
    });
    conclude(&home, &principal, &id, mutating, outcome)
        .map_err(|e| ErrorData::invalid_request(e, None))
}

/// The task's outcome; a panic (a join error) becomes a failed outcome and,
/// unless the operation already ended, its one terminal `failed` event.
fn joined_outcome<T, E: std::fmt::Display>(
    joined: Result<Result<T, String>, E>,
    ended: &AtomicBool,
    emit_failed: &dyn Fn(),
) -> Result<T, String> {
    match joined {
        Ok(outcome) => outcome,
        Err(error) => {
            if !ended.swap(true, Ordering::SeqCst) {
                emit_failed();
            }
            Err(format!("the operation stopped unexpectedly: {error}"))
        }
    }
}

fn story_scope_json(
    story_id: &str,
    story: &crate::commands::route_to_context::RemoteStoryFiles,
    outside: usize,
) -> Value {
    json!({
        "storyId": story_id,
        "epicDir": story.epic_dir,
        "filesScoped": story.files.len(),
        "outsideTeamContent": outside,
    })
}

/// Keep a mutating operation's result and release its registry entry
/// (whatever way it ended, its id never answers `running` again).
fn conclude(
    home: &Path,
    principal: &Principal,
    id: &str,
    mutating: bool,
    outcome: Result<(Value, &'static str), String>,
) -> Result<Value, String> {
    match outcome {
        Ok((value, state)) => {
            if mutating {
                finish(home, principal, id, state, &value);
            }
            Ok(value)
        }
        Err(error) => {
            if mutating {
                finish(
                    home,
                    principal,
                    id,
                    "failed",
                    &json!({"operationId": id, "status": "failed", "error": error}),
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "explicit_tests.rs"]
mod tests;

#[cfg(test)]
mod caller_receipt_tests {
    use super::*;
    use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
    #[test]
    fn retained_operation_status_and_recent_gate_authority_before_paths_and_limits() {
        let f = Fixture::new();
        let principal = Principal::from(1u64);
        let dir = record_dir(&f.home, &principal);
        std::fs::create_dir_all(&dir).unwrap();
        let mut ids = Vec::new();
        for team in ["parent", "child", "sibling"] {
            let id = uuid::Uuid::new_v4().to_string();
            let record = OperationRecord {
                operation_id: id.clone(),
                kind: "hydrate".into(),
                state: "ready".into(),
                target: json!({"kind":"local","codeRepoPath":"/work/parent"}),
                authority: Some(crate::commands::dashboard_launch::LaunchTeam {
                    org: "acme/ctx".into(),
                    team: team.into(),
                }),
                started_at_ms: 1,
                finished_at_ms: Some(2),
                result: Some(json!({"path":format!("{team}/content/private.md")})),
            };
            std::fs::write(
                dir.join(format!("{id}.json")),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            ids.push(id);
        }
        assert_eq!(
            status_for(&f.home, &principal, &ids[0], &f.scope),
            status_for(
                &f.home,
                &principal,
                &uuid::Uuid::new_v4().to_string(),
                &f.scope
            )
        );
        assert!(status_for(&f.home, &principal, &ids[0], &CallerScope::Human).is_some());
        for id in &ids[1..] {
            assert!(status_for(&f.home, &principal, id, &f.scope).is_some());
        }
        let recent = recent_for(&f.home, &principal, usize::MAX, &f.scope);
        assert!(!recent.iter().any(|record| record.operation_id == ids[0]));
        for id in &ids[1..] {
            assert!(recent.iter().any(|record| &record.operation_id == id));
        }
        assert!(f
            .scope
            .existing(&f.path("parent", "content/shared.md"))
            .is_ok());
    }
}
