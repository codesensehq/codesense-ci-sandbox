// SPDX-License-Identifier: MIT
//! Shared authoritative completed W1 workspace resolution for hydration and publication.
use super::{
    remote_authority::RemoteAuthority,
    remote_workspace::{self, IntentReceipt},
};
use crate::commands::{machines, pty_backend, route_to_context::ActiveContext};
use crate::terminal::session_server::SessionServerClient;
use codesense_pty_core::remote_files as wire;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Arc;

pub(crate) struct ResolvedWorkspace {
    pub(crate) cwd: String,
    pub(crate) context: ActiveContext,
    pub(crate) client: Arc<SessionServerClient>,
    pub(crate) target: wire::FileTarget,
    pub(crate) source_repo: crate::domain::SourceRepoSlug,
    pub(crate) team_key: String,
    pub(crate) workspace_id: String,
    pub(crate) machine_id: String,
    pub(crate) server_id: String,
    /// The commit the workspace was prepared from, which the desktop sent.
    pub(crate) prepared_commit: String,
    /// The branch the workspace was prepared on, which the desktop chose.
    pub(crate) prepared_branch: String,
    /// The whole preparation request and the base it was made from.
    pub(crate) prepared_request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    pub(crate) base_ref: String,
    /// The base commit the machine reports the worktree on (its
    /// prepared commit, or the one `update_workspace_base` last moved it to).
    /// `None` when only the receipt was checked.
    pub(crate) base_commit: Option<String>,
    pub(crate) validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
fn read_receipt_files(
    root: std::path::PathBuf,
    owner: u64,
    machine: Option<String>,
    handle: Option<String>,
    selection: Option<(String, String)>,
) -> Result<Vec<(std::path::PathBuf, IntentReceipt)>, String> {
    let entries = std::fs::read_dir(root).map_err(|_| "managed workspace intents unavailable")?;
    let mut result: Vec<(std::path::PathBuf, IntentReceipt)> = Vec::new();
    let mut matching_handles = std::collections::BTreeSet::new();
    let mut retained_bytes = 0usize;
    for (count, entry) in entries.enumerate() {
        if count >= 100_000 {
            return Err("workspace intent directory exceeds bound".to_string());
        }
        let entry = entry.map_err(|_| "workspace intent directory unavailable")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.len() != 69
            || !name.ends_with(".json")
            || !name.as_bytes()[..64].iter().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(entry.path())
            .map_err(|_| "workspace intent unavailable")?;
        let meta = file
            .metadata()
            .map_err(|_| "workspace intent unavailable")?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.len() > 1024 * 1024
        {
            return Err("workspace intent is unsafe".into());
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "workspace intent unavailable")?;
        if bytes.len() > 1024 * 1024 {
            return Err("workspace intent exceeds bound".into());
        }
        let receipt: IntentReceipt =
            serde_json::from_slice(&bytes).map_err(|_| "workspace intent is invalid")?;
        let expected = receipt_handle(&receipt);
        if machine
            .as_ref()
            .is_none_or(|machine| receipt.params.machine_id == *machine)
            && receipt.request.owner_id == owner
            && handle.as_ref().is_none_or(|handle| &expected == handle)
            && selection.as_ref().is_none_or(|(team, workspace)| {
                &receipt.params.team_key == team && &receipt.params.workspace_id == workspace
            })
        {
            matching_handles.insert(expected);
            retained_bytes = retained_bytes.saturating_add(bytes.len());
            if matching_handles.len() > 256 || retained_bytes > 8 * 1024 * 1024 {
                return Err("matching prepared workspace receipts exceed safe bounds".into());
            }
            result.push((entry.path(), receipt));
        }
    }
    Ok(result)
}

pub(super) fn read_receipts(
    root: std::path::PathBuf,
    owner: u64,
    machine: Option<String>,
    handle: Option<String>,
    selection: Option<(String, String)>,
) -> Result<Vec<IntentReceipt>, String> {
    let mut result: Vec<IntentReceipt> = Vec::new();
    for (_, receipt) in read_receipt_files(root, owner, machine, handle, selection)? {
        if !result.iter().any(|prior| {
            prior.request.origin_key == receipt.request.origin_key
                && prior.request.worktree_path == receipt.request.worktree_path
                && prior.request.request_id == receipt.request.request_id
                && prior.server_id == receipt.server_id
                && prior.params.team_key == receipt.params.team_key
                && prior.params.workspace_id == receipt.params.workspace_id
                && prior.params.source_repo == receipt.params.source_repo
        }) {
            result.push(receipt);
        }
    }
    Ok(result)
}

/// Capture the preparation receipts owned by exactly this machine and handle.
/// A new preparation begun during cleanup must keep its own receipt.
pub(super) fn receipts_to_forget(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    owner: u64,
    machine: &str,
    handle: &str,
) -> Result<Vec<std::path::PathBuf>, String> {
    let root =
        crate::context_repo::path_to_user_home(home, principal).join("remote-workspace-intents-v1");
    Ok(
        read_receipt_files(root, owner, Some(machine.into()), Some(handle.into()), None)?
            .into_iter()
            .map(|(path, _)| path)
            .collect(),
    )
}

/// Forget only the captured receipts after the machine confirms cleanup.
pub(super) fn forget_receipts(files: Vec<std::path::PathBuf>) -> Result<(), String> {
    for path in files {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// The managed handle the machine gave a workspace prepared under `receipt`.
fn receipt_handle(receipt: &IntentReceipt) -> String {
    format!(
        "remote-workspace-{:x}",
        Sha256::digest(
            format!(
                "{}:{}",
                receipt.request.origin_key, receipt.request.worktree_path
            )
            .as_bytes()
        )
    )
}

/// One workspace this account prepared on a machine, as its
/// durable receipt records it: where a routine's agent can start there with
/// no viewer state at all. Choosing one is not authority: the launch still
/// goes through the receipt gate ([`resolve_durable`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedPlace {
    pub team_key: String,
    pub workspace_id: String,
    pub machine_id: String,
    /// The terminal server it was prepared under.
    pub server_id: String,
    pub source_repo: crate::domain::SourceRepoSlug,
    pub handle: String,
    /// Its worktree on the machine.
    pub cwd: String,
    /// The preparation request the receipt holds (what the machine is asked
    /// to confirm).
    pub request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    /// A story's workspace (that story's work): from the kind the receipt
    /// recorded, or, for an older receipt, from where its worktree lives.
    pub story: bool,
}

/// The workspaces `owner` prepared on `machine`, from the durable
/// `remote-workspace-intents-v1` receipts under `home` (one per handle and
/// workspace). Empty when nothing was ever prepared.
pub(crate) fn prepared_places(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    owner: u64,
    machine: &str,
) -> Result<Vec<PreparedPlace>, String> {
    places_from_receipts(home, principal, owner, Some(machine.to_string()))
}

/// [`prepared_places`] on every machine.
pub(crate) fn prepared_places_everywhere(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    owner: u64,
) -> Result<Vec<PreparedPlace>, String> {
    places_from_receipts(home, principal, owner, None)
}

fn places_from_receipts(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    owner: u64,
    machine: Option<String>,
) -> Result<Vec<PreparedPlace>, String> {
    let root =
        crate::context_repo::path_to_user_home(home, principal).join("remote-workspace-intents-v1");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut places: Vec<PreparedPlace> = read_receipts(root, owner, machine, None, None)?
        .into_iter()
        .map(|receipt| PreparedPlace {
            handle: receipt_handle(&receipt),
            story: match receipt.kind.as_deref() {
                Some(kind) => kind == "story",
                None => super::remote_story_workspace::is_story_worktree(
                    &receipt.params.team_key,
                    &receipt.request.worktree_path,
                ),
            },
            team_key: receipt.params.team_key,
            workspace_id: receipt.params.workspace_id,
            machine_id: receipt.params.machine_id,
            server_id: receipt.server_id,
            source_repo: receipt.params.source_repo,
            cwd: receipt.request.worktree_path.clone(),
            request: receipt.request,
        })
        .collect();
    places.sort_by(|a, b| {
        (&a.team_key, &a.workspace_id, &a.handle, &a.server_id).cmp(&(
            &b.team_key,
            &b.workspace_id,
            &b.handle,
            &b.server_id,
        ))
    });
    places.dedup();
    Ok(places)
}

/// A receipt as [`prepared_places`] reads it, written as a preparation would.
/// Returns the handle the machine gives it.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_test_receipt(
    root: &std::path::Path,
    request_id: &str,
    machine_id: &str,
    team_key: &str,
    workspace_id: &str,
    source_repo: crate::domain::SourceRepoSlug,
    worktree_path: &str,
    owner: u64,
    server_id: &str,
    kind: Option<&str>,
) -> String {
    use codesense_pty_core::remote_workspace::{GitBundle, PrepareWorkspace};
    write_receipt_for_test(
        root,
        remote_workspace::PrepareRemoteWorkspaceParams {
            request_id: request_id.into(),
            machine_id: machine_id.into(),
            team_key: team_key.into(),
            workspace_id: workspace_id.into(),
            source_repo,
            base_ref: "main".into(),
            branch: "work".into(),
            repo_path: "/home/dev/codesense/repos/r".into(),
            worktree_path: worktree_path.into(),
        },
        PrepareWorkspace {
            request_id: request_id.into(),
            owner_id: owner,
            origin_key: format!("origin-{workspace_id}"),
            repository_key: "repository".into(),
            clone_url: "https://example.invalid/r.git".into(),
            repo_path: "/home/dev/codesense/repos/r".into(),
            worktree_path: worktree_path.into(),
            base_ref: "main".into(),
            branch: "work".into(),
            bundle: GitBundle {
                digest: "digest".into(),
                size: 0,
                commit: "commit".into(),
            },
        },
        server_id,
        kind,
    )
}

/// The durable receipt a preparation of `request` under `params` writes into
/// `root` (`remote-workspace-intents-v1`), for a fixture that prepared the
/// workspace directly. Returns the handle the machine gives it.
#[cfg(test)]
pub(crate) fn write_receipt_for_test(
    root: &std::path::Path,
    params: remote_workspace::PrepareRemoteWorkspaceParams,
    request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    server_id: &str,
    kind: Option<&str>,
) -> String {
    let key = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&params.machine_id, &params.team_key, &params.request_id))
                .unwrap()
        )
    );
    let receipt = IntentReceipt {
        params,
        request,
        server_id: server_id.into(),
        kind: kind.map(str::to_string),
    };
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join(format!("{key}.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    receipt_handle(&receipt)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedWorkspaceChoice {
    handle: String,
    cwd: String,
    source_repo: crate::domain::SourceRepoSlug,
    branch: String,
}

/// UI choices are filtered by current authority, never supplied as launch authority.
#[tauri::command]
pub async fn list_prepared_remote_workspaces(
    app: tauri::AppHandle,
    machine_id: String,
    team_key: String,
    workspace_id: String,
) -> Result<Vec<PreparedWorkspaceChoice>, String> {
    let (org, team) = crate::commands::pty_layout::parse_team_key(&team_key);
    let ctx = match crate::commands::route_to_context::derive_context_for(
        &app,
        org,
        team.ok_or("workspace requires a team")?,
    )
    .await?
    {
        crate::commands::route_to_context::ContextResolution::Ok(ctx) => ctx,
        _ => return Err("workspace context unavailable".into()),
    };
    machines::authorize_terminal_machine(&app, &machine_id, &team_key).await?;
    let root = crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
        .join("remote-workspace-intents-v1");
    let owner = ctx.user_id;
    let machine = machine_id.clone();
    let selection = (team_key.clone(), workspace_id.clone());
    let receipts = crate::commands::await_blocking("list_prepared_workspaces", move || {
        if !root.exists() {
            return Ok(Vec::new());
        }
        read_receipts(root, owner, Some(machine), None, Some(selection))
    })
    .await?;
    let mut choices = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for receipt in receipts
        .into_iter()
        .filter(|r| r.params.team_key == team_key && r.params.workspace_id == workspace_id)
    {
        let handle = format!(
            "remote-workspace-{:x}",
            Sha256::digest(
                format!(
                    "{}:{}",
                    receipt.request.origin_key, receipt.request.worktree_path
                )
                .as_bytes()
            )
        );
        if !seen.insert(handle.clone()) {
            continue;
        }
        if let Ok(resolved) = resolve_selected(
            app.clone(),
            ctx.clone(),
            Some(&machine_id),
            &handle,
            None,
            false,
        )
        .await
        {
            choices.push(PreparedWorkspaceChoice {
                handle,
                cwd: resolved.cwd,
                source_repo: resolved.source_repo,
                branch: receipt.request.branch,
            });
        }
    }
    Ok(choices)
}

pub(crate) async fn resolve(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine_filter: Option<&str>,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
) -> Result<ResolvedWorkspace, String> {
    resolve_inner(
        app,
        ctx,
        machine_filter,
        workspace_handle,
        caller,
        false,
        false,
        false,
    )
    .await
}

pub(crate) async fn resolve_selected(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine_filter: Option<&str>,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
    receipt_only: bool,
) -> Result<ResolvedWorkspace, String> {
    resolve_inner(
        app,
        ctx,
        machine_filter,
        workspace_handle,
        caller,
        true,
        receipt_only,
        false,
    )
    .await
}

/// [`resolve_selected`] for the app's own routine launch into a
/// workspace it chose from the receipts: every check but whether the viewer
/// has the workspace open (it may be closed, or not published yet after a
/// start). Owner, team, configured repo, machine, server and the machine's own
/// confirmation still hold; there is no caller, so no remote authority.
pub(crate) async fn resolve_durable(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine: &str,
    workspace_handle: &str,
) -> Result<ResolvedWorkspace, String> {
    resolve_inner(
        app,
        ctx,
        Some(machine),
        workspace_handle,
        None,
        true,
        false,
        true,
    )
    .await
}

/// [`resolve_durable`] for a read-only count: the same owner,
/// team, repo, machine and server checks, but nothing is dispatched to the
/// machine (no preparation reconcile: its call graph never reaches
/// [`confirm_prepared`]), and the workspace need not be open in the viewer.
pub(crate) async fn resolve_for_count(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine: &str,
    workspace_handle: &str,
) -> Result<ResolvedWorkspace, String> {
    let receipts =
        candidate_receipts(&app, &ctx, Some(machine), workspace_handle, None, true).await?;
    for receipt in receipts {
        let Some(adapter) = admit(&app, &ctx, receipt, workspace_handle, None, true, true).await?
        else {
            continue;
        };
        validate_destination(true, adapter.validate.clone(), || Ok(())).await?;
        return Ok(adapter);
    }
    Err("no current prepared workspace matches this owner, team, machine and handle".into())
}

/// The viewer-grouping check, which a durable (routine) launch skips.
fn grouped(
    app: &tauri::AppHandle,
    params: &remote_workspace::PrepareRemoteWorkspaceParams,
    durable: bool,
) -> Result<(), String> {
    if durable {
        return Ok(());
    }
    remote_workspace::group(app, params)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_inner(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine_filter: Option<&str>,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
    selected: bool,
    receipt_only: bool,
    durable: bool,
) -> Result<ResolvedWorkspace, String> {
    let receipts = candidate_receipts(
        &app,
        &ctx,
        machine_filter,
        workspace_handle,
        caller.as_ref(),
        durable,
    )
    .await?;
    for receipt in receipts {
        let request = receipt.request.clone();
        let Some(adapter) = admit(
            &app,
            &ctx,
            receipt,
            workspace_handle,
            caller.clone(),
            selected,
            durable,
        )
        .await?
        else {
            continue;
        };
        let prepared_client = adapter.client.clone();
        let prepared_handle = adapter.target.handle.clone();
        let prepared_cwd = adapter.cwd.clone();
        let reported = Arc::new(std::sync::Mutex::new(None::<String>));
        let reported_base = reported.clone();
        let validated = validate_destination(receipt_only, adapter.validate.clone(), move || {
            let commit =
                confirm_prepared(&prepared_client, request, &prepared_handle, &prepared_cwd)?;
            *reported_base.lock().unwrap_or_else(|p| p.into_inner()) = commit;
            Ok(())
        })
        .await;
        let mut adapter = adapter;
        adapter.base_commit = reported.lock().unwrap_or_else(|p| p.into_inner()).take();
        if receipt_only {
            // Status refuses a revoked authority; it never dispatches a prepared-
            // workspace reconciliation or searches another receipt after refusal.
            validated?;
            return Ok(adapter);
        }
        if validated.is_ok() {
            return Ok(adapter);
        }
    }
    Err("no current prepared workspace matches this owner, team, machine and handle".into())
}

/// The receipts that could be `workspace_handle`: this owner's, on the
/// machine's live terminal server, all naming the same workspace (else
/// ambiguous).
async fn candidate_receipts(
    app: &tauri::AppHandle,
    ctx: &ActiveContext,
    machine_filter: Option<&str>,
    workspace_handle: &str,
    caller: Option<&RemoteAuthority>,
    durable: bool,
) -> Result<Vec<IntentReceipt>, String> {
    if durable && caller.is_some() {
        return Err("a durable workspace launch is the app's own".into());
    }
    if !workspace_handle.starts_with("remote-workspace-") || workspace_handle.len() != 81 {
        return Err("invalid managed remote workspace handle".into());
    }
    if let Some(caller) = caller {
        caller
            .validate()
            .await
            .map_err(|_| "originating remote authority unavailable")?;
    }
    let root = crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
        .join("remote-workspace-intents-v1");
    let machine = machine_filter.map(str::to_string);
    let handle = workspace_handle.to_string();
    let owner = ctx.user_id;
    let mut receipts =
        crate::commands::await_blocking("resolve_remote_workspace_handle", move || {
            read_receipts(root, owner, machine, Some(handle), None)
        })
        .await?;
    // A receipt made under a terminal server the machine no longer
    // runs can never be the live workspace; it is set aside before the
    // ambiguity check, so a stale one cannot block the live one.
    if let Some(machine) = machine_filter {
        if let Ok(backend) = pty_backend::resolve_spawn_backend(app, machine, "managed-workspace") {
            let live = backend.server_id().to_string();
            receipts.retain(|receipt| receipt.server_id == live);
        }
    }
    if let Some(first) = receipts.first() {
        if receipts.iter().any(|receipt| {
            receipt.params.team_key != first.params.team_key
                || receipt.params.workspace_id != first.params.workspace_id
                || receipt.params.machine_id != first.params.machine_id
                || receipt.params.source_repo != first.params.source_repo
                || receipt.server_id != first.server_id
                || receipt.request.repository_key != first.request.repository_key
                || receipt.request.worktree_path != first.request.worktree_path
        }) {
            return Err("ambiguous prepared workspace ownership".into());
        }
    }
    Ok(receipts)
}

/// One receipt's destination, every local check passed (owner, team, repo
/// configuration, machine authorization, live server) — `None` when it was
/// prepared under another server or for a team this owner no longer has.
/// Dispatches nothing to the machine.
async fn admit(
    app: &tauri::AppHandle,
    ctx: &ActiveContext,
    receipt: IntentReceipt,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
    selected: bool,
    durable: bool,
) -> Result<Option<ResolvedWorkspace>, String> {
    let ctx = if selected && caller.is_none() {
        let (org, team) = crate::commands::pty_layout::parse_team_key(&receipt.params.team_key);
        match crate::commands::route_to_context::derive_context_for(
            app,
            org,
            team.ok_or("workspace has no team")?,
        )
        .await?
        {
            crate::commands::route_to_context::ContextResolution::Ok(captured) => {
                captured_selected_context(ctx, &receipt.params.team_key, captured)?
            }
            _ => return Err("workspace owner or team unavailable".into()),
        }
    } else {
        ctx.clone()
    };
    let machine_id = receipt.params.machine_id.as_str();
    let backend = pty_backend::resolve_spawn_backend(app, machine_id, "managed-workspace")?;
    backend.revalidate(app)?;
    let client = backend
        .client()
        .filter(|_| backend.is_remote())
        .ok_or("admitted remote connection unavailable")?;
    let lease = client
        .private_lease()
        .ok_or("private remote authority unavailable")?;
    lease.validate()?;
    if lease.owner_id() != ctx.user_id {
        return Err("private workspace owner changed".into());
    }
    if receipt.server_id != backend.server_id()
        || remote_workspace::bound_team(&ctx, &receipt.params.team_key).is_err()
    {
        return Ok(None);
    }
    remote_workspace::configured(&ctx, &receipt.params)?;
    grouped(app, &receipt.params, durable)?;
    machines::authorize_terminal_machine(app, machine_id, &receipt.params.team_key).await?;
    let bound_app = app.clone();
    let bound_ctx = ctx.clone();
    let bound_params = receipt.params.clone();
    let result_params = receipt.params.clone();
    let bound_backend = backend.clone();
    let bound_client = client.clone();
    let bound_caller = caller.clone();
    Ok(Some(ResolvedWorkspace {
        cwd: receipt.request.worktree_path.clone(),
        context: ctx.clone(),
        machine_id: machine_id.into(),
        server_id: backend.server_id().into(),
        prepared_commit: receipt.request.bundle.commit.clone(),
        prepared_branch: receipt.request.branch.clone(),
        prepared_request: receipt.request.clone(),
        base_ref: receipt.params.base_ref.clone(),
        base_commit: None,
        source_repo: result_params.source_repo,
        team_key: result_params.team_key,
        workspace_id: result_params.workspace_id,
        target: wire::FileTarget {
            owner_id: ctx.user_id,
            origin_key: receipt.request.origin_key.clone(),
            request_id: receipt.request.request_id.clone(),
            handle: workspace_handle.into(),
        },
        client: client.clone(),
        validate: Arc::new(move || {
            remote_workspace::validate_caller(&bound_caller)?;
            bound_backend.revalidate(&bound_app)?;
            bound_client
                .private_lease()
                .ok_or("private remote authority retired")?
                .validate()?;
            remote_workspace::bound_team(&bound_ctx, &bound_params.team_key)?;
            remote_workspace::configured(&bound_ctx, &bound_params)?;
            grouped(&bound_app, &bound_params, durable)?;
            tauri::async_runtime::block_on(machines::authorize_terminal_machine(
                &bound_app,
                &bound_params.machine_id,
                &bound_params.team_key,
            ))?;
            Ok(())
        }),
    }))
}

/// The machine's own word that `request` is prepared, at `handle` in `cwd`:
/// `alreadyPrepared` there, with the base commit it reports. Anything else
/// (no such workspace, gone, another place) is not prepared.
pub(crate) fn confirm_prepared(
    client: &SessionServerClient,
    request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    handle: &str,
    cwd: &str,
) -> Result<Option<String>, String> {
    match client.reconcile_remote_workspace(request)? {
        Some(result)
            if result.status == "alreadyPrepared"
                && result.handle.as_deref() == Some(handle)
                && result.cwd.as_deref() == Some(cwd) =>
        {
            Ok(result.resolved_commit)
        }
        _ => Err("remote workspace is not authoritatively prepared".into()),
    }
}

/// Authority checks are synchronous and may bridge async authorization with
/// `block_on`. Both status and prepared-workspace resolution must invoke them
/// off runtime workers. Receipt-only status skips the prepared-workspace RPC.
async fn validate_destination(
    receipt_only: bool,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    reconcile: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    crate::commands::await_blocking("validate_remote_workspace_destination", move || {
        validate()?;
        if !receipt_only {
            reconcile()?;
            validate()?;
        }
        Ok(())
    })
    .await
}

/// Receipt ownership chooses the team; the ambient desktop team never becomes
/// replay identity. Membership was resolved immediately before this boundary.
fn captured_selected_context(
    ambient: &ActiveContext,
    team_key: &str,
    captured: ActiveContext,
) -> Result<ActiveContext, String> {
    if captured.user_id != ambient.user_id
        || std::fs::canonicalize(&captured.codesense_home).map_err(|e| e.to_string())?
            != std::fs::canonicalize(&ambient.codesense_home).map_err(|e| e.to_string())?
    {
        return Err("workspace owner or namespace changed".into());
    }
    remote_workspace::bound_team(&captured, team_key)?;
    Ok(captured)
}

#[cfg(test)]
mod selected_admission_tests {
    use super::*;
    #[test]
    fn prepared_choice_bound_counts_matching_deduplicated_receipts() {
        let root = tempfile::tempdir().unwrap();
        let mut receipt = serde_json::json!({
            "params": {"requestId":"request", "machineId":"wanted", "teamKey":"org/context:team", "workspaceId":"group", "sourceRepo":{"owner":"org", "repo":"source"}, "baseRef":"main", "branch":"work", "repoPath":"/repo", "worktreePath":"/work"},
            "request": {"requestId":"request", "ownerId":42, "originKey":"origin", "repositoryKey":"repo", "cloneUrl":"https://example.com/source", "repoPath":"/repo", "worktreePath":"/work", "baseRef":"main", "branch":"work", "bundle":{"digest":"hash", "size":0, "commit":"commit"}},
            "server_id":"server"
        });
        for i in 0..300 {
            receipt["params"]["machineId"] = "other".into();
            receipt["request"]["worktreePath"] = format!("/unrelated/{i}").into();
            std::fs::write(
                root.path().join(format!("{i:064x}.json")),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
        }
        receipt["params"]["machineId"] = "wanted".into();
        receipt["request"]["worktreePath"] = "/work".into();
        for i in 300..600 {
            std::fs::write(
                root.path().join(format!("{i:064x}.json")),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
        }
        let selected = read_receipts(
            root.path().into(),
            42,
            Some("wanted".into()),
            None,
            Some(("org/context:team".into(), "group".into())),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        for i in 600..856 {
            receipt["request"]["worktreePath"] = format!("/selected/{i}").into();
            std::fs::write(
                root.path().join(format!("{i:064x}.json")),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
        }
        assert!(read_receipts(
            root.path().into(),
            42,
            Some("wanted".into()),
            None,
            Some(("org/context:team".into(), "group".into()))
        )
        .err()
        .unwrap()
        .contains("safe bounds"));
    }

    #[tokio::test]
    async fn receipt_status_validates_off_runtime_without_prepared_workspace_rpc() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let checks = Arc::new(AtomicUsize::new(0));
        let checked = checks.clone();
        // Same sync-to-async bridge as the real authority validator. Calling it
        // directly in resolve_inner's async receipt-only branch panics.
        let validate = Arc::new(move || {
            tauri::async_runtime::block_on(async { tokio::task::yield_now().await });
            checked.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        validate_destination(true, validate, || {
            panic!("receipt-only status must not dispatch workspace reconciliation")
        })
        .await
        .unwrap();
        assert_eq!(checks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn receipt_status_returns_current_authority_refusal() {
        let result = validate_destination(
            true,
            Arc::new(|| {
                tauri::async_runtime::block_on(async {
                    Err("private remote authority retired".into())
                })
            }),
            || panic!("revoked authority must not dispatch reconciliation"),
        )
        .await;
        assert_eq!(result.unwrap_err(), "private remote authority retired");
    }

    #[tokio::test]
    async fn prepared_resolution_revalidates_after_its_workspace_rpc() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let checks = Arc::new(AtomicUsize::new(0));
        let before_and_after = checks.clone();
        let during_rpc = checks.clone();
        validate_destination(
            false,
            Arc::new(move || {
                tauri::async_runtime::block_on(async { tokio::task::yield_now().await });
                before_and_after.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            move || {
                assert_eq!(during_rpc.load(Ordering::SeqCst), 1);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(checks.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn receipt_team_survives_ambient_team_switch_but_owner_cannot_change() {
        let home = tempfile::tempdir().unwrap();
        let ambient = ActiveContext {
            codesense_home: home.path().to_path_buf(),
            user_id: 42,
            org_slug: "acme/context".into(),
            team_name: Some("other".into()),
        };
        let captured = ActiveContext {
            team_name: Some("captured".into()),
            ..ambient.clone()
        };
        let resolved =
            captured_selected_context(&ambient, "acme/context:captured", captured.clone()).unwrap();
        assert_eq!(resolved.team_name.as_deref(), Some("captured"));
        assert!(
            captured_selected_context(&ambient, "acme/context:other", captured.clone()).is_err()
        );
        assert!(captured_selected_context(
            &ambient,
            "acme/context:captured",
            ActiveContext {
                user_id: 99,
                ..captured
            }
        )
        .is_err());
    }
}
