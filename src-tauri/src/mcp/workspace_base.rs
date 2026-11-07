// SPDX-License-Identifier: MIT
//! `update_workspace_base` — move a workspace on a machine to the
//! newest commit of its base, explicitly. Re-opening a workspace never does
//! this; it stays transfer-free.
//!
//! The desktop builds an incremental bundle from the attached clone (only the
//! commits the machine lacks, with the machine's imports as prerequisites),
//! uploads it pipelined, and the machine fast-forwards the worktree when it is
//! clean. A dirty worktree is refused before anything is built or sent.
use super::remote_authority::RemoteAuthority;
use super::remote_workspace::{transfer, BundlePeer, PhaseTracker, PrepareProgress, ProgressSink};
use super::remote_workspace_bundle::{self as bundle, BuildSource};
use codesense_pty_core::remote_files::FileTarget;
use codesense_pty_core::remote_workspace::{
    GitBundle, InspectRepository, ManagedCopy, PrepareWorkspace, RepositoryKnowledge,
    UpdateWorkspaceBase, WorkspaceBaseUpdate,
};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Progress of a base update, keyed by the workspace handle.
pub const WORKSPACE_BASE_PROGRESS_EVENT: &str = "workspace-base-progress";

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkspaceBaseParams {
    /// The machine the workspace lives on. Optional when `storyId` or
    /// `workspaceId` names an open workspace that lives on a machine.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The managed workspace handle (`remote-workspace-…`) on the workspace record.
    #[serde(default)]
    pub workspace_handle: Option<String>,
    /// An open story workspace's story id.
    #[serde(default)]
    pub story_id: Option<String>,
    /// An open workspace's id.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The team whose open workspaces are searched, when more than one matches.
    #[serde(default)]
    pub team_key: Option<String>,
    /// When the new base tracks files CodeSense copied into the workspace,
    /// remove those copies and update (default: refuse and list them). Only
    /// unedited copies are ever removed; the repo's version replaces them.
    #[serde(default)]
    pub remove_managed_copies: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseUpdateReport {
    pub machine_id: String,
    pub workspace_handle: String,
    pub base_ref: String,
    /// `updated`, `upToDate`, `refusedDirty`, `refusedDiverged`,
    /// `refusedManagedOverlap`, `refused` or `unknown`.
    pub status: String,
    pub previous_commit: Option<String>,
    pub commit: Option<String>,
    /// Bytes of bundle sent to the machine (the delta).
    pub bundle_bytes: u64,
    pub managed_overlap: Vec<String>,
    /// Managed copies the new base tracks that were edited here.
    pub modified_managed: Vec<String>,
    /// Ignored files the new base tracks that CodeSense did not place.
    pub unmanaged_overlap: Vec<String>,
    pub dirty: Vec<String>,
    pub message: Option<String>,
}

/// The machine side of a base update.
pub(crate) trait BasePeer: BundlePeer {
    fn supports_base_update(&self) -> bool;
    fn inspect(&self, request: InspectRepository) -> Result<RepositoryKnowledge, String>;
    fn update_base(&self, request: UpdateWorkspaceBase) -> Result<WorkspaceBaseUpdate, String>;
}

impl BasePeer for crate::terminal::session_server::SessionServerClient {
    fn supports_base_update(&self) -> bool {
        self.supports_workspace_base_update()
    }
    fn inspect(&self, request: InspectRepository) -> Result<RepositoryKnowledge, String> {
        self.inspect_workspace_repository(request)
    }
    fn update_base(&self, request: UpdateWorkspaceBase) -> Result<WorkspaceBaseUpdate, String> {
        self.update_workspace_base(request)
    }
}

/// One update: the workspace, as prepared, and this update's own identity.
pub(crate) struct BasePlan {
    pub target: FileTarget,
    pub prepared: PrepareWorkspace,
    /// Names this update's bundle upload on both sides. Reused while an update
    /// to the same base commit is unfinished, so a retry resumes its upload.
    pub update_id: String,
    pub remove_managed_copies: bool,
    /// What CodeSense last wrote at each managed path of this workspace.
    pub managed: Vec<ManagedCopy>,
    /// Earlier update ids of this workspace that were abandoned (the base
    /// moved on): the machine reclaims what they staged.
    pub discard: Vec<String>,
}

impl BasePlan {
    fn carrier(&self, bundle: GitBundle) -> PrepareWorkspace {
        PrepareWorkspace {
            request_id: self.update_id.clone(),
            bundle,
            ..self.prepared.clone()
        }
    }

    fn update(&self, upload: PrepareWorkspace, precheck: bool) -> UpdateWorkspaceBase {
        UpdateWorkspaceBase {
            target: self.target.clone(),
            upload,
            precheck,
            remove_managed_copies: self.remove_managed_copies,
            managed: self.managed.clone(),
            discard_uploads: self.discard.clone(),
        }
    }
}

/// Builds the bundle of the base's newest commit, asking the machine (through
/// the probe it is given) what it already holds.
pub(crate) type BuildBundle<'a> = dyn FnOnce(
        &dyn Fn(&str) -> Result<RepositoryKnowledge, String>,
        &dyn Fn(PrepareProgress),
    ) -> Result<(PathBuf, GitBundle), String>
    + 'a;

/// What the caller keeps around an update.
pub(crate) struct UpdateHooks<'a> {
    /// The precheck passed and work starts: remember this update's id.
    pub begun: &'a dyn Fn(),
    /// The update reached a final answer: drop every local trace of it.
    pub settled: &'a dyn Fn(),
}

/// The update itself, over any machine `peer`. Returns the machine's answer
/// and the bundle bytes that were sent. An `Err` is an unknown outcome: the
/// update id and staged bytes are kept so a retry resumes.
pub(crate) fn update_with(
    peer: &dyn BasePeer,
    plan: &BasePlan,
    build: Box<BuildBundle<'_>>,
    validate: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(PrepareProgress),
    hooks: &UpdateHooks<'_>,
) -> Result<(WorkspaceBaseUpdate, u64), String> {
    if !peer.supports_base_update() {
        return Err(
            "This machine's terminal server is too old to update a workspace's base; update its terminal server"
                .into(),
        );
    }
    validate()?;
    // Refuse a dirty (or moved) worktree before anything is built or sent.
    let empty = GitBundle {
        size: 0,
        digest: format!("{:x}", Sha256::digest([])),
        commit: plan.prepared.bundle.commit.clone(),
    };
    let check = peer.update_base(plan.update(plan.carrier(empty), true))?;
    if check.status != "ready" {
        return Ok((check, 0));
    }
    (hooks.begun)();
    validate()?;
    let probe = |commit: &str| {
        peer.inspect(InspectRepository {
            owner_id: plan.prepared.owner_id,
            repository_key: plan.prepared.repository_key.clone(),
            clone_url: plan.prepared.clone_url.clone(),
            repo_path: plan.prepared.repo_path.clone(),
            commit: commit.into(),
        })
    };
    let (path, bundle) = build(&probe, progress)?;
    if Some(&bundle.commit) == check.previous_commit.as_ref() {
        (hooks.settled)();
        return Ok((
            WorkspaceBaseUpdate {
                status: "upToDate".into(),
                previous_commit: check.previous_commit.clone(),
                commit: check.previous_commit,
                ..Default::default()
            },
            0,
        ));
    }
    let carrier = plan.carrier(bundle.clone());
    transfer(peer, &carrier, &path, validate, progress)?;
    validate()?;
    progress(PrepareProgress::UpdatingWorktree);
    let answer = peer.update_base(plan.update(carrier, false))?;
    // Any answer is final; the machine reclaimed its staging with it.
    (hooks.settled)();
    Ok((answer, bundle.size))
}

/// Progress of an update, as the UI hears it: keyed by the resolved handle.
pub(crate) fn base_progress_sink(
    handle: String,
    emit: std::sync::Arc<dyn Fn(BaseProgressPayload) + Send + Sync>,
) -> ProgressSink {
    std::sync::Arc::new(move |progress| {
        emit(BaseProgressPayload {
            workspace_handle: handle.clone(),
            progress,
        })
    })
}

/// [`update_with`] with its progress on the workspace's channel and a failure
/// event for any answer but `updated` / `upToDate`.
pub(crate) fn run_update(
    peer: &dyn BasePeer,
    plan: &BasePlan,
    build: Box<BuildBundle<'_>>,
    validate: &dyn Fn() -> Result<(), String>,
    emit: std::sync::Arc<dyn Fn(BaseProgressPayload) + Send + Sync>,
    hooks: &UpdateHooks<'_>,
) -> (WorkspaceBaseUpdate, u64) {
    let tracker = PhaseTracker::new(base_progress_sink(plan.target.handle.clone(), emit));
    let sink = tracker.sink();
    let (answer, bytes) = match update_with(peer, plan, build, validate, &*sink, hooks) {
        Ok(done) => done,
        Err(error) => (
            WorkspaceBaseUpdate {
                status: "unknown".into(),
                message: Some(error),
                ..Default::default()
            },
            0,
        ),
    };
    if !matches!(answer.status.as_str(), "updated" | "upToDate") {
        tracker.settle(
            codesense_pty_core::remote_workspace::PreparedWorkspace::failure(
                &answer.status,
                answer
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("the machine answered {}", answer.status)),
            ),
        );
    }
    (answer, bytes)
}

/// The update id of an unfinished update per workspace, so a retry to the
/// same base commit resumes its upload instead of starting another.
pub(crate) struct UpdateIds {
    pub dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct UpdateRecord {
    target: String,
    update_id: String,
}

impl UpdateIds {
    fn path(&self, handle: &str) -> PathBuf {
        self.dir
            .join(format!("{:x}.json", Sha256::digest(handle.as_bytes())))
    }

    /// The id an unfinished update to `target` used, or a fresh one.
    #[cfg(test)]
    pub(crate) fn for_target(&self, handle: &str, target: &str) -> String {
        self.resume(handle, target).0
    }

    /// The id to use for `target`, and the id of an unfinished update to an
    /// older target that this one abandons.
    pub(crate) fn resume(&self, handle: &str, target: &str) -> (String, Option<String>) {
        let record = std::fs::read(self.path(handle))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<UpdateRecord>(&bytes).ok())
            .filter(|record| uuid::Uuid::parse_str(&record.update_id).is_ok());
        match record {
            Some(record) if record.target == target => (record.update_id, None),
            Some(record) => (uuid::Uuid::new_v4().to_string(), Some(record.update_id)),
            None => (uuid::Uuid::new_v4().to_string(), None),
        }
    }

    pub(crate) fn keep(&self, handle: &str, target: &str, update_id: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|_| "update record directory unavailable")?;
        let bytes = serde_json::to_vec(&UpdateRecord {
            target: target.into(),
            update_id: update_id.into(),
        })
        .map_err(|_| "invalid update record")?;
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)
            .map_err(|_| "update record staging unavailable")?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|_| "update record not written")?;
        file.persist(self.path(handle))
            .map_err(|_| "update record cannot commit")?;
        Ok(())
    }

    pub(crate) fn forget(&self, handle: &str) {
        let _ = std::fs::remove_file(self.path(handle));
    }
}

/// The commit `base_ref` names in the attached clone now: the base the update
/// moves to.
fn local_base_commit(seed: &std::path::Path, base_ref: &str) -> Option<String> {
    let repository = git2::Repository::open(seed).ok()?;
    let name = if base_ref.starts_with("refs/") {
        base_ref.to_string()
    } else {
        format!("refs/heads/{base_ref}")
    };
    let commit = repository
        .revparse_single(&name)
        .ok()?
        .peel_to_commit()
        .ok()?;
    Some(commit.id().to_string())
}

/// The workspace's managed records, as the desktop last wrote them.
fn managed_records(
    resolved: &super::remote_workspace_target::ResolvedWorkspace,
    ctx: &crate::content_engine::ResolverContext,
    identity: &crate::content_engine::destination::RemoteDestinationIdentity,
) -> Result<Vec<ManagedCopy>, String> {
    use crate::content_engine::destination::DestinationReference;
    let Some(store) =
        crate::content_engine::lockfile::open_sync_state_store(&resolved.context.codesense_home)
    else {
        return Ok(Vec::new());
    };
    let records = store
        .read_remote_target(ctx, &resolved.source_repo, identity)
        .map_err(|error| error.to_string())?
        .map(|lock| lock.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| match entry.dest_path {
            DestinationReference::Remote { relative, .. } => Some(ManagedCopy {
                path: relative.as_str().to_string(),
                blake3: blake3::Hash::from(entry.last_materialized_hash)
                    .to_hex()
                    .to_string(),
            }),
            _ => None,
        })
        .collect();
    Ok(records)
}

/// The MCP tool and Tauri command: locate the workspace, check it is the
/// caller's team's, then update it while holding its destination lock.
/// Progress and failure go to [`WORKSPACE_BASE_PROGRESS_EVENT`], keyed by the
/// resolved workspace handle.
pub(crate) async fn update(
    app: tauri::AppHandle,
    params: UpdateWorkspaceBaseParams,
    caller: Option<RemoteAuthority>,
) -> Result<BaseUpdateReport, String> {
    use tauri::{Emitter, Manager};
    let located = super::remote_workspace_fetch::RemoteWorkspaceFetchParams {
        machine_id: params.machine_id.clone(),
        workspace_handle: params.workspace_handle.clone(),
        story_id: params.story_id.clone(),
        workspace_id: params.workspace_id.clone(),
        team_key: params.team_key.clone(),
    };
    let (machine_id, handle) = super::remote_workspace_fetch::locate(&app, &located)?;
    let ambient = match &caller {
        Some(caller) => caller
            .context()
            .await
            .map_err(|error| error.message.to_string())?,
        None => {
            let (codesense_home, user_id) =
                crate::commands::route_to_context::resolve_ambient(&app).await?;
            crate::commands::route_to_context::ActiveContext {
                codesense_home,
                user_id,
                org_slug: String::new(),
                team_name: None,
            }
        }
    };
    let resolved = super::remote_workspace_target::resolve_selected(
        app.clone(),
        ambient,
        Some(&machine_id),
        &handle,
        caller.clone(),
        false,
    )
    .await?;
    let handle = resolved.target.handle.clone();
    let identity = crate::content_engine::destination::RemoteDestinationIdentity {
        owner_id: resolved.context.user_id,
        machine_id: resolved.machine_id.clone(),
        server_id: resolved.server_id.clone(),
        workspace_handle: handle.clone(),
    };
    let resolver = crate::commands::content_engine::derive_resolver_context(
        resolved.context.codesense_home.clone(),
        resolved.context.user_id,
        resolved.context.org_slug.clone(),
        resolved.context.team_name.clone(),
    )
    .map_err(|error| error.to_string())?;
    // One writer per destination, from the precheck through the final
    // answer; Step 2 and explicit operations wait for it.
    let coordinator = app
        .try_state::<std::sync::Arc<crate::content_engine::sync::wave::WaveCoordinator>>()
        .ok_or("the workspace coordinator is unavailable")?
        .inner()
        .clone();
    let guard = coordinator
        .acquire_remote_destination_lock(&resolver, &resolved.source_repo, &identity)
        .await;
    // Read under the lock: no Step 2 or operation changes them meanwhile.
    let managed = managed_records(&resolved, &resolver, &identity)?;
    let seed = bundle::local_seed(&resolved.context, &resolved.source_repo)?;
    let url = resolved
        .source_repo
        .cloud_https_remote()
        .map_err(|error| error.to_string())?;
    let root = crate::context_repo::path_to_user_home(
        &resolved.context.codesense_home,
        &resolved.context.principal(),
    )
    .join("remote-workspace-intents-v1");
    let ids = UpdateIds {
        dir: root.join("base-updates"),
    };
    let base_ref = resolved.base_ref.clone();
    let target = seed
        .as_deref()
        .and_then(|seed| local_base_commit(seed, &base_ref));
    let (update_id, abandoned) = match target.as_deref() {
        Some(target) => ids.resume(&handle, target),
        None => (uuid::Uuid::new_v4().to_string(), None),
    };
    // The base moved on from an unfinished update: drop its local bundle and
    // have the machine reclaim what it staged.
    if let Some(abandoned) = &abandoned {
        let _ = bundle::discard(
            &root,
            &format!("base-{abandoned}"),
            &resolved.prepared_request.repository_key,
        );
    }
    let plan = BasePlan {
        target: resolved.target.clone(),
        prepared: resolved.prepared_request.clone(),
        update_id,
        remove_managed_copies: params.remove_managed_copies,
        managed,
        discard: abandoned.into_iter().collect(),
    };
    let client = resolved.client.clone();
    let valid = resolved.validate.clone();
    let repository_key = plan.prepared.repository_key.clone();
    let emitter = app.clone();
    let emit: std::sync::Arc<dyn Fn(BaseProgressPayload) + Send + Sync> =
        std::sync::Arc::new(move |payload| {
            let _ = emitter.emit(WORKSPACE_BASE_PROGRESS_EVENT, payload);
        });
    let worker_handle = handle.clone();
    let home = resolved.context.codesense_home.clone();
    let source_repo = resolved.source_repo.clone();
    let (answer, bundle_bytes) =
        crate::commands::await_blocking("update_workspace_base", move || {
            let _guard = guard;
            let key = format!("base-{}", plan.update_id);
            let intent = format!("{:x}", Sha256::digest(key.as_bytes()));
            let alive = valid.clone();
            let (build_root, build_key, build_ref, build_repo) = (
                root.clone(),
                key.clone(),
                base_ref.clone(),
                repository_key.clone(),
            );
            let build: Box<BuildBundle<'_>> = Box::new(move |probe, progress| {
                bundle::build_reporting(
                    &build_root,
                    &build_key,
                    &intent,
                    BuildSource {
                        url,
                        repository_key: build_repo,
                        base_ref: build_ref,
                        seed,
                    },
                    move || alive().is_ok(),
                    probe,
                    progress,
                )
            });
            let begun = || {
                if let Some(target) = &target {
                    let _ = ids.keep(&worker_handle, target, &plan.update_id);
                }
            };
            let settled = || {
                let _ = bundle::discard(&root, &key, &repository_key);
                ids.forget(&worker_handle);
            };
            let done = run_update(
                client.as_ref(),
                &plan,
                build,
                valid.as_ref(),
                emit,
                &UpdateHooks {
                    begun: &begun,
                    settled: &settled,
                },
            );
            // The repo tracks the replaced copies now: they are no longer
            // managed, so their records go (still under the destination lock).
            if done.0.status == "updated" && !done.0.managed_overlap.is_empty() {
                if let Some(store) = crate::content_engine::lockfile::open_sync_state_store(&home) {
                    if let Err(error) = store.forget_remote_entries(
                        &resolver,
                        &source_repo,
                        &identity,
                        &done.0.managed_overlap,
                    ) {
                        log::warn!("update_workspace_base: managed records not trimmed: {error}");
                    }
                }
            }
            Ok::<_, String>(done)
        })
        .await?;
    Ok(BaseUpdateReport {
        machine_id: resolved.machine_id,
        workspace_handle: handle,
        base_ref: resolved.base_ref,
        status: answer.status,
        previous_commit: answer.previous_commit,
        commit: answer.commit,
        bundle_bytes,
        managed_overlap: answer.managed_overlap,
        modified_managed: answer.modified_managed,
        unmanaged_overlap: answer.unmanaged_overlap,
        dirty: answer.dirty,
        message: answer.message,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaseProgressPayload {
    pub workspace_handle: String,
    #[serde(flatten)]
    pub progress: PrepareProgress,
}

/// The Tauri command behind the workspace's "Update to latest base".
#[tauri::command]
pub async fn update_workspace_base(
    app: tauri::AppHandle,
    request: UpdateWorkspaceBaseParams,
) -> Result<BaseUpdateReport, String> {
    update(app, request, None).await
}

#[cfg(test)]
#[path = "workspace_base_tests.rs"]
mod tests;

#[cfg(test)]
mod wiring {
    /// The destination lock is taken before the managed records
    /// are read and is held by the blocking worker until the final answer.
    #[test]
    fn xs9_update_holds_the_destination_lock_across_the_whole_update() {
        let source = include_str!("workspace_base.rs");
        let body = source
            .split("pub(crate) async fn update(")
            .nth(1)
            .unwrap()
            .split("pub(crate) struct BaseProgressPayload")
            .next()
            .unwrap();
        let at = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("update() lacks {needle}"))
        };
        let blocking = "await_blocking(\"update_workspace_base\"";
        assert!(at(".acquire_remote_destination_lock(") < at("managed_records(&resolved"));
        assert!(at("managed_records(&resolved") < at(blocking));
        let worker = &body[at(blocking)..];
        let find = |needle: &str| {
            worker
                .find(needle)
                .unwrap_or_else(|| panic!("worker lacks {needle}"))
        };
        assert!(find("let _guard = guard;") < find("run_update("));
        assert!(find("run_update(") < find("forget_remote_entries("));
    }
}
