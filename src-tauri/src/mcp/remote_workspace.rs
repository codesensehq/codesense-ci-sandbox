// SPDX-License-Identifier: MIT
//! Explicit-team remote workspace preparation; remote paths never enter desktop Git.
use crate::commands::{
    machines, pty_backend,
    route_to_context::{self, ActiveContext, ContextResolution},
};
use crate::domain::SourceRepoSlug;
use codesense_pty_core::remote_workspace::pipeline::{self, ChunkLink};
use codesense_pty_core::remote_workspace::{
    chunk_range_in, BundleChunk, BundleUploadStatus, PrepareWorkspace, PreparedWorkspace,
    BUNDLE_CHUNK_BYTES, MIN_PIPELINED_CHUNK_BYTES, UPLOAD_WINDOW,
};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tauri::Manager;

static RECEIPTS: Mutex<()> = Mutex::new(());
// The protocol accepts 1 MiB raw chunks, but base64 plus the request envelope
// must fit the session writer's 1 MiB encoded-byte budget. Keep receiver
// compatibility and backpressure unchanged; only bound this sender's payload.
const OUTGOING_BUNDLE_CHUNK_BYTES: usize = 256 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareRemoteWorkspaceParams {
    pub request_id: String,
    pub machine_id: String,
    pub team_key: String,
    pub workspace_id: String,
    pub source_repo: SourceRepoSlug,
    pub base_ref: String,
    pub branch: String,
    pub repo_path: String,
    pub worktree_path: String,
}
/// A Step 1 phase, named in progress and in its failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Step1Phase {
    /// Checking the request before any phase ran.
    Starting,
    Fetching,
    Bundling,
    Copying,
    Verifying,
    CreatingWorktree,
    /// `update_workspace_base`: the machine fast-forwards the worktree.
    UpdatingWorktree,
    /// This Mac only: the repo's `worktree.yaml` setup.
    RunningSetup,
}

/// How far a preparation has got, for a person watching it (every
/// Step 1 phase, with determinate counts where Git or the transfer has them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub(crate) enum PrepareProgress {
    /// Fetching the base's committed objects from the attached clone into the
    /// bundle cache: `objects` of `total` when Git reports them.
    Fetching {
        objects: Option<u64>,
        total: Option<u64>,
    },
    /// Packing the committed objects the machine lacks (`objects` once known).
    Bundling { objects: Option<u64> },
    /// Sending them: `sent` of `total` bytes.
    Copying { sent: u64, total: u64 },
    /// The machine flushes the upload once and checks its digest.
    Verifying,
    /// The machine is creating the repository and worktree.
    CreatingWorktree,
    /// The machine is fast-forwarding the worktree to the new base.
    UpdatingWorktree,
    /// A phase failed; the operation stopped there.
    #[serde(rename_all = "camelCase")]
    Failed {
        failed_phase: Step1Phase,
        message: String,
    },
}

impl PrepareProgress {
    pub(crate) fn phase(&self) -> Option<Step1Phase> {
        Some(match self {
            Self::Fetching { .. } => Step1Phase::Fetching,
            Self::Bundling { .. } => Step1Phase::Bundling,
            Self::Copying { .. } => Step1Phase::Copying,
            Self::Verifying => Step1Phase::Verifying,
            Self::CreatingWorktree => Step1Phase::CreatingWorktree,
            Self::UpdatingWorktree => Step1Phase::UpdatingWorktree,
            Self::Failed { .. } => return None,
        })
    }
}

pub(crate) type ProgressSink = std::sync::Arc<dyn Fn(PrepareProgress) + Send + Sync>;

/// A sink that remembers the last phase it reported, so a failure names it.
pub(crate) struct PhaseTracker {
    sink: ProgressSink,
    last: std::sync::Arc<Mutex<Step1Phase>>,
}

impl PhaseTracker {
    pub(crate) fn new(sink: ProgressSink) -> Self {
        Self {
            sink,
            last: std::sync::Arc::new(Mutex::new(Step1Phase::Starting)),
        }
    }

    /// The sink to report phases through.
    pub(crate) fn sink(&self) -> ProgressSink {
        let (sink, last) = (self.sink.clone(), self.last.clone());
        std::sync::Arc::new(move |progress: PrepareProgress| {
            if let Some(phase) = progress.phase() {
                *last.lock().unwrap_or_else(|p| p.into_inner()) = phase;
            }
            sink(progress)
        })
    }

    pub(crate) fn last(&self) -> Step1Phase {
        *self.last.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A failed preparation reports which phase failed, never only a log line.
    pub(crate) fn settle(&self, result: PreparedWorkspace) -> PreparedWorkspace {
        if !matches!(result.status.as_str(), "prepared" | "alreadyPrepared") {
            (self.sink)(PrepareProgress::Failed {
                failed_phase: self.last(),
                message: result
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("the machine answered {}", result.status)),
            });
        }
        result
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct IntentReceipt {
    pub(super) params: PrepareRemoteWorkspaceParams,
    pub(super) request: PrepareWorkspace,
    pub(super) server_id: String,
    /// The kind of the workspace it was prepared for (`story`,
    /// `generic`, ...), as the viewer had it then. Older receipts have none;
    /// a reader falls back to where the worktree lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<String>,
}

/// The kind of the open workspace `params` names, as the viewer has it.
fn group_kind(app: &tauri::AppHandle, params: &PrepareRemoteWorkspaceParams) -> Option<String> {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    viewer
        .get("workspaces")?
        .get(&params.team_key)?
        .as_array()?
        .iter()
        .find(|row| row.get("id").and_then(|v| v.as_str()) == Some(params.workspace_id.as_str()))?
        .get("kind")?
        .as_str()
        .map(str::to_string)
}

pub(super) fn group(
    app: &tauri::AppHandle,
    params: &PrepareRemoteWorkspaceParams,
) -> Result<(), String> {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    if viewer
        .get("workspaces")
        .and_then(|v| v.get(&params.team_key))
        .and_then(|v| v.as_array())
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.get("id").and_then(|v| v.as_str()) == Some(params.workspace_id.as_str())
            })
        })
    {
        Ok(())
    } else {
        Err("workspace is not open in the selected team".into())
    }
}
pub(super) fn bound_team(ctx: &ActiveContext, team_key: &str) -> Result<(), String> {
    let (org, team) = crate::commands::pty_layout::parse_team_key(team_key);
    if org != ctx.org_slug || team != ctx.team_name.as_deref() || team.is_none() {
        return Err("request does not belong to the bound team".into());
    }
    Ok(())
}
fn same_repository(configured: &SourceRepoSlug, requested: &SourceRepoSlug) -> bool {
    configured.remote_key() == requested.remote_key() && configured.provider == requested.provider
}
pub(super) fn configured(
    ctx: &ActiveContext,
    params: &PrepareRemoteWorkspaceParams,
) -> Result<(), String> {
    configured_repo(ctx, &params.source_repo)
}

/// Whether `repo` is one of the team's (`ctx`) source repos.
pub(crate) fn configured_repo(ctx: &ActiveContext, repo: &SourceRepoSlug) -> Result<(), String> {
    let root = crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?;
    let org = crate::domain::load_org(ctx.org_slug.clone(), &root, None)
        .org
        .ok_or("team configuration unavailable")?;
    if org
        .teams
        .iter()
        .find(|t| Some(t.name.as_str()) == ctx.team_name.as_deref())
        .is_some_and(|t| t.source_repos.iter().any(|r| same_repository(r, repo)))
    {
        Ok(())
    } else {
        Err("source repository is not configured for the selected team".into())
    }
}

pub(crate) async fn prepare(
    app: tauri::AppHandle,
    params: PrepareRemoteWorkspaceParams,
) -> PreparedWorkspace {
    let (org, team) = crate::commands::pty_layout::parse_team_key(&params.team_key);
    let context = match team {
        Some(team) => route_to_context::derive_context_for(&app, org, team).await,
        None => return PreparedWorkspace::failure("refused", "explicit team required"),
    };
    let ctx = match context {
        Ok(ContextResolution::Ok(ctx)) => ctx,
        _ => return PreparedWorkspace::failure("refused", "selected team is unavailable"),
    };
    prepare_bound(app, params, ctx, None).await
}

/// Reverse MCP calls this with its captured context, never mutable UI selection.
pub(crate) async fn prepare_bound(
    app: tauri::AppHandle,
    params: PrepareRemoteWorkspaceParams,
    ctx: ActiveContext,
    caller: Option<super::remote_authority::RemoteAuthority>,
) -> PreparedWorkspace {
    prepare_reporting(app, params, ctx, caller, std::sync::Arc::new(|_| {})).await
}

/// [`prepare_bound`], telling `progress` how far it has got.
pub(crate) async fn prepare_reporting(
    app: tauri::AppHandle,
    params: PrepareRemoteWorkspaceParams,
    ctx: ActiveContext,
    caller: Option<super::remote_authority::RemoteAuthority>,
    progress: ProgressSink,
) -> PreparedWorkspace {
    report_step1(progress, |sink, consumed| async move {
        prepare_inner(app, params, ctx, &consumed, caller, sink).await
    })
    .await
}

/// A remote Step 1 as [`prepare_reporting`] runs it: `run` reports its phases
/// through the tracked sink it is given; an error becomes `refused` (nothing
/// consumed yet) or `unknown`, and any result but prepared / alreadyPrepared
/// emits a failure event naming the phase it stopped in.
pub(crate) async fn report_step1<F, Fut>(progress: ProgressSink, run: F) -> PreparedWorkspace
where
    F: FnOnce(ProgressSink, std::sync::Arc<AtomicBool>) -> Fut,
    Fut: std::future::Future<Output = Result<PreparedWorkspace, String>>,
{
    let consumed = std::sync::Arc::new(AtomicBool::new(false));
    let tracker = PhaseTracker::new(progress);
    let result = match run(tracker.sink(), consumed.clone()).await {
        Ok(result) => result,
        Err(error) => PreparedWorkspace::failure(
            if consumed.load(Ordering::SeqCst) {
                "unknown"
            } else {
                "refused"
            },
            error,
        ),
    };
    tracker.settle(result)
}
pub(super) fn validate_caller(
    caller: &Option<super::remote_authority::RemoteAuthority>,
) -> Result<(), String> {
    if let Some(caller) = caller {
        tauri::async_runtime::block_on(caller.validate()).map_err(|_| {
            "originating remote caller authority retired; reconcile original request".to_string()
        })?;
    }
    Ok(())
}

async fn prepare_inner(
    app: tauri::AppHandle,
    params: PrepareRemoteWorkspaceParams,
    ctx: ActiveContext,
    consumed: &AtomicBool,
    caller: Option<super::remote_authority::RemoteAuthority>,
    progress: ProgressSink,
) -> Result<PreparedWorkspace, String> {
    if let Some(caller) = &caller {
        caller
            .validate()
            .await
            .map_err(|_| "originating remote caller unavailable")?;
    }
    bound_team(&ctx, &params.team_key)?;
    uuid::Uuid::parse_str(&params.request_id).map_err(|_| "requestId must be a UUID")?;
    let root = crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
        .join("remote-workspace-intents-v1");
    let key = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&params.machine_id, &params.team_key, &params.request_id))
                .map_err(|_| "invalid intent")?
        )
    );
    let path = root.join(format!("{key}.json"));
    let read_path = path.clone();
    let prior_result: Result<Option<IntentReceipt>, String> =
        crate::commands::await_blocking("read_workspace_intent", move || {
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&read_path)
                .and_then(|file| {
                    if !file.metadata()?.is_file() {
                        return Err(std::io::Error::other(
                            "workspace receipt is not a regular file",
                        ));
                    }
                    let mut bytes = Vec::new();
                    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                    Ok(bytes)
                }) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|_| "workspace intent requires reconciliation".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err("workspace intent unavailable".into()),
            }
        })
        .await;
    consumed.store(
        prior_result.as_ref().map_or(true, |prior| prior.is_some()),
        Ordering::SeqCst,
    );
    let prior = prior_result?;
    configured(&ctx, &params)?;
    group(&app, &params)?;
    machines::authorize_terminal_machine(&app, &params.machine_id, &params.team_key).await?;
    let backend = pty_backend::resolve_spawn_backend(
        &app,
        &params.machine_id,
        "remote-workspace-preparation",
    )?;
    backend.revalidate(&app)?;
    let client = backend
        .client()
        .filter(|_| backend.is_remote())
        .ok_or("admitted remote connection unavailable")?;
    let lease = client
        .private_lease()
        .ok_or("private authority unavailable")?;
    lease.validate()?;
    if lease.owner_id() != ctx.user_id {
        return Err("private owner changed".into());
    }
    let bundle_path = root.join(format!("bundle-{key}")).join("objects.bundle");
    let request = if let Some(prior) = prior {
        if prior.params != params || prior.server_id != backend.server_id() {
            return Err("request identity belongs to another intent or server".into());
        }
        prior.request
    } else {
        let url = params
            .source_repo
            .cloud_https_remote()
            .map_err(|error| error.to_string())?;
        lease.validate()?;
        backend.revalidate(&app)?;
        let bundle_intent = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&params).map_err(|_| "invalid bundle intent")?)
        );
        let bundle_root = root.clone();
        let bundle_key = key.clone();
        let base_ref = params.base_ref.clone();
        let clone_url = url.clone();
        let bundle_lease = lease.clone();
        let seed = super::remote_workspace_bundle::local_seed(&ctx, &params.source_repo)?;
        let repository_key = params.source_repo.remote_key();
        let probe_client = client.clone();
        let probe_request = codesense_pty_core::remote_workspace::InspectRepository {
            owner_id: ctx.user_id,
            repository_key: repository_key.clone(),
            clone_url: clone_url.clone(),
            repo_path: params.repo_path.clone(),
            commit: String::new(),
        };
        let bundle_app = app.clone();
        let bundle_backend = backend.clone();
        let bundle_params = params.clone();
        let bundle_ctx = ctx.clone();
        let bundle_caller = caller.clone();
        let bundle_progress = progress.clone();
        let (_, bundle) = crate::commands::await_blocking("fetch_workspace_bundle", move || {
            super::remote_workspace_bundle::build_reporting(
                &bundle_root,
                &bundle_key,
                &bundle_intent,
                super::remote_workspace_bundle::BuildSource {
                    url: clone_url,
                    repository_key,
                    base_ref,
                    seed,
                },
                move || {
                    validate_caller(&bundle_caller).is_ok()
                        && bundle_lease.validate().is_ok()
                        && bundle_backend.revalidate(&bundle_app).is_ok()
                        && group(&bundle_app, &bundle_params).is_ok()
                        && configured(&bundle_ctx, &bundle_params).is_ok()
                },
                move |commit| {
                    let mut request = probe_request.clone();
                    request.commit = commit.into();
                    probe_client.inspect_workspace_repository(request)
                },
                &*bundle_progress,
            )
        })
        .await?;
        lease.validate()?;
        backend.revalidate(&app)?;
        let request = PrepareWorkspace {
            request_id: params.request_id.clone(),
            owner_id: ctx.user_id,
            origin_key: format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&(
                        ctx.user_id,
                        &params.machine_id,
                        backend.server_id(),
                        &params.team_key,
                        &params.workspace_id
                    ))
                    .map_err(|_| "invalid origin")?
                )
            ),
            repository_key: params.source_repo.remote_key(),
            clone_url: url,
            bundle,
            repo_path: params.repo_path.clone(),
            worktree_path: params.worktree_path.clone(),
            base_ref: params.base_ref.clone(),
            branch: params.branch.clone(),
        };
        let receipt = IntentReceipt {
            params: params.clone(),
            request: request.clone(),
            server_id: backend.server_id().into(),
            kind: group_kind(&app, &params),
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|_| "invalid workspace receipt")?;
        if bytes.len() > 1024 * 1024 {
            return Err("workspace intent exceeds bounded limit".into());
        }
        crate::commands::await_blocking("reserve_workspace_intent", move || {
            let _serial = RECEIPTS
                .lock()
                .map_err(|_| "workspace receipt lock unavailable")?;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&root)
                .map_err(|_| "workspace receipt root unavailable")?;
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&path)
                        .map_err(|_| "workspace receipt unavailable")?;
                    if !file
                        .metadata()
                        .map_err(|_| "workspace receipt unavailable")?
                        .is_file()
                    {
                        return Err("workspace receipt is not regular".into());
                    }
                    let mut current = Vec::new();
                    file.take(1024 * 1024 + 1)
                        .read_to_end(&mut current)
                        .map_err(|_| "workspace receipt unavailable")?;
                    return if current == bytes {
                        Ok(())
                    } else {
                        Err("request identity already reserved".into())
                    };
                }
                Err(_) => return Err("cannot reserve workspace intent".into()),
            };
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| "workspace receipt incomplete")?;
            std::fs::File::open(root)
                .and_then(|f| f.sync_all())
                .map_err(|_| "workspace receipt not durable".into())
        })
        .await?;
        consumed.store(true, Ordering::SeqCst);
        request
    };
    machines::authorize_terminal_machine(&app, &params.machine_id, &params.team_key).await?;
    configured(&ctx, &params)?;
    group(&app, &params)?;
    lease.validate()?;
    backend.revalidate(&app)?;
    crate::commands::await_blocking("prepare_remote_workspace", move || {
        dispatch_reporting(
            &client,
            request,
            &bundle_path,
            || {
                validate_caller(&caller)?;
                backend.revalidate(&app)?;
                group(&app, &params)?;
                configured(&ctx, &params)
            },
            &*progress,
        )
    })
    .await
}

/// [`dispatch_reporting`] without progress, as the owned-SSH tests drive it.
#[cfg(test)]
pub(crate) fn dispatch(
    client: &crate::terminal::session_server::SessionServerClient,
    request: PrepareWorkspace,
    bundle_path: &std::path::Path,
    validate: impl Fn() -> Result<(), String>,
) -> Result<PreparedWorkspace, String> {
    dispatch_reporting(client, request, bundle_path, validate, &|_| {})
}

/// The machine side of a preparation, as [`dispatch_with`] drives it.
pub(crate) trait BundlePeer {
    fn reconcile(&self, request: PrepareWorkspace) -> Result<Option<PreparedWorkspace>, String>;
    /// The machine takes indexed chunks, a bitmap resume and a seal.
    fn pipelined(&self) -> bool;
    fn upload_status(&self, request: PrepareWorkspace) -> Result<BundleUploadStatus, String>;
    fn chunk_pipe(&self, request: PrepareWorkspace) -> Box<dyn ChunkLink + '_>;
    /// An older machine: one chunk at a time from its single offset.
    fn upload_chunk(&self, chunk: BundleChunk) -> Result<u64, String>;
    fn seal(&self, request: PrepareWorkspace) -> Result<(), String>;
    fn prepare(&self, request: PrepareWorkspace) -> PreparedWorkspace;
}

impl BundlePeer for crate::terminal::session_server::SessionServerClient {
    fn reconcile(&self, request: PrepareWorkspace) -> Result<Option<PreparedWorkspace>, String> {
        self.reconcile_remote_workspace(request)
    }
    fn pipelined(&self) -> bool {
        self.supports_bundle_pipeline()
    }
    fn upload_status(&self, request: PrepareWorkspace) -> Result<BundleUploadStatus, String> {
        self.bundle_upload_status(request)
    }
    fn chunk_pipe(&self, request: PrepareWorkspace) -> Box<dyn ChunkLink + '_> {
        Box::new(self.bundle_chunk_pipe(request))
    }
    fn upload_chunk(&self, chunk: BundleChunk) -> Result<u64, String> {
        self.upload_workspace_bundle(chunk)
    }
    fn seal(&self, request: PrepareWorkspace) -> Result<(), String> {
        self.seal_workspace_bundle(request)
    }
    fn prepare(&self, request: PrepareWorkspace) -> PreparedWorkspace {
        self.prepare_remote_workspace(request)
    }
}

/// Shared production transfer/reconciliation seam, also exercised over owned
/// SSH, telling `progress` about each step. A repeat of a settled request
/// reconciles and reports nothing: no bundle is sent again.
pub(crate) fn dispatch_reporting(
    client: &crate::terminal::session_server::SessionServerClient,
    request: PrepareWorkspace,
    bundle_path: &std::path::Path,
    validate: impl Fn() -> Result<(), String>,
    progress: &dyn Fn(PrepareProgress),
) -> Result<PreparedWorkspace, String> {
    dispatch_with(client, request, bundle_path, &validate, progress)
}

/// Send `bundle_path` to the machine (resuming whatever it already holds)
/// and prepare the workspace there.
pub(crate) fn dispatch_with(
    peer: &dyn BundlePeer,
    request: PrepareWorkspace,
    bundle_path: &std::path::Path,
    validate: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(PrepareProgress),
) -> Result<PreparedWorkspace, String> {
    validate()?;
    if let Some(mut result) = peer.reconcile(request.clone())? {
        validate()?;
        if let Err(error) = super::remote_workspace_bundle::settle(bundle_path, &request.bundle) {
            result.message = Some(error);
        }
        return Ok(result);
    }
    if let Err(error) = transfer(peer, &request, bundle_path, validate, progress) {
        return Ok(PreparedWorkspace::failure("unknown", error));
    }
    validate()?;
    progress(PrepareProgress::CreatingWorktree);
    let mut result = peer.prepare(request.clone());
    if validate().is_err() {
        return Ok(PreparedWorkspace::failure(
            "unknown",
            "authority changed; reconcile original request",
        ));
    }
    if matches!(result.status.as_str(), "prepared" | "alreadyPrepared") {
        if let Err(error) = super::remote_workspace_bundle::settle(bundle_path, &request.bundle) {
            result.message = Some(error);
        }
    }
    Ok(result)
}

/// Copy the bundle the machine lacks: pipelined (8 chunks in flight, resumed
/// from the machine's bitmap) and sealed once, or, for an older machine, one
/// chunk at a time from its offset.
pub(crate) fn transfer(
    peer: &dyn BundlePeer,
    request: &PrepareWorkspace,
    bundle_path: &std::path::Path,
    validate: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(PrepareProgress),
) -> Result<(), String> {
    if request.bundle.size == 0 {
        return Ok(());
    }
    let (size, digest) = super::remote_workspace_bundle::inspect(bundle_path)?;
    if size != request.bundle.size || digest != request.bundle.digest {
        return Err("original bundle identity changed".into());
    }
    validate()?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(bundle_path)
        .map_err(|_| "original bundle unavailable")?;
    if peer.pipelined() {
        let status = peer.upload_status(request.clone())?;
        // The machine names its chunk size; any within the protocol's bounds
        // is followed, so the two sides never disagree on a chunk's range.
        let chunk = status.chunk_bytes as usize;
        if !(MIN_PIPELINED_CHUNK_BYTES..=BUNDLE_CHUNK_BYTES).contains(&chunk)
            || !status.received.fits_chunks(size, chunk)
        {
            return Err("the machine's upload status does not match this bundle".into());
        }
        let mut received = status.received;
        progress(PrepareProgress::Copying {
            sent: received.held_bytes(size, chunk),
            total: size,
        });
        if !status.sealed && !received.is_complete() {
            let mut pipe = peer.chunk_pipe(request.clone());
            let mut read = |index: u32| -> Result<Vec<u8>, String> {
                use std::os::unix::fs::FileExt;
                validate()?;
                let (offset, len) =
                    chunk_range_in(size, index, chunk).ok_or("bundle chunk out of range")?;
                let mut bytes = vec![0; len];
                file.read_exact_at(&mut bytes, offset)
                    .map_err(|_| "bundle read failed")?;
                Ok(bytes)
            };
            pipeline::send_missing(
                pipe.as_mut(),
                &mut received,
                size,
                chunk,
                UPLOAD_WINDOW,
                &mut read,
                &mut |sent, total| progress(PrepareProgress::Copying { sent, total }),
            )?;
        }
        validate()?;
        progress(PrepareProgress::Verifying);
        return peer.seal(request.clone());
    }
    let mut file = file;
    let mut offset = peer.upload_chunk(BundleChunk {
        request: request.clone(),
        offset: 0,
        bytes: Vec::new(),
    })?;
    progress(PrepareProgress::Copying {
        sent: offset.min(size),
        total: size,
    });
    while offset < size {
        validate()?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "bundle offset unavailable")?;
        let mut bytes = vec![0; OUTGOING_BUNDLE_CHUNK_BYTES.min((size - offset) as usize)];
        file.read_exact(&mut bytes)
            .map_err(|_| "bundle read failed")?;
        let expected = offset + bytes.len() as u64;
        let next = peer.upload_chunk(BundleChunk {
            request: request.clone(),
            offset,
            bytes,
        })?;
        if next != expected {
            return Err("unexpected bundle offset; reconcile original request".into());
        }
        offset = next;
        progress(PrepareProgress::Copying {
            sent: offset,
            total: size,
        });
    }
    if offset != size {
        return Err("bundle exceeds declared size".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_context_denies_different_team_and_repository_identity() {
        let ctx = ActiveContext::for_team("/unused".into(), 41, "org".into(), "bound".into());
        assert!(bound_team(&ctx, "org:bound").is_ok());
        for key in ["org:other", "other:bound", "org"] {
            assert!(bound_team(&ctx, key).is_err());
        }
        let source: SourceRepoSlug = serde_json::from_value(serde_json::json!({"owner":"org","repo":"repo","provider":"github","host":"github.com"})).unwrap();
        assert!(same_repository(&source, &source));
        for replacement in [
            serde_json::json!({"owner":"other","repo":"repo","provider":"github","host":"github.com"}),
            serde_json::json!({"owner":"org","repo":"repo","provider":"gitlab","host":"gitlab.com"}),
        ] {
            let other = serde_json::from_value(replacement).unwrap();
            assert!(!same_repository(&source, &other));
        }
    }
    #[test]
    fn workspace_tool_does_not_accept_url_or_unbound_origin_authority() {
        let value = serde_json::json!({"requestId":"00000000-0000-4000-8000-000000000001","machineId":"box","teamKey":"org:team","workspaceId":"root","sourceRepo":{"owner":"org","repo":"repo","provider":"github","host":"github.com"},"baseRef":"main","branch":"work","repoPath":"/home/owner/repo","worktreePath":"/home/owner/work"});
        assert!(serde_json::from_value::<PrepareRemoteWorkspaceParams>(value.clone()).is_ok());
        for field in ["cloneUrl", "originKey", "ownerId", "target", "context"] {
            let mut invalid = value.clone();
            invalid[field] = serde_json::json!("forged");
            assert!(serde_json::from_value::<PrepareRemoteWorkspaceParams>(invalid).is_err());
        }
    }
}
