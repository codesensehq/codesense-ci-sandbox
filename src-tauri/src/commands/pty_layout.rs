// SPDX-License-Identifier: MIT
//! The terminal-layout verbs as Tauri commands. Each host keeps the layout of
//! the sessions it hosts, and every verb reaches it through the one terminal
//! boundary (`commands::pty_backend`): a tab opens on the host its session is
//! spawned on, a verb about a session goes to the session's host, one about a
//! workspace to every host holding it, and the snapshot is every host's. The
//! webview is the app's one viewer in each; every change is pushed back on
//! `terminal-layout-changed`, stamped with its host's epoch.

use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_pty_core::domain::{
    ClientBootId, PtySessionId, TerminalDomain, ViewerId, WorkspaceKey,
};
use codesense_pty_core::layout::{
    CloseMode, CloseOutcome, GridState, LayoutError, LayoutSnapshot, OpenTab, Placement, TabEntry,
    TagPatch, Tags, ViewHint, WorkspaceDescriptor,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::commands::async_helpers::await_blocking;
use crate::commands::machines::RuntimeSessionReference;
use crate::commands::pty::{attach_warm_from_args, spawn_from_args, AttachWarmArgs, SpawnPtyArgs};
use crate::commands::pty_backend::{self, SpawnPlacement};
use tauri::{Emitter, Manager};

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum ActivityAvailability {
    Available,
    UpgradeRequired,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnapshotReply {
    availability: ActivityAvailability,
    target: super::machines::RuntimeMemberTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    private_authority_epoch: Option<String>,
    snapshots: Vec<codesense_pty_core::activity::Snapshot>,
}

/// One independent member read; callers schedule members independently so a
/// slow remote cannot delay a local host's activity projection.
#[tauri::command]
pub async fn terminal_activity_snapshot(
    app: tauri::AppHandle,
    target: super::machines::RuntimeMemberTarget,
) -> Result<ActivitySnapshotReply, String> {
    await_blocking("terminal_activity_snapshot", move || {
        let backend = pty_backend::resolve_member_backend(&app, &target, true)?;
        backend.revalidate(&app)?;
        let private_authority_epoch = terminal_layout_admission(app.clone())
            .rows
            .into_iter()
            .find(|row| row.member == target.member && row.revision == target.admission_revision)
            .and_then(|row| row.private_authority_epoch);
        let (availability, snapshots) = if backend
            .client()
            .is_some_and(|client| !client.supports_activity())
        {
            (ActivityAvailability::UpgradeRequired, Vec::new())
        } else {
            (
                ActivityAvailability::Available,
                backend
                    .domain()
                    .activity_snapshot()
                    .map_err(|error| error.to_string())?,
            )
        };
        backend.revalidate(&app)?;
        Ok(ActivitySnapshotReply {
            availability,
            target,
            private_authority_epoch,
            snapshots,
        })
    })
    .await
}

#[tauri::command]
pub async fn terminal_prepare_shell_activity(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    provider: codesense_pty_core::agent_cli::AgentCli,
) -> Result<codesense_pty_core::activity::ShellLaunch, String> {
    let remote = authorize_human_terminal(&app, &session).await?;
    await_blocking("terminal_prepare_shell_activity", move || {
        let backend = pty_backend::resolve_session_reference(&app, &session, remote)?;
        backend.revalidate(&app)?;
        let launch = backend
            .domain()
            .activity_prepare_shell(&PtySessionId::new(session.session()), provider)
            .map_err(|error| error.to_string())?;
        backend.revalidate(&app)?;
        Ok(launch)
    })
    .await
}

#[tauri::command]
pub async fn terminal_launch_activity_line(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    provider: codesense_pty_core::agent_cli::AgentCli,
    line: String,
    submit: Option<bool>,
) -> Result<codesense_pty_core::domain::SubmitOutcome, String> {
    let remote = authorize_human_terminal(&app, &session).await?;
    super::pty::submit_automatic_input(
        app.clone(),
        session.clone(),
        await_blocking("terminal_launch_activity_line", move || {
            let backend = pty_backend::resolve_session_reference(&app, &session, remote)?;
            backend.revalidate(&app)?;
            let domain = backend.domain();
            let id = PtySessionId::new(session.session());
            let line = line.trim_end_matches(['\r', '\n']).trim_start();
            let launch = domain.activity_prepare_shell(&id, provider);
            backend.revalidate(&app)?;
            if !backend.is_remote()
                && backend
                    .client()
                    .is_some_and(|client| !client.supports_activity())
            {
                let _ = app.emit("terminal-activity-upgrade-required", ());
            }
            // A managed launch must never silently run without its observers.
            // Preparation failure leaves the shell and existing sessions untouched.
            let command = observed_launch_line(launch, line)?;
            if submit.unwrap_or(false) {
                domain
                    .submit_shell_line(&id, command.trim_end_matches('\r'), 200, 4000)
                    .map_err(|error| error.to_string())
            } else {
                domain
                    .write(&id, command.as_bytes())
                    .map_err(|error| error.to_string())?;
                Ok(codesense_pty_core::domain::SubmitOutcome { submitted: true })
            }
        }),
    )
    .await
}

/// SW-WA-F10: the shell-line form of the activity-exec launch prefix, for the
/// one remaining consumer that still TYPES a user-authored agent CLI line
/// into an already-running interactive shell ([`terminal_launch_activity_line`]
/// — a user hits the activity-exec button in the composer, so the input is a
/// shell string by construction, not pre-tokenized argv). The MCP managed
/// launch's `start_agent` no longer goes through this function: it composes
/// argv up front and spawns the agent as PID-1 of the PTY via
/// [`crate::commands::pty::SpawnPtyArgs::command_argv`], no helper
/// indirection — the activity bindings ride on SOCKET_ENV / TOKEN_ENV set by
/// the host's native-prep (see `mcp_workspace.rs` `spawn_and_bind`).
fn observed_launch_line(
    launch: Result<
        codesense_pty_core::activity::ShellLaunch,
        codesense_pty_core::domain::PortUnavailable,
    >,
    line: &str,
) -> Result<String, String> {
    let launch = launch.map_err(|error| error.to_string())?;
    Ok(format!(
        " {} --activity-exec {} -- {}\r",
        codesense_pty_core::shell_quote(&launch.helper),
        codesense_pty_core::shell_quote(&launch.bootstrap),
        line
    ))
}

#[tauri::command]
pub fn terminal_layout_admission(
    app: tauri::AppHandle,
) -> super::machines::RuntimeAdmissionBaseline {
    let mut baseline = app
        .state::<crate::terminal::session_server::SessionServerState>()
        .admission
        .baseline();
    baseline.rows.extend(
        app.state::<super::host_connection_manager::HostConnectionManager>()
            .admission_baseline()
            .rows,
    );
    baseline
}

/// This app's webview: its one viewer in every terminal host it uses. A
/// webview reload keeps it, so the reloaded view keeps its focus and scroll.
pub(crate) fn app_viewer() -> ViewerId {
    ViewerId::new("app")
}

/// Separates the team from the workspace form in a [`WorkspaceKey`]. The
/// frontend's `WORKSPACE_KEY_SEPARATOR` (`src/shared/lib/workspace-key.ts`) is
/// the same character; a contract test pins the two.
pub(crate) const WORKSPACE_KEY_SEPARATOR: char = '|';

/// The frontend's team key for `team_name` of `org_slug`: `{org}:{team}`,
/// `_none` for no team (the frontend's `buildTeamKey`).
pub(crate) fn team_key(org_slug: &str, team_name: Option<&str>) -> String {
    format!("{org_slug}:{}", team_name.unwrap_or(NO_TEAM))
}

/// The team key's `no team` name.
const NO_TEAM: &str = "_none";

/// Split a [`team_key`] back into its org slug and team (`None` for no team).
/// An org slug never holds a `:`, so the first one ends it.
pub(crate) fn parse_team_key(team_key: &str) -> (&str, Option<&str>) {
    match team_key.split_once(':') {
        Some((org, team)) => (org, (team != NO_TEAM).then_some(team)),
        None => (team_key, None),
    }
}

/// The terminal layout's key for a CodeSense workspace in the team keyed
/// `team_key` — the ONE Rust minting site (grep-tested), the twin of the
/// frontend's `workspaceKey`. A workspace id alone is not unique across teams,
/// so the key carries the team; a story's `form` is its frontmatter `story_id`.
pub(crate) fn workspace_key(team_key: &str, form: &str) -> WorkspaceKey {
    WorkspaceKey::new(format!("{team_key}{WORKSPACE_KEY_SEPARATOR}{form}"))
}

/// The team key a workspace key carries (`{team}|{form}`).
fn workspace_team_key(workspace: &WorkspaceKey) -> Option<&str> {
    workspace
        .as_str()
        .split_once(WORKSPACE_KEY_SEPARATOR)
        .map(|(team, _)| team)
        .filter(|team| !team.is_empty())
}

/// A remote Machine's workspace is reached only for a team the Machine is
/// visible to: the team its key carries. True for a remote workspace, false
/// for This Mac's (which needs no team).
async fn authorize_remote_workspace(
    app: &tauri::AppHandle,
    workspace: &super::machines::RuntimeWorkspaceReference,
) -> Result<bool, String> {
    let super::machines::RuntimeWorkspaceReference::Admitted(target) = workspace else {
        return Ok(false);
    };
    if target.runtime.member.slot != super::machines::RuntimeMemberSlot::Remote {
        return Ok(false);
    }
    let team = workspace_team_key(&target.workspace).ok_or("the workspace names no team")?;
    super::machines::authorize_terminal_machine(app, &target.runtime.member.machine_id, team)
        .await?;
    Ok(true)
}

/// The workspace a remote session's tab is in on its Machine, once the Machine
/// is authorized for that workspace's team; `None` for This Mac's sessions.
async fn authorize_remote_session(
    app: &tauri::AppHandle,
    session: &RuntimeSessionReference,
) -> Result<Option<WorkspaceKey>, String> {
    let RuntimeSessionReference::Admitted(target) = session else {
        return Ok(None);
    };
    if target.runtime.member.slot != super::machines::RuntimeMemberSlot::Remote {
        return Ok(None);
    }
    let workspace = await_blocking("remote_session_workspace", {
        let app = app.clone();
        let target = target.clone();
        move || {
            let backend = pty_backend::resolve_member_backend(&app, &target.runtime, true)?;
            backend.revalidate(&app)?;
            // Read under the webview's own boot, so the read hides nothing it shows.
            let boot = WEBVIEW_BOOT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or("the terminal layout has not been read yet")?;
            let snapshot = backend
                .domain()
                .layout_snapshot(&app_viewer(), &boot)
                .map_err(|e| e.to_string())?;
            backend.revalidate(&app)?;
            snapshot
                .workspaces
                .into_iter()
                .find(|(_, layout)| layout.tabs.iter().any(|tab| tab.session == target.session))
                .map(|(key, _)| key)
                .ok_or_else(|| "this terminal is not on its machine".to_string())
        }
    })
    .await?;
    let team = workspace_team_key(&workspace).ok_or("the workspace names no team")?;
    super::machines::authorize_terminal_machine(app, &target.runtime.member.machine_id, team)
        .await?;
    Ok(Some(workspace))
}

/// A remote verb runs only under a current private lease held by the app's own
/// private owner: checked before the verb and again after it.
fn current_private_lease(
    lease: Option<&crate::commands::auth::private_authority::PrivateAuthorityLease>,
    owner: Result<u64, String>,
) -> Result<(), String> {
    let lease = lease.ok_or("private authority unavailable")?;
    lease.validate()?;
    if owner? != lease.owner_id() {
        return Err("the private owner changed".into());
    }
    Ok(())
}

/// The lease gate for a resolved remote backend (a no-op for This Mac's).
fn check_remote_lease(
    app: &tauri::AppHandle,
    backend: &pty_backend::ResolvedTerminalBackend,
) -> Result<(), String> {
    if !backend.is_remote() {
        return Ok(());
    }
    let owner = app
        .state::<crate::commands::local_profile::LocalProfileState>()
        .snapshot()
        .map(|authority| authority.owner_id());
    current_private_lease(
        backend
            .client()
            .and_then(|client| client.private_lease())
            .as_ref(),
        owner,
    )
}

/// A remote tab acts only within the workspace it is in on its Machine: a move
/// or focus naming another workspace (another team's, another story's) is
/// refused.
fn within_remote_workspace(
    current: Option<&WorkspaceKey>,
    target: &WorkspaceKey,
) -> Result<(), LayoutError> {
    match current {
        Some(current) if current != target => Err(LayoutError::UnknownWorkspace(target.clone())),
        _ => Ok(()),
    }
}

/// Run a layout verb about `session` in the host that owns it, on the blocking
/// pool (a close may census the process tree, a kill reaps, and a sidecar
/// answers over its socket). A remote Machine's session is reached by its
/// admitted target once the Machine is authorized for the session's team; the
/// verb is told the workspace its tab is in there.
async fn in_session_host<T, F>(
    app: tauri::AppHandle,
    label: &'static str,
    session: RuntimeSessionReference,
    verb: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&dyn TerminalDomain, &PtySessionId, Option<&WorkspaceKey>) -> Result<T, LayoutError>
        + Send
        + 'static,
{
    let remote = authorize_remote_session(&app, &session).await?;
    await_blocking(label, move || {
        let backend = pty_backend::resolve_session_reference(&app, &session, true)?;
        check_remote_lease(&app, &backend)?;
        backend.revalidate(&app)?;
        let out = verb(
            &*backend.domain(),
            &PtySessionId::new(session.session()),
            remote.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        backend.revalidate(&app)?;
        check_remote_lease(&app, &backend)?;
        Ok(out)
    })
    .await
}

/// [`in_workspace_hosts`], reaching a remote Machine's admitted member too when
/// the Machine is authorized for the workspace's team — under its current
/// private lease, checked before and after the verb.
async fn in_workspace_members<F>(
    app: tauri::AppHandle,
    label: &'static str,
    workspace: super::machines::RuntimeWorkspaceReference,
    verb: F,
) -> Result<(), String>
where
    F: Fn(&dyn TerminalDomain, &WorkspaceKey) -> Result<(), LayoutError> + Send + 'static,
{
    if !authorize_remote_workspace(&app, &workspace).await? {
        return in_workspace_hosts(app, label, workspace, verb).await;
    }
    let super::machines::RuntimeWorkspaceReference::Admitted(target) = workspace else {
        return Err("an authorized remote workspace is an admitted target".into());
    };
    await_blocking(label, move || {
        let backend = pty_backend::resolve_member_backend(&app, &target.runtime, true)?;
        check_remote_lease(&app, &backend)?;
        backend.revalidate(&app)?;
        verb(&*backend.domain(), &target.workspace).map_err(|error| error.to_string())?;
        backend.revalidate(&app)?;
        check_remote_lease(&app, &backend)
    })
    .await
}

/// Run a layout verb about one workspace in every host holding it, on the
/// blocking pool.
pub(crate) async fn in_workspace_hosts<R: tauri::Runtime, F>(
    app: tauri::AppHandle<R>,
    label: &'static str,
    workspace: super::machines::RuntimeWorkspaceReference,
    verb: F,
) -> Result<(), String>
where
    F: Fn(&dyn TerminalDomain, &WorkspaceKey) -> Result<(), LayoutError> + Send + 'static,
{
    await_blocking(label, move || match workspace {
        super::machines::RuntimeWorkspaceReference::Admitted(target) => {
            let backend = pty_backend::resolve_member_backend(&app, &target.runtime, false)?;
            backend.revalidate(&app)?;
            verb(&*backend.domain(), &target.workspace).map_err(|error| error.to_string())?;
            backend.revalidate(&app)
        }
        super::machines::RuntimeWorkspaceReference::Legacy(key) => {
            pty_backend::in_workspace_hosts(&app, |host| verb(host, &key))
        }
    })
    .await
}

/// How a layout tab's session is spawned: a command, or a shell preferring the
/// warm pool.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabSpawn {
    Cold(Box<SpawnPtyArgs>),
    Warm(AttachWarmArgs),
}

/// A `terminal_open_tab` request; the session id is the tab id.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTabRequest {
    pub machine_id: String,
    pub workspace: WorkspaceKey,
    pub session: PtySessionId,
    pub descriptor: Option<WorkspaceDescriptor>,
    pub name: Option<String>,
    pub role: Option<String>,
    #[serde(default)]
    pub tags: Tags,
    pub placement: Placement,
    pub index: Option<usize>,
    pub activate: bool,
    pub spawn: TabSpawn,
}

/// The app launch the webview last asked the hosts' layout under, so a read
/// on its behalf (the MCP viewer state) asks under the same one — a read under
/// another would prune the exited tabs the webview still shows.
static WEBVIEW_BOOT: Mutex<Option<ClientBootId>> = Mutex::new(None);

/// Record `id` as the webview's boot. True for a new boot (the first ask, or
/// a reload), which re-syncs the connection manager with the registry.
fn adopt_webview_boot(slot: &Mutex<Option<ClientBootId>>, id: &ClientBootId) -> bool {
    let previous = slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace(id.clone());
    previous.as_ref() != Some(id)
}

/// Every host's layout as the webview sees it: one snapshot per host, each
/// keyed by its own `epoch`.
#[tauri::command]
pub async fn terminal_layout_snapshot(
    app: tauri::AppHandle,
    client_boot_id: ClientBootId,
) -> Result<Vec<super::machines::RuntimeLayoutReply>, String> {
    let new_boot = adopt_webview_boot(&WEBVIEW_BOOT, &client_boot_id);
    if new_boot {
        let _ = app.emit("sessions-source-changed", ());
        // A new webview boot is the frontend re-registering: re-read the
        // machine registry so connect never meets a machine the list shows
        // but the connection manager lost.
        let sync_app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = super::machines::sync_connection_manager(&sync_app).await {
                log::warn!(target: "machines", "refresh connection registry: {error}");
            }
        });
    }
    let read_app = app.clone();
    let replies = await_blocking("terminal_layout_snapshot", move || {
        pty_backend::admitted_layout_snapshots(&read_app, &client_boot_id)
    })
    .await?;
    // Read under the webview's own boot, so the restore never prunes what it shows.
    super::dashboard_launch::spawn_local_launch_restore(&app, &replies);
    // SV-7: a read of the layout is the app settling what it hosts: claim it
    // (a restore still running claims as it settles).
    let claim_app = app.clone();
    let _ = await_blocking("claim held sessions", move || {
        pty_backend::claim_held_sessions(&claim_app, new_boot);
        Ok(())
    })
    .await;
    Ok(replies)
}

/// The tag a remote tab names its saved agent with: only the app's remote
/// launch writes it (SW-AGR-2: it makes the tab a wake candidate).
const TAG_REMOTE_AGENT_KEY: &str = "remoteAgentKey";

/// The remote launch tags only the app writes, beside the agent key: what a
/// recovery from the layout trusts (SW-AGR-12: a durable routine launch, its
/// prepared workspace, its launch-if-absent).
const APP_REMOTE_TAGS: [&str; 4] = [
    TAG_REMOTE_AGENT_KEY,
    remote_shell::TAG_REMOTE_DURABLE,
    "remotePreparedWorkspace",
    codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT,
];

/// Refuse a renderer tag write naming launch metadata, which only the app writes.
fn refuse_launch_tags<'a>(mut keys: impl Iterator<Item = &'a String>) -> Result<(), String> {
    match keys.find(|key| {
        super::dashboard_launch::is_launch_tag(key) || APP_REMOTE_TAGS.contains(&key.as_str())
    }) {
        Some(key) => Err(format!(
            "invalid tag {key:?}: launch tags are written by the app only"
        )),
        None => Ok(()),
    }
}

/// Every host's layout as the webview sees it, read without changing what it
/// sees — `None` before the webview first asked for it.
pub(crate) async fn webview_layout_snapshots(
    app: tauri::AppHandle,
) -> Option<Result<Vec<LayoutSnapshot>, String>> {
    let boot = WEBVIEW_BOOT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    Some(
        await_blocking("webview_layout_snapshots", move || {
            pty_backend::layout_snapshots(&app, &boot)
        })
        .await,
    )
}

#[tauri::command]
pub async fn terminal_open_tab(
    app: tauri::AppHandle,
    request: OpenTabRequest,
) -> Result<TabEntry, String> {
    let OpenTabRequest {
        machine_id,
        workspace,
        session,
        descriptor,
        name,
        role,
        tags,
        placement,
        index,
        activate,
        spawn,
    } = request;
    refuse_launch_tags(tags.keys())?;
    let open = OpenTab {
        viewer: app_viewer(),
        workspace,
        session,
        descriptor,
        name,
        role,
        tags,
        placement,
        index,
        activate,
    };
    open_layout_tab(app, machine_id, open, spawn).await
}

pub(crate) fn webview_boot() -> Option<ClientBootId> {
    WEBVIEW_BOOT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Every admitted host's layout, read now on this thread (blocking); `None` before the
/// webview has booted.
pub(crate) fn member_layout_snapshots_blocking(
    app: &tauri::AppHandle,
) -> Option<Result<Vec<super::machines::RuntimeLayoutReply>, String>> {
    let boot = WEBVIEW_BOOT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()?;
    Some(pty_backend::admitted_layout_snapshots(app, &boot))
}

/// Every admitted host's layout, read off the main thread; `None` before the webview has
/// booted.
pub(crate) async fn webview_member_layout_snapshots(
    app: tauri::AppHandle,
) -> Option<Result<Vec<super::machines::RuntimeLayoutReply>, String>> {
    webview_layout_replies(app, true).await
}

/// [`webview_member_layout_snapshots`] of this Mac's own runtimes: no machine is asked,
/// so a slow or unreachable one never delays the read.
pub(crate) async fn webview_local_layout_snapshots(
    app: tauri::AppHandle,
) -> Option<Result<Vec<super::machines::RuntimeLayoutReply>, String>> {
    webview_layout_replies(app, false).await
}

async fn webview_layout_replies(
    app: tauri::AppHandle,
    machines: bool,
) -> Option<Result<Vec<super::machines::RuntimeLayoutReply>, String>> {
    let boot = WEBVIEW_BOOT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()?;
    let label = if machines {
        "webview_member_layout_snapshots"
    } else {
        "webview_local_layout_snapshots"
    };
    Some(
        await_blocking(label, move || {
            pty_backend::admitted_layout_snapshots_of(&app, &boot, machines)
        })
        .await,
    )
}

/// Open a layout tab — its session spawned from `spawn` on the host the tab
/// is placed on, atomically — for every start path: the frontend's tabs, the
/// MCP verbs' tabs and agent sessions. Nothing outside the layout is started.
/// SW-AN-41: a failed open never inserts a session, so its teardown (which
/// reaps a scoped launch's 0600 MCP config and what an agent launch staged for
/// the session) never fires — both are reaped here instead (a no-op for a plain
/// launch).
pub(crate) async fn open_layout_tab(
    app: tauri::AppHandle,
    machine_id: String,
    open: OpenTab,
    spawn: TabSpawn,
) -> Result<TabEntry, String> {
    let session_id = open.session.as_str().to_string();
    let reap = session_id.clone();
    let reaper = app.clone();
    await_blocking("terminal_open_tab", move || {
        let spawner = app.clone();
        pty_backend::open_tab(&app, &machine_id, open, |host| {
            let host = SpawnPlacement::Exact(Box::new(host));
            tauri::async_runtime::block_on(async move {
                match spawn {
                    TabSpawn::Cold(args) => {
                        spawn_from_args(&spawner, session_id, *args, host).await
                    }
                    TabSpawn::Warm(args) => attach_warm_from_args(&spawner, session_id, args, host)
                        .await
                        .map(|_| ()),
                }
            })
        })
    })
    .await
    .inspect_err(|_| {
        super::mcp_scope::remove_scope_config(&reap);
        if let Some(launch) =
            reaper.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        {
            drop(launch.take_staged_agent_launch(&reap));
        }
    })
}

/// What a respawn re-projects for a scoped agent launch: the same inputs the
/// launch gave `project_mcp_scope_config`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespawnScope {
    pub cwd: String,
    pub mcp_allow: Option<Vec<String>>,
    /// Captured effective CodeSense bridge grant; absent/null is unrestricted, [] denies all.
    pub bridge_tools: Option<Vec<String>>,
    pub account: Option<crate::commands::agent_accounts::ResolveAgentAccountInput>,
    #[serde(default)]
    pub launch_args: Option<Vec<String>>,
    pub binary: Option<String>,
    /// A named agent's relaunch, whose own settings file rides its launch line.
    #[serde(default)]
    pub agent: bool,
    /// The launch runs the launcher's default args, not an agent's.
    #[serde(default)]
    pub default_args: bool,
    /// A named agent's own environment variables, set again on the relaunch.
    #[serde(default)]
    pub agent_env: std::collections::BTreeMap<String, String>,
}

impl RespawnScope {
    pub(super) fn take_launch(&mut self) -> crate::commands::mcp_authoring::ScopeLaunch {
        crate::commands::mcp_authoring::ScopeLaunch {
            binary: self.binary.take(),
            agent: self.agent,
            default_args: self.default_args,
            agent_env: std::mem::take(&mut self.agent_env),
            bridge_tools: self.bridge_tools.take(),
            ..Default::default()
        }
    }
}

/// A respawned tab: its entry (same id, same place, now live) and, for a
/// scoped launch, the fresh scope's launch arguments for the agent's line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RespawnedTab {
    /// Present when the fresh projection already executed the native argv.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<super::mcp_scope::ScopedExecution>,
    pub entry: TabEntry,
    pub extra_args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_args: Option<Vec<String>>,
}

/// Start a new process in `session`'s exited tab, under the same session id
/// and in the same place (its workspace, position, name, role and tags), for
/// Restart and Relaunch on an ended pane:
///
/// - refused while the tab or its process is still running;
/// - the ended session is reaped on the host that holds it — the sidecar
///   answers once its teardown is complete — keeping the tab, its dashboard
///   binding and its launch provenance (nothing is audited as a kill);
/// - its old scoped MCP config is removed; with `scope`, a fresh one is
///   projected (a new one-use environment binding rides `spawn`);
/// - `spawn` runs pinned to that same host, and the tab turns live.
///
/// Returns the tab and the fresh scope's launch arguments (empty without
/// `scope`). A spawn that fails after the projection removes the fresh config
/// again, and the tab stays exited; a refused respawn touches nothing.
#[tauri::command]
pub async fn terminal_respawn_tab(
    app: tauri::AppHandle,
    machine_id: String,
    session: RuntimeSessionReference,
    spawn: TabSpawn,
    scope: Option<RespawnScope>,
) -> Result<RespawnedTab, String> {
    await_blocking("terminal_respawn_tab", move || {
        let backend = pty_backend::resolve_session_reference(&app, &session, true)?;
        if backend.machine_id() != machine_id {
            return Err("session target belongs to another machine".into());
        }
        // Refused before the ended session is reaped, so a refusal touches nothing.
        if let Some(client) =
            crate::commands::pty_backend::ResolvedTerminalBackend::client(&backend)
        {
            client.require_agent_env(
                scope
                    .as_ref()
                    .is_some_and(|scope| !scope.agent_env.is_empty()),
            )?;
        }
        let session = PtySessionId::new(session.session());
        let spawner = app.clone();
        let session_id = session.as_str().to_string();
        let remove_args = std::sync::Mutex::new(None);
        let execution = Mutex::new(None);
        let projection_backend = backend.clone();
        let project = |mut scope: RespawnScope| {
            let (app, session) = (spawner.clone(), session_id.clone());
            let remove_args = &remove_args;
            let execution = &execution;
            let backend = projection_backend.clone();
            async move {
                let launch = scope.take_launch();
                let outcome =
                    crate::commands::mcp_authoring::project_mcp_scope_config_with_backend(
                        app,
                        session,
                        scope.cwd,
                        scope.mcp_allow,
                        scope.account,
                        scope.launch_args,
                        Some(launch),
                        Some(backend),
                    )
                    .await?;
                *remove_args.lock().unwrap_or_else(|e| e.into_inner()) =
                    outcome.remove_args.clone();
                *execution.lock().unwrap_or_else(|e| e.into_inner()) = outcome.execution.clone();
                Ok(outcome)
            }
        };
        let mut extra_args = Vec::new();
        let (entry, revived_args) = scoped_respawn(
            scope,
            |pending| {
                pty_backend::respawn_tab_admitted(&app, backend, &session, |host| {
                    let host = SpawnPlacement::Exact(Box::new(host));
                    tauri::async_runtime::block_on(async {
                        let project = pending.take().map(project);
                        extra_args =
                            spawn_with_fresh_scope(&session_id, project, |binding, direct| async {
                                match apply_respawn_projection(spawn, binding, direct)? {
                                    TabSpawn::Cold(args) => {
                                        spawn_from_args(&spawner, session_id.clone(), *args, host)
                                            .await
                                    }
                                    TabSpawn::Warm(args) => attach_warm_from_args(
                                        &spawner,
                                        session_id.clone(),
                                        args,
                                        host,
                                    )
                                    .await
                                    .map(|_| ()),
                                }
                            })
                            .await?;
                        Ok(())
                    })
                })
            },
            |scope| {
                let outcome = tauri::async_runtime::block_on(project(scope))?;
                // Its process already runs: the one-use spawn environment is moot.
                let _ = crate::commands::agent_accounts::consume_environment_binding(
                    &spawner,
                    outcome.environment_binding.as_deref(),
                );
                Ok(outcome.extra_args)
            },
        )?;
        Ok(RespawnedTab {
            execution: execution.into_inner().unwrap_or_else(|e| e.into_inner()),
            entry,
            extra_args: revived_args.unwrap_or(extra_args),
            remove_args: remove_args.into_inner().unwrap_or_else(|e| e.into_inner()),
        })
    })
    .await
}

/// A respawn with an optional scope for its agent's line. `respawn` hands the
/// scope to its spawn by taking it from the slot; when the host revived an
/// earlier respawn's process instead (whose finish never arrived), the spawn
/// never ran and the scope is still there: it is projected now, so the line
/// still gets the scope's launch arguments (`Some`) rather than none.
pub(crate) fn scoped_respawn<S, T>(
    scope: Option<S>,
    respawn: impl FnOnce(&mut Option<S>) -> Result<T, String>,
    reproject: impl FnOnce(S) -> Result<Vec<String>, String>,
) -> Result<(T, Option<Vec<String>>), String> {
    let mut slot = scope;
    let respawned = respawn(&mut slot)?;
    let revived_args = slot.map(reproject).transpose()?;
    Ok((respawned, revived_args))
}

/// Apply the fresh projection to the actual spawn request before any process starts.
pub(crate) fn apply_respawn_projection(
    mut spawn: TabSpawn,
    binding: Option<String>,
    execution: Option<super::mcp_scope::ScopedExecution>,
) -> Result<TabSpawn, String> {
    match &mut spawn {
        TabSpawn::Cold(args) => {
            if let Some(execution) = execution {
                args.command_argv = Some(execution.argv);
                args.native_launch = Some(NativeLaunch::Cli(execution.native_launch));
            }
            if binding.is_some() {
                args.environment_binding = binding;
            }
        }
        TabSpawn::Warm(_) if execution.is_some() => {
            return Err("a scoped direct relaunch requires a cold spawn".into());
        }
        TabSpawn::Warm(_) => {}
    }
    Ok(spawn)
}

/// A respawn's spawn with an optional fresh scope: `project` runs first and its
/// one-use environment binding rides `spawn`; the scope's launch arguments are
/// returned. A spawn that fails after the projection removes the fresh config
/// again — it has no session to be reaped with.
pub(crate) async fn spawn_with_fresh_scope<P, S, F>(
    session_id: &str,
    project: Option<P>,
    spawn: S,
) -> Result<Vec<String>, String>
where
    P: std::future::Future<Output = Result<super::mcp_scope::ScopeOutcome, String>>,
    S: FnOnce(Option<String>, Option<super::mcp_scope::ScopedExecution>) -> F,
    F: std::future::Future<Output = Result<(), String>>,
{
    let (extra_args, binding, execution, projected) = match project {
        Some(project) => {
            let outcome = project.await?;
            (
                outcome.extra_args,
                outcome.environment_binding,
                outcome.execution,
                true,
            )
        }
        None => (Vec::new(), None, None, false),
    };
    spawn(binding, execution).await.inspect_err(|_| {
        if projected {
            super::mcp_scope::remove_scope_config(session_id);
        }
    })?;
    Ok(extra_args)
}

#[tauri::command]
pub async fn terminal_close_tab(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    mode: CloseMode,
) -> Result<CloseOutcome, String> {
    let remote = match &session {
        RuntimeSessionReference::Admitted(target) => Some((**target).clone()),
        RuntimeSessionReference::Legacy(_) => None,
    };
    let launch_app = app.clone();
    let outcome = in_session_host(
        app,
        "terminal_close_tab",
        session,
        move |host, session, _| host.close_tab(&app_viewer(), session, mode),
    )
    .await;
    // A remote tab closed for good is no longer one of this app's launches; one
    // moved to the background still runs and stays.
    if let (Some(target), Ok(CloseOutcome::Killed | CloseOutcome::Removed)) = (remote, &outcome) {
        let launch = launch_app.state::<super::dashboard_launch::DashboardLaunchState>();
        // By machine and session: the admission a close names moves on each
        // reconnect, the launch's record keeps the one it opened under.
        if let Some(record) = launch
            .remote_shell_on_machine(&target.runtime.member.machine_id, target.session.as_str())
        {
            launch.abort(&record.handle);
            super::dashboard_launch::emit_session_state_changed(&launch_app);
        }
    }
    outcome
}

#[tauri::command]
pub async fn terminal_place_tab(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    workspace: WorkspaceKey,
    placement: Placement,
    index: Option<usize>,
    descriptor: Option<WorkspaceDescriptor>,
) -> Result<(), String> {
    in_session_host(
        app,
        "terminal_place_tab",
        session,
        move |host, session, remote| {
            // A remote tab moves only within its own workspace there (Hide, back to
            // the strip): never into another team's or workspace's layout.
            within_remote_workspace(remote, &workspace)?;
            host.place_tab(session, &workspace, placement, index, descriptor)
        },
    )
    .await
}

#[tauri::command]
pub async fn terminal_rename_tab(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    name: String,
) -> Result<(), String> {
    in_session_host(
        app,
        "terminal_rename_tab",
        session,
        move |host, session, _| host.rename_tab(session, name),
    )
    .await
}

#[tauri::command]
pub async fn terminal_move_tab(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    to_index: usize,
) -> Result<(), String> {
    in_session_host(
        app,
        "terminal_move_tab",
        session,
        move |host, session, _| host.move_tab(session, to_index),
    )
    .await
}

/// Focus `session` in `workspace` — in the session's host — or clear the
/// focus in every host holding the workspace.
#[tauri::command]
pub async fn terminal_set_active(
    app: tauri::AppHandle,
    workspace: super::machines::RuntimeWorkspaceReference,
    session: Option<RuntimeSessionReference>,
) -> Result<(), String> {
    if let (super::machines::RuntimeWorkspaceReference::Admitted(workspace), Some(session)) =
        (&workspace, &session)
    {
        match session {
            RuntimeSessionReference::Admitted(session) if session.runtime == workspace.runtime => {}
            _ => {
                return Err(
                    "active session does not belong to the selected workspace member".into(),
                )
            }
        }
    }
    match session {
        Some(session) => {
            in_session_host(
                app,
                "terminal_set_active",
                session,
                move |host, session, remote| {
                    within_remote_workspace(remote, workspace.workspace())?;
                    host.set_active(&app_viewer(), workspace.workspace(), Some(session))
                },
            )
            .await
        }
        None => {
            in_workspace_members(
                app,
                "terminal_set_active",
                workspace,
                move |host, workspace| host.set_active(&app_viewer(), workspace, None),
            )
            .await
        }
    }
}

#[tauri::command]
pub async fn terminal_set_grid(
    app: tauri::AppHandle,
    workspace: super::machines::RuntimeWorkspaceReference,
    grid: GridState,
) -> Result<(), String> {
    in_workspace_hosts(
        app,
        "terminal_set_grid",
        workspace,
        move |host, workspace| host.set_grid(workspace, grid.clone()),
    )
    .await
}

#[tauri::command]
pub async fn terminal_set_view_hint(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    hint: ViewHint,
) -> Result<(), String> {
    in_session_host(
        app,
        "terminal_set_view_hint",
        session,
        move |host, session, _| host.set_view_hint(&app_viewer(), session, hint),
    )
    .await
}

#[tauri::command]
pub async fn terminal_set_tags(
    app: tauri::AppHandle,
    session: RuntimeSessionReference,
    patch: TagPatch,
) -> Result<(), String> {
    refuse_launch_tags(patch.keys())?;
    in_session_host(
        app,
        "terminal_set_tags",
        session,
        move |host, session, _| host.set_tags(session, patch),
    )
    .await
}

#[tauri::command]
pub async fn terminal_set_descriptor(
    app: tauri::AppHandle,
    workspace: super::machines::RuntimeWorkspaceReference,
    descriptor: WorkspaceDescriptor,
) -> Result<(), String> {
    in_workspace_hosts(
        app,
        "terminal_set_descriptor",
        workspace,
        move |host, workspace| host.set_descriptor(workspace, descriptor.clone()),
    )
    .await
}

#[tauri::command]
pub async fn terminal_close_workspace(
    app: tauri::AppHandle,
    workspace: super::machines::RuntimeWorkspaceReference,
) -> Result<(), String> {
    in_workspace_hosts(
        app,
        "terminal_close_workspace",
        workspace,
        move |host, workspace| host.close_workspace(workspace),
    )
    .await
}

#[cfg(test)]
mod tests {

    #[test]
    fn managed_activity_preparation_failure_never_falls_back_to_a_plain_launch() {
        let line = super::observed_launch_line(
            Err(codesense_pty_core::domain::PortUnavailable(
                "host update required".into(),
            )),
            "claude --continue",
        );
        assert_eq!(line.unwrap_err(), "host update required");
    }
    use super::*;

    /// A relaunch's scope carries the named agent's env as the renderer sends it.
    #[test]
    fn a_relaunch_scope_carries_the_agents_env() {
        let scope: RespawnScope = serde_json::from_value(serde_json::json!({
            "cwd": "/wt",
            "mcpAllow": [],
            "binary": "claude",
            "agent": true,
            "agentEnv": { "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1" },
        }))
        .unwrap();
        assert_eq!(
            scope
                .agent_env
                .get("CLAUDE_CODE_DISABLE_AUTO_MEMORY")
                .map(String::as_str),
            Some("1")
        );
        let plain: RespawnScope =
            serde_json::from_value(serde_json::json!({ "cwd": "/wt", "mcpAllow": null })).unwrap();
        assert!(plain.agent_env.is_empty());
    }

    #[test]
    fn the_renderer_cannot_write_launch_tags() {
        let patch = TagPatch::from([("hidden".to_string(), Some("tab".to_string()))]);
        assert_eq!(refuse_launch_tags(patch.keys()), Ok(()));
        for key in [
            "launchHandle",
            "launchAgentRevision",
            "launcher",
            "launchTeam",
        ] {
            let forged = TagPatch::from([
                ("hidden".to_string(), None),
                (
                    key.to_string(),
                    Some("acme/backend/controller@2".to_string()),
                ),
            ]);
            let refused = refuse_launch_tags(forged.keys()).unwrap_err();
            assert!(refused.starts_with("invalid tag"), "{refused}");
        }
        let tags = Tags::from([("launchHandle".to_string(), "h".to_string())]);
        assert!(refuse_launch_tags(tags.keys()).is_err());
        // SW-AGR-2: nor can it forge the agent a remote tab runs.
        let forged = TagPatch::from([("remoteAgentKey".to_string(), Some("acme/x/barb".into()))]);
        assert!(refuse_launch_tags(forged.keys()).is_err());
        // SW-AGR-12: nor the marks a recovery trusts: a durable launch, its
        // prepared workspace, its launch-if-absent.
        for key in [
            "remoteDurable",
            "remotePreparedWorkspace",
            "remoteLaunchIfAbsent",
        ] {
            let forged = TagPatch::from([(key.to_string(), Some("1".to_string()))]);
            let refused = refuse_launch_tags(forged.keys()).unwrap_err();
            assert!(refused.starts_with("invalid tag"), "{key}: {refused}");
            let tags = Tags::from([(key.to_string(), "1".to_string())]);
            assert!(refuse_launch_tags(tags.keys()).is_err(), "{key}");
        }
    }

    #[test]
    fn only_a_new_webview_boot_re_syncs_the_machine_registry() {
        let slot = Mutex::new(None);
        let first = ClientBootId::new("boot-1");
        assert!(adopt_webview_boot(&slot, &first), "first ask");
        assert!(!adopt_webview_boot(&slot, &first), "same boot polling");
        let reload = ClientBootId::new("boot-2");
        assert!(adopt_webview_boot(&slot, &reload), "webview reload");
        assert!(!adopt_webview_boot(&slot, &reload));
        assert_eq!(*slot.lock().unwrap(), Some(reload));
    }

    use crate::test_support::source_guard::production_text;

    /// The production source (each file's text before its test module) of
    /// every `.rs` file under `src/` but the test modules' own files, by path.
    fn production_sources() -> Vec<(std::path::PathBuf, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && path.file_name().is_some_and(|name| name != "tests.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let production = production_text(&text);
                    out.push((path, production));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut out,
        );
        out
    }

    /// The files whose production code calls `needle` (outside comments and
    /// the callee's own definition).
    fn files_calling(needle: &str) -> Vec<String> {
        let definition = format!("fn {needle}");
        let mut files: Vec<String> = production_sources()
            .into_iter()
            .filter(|(_, text)| {
                text.lines().any(|line| {
                    !line.trim_start().starts_with("//")
                        && !line.contains(&definition)
                        && line.contains(needle)
                })
            })
            .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        files.sort();
        files
    }

    /// AC-9: the key carries the team, so two teams' same-named workspaces —
    /// a `root:owner/repo`, every team's `git-sync` — never share a layout, and
    /// a story keys by its `story_id` as the frontend's story workspace does.
    #[test]
    fn the_workspace_key_carries_the_team() {
        let core = team_key("acme", Some("core"));
        let infra = team_key("acme", Some("infra"));
        assert_eq!(core, "acme:core");
        assert_eq!(team_key("acme", None), "acme:_none");
        assert_eq!(parse_team_key(&core), ("acme", Some("core")));
        assert_eq!(
            parse_team_key(&team_key("acme/app", None)),
            ("acme/app", None)
        );
        assert_eq!(
            workspace_key(&core, "SW-RT-7b").as_str(),
            "acme:core|SW-RT-7b"
        );
        assert_ne!(
            workspace_key(&core, "notes"),
            workspace_key(&infra, "notes")
        );
        assert_ne!(
            workspace_key(&core, "root:acme/app"),
            workspace_key(&infra, "root:acme/app")
        );
    }

    /// A remote Machine's layout verb is authorized for the team its workspace
    /// key carries: two teams' same workspace id name two different teams, and
    /// a key without a team names none.
    #[test]
    fn a_remote_verb_names_the_team_of_its_workspace_key() {
        let core = workspace_key(&team_key("acme", Some("core")), "W");
        let infra = workspace_key(&team_key("acme", Some("infra")), "W");
        assert_eq!(workspace_team_key(&core), Some("acme:core"));
        assert_eq!(workspace_team_key(&infra), Some("acme:infra"));
        assert_eq!(workspace_team_key(&WorkspaceKey::new("no-team")), None);
        assert_eq!(workspace_team_key(&WorkspaceKey::new("|W")), None);
    }

    /// A remote layout verb runs only under the current private lease of the
    /// app's own private owner: a revoked lease, a missing one, or another
    /// owner's is refused.
    #[test]
    fn a_remote_verb_is_refused_without_its_owners_current_lease() {
        let (lease, revoke) = crate::commands::auth::private_authority::test_lease(41);
        assert_eq!(current_private_lease(Some(&lease), Ok(41)), Ok(()));
        assert!(current_private_lease(Some(&lease), Ok(42)).is_err());
        assert!(current_private_lease(Some(&lease), Err("no private owner".into())).is_err());
        assert!(current_private_lease(None, Ok(41)).is_err());
        revoke();
        assert!(current_private_lease(Some(&lease), Ok(41)).is_err());
    }

    /// A remote tab is moved or focused only within the workspace it is in on
    /// its Machine; This Mac's tabs are not limited.
    #[test]
    fn a_remote_tab_never_moves_into_another_workspace() {
        let core = workspace_key(&team_key("acme", Some("core")), "W");
        let infra = workspace_key(&team_key("acme", Some("infra")), "W");
        assert!(within_remote_workspace(Some(&core), &core).is_ok());
        assert!(matches!(
            within_remote_workspace(Some(&core), &infra),
            Err(LayoutError::UnknownWorkspace(key)) if key == infra
        ));
        assert!(within_remote_workspace(None, &infra).is_ok());
    }

    /// A remote verb reaches a Machine only for a team it is visible to: a
    /// personal machine, or one shared with that very team — never one shared
    /// with another team, This Mac, or an unlisted id.
    #[test]
    fn a_remote_verb_is_refused_for_a_machine_hidden_from_the_team() {
        use super::super::machines::registry::Scope;
        use super::super::machines::{
            terminal_machine_visible, MachineCapabilities, MachineStatus, MachineSummary,
        };
        let machine = |id: &str, kind: &'static str, scope: Option<Scope>| MachineSummary {
            machine_id: id.into(),
            kind,
            label: id.into(),
            scope,
            declaration: None,
            binding: None,
            can_publish: true,
            remote_mcp_control: false,
            status: MachineStatus::Idle,
            attention: 0,
            capabilities: MachineCapabilities {
                agent_callbacks: false,
            },
            machine_stats: None,
            machine_stats_supported: None,
            terminal_server: None,
        };
        let team = |team: &str| {
            Some(Scope::Team {
                org_slug: "acme".into(),
                team: team.into(),
            })
        };
        let listed = [
            machine("personal", "ssh", Some(Scope::Personal)),
            machine("core-box", "ssh", team("core")),
            machine("infra-box", "ssh", team("infra")),
            machine("local", "local", None),
        ];
        assert!(terminal_machine_visible(
            &listed, "personal", "acme", "core"
        ));
        assert!(terminal_machine_visible(
            &listed, "core-box", "acme", "core"
        ));
        assert!(!terminal_machine_visible(
            &listed,
            "infra-box",
            "acme",
            "core"
        ));
        assert!(!terminal_machine_visible(
            &listed, "core-box", "other", "core"
        ));
        assert!(!terminal_machine_visible(&listed, "local", "acme", "core"));
        assert!(!terminal_machine_visible(&listed, "gone", "acme", "core"));
    }

    /// AC-9: `workspace_key` is the ONE place a workspace key is minted.
    #[test]
    fn the_workspace_key_is_minted_in_one_place() {
        assert_eq!(files_calling("WorkspaceKey::new("), ["pty_layout.rs"]);
    }

    /// Every terminal the user, a dashboard, or an MCP verb starts is opened
    /// through the host's layout — spawned and placed at once by
    /// `open_layout_tab` — so nothing started outside it is left for the quit
    /// reap. Only the warm pool spawns outside it, its members unplaced until a
    /// layout open hands one off.
    #[test]
    fn every_start_path_opens_through_the_layout() {
        assert_eq!(files_calling("spawn_from_args("), ["pty_layout.rs"]);
        assert_eq!(files_calling("attach_warm_from_args("), ["pty_layout.rs"]);
        assert_eq!(files_calling("spawn_pty_inner("), ["pty.rs", "pty_host.rs"]);
        let open_paths = files_calling("open_layout_tab(");
        for caller in ["mcp_workspace.rs", "handler.rs", "pty_layout.rs"] {
            assert!(
                open_paths.iter().any(|f| f == caller),
                "{caller} opens through the layout"
            );
        }
    }

    /// The wire shape the frontend sends: a camelCase request whose spawn
    /// source is tagged by `kind` and carries the existing spawn inputs.
    #[test]
    fn an_open_tab_request_deserializes_from_the_frontend_shape() {
        let request: OpenTabRequest = serde_json::from_value(serde_json::json!({
            "machineId": "local",
            "workspace": "root:acme/app",
            "session": "shell-1",
            "descriptor": { "displayName": "app", "kind": "root", "worktreePath": "/w" },
            "placement": "tab",
            "activate": true,
            "spawn": {
                "kind": "warm",
                "targetCwd": "/w",
                "size": { "cols": 80, "rows": 24 },
                "workspaceId": "root:acme/app"
            }
        }))
        .expect("the frontend shape deserializes");
        assert_eq!(request.machine_id, "local");
        assert_eq!(request.session.as_str(), "shell-1");
        assert_eq!(
            request.descriptor.map(|d| d.display_name).as_deref(),
            Some("app")
        );
        assert!(request.tags.is_empty() && request.index.is_none());
        match request.spawn {
            TabSpawn::Warm(args) => {
                assert_eq!(args.target_cwd, "/w");
                assert_eq!(args.workspace_id.as_deref(), Some("root:acme/app"));
            }
            TabSpawn::Cold(_) => panic!("expected a warm spawn"),
        }

        let cold: TabSpawn = serde_json::from_value(serde_json::json!({
            "kind": "cold",
            "command": "claude",
            "cwd": "/w",
            "size": { "cols": 80, "rows": 24 },
            "interactive": true
        }))
        .expect("a cold spawn deserializes");
        match cold {
            TabSpawn::Cold(args) => assert_eq!(args.interactive, Some(true)),
            TabSpawn::Warm(_) => panic!("expected a cold spawn"),
        }
    }

    /// A Relaunch the host answers by reviving an earlier respawn's process (its
    /// spawn never runs) still gets the scope's launch arguments — its line
    /// keeps its MCP allowlist; a spawned respawn's scope is projected once, by
    /// its spawn; an unscoped one projects nothing.
    #[test]
    fn a_revived_relaunch_still_gets_its_scope_args() {
        let reproject = |scope: &str| Ok(vec!["--mcp-config".to_string(), format!("/{scope}")]);

        let revived = scoped_respawn(Some("s1"), |_slot| Ok("entry"), reproject).unwrap();
        assert_eq!(
            revived,
            (
                "entry",
                Some(vec!["--mcp-config".to_string(), "/s1".to_string()])
            )
        );

        let spawned = scoped_respawn(
            Some("s1"),
            |slot| {
                assert_eq!(slot.take(), Some("s1"), "the spawn projects it");
                Ok("entry")
            },
            |_| panic!("a spawned respawn is not projected twice"),
        )
        .unwrap();
        assert_eq!(spawned, ("entry", None));

        let unscoped = scoped_respawn(None::<&str>, |_| Ok("entry"), |_| panic!("no scope"));
        assert_eq!(unscoped, Ok(("entry", None)));

        let refused = scoped_respawn(
            Some("s1"),
            |_| Err::<&str, _>("still running".to_string()),
            |_| panic!("a refused respawn projects nothing"),
        );
        assert_eq!(refused, Err("still running".to_string()));
    }

    /// A respawn's glue: the fresh scope's binding rides the spawn and its
    /// arguments come back; a spawn that fails after the projection removes
    /// the fresh config; without a scope nothing is projected or removed.
    #[test]
    fn a_respawn_spawn_carries_the_fresh_scope_and_removes_it_on_failure() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use crate::commands::mcp_scope::{mcp_scope_session_dir, ScopeOutcome};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let id = format!("respawn-glue-{}", std::process::id());
        let project = |id: String| async move {
            let dir = mcp_scope_session_dir(&id);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Ok(ScopeOutcome {
                execution: Some(crate::commands::mcp_scope::ScopedExecution {
                    argv: vec!["/fresh/agent".into(), "chat".into()],
                    native_launch: codesense_pty_core::agent_cli::AgentCli::Hermes,
                }),
                extra_args: vec!["--mcp-config".into(), dir.display().to_string()],
                environment_binding: Some("binding-2".into()),
                ..ScopeOutcome::default()
            })
        };

        let (mut seen, mut direct) = (None, None);
        let args = runtime
            .block_on(spawn_with_fresh_scope(
                &id,
                Some(project(id.clone())),
                |binding, execution| {
                    seen = binding;
                    direct = execution;
                    async { Ok(()) }
                },
            ))
            .unwrap();
        assert_eq!(seen.as_deref(), Some("binding-2"));
        assert_eq!(
            direct.map(|execution| execution.argv),
            Some(vec!["/fresh/agent".to_string(), "chat".to_string()]),
            "the fresh projection's execution reaches the spawn"
        );
        assert_eq!(args[0], "--mcp-config");
        assert!(
            mcp_scope_session_dir(&id).exists(),
            "a spawned respawn keeps it"
        );

        crate::commands::mcp_scope::remove_scope_config(&id);
        let failed = runtime.block_on(spawn_with_fresh_scope(
            &id,
            Some(project(id.clone())),
            |_, _| async { Err("spawn_pty: no shell".to_string()) },
        ));
        assert_eq!(failed, Err("spawn_pty: no shell".to_string()));
        assert!(
            !mcp_scope_session_dir(&id).exists(),
            "a failed spawn removes it"
        );

        let unscoped = runtime.block_on(spawn_with_fresh_scope(
            &id,
            None::<std::future::Ready<Result<ScopeOutcome, String>>>,
            |binding, _execution| {
                assert!(binding.is_none());
                async { Ok(()) }
            },
        ));
        assert_eq!(unscoped, Ok(Vec::new()));
    }
}

mod remote_shell;
pub use remote_shell::{
    RemotePermissionMode, RemoteShellTabRequest, RemoteTabOutcome, RemoteTabReference,
};

#[tauri::command]
pub async fn terminal_remote_shell_tab(
    app: tauri::AppHandle,
    request: RemoteShellTabRequest,
    frame_id: Option<String>,
    page_path: Option<String>,
) -> RemoteTabOutcome {
    let scope = match crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    ) {
        Ok(scope) => scope,
        Err(error) => return RemoteTabOutcome::refused("notFound", error),
    };
    remote_shell::remote_shell_tab_bound_for(app, request, None, scope).await
}
#[cfg(test)]
pub(crate) use remote_shell::remote_shell_group_in;
pub(crate) use remote_shell::{
    arm_tab_home, authorize_human_terminal, host_home, remote_shell_tab_bound,
    remote_shell_tab_bound_for, restore_on_machine, validate_remote_shell_group, LaunchSettlement,
    LiveRemoteTab, McpInventoryEntry, RemoteLaunchIdentity, RemoteShellRecord,
    LAUNCH_SETTLED_EVENT, LAUNCH_SETTLEMENT_TTL, TAG_REMOTE_DURABLE,
};

async fn open_remote_layout_tab(
    app: tauri::AppHandle,
    backend: pty_backend::ResolvedTerminalBackend,
    open: OpenTab,
    args: SpawnPtyArgs,
    launch: Option<NativeLaunch>,
) -> Result<TabEntry, LayoutError> {
    let session = open.session.as_str().to_string();
    await_blocking("remote_shell_open", move || {
        let spawner = app.clone();
        Ok(pty_backend::open_tab_pinned(
            &app,
            backend,
            open,
            |backend| {
                let mut intent = super::pty::build_remote_spawn_intent(
                    args.command,
                    // SW-WA-F10: a remote-shell tab is a plain interactive
                    // shell — no argv-direct spawn.
                    None,
                    args.cwd,
                    args.size.cols,
                    args.size.rows,
                    args.interactive,
                    args.shell_integration,
                    args.env_remove,
                );
                intent.remote_launch = launch;
                let registration = codesense_pty_core::SessionRegistration {
                    session_id: session,
                    meta: codesense_pty_core::SessionMeta {
                        story_id: args.story_id,
                        workspace_id: args.workspace_id,
                        epic_id: args.epic_id,
                    },
                    home_cwd: intent.spawn_dir(),
                    osc_enabled: intent.osc_enabled(),
                    is_pool_member: false,
                    scrollback: args
                        .scrollback
                        .unwrap_or(codesense_pty_core::emulator::DEFAULT_SCROLLBACK),
                };
                tauri::async_runtime::block_on(pty_backend::spawn_on_session_server(
                    &spawner,
                    backend,
                    intent,
                    registration,
                    None,
                    None,
                ))
            },
        ))
    })
    .await
    .map_err(LayoutError::Unreachable)?
}
