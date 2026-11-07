// SPDX-License-Identifier: MIT
use super::super::dashboard_launch::DashboardLaunchState;
use super::super::machines::{RuntimeMemberTarget, RuntimeSessionTarget};
use super::super::mcp_workspace::{LaunchHome, SessionView};
use super::*;
use crate::content_engine::destination::{DestinationBatchError, TERMINAL_SERVER_OUTDATED};
use crate::terminal::session_server::AccountHomeError;
use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_pty_core::agent_cli::{adapter, descriptor};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum RemoteShellTabRequest {
    #[serde(rename_all = "camelCase")]
    Create {
        request_id: Uuid,
        machine_id: String,
        team_key: String,
        workspace_id: String,
        /// `None` opens in the host account's home, as the host resolves it.
        cwd: Option<String>,
        name: Option<String>,
        #[serde(default)]
        launch: Option<NativeLaunch>,
        /// How a raw claude/codex launch asks before running tools.
        #[serde(default)]
        permission_mode: RemotePermissionMode,
        agent_key: Option<String>,
        prepared_workspace_handle: Option<String>,
        headed: bool,
        /// A saved Agent's first prompt for this launch, in place of the Agent's own:
        /// it starts with it, as a local launch does.
        #[serde(default)]
        initial_prompt: Option<String>,
        /// Who launched it and how it looks. Set only by the app's own launch, never
        /// over the wire.
        #[serde(skip)]
        identity: Box<RemoteLaunchIdentity>,
    },
    #[serde(rename_all = "camelCase")]
    Attach {
        machine_id: String,
        team_key: String,
        reference: RemoteTabReference,
        headed: bool,
    },
}

/// How a raw Claude/Codex launch on a machine handles approvals. A saved Agent
/// declares its own permissions instead.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum RemotePermissionMode {
    /// The runtime's ordinary interactive approvals.
    #[default]
    Default,
    /// No approval prompts: the runtime's bypass flag (Claude
    /// `--dangerously-skip-permissions`, Codex `--dangerously-bypass-approvals-and-sandbox`).
    Bypass,
}

impl RemotePermissionMode {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::Default
    }

    /// The startup a bypass launch hands the machine's launcher: the same shape a
    /// saved Agent uses, so the launcher still adds CodeSense's MCP server, status
    /// hooks and inbox nudges around the runtime's own bypass switch.
    pub(crate) fn startup(
        self,
        launch: NativeLaunch,
    ) -> Option<codesense_host_proto::reverse_mcp::AgentStartup> {
        let cli = match self {
            Self::Default => return None,
            Self::Bypass => launch.cli()?,
        };
        Some(codesense_host_proto::reverse_mcp::AgentStartup {
            args: vec![descriptor(cli).bypass_flag.clone()],
            settings: serde_json::json!({}),
            prompt: None,
            env: Default::default(),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteTabReference {
    Handle(String),
    RequestId(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpInventoryEntry {
    pub id: String,
    pub logical_name: String,
    pub execution_location: String,
    pub availability: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSelection {
    pub key: String,
    pub revision: u32,
    pub manifest_hash: String,
    pub prepared_workspace_handle: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTabOutcome {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub headed: bool,
    pub retryable: bool,
    pub agent: Option<RemoteAgentSelection>,
    pub workspace_id: Option<String>,
    pub cwd: Option<String>,
    /// Backend-issued evidence for the human adapter; every use is revalidated.
    pub target: Option<RuntimeSessionTarget>,
    pub workspace_key: Option<WorkspaceKey>,
    pub(crate) mcp_inventory: Vec<McpInventoryEntry>,
    pub mcp_profile_revision: Option<String>,
    /// SW-XS-7: what Step 2 did for this launch (kept, skipped, pinned
    /// drift, failures), for the MCP result and the workspace details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared: Option<crate::agents::prepare::progress::PrepareSummary>,
}

impl RemoteTabOutcome {
    pub(super) fn refused(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: "refused",
            code: Some(code.into()),
            message: Some(message.into()),
            handle: None,
            request_id: None,
            headed: false,
            retryable: false,
            agent: None,
            workspace_id: None,
            cwd: None,
            target: None,
            workspace_key: None,
            mcp_inventory: Vec::new(),
            mcp_profile_revision: None,
            prepared: None,
        }
    }
    fn unknown(reference: &RemoteTabReference, message: impl Into<String>) -> Self {
        let mut outcome = Self::refused("unconfirmed", message);
        outcome.status = "unknown";
        match reference {
            RemoteTabReference::Handle(handle) => outcome.handle = Some(handle.clone()),
            RemoteTabReference::RequestId(id) => outcome.request_id = Some(*id),
        }
        outcome
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RemoteSpawnPhase {
    Preparing,
    #[default]
    Attempted,
}

/// Who launched a remote agent and how it looks, as its launcher said, and the story
/// it works on: display only, nothing decides anything on it. Apart from
/// `if_absent`, which asks the machine for a launch-if-absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteLaunchIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_context: Option<String>,
    /// SW-AGR-8: a routine wake's launch-if-absent: the machine starts it
    /// only when no session of the agent runs or is being started in the
    /// workspace (its single flight, shared with its own routines).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub if_absent: bool,
    #[serde(default)]
    pub launched_by: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::queue_roles::nullable_role"
    )]
    pub queue_role: Option<Option<String>>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub story: Option<String>,
    /// SW-AGR-12: a routine's launch into a workspace it chose from the
    /// durable preparation receipts: it homes, and passes the receipt gate,
    /// without the viewer having the workspace open. Only the app sets it
    /// (never over the wire), always with a saved Agent and its prepared
    /// workspace handle, so the exact receipt gate still runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub durable_workspace: bool,
}

/// Launch `agent` in the tab `record` opens: it runs the Agent's runtime, and the
/// tab is titled with the Agent's name unless the launch named it.
pub(crate) fn take_agent(
    record: &mut RemoteShellRecord,
    mut agent: crate::agents::remote_launch::CapturedAgent,
) {
    record.launch = NativeLaunch::Cli(agent.runtime);
    if record.name.is_none() {
        record.name = crate::commands::dashboard_launch::launch_label(Some(&agent.name));
    }
    let role = crate::queue_roles::effective_role(
        agent.default_role.as_deref(),
        record.identity.queue_role.as_ref(),
    )
    .ok()
    .flatten();
    record.identity.queue_role = Some(role.clone());
    if let Some(team) = crate::commands::dashboard_launch::launch_team_from_key(&record.team_key) {
        agent
            .startup
            .env
            .extend(super::super::role_startup::environment(
                &team,
                role.as_deref(),
            ));
    }
    record.agent = Some(agent);
}

impl RemoteLaunchIdentity {
    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemoteShellRecord {
    #[serde(default)]
    pub agent: Option<crate::agents::remote_launch::CapturedAgent>,
    #[serde(default)]
    pub phase: RemoteSpawnPhase,
    pub handle: String,
    pub owner_id: u64,
    pub request_id: Uuid,
    pub machine_id: String,
    pub team_key: String,
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_story_id: Option<String>,
    pub workspace: WorkspaceKey,
    pub cwd: String,
    pub name: Option<String>,
    #[serde(default)]
    pub mcp_profile_revision: Option<String>,
    #[serde(default)]
    pub mcp_inventory: Vec<McpInventoryEntry>,
    /// The machine generated this terminal's Claude settings (a launched Claude's,
    /// or a shell's for a `claude` typed there), so its status hooks are its own;
    /// without them a hook is refused.
    #[serde(default)]
    pub hooks: bool,
    #[serde(default)]
    pub launch: NativeLaunch,
    /// How this raw runtime handles approvals; fixed when the tab is created.
    #[serde(default, skip_serializing_if = "RemotePermissionMode::is_default")]
    pub permission_mode: RemotePermissionMode,
    pub target: RuntimeSessionTarget,
    #[serde(skip_deserializing, default = "restored_status")]
    pub status: &'static str,
    /// Who launched it and how it looks; empty for a tab a person opened.
    #[serde(default, skip_serializing_if = "RemoteLaunchIdentity::is_empty")]
    pub identity: RemoteLaunchIdentity,
}

fn restored_status() -> &'static str {
    // Disk remembers consumption, never live runtime authorization.
    "unknown"
}

impl RemoteShellRecord {
    /// The launch request is already durable in the remote restore tags. Reuse
    /// it for output, independently of whether its tab is still retained.
    pub(crate) fn output_run_id(&self) -> Option<String> {
        let cli = self.launch.cli()?;
        adapter(cli).output().session_record?;
        Some(self.request_id.to_string())
    }

    pub(crate) fn preparing(&self) -> bool {
        self.phase == RemoteSpawnPhase::Preparing
    }
    pub(crate) fn mark_spawn_attempted(&mut self) {
        self.phase = RemoteSpawnPhase::Attempted;
    }

    fn matches_agent(&self, key: &Option<String>, handle: &Option<String>) -> bool {
        self.agent.as_ref().map(|a| a.key()) == *key
            && self.saved_agent_workspace_handle() == *handle
    }

    fn matches_intent(
        &self,
        cwd: &str,
        workspace: &str,
        name: &Option<String>,
        launch: NativeLaunch,
        permission_mode: RemotePermissionMode,
    ) -> bool {
        self.cwd == cwd
            && self.workspace_id == workspace
            && self.name == *name
            && self.launch == launch
            && self.permission_mode == permission_mode
    }

    /// The workspace a saved Agent's tab was prepared into, by its handle (`None`
    /// for a raw runtime's tab).
    pub(crate) fn saved_agent_workspace_handle(&self) -> Option<String> {
        self.agent
            .as_ref()
            .filter(|agent| !agent.workspace_handle.is_empty())
            .map(|agent| agent.workspace_handle.clone())
    }

    /// What the machine's launcher starts the runtime with: a saved Agent's own
    /// startup, or a raw runtime's chosen permission mode.
    pub(crate) fn agent_startup(&self) -> Option<codesense_host_proto::reverse_mcp::AgentStartup> {
        match &self.agent {
            Some(agent) => Some(agent.startup.clone()),
            None => self.permission_mode.startup(self.launch),
        }
    }
    fn matches_capture(&self, other: &Self) -> bool {
        self.agent == other.agent
            && self.hooks == other.hooks
            && self.mcp_profile_revision == other.mcp_profile_revision
            && self.mcp_inventory == other.mcp_inventory
    }
    fn outcome(&self, headed: bool) -> RemoteTabOutcome {
        RemoteTabOutcome {
            status: self.status,
            handle: Some(self.handle.clone()),
            request_id: Some(self.request_id),
            code: None,
            message: None,
            headed,
            retryable: self.phase == RemoteSpawnPhase::Preparing,
            agent: self.agent.as_ref().map(|agent| RemoteAgentSelection {
                key: agent.key(),
                revision: agent.manifest.agent.revision,
                manifest_hash: agent.manifest.manifest_hash.clone(),
                prepared_workspace_handle: agent.workspace_handle.clone(),
            }),
            workspace_id: Some(self.workspace_id.clone()),
            cwd: Some(self.cwd.clone()),
            target: Some(self.target.clone()),
            workspace_key: Some(self.workspace.clone()),
            mcp_inventory: self.mcp_inventory.clone(),
            mcp_profile_revision: self.mcp_profile_revision.clone(),
            prepared: None,
        }
    }
}

fn normalized_cwd(cwd: &str) -> Result<String, &'static str> {
    if !cwd.starts_with('/')
        || cwd.contains(['\0', '$', '~'])
        || cwd.split('/').any(|part| part == "." || part == "..")
    {
        return Err("Enter an absolute host directory without expansions or dot components");
    }
    let normalized = format!(
        "/{}",
        cwd.split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    Ok(normalized)
}

/// The connected machine's account home, read by its terminal server.
pub(crate) async fn host_home(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Result<String, AccountHomeError> {
    let client = pty_backend::resolve_spawn_backend(app, machine_id, "")
        .map_err(AccountHomeError::Failed)?
        .client()
        .ok_or_else(|| AccountHomeError::Failed("Remote admission is unavailable".into()))?;
    let home = await_blocking("remote_account_home", move || Ok(client.account_home()))
        .await
        .map_err(AccountHomeError::Failed)??;
    home.to_str().map(str::to_owned).ok_or_else(|| {
        AccountHomeError::Failed("This machine's home folder is not valid UTF-8".into())
    })
}

/// The refusal for a home-folder open the host can't serve. An outdated
/// terminal server gets its own code, so the UI can offer the update.
fn home_refusal(error: AccountHomeError) -> RemoteTabOutcome {
    let code = match error {
        AccountHomeError::OutdatedServer => "terminalServerOutdated",
        AccountHomeError::Failed(_) => "unavailable",
    };
    RemoteTabOutcome::refused(code, error.to_string())
}

fn launch_home(
    app: &tauri::AppHandle,
    team: &str,
    workspace: &str,
    cwd: &str,
    headed: bool,
    durable: bool,
) -> Result<LaunchHome, String> {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    home_from(&viewer, team, workspace, cwd, headed, durable)
}

fn workspace_story_id(app: &tauri::AppHandle, team: &str, workspace: &str) -> Option<String> {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    workspace_story_id_in(&viewer, team, workspace)
}
fn workspace_story_id_in(
    viewer: &serde_json::Value,
    team: &str,
    workspace: &str,
) -> Option<String> {
    viewer
        .get("workspaces")?
        .get(team)?
        .as_array()?
        .iter()
        .find(|row| row.get("id").and_then(|id| id.as_str()) == Some(workspace))?
        .get("storyId")?
        .as_str()
        .map(str::to_string)
}

/// [`launch_home`] from the viewer state `viewer`. A `durable` launch (a
/// routine's, SW-AGR-12) whose workspace the viewer does not have open homes
/// as that repo workspace would: keyed by its id, so it groups with the
/// workspace once it is opened. Every other launch needs the open row.
fn home_from(
    viewer: &serde_json::Value,
    team: &str,
    workspace: &str,
    cwd: &str,
    headed: bool,
    durable: bool,
) -> Result<LaunchHome, String> {
    let row = viewer
        .get("workspaces")
        .and_then(|teams| teams.get(team))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|id| id.as_str()) == Some(workspace))
        });
    let Some(row) = row else {
        if !durable {
            return Err("workspace is not open in this team".into());
        }
        let (org, team) = parse_team_key(team);
        return Ok(LaunchHome::new(
            org,
            team,
            workspace,
            "generic",
            std::path::Path::new(cwd),
            SessionView::Tab { focus: headed },
        ));
    };
    let kind = row
        .get("kind")
        .and_then(|value| value.as_str())
        .ok_or("workspace kind unavailable")?;
    let form = if kind == "story" {
        row.get("storyId")
            .and_then(|value| value.as_str())
            .unwrap_or(workspace)
    } else {
        workspace
    };
    let (org, team) = parse_team_key(team);
    let mut home = LaunchHome::new(
        org,
        team,
        form,
        kind,
        std::path::Path::new(cwd),
        SessionView::Tab { focus: headed },
    );
    home.descriptor.display_name = row
        .get("storyTitle")
        .and_then(|value| value.as_str())
        .unwrap_or(form)
        .to_string();
    Ok(home)
}

/// SW-AGR-8: the layout group (and its descriptor) a machine-armed
/// routine's agent opens its tab in, as a launch into that workspace would.
pub(crate) fn arm_tab_home(
    app: &tauri::AppHandle,
    team: &str,
    workspace: &str,
    cwd: &str,
) -> Result<
    (
        WorkspaceKey,
        codesense_pty_core::layout::WorkspaceDescriptor,
    ),
    String,
> {
    let home = launch_home(app, team, workspace, cwd, false, true)?;
    Ok((home.workspace, home.descriptor))
}

fn same_lifetime(a: &RuntimeMemberTarget, b: &RuntimeMemberTarget) -> bool {
    a.member == b.member
        && a.namespace.server_id == b.namespace.server_id
        && a.namespace.epoch == b.namespace.epoch
}

async fn intent_lock(
    owner: u64,
    team: &str,
    machine: &str,
    request: &str,
) -> tokio::sync::OwnedMutexGuard<()> {
    type Locks = std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >;
    static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();
    let key = format!("{owner}:{team}:{machine}:{request}");
    let lock = {
        let mut locks = LOCKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
            lock
        } else {
            let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(key, std::sync::Arc::downgrade(&lock));
            lock
        }
    };
    lock.lock_owned().await
}

/// A remote launch's phase progress, keyed by the request it was sent with
/// (SW-XS-2). Its budget is the sum of the phases it enters; see
/// [`crate::agents::launch_budget`].
pub const REMOTE_LAUNCH_PROGRESS_EVENT: &str = "remote-launch-progress";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteLaunchProgressPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<String>,
    #[serde(flatten)]
    progress: crate::agents::launch_budget::LaunchProgress,
}

/// A launch's budget, reporting each phase as [`REMOTE_LAUNCH_PROGRESS_EVENT`].
fn launch_budget(
    app: &tauri::AppHandle,
    reference: &RemoteTabReference,
) -> crate::agents::launch_budget::LaunchBudget {
    use tauri::Emitter;
    let app = app.clone();
    let (request_id, handle) = match reference {
        RemoteTabReference::RequestId(id) => (Some(*id), None),
        RemoteTabReference::Handle(handle) => (None, Some(handle.clone())),
    };
    crate::agents::launch_budget::LaunchBudget::new(move |progress| {
        let _ = app.emit(
            REMOTE_LAUNCH_PROGRESS_EVENT,
            RemoteLaunchProgressPayload {
                request_id,
                handle: handle.clone(),
                progress,
            },
        );
    })
}

pub(crate) async fn remote_shell_tab_bound(
    app: tauri::AppHandle,
    request: RemoteShellTabRequest,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
) -> RemoteTabOutcome {
    remote_shell_tab_bound_for(app, request, caller, Default::default()).await
}

pub(crate) async fn remote_shell_tab_bound_for(
    app: tauri::AppHandle,
    request: RemoteShellTabRequest,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> RemoteTabOutcome {
    if let Err(error) =
        crate::mcp::machine_tools::admit_remote_tab_request(&app, &request, &scope).await
    {
        return RemoteTabOutcome::refused("notFound", error);
    }
    let (reference, settle) = match &request {
        RemoteShellTabRequest::Create {
            request_id,
            machine_id,
            team_key,
            ..
        } => (
            RemoteTabReference::RequestId(*request_id),
            Some(
                std::sync::Arc::new(AppSettlement::new(&app, team_key, machine_id, *request_id))
                    as std::sync::Arc<dyn SettleLaunch>,
            ),
        ),
        // An attach can finish a launch that was still preparing: it reports too.
        RemoteShellTabRequest::Attach {
            reference,
            machine_id,
            team_key,
            ..
        } => (
            reference.clone(),
            match reference {
                RemoteTabReference::RequestId(request_id) => Some(std::sync::Arc::new(
                    AppSettlement::new(&app, team_key, machine_id, *request_id),
                )
                    as std::sync::Arc<dyn SettleLaunch>),
                RemoteTabReference::Handle(_) => None,
            },
        ),
    };
    let budget = launch_budget(&app, &reference);
    within_bound(
        remote_shell_tab_unbounded(app, request, caller, scope),
        budget,
        reference,
        settle,
    )
    .await
}

/// SW-XS-1 (X3): the event a remote launch emits when it ends after its caller was
/// already answered: `{requestId, teamKey, machineId, status, handle?, code?, message?}`.
pub(crate) const LAUNCH_SETTLED_EVENT: &str = "launch-settled";
/// How long a launch's settlement stays answerable to `attach {requestId}`.
pub(crate) const LAUNCH_SETTLEMENT_TTL: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// What a remote launch whose caller was answered early is known as.
#[derive(Debug, Clone)]
pub(crate) enum LaunchSettlement {
    /// Still running: `attach` answers `pending`.
    Running,
    /// Ended: with a handle (attach reconciles its reserved record) or a refusal.
    Settled(Box<RemoteTabOutcome>),
}

/// Where an open reports once its caller was answered before it settled.
pub(crate) trait SettleLaunch: Send + Sync + 'static {
    /// The caller was answered while the open still runs.
    fn running(&self);
    /// The open ended after its caller was answered.
    fn settled(&self, outcome: &RemoteTabOutcome);
}

struct AppSettlement {
    app: tauri::AppHandle,
    /// The private owner when the launch was asked for; `None` reports nothing.
    owner: Option<u64>,
    team: String,
    machine: String,
    request_id: Uuid,
}
impl AppSettlement {
    fn new(app: &tauri::AppHandle, team: &str, machine: &str, request_id: Uuid) -> Self {
        let owner = app
            .state::<crate::commands::local_profile::LocalProfileState>()
            .snapshot()
            .ok()
            .map(|authority| authority.owner_id());
        Self {
            app: app.clone(),
            owner,
            team: team.to_string(),
            machine: machine.to_string(),
            request_id,
        }
    }
    /// The settlement key under the owner the launch was asked for by.
    fn key(&self) -> Option<super::super::dashboard_launch::SettlementKey> {
        Some((
            self.owner?,
            self.team.clone(),
            self.machine.clone(),
            self.request_id,
        ))
    }
}
impl SettleLaunch for AppSettlement {
    fn running(&self) {
        if let Some(key) = self.key() {
            self.app
                .state::<DashboardLaunchState>()
                .remote_launch_running(key);
        }
    }
    fn settled(&self, outcome: &RemoteTabOutcome) {
        use tauri::Emitter;
        if let Some(key) = self.key() {
            self.app
                .state::<DashboardLaunchState>()
                .settle_remote_launch(key, outcome.clone());
        }
        let _ = self.app.emit(
            LAUNCH_SETTLED_EVENT,
            serde_json::json!({
                "requestId": self.request_id,
                "teamKey": self.team,
                "machineId": self.machine,
                "status": outcome.status,
                "handle": outcome.handle,
                "code": outcome.code,
                "message": outcome.message,
            }),
        );
    }
}

/// What `attach {requestId}` answers from a launch's settlement, before looking
/// for its record: `pending` while it runs, and a final (non-retryable) refusal
/// when it ended that way. Anything else — no settlement, a settled handle, an
/// unknown or retryable outcome — attaches as before, through the launch's
/// reserved intent record, which restores and reconciles it.
fn attach_settlement(
    settlement: Option<LaunchSettlement>,
    reference: &RemoteTabReference,
) -> Option<RemoteTabOutcome> {
    match settlement? {
        LaunchSettlement::Running => {
            let mut outcome = RemoteTabOutcome::unknown(
                reference,
                "The launch is still running on the machine. Wait for launch-settled, then attach with this request id; do not create again.",
            );
            outcome.status = "pending";
            outcome.code = Some("launching".into());
            Some(outcome)
        }
        LaunchSettlement::Settled(outcome) if outcome.status == "refused" && !outcome.retryable => {
            Some(*outcome)
        }
        LaunchSettlement::Settled(_) => None,
    }
}

/// Runs `open` to completion in the background, but answers once the phase
/// it is in outlives its budget: "not confirmed", naming the request to check
/// again by. Never cancels the open halfway (a cancelled open could leave a
/// terminal its receipt does not record).
///
/// SW-XS-1: when the caller is answered first, `settle` hears that the open is
/// still running, and then what it ended as, so it never spawns unreported.
async fn within_bound<F>(
    open: F,
    budget: crate::agents::launch_budget::LaunchBudget,
    reference: RemoteTabReference,
    settle: Option<std::sync::Arc<dyn SettleLaunch>>,
) -> RemoteTabOutcome
where
    F: std::future::Future<Output = RemoteTabOutcome> + Send + 'static,
{
    let mut task = tokio::spawn(budget.scope(open));
    tokio::select! {
        biased;
        joined = &mut task => match joined {
            Ok(outcome) => outcome,
            Err(_) => RemoteTabOutcome::unknown(
                &reference,
                "The open stopped before the machine answered. Check this request again before opening another.",
            ),
        },
        expired = budget.expired() => {
            if let Some(settle) = settle {
                settle.running();
                let reference = reference.clone();
                tokio::spawn(async move {
                    let outcome = task.await.unwrap_or_else(|_| {
                        RemoteTabOutcome::unknown(
                            &reference,
                            "The open stopped before the machine answered. Check this request again before opening another.",
                        )
                    });
                    settle.settled(&outcome);
                });
            }
            let mut outcome = RemoteTabOutcome::unknown(
                &reference,
                format!(
                    "The launch is still {} after its {}s budget. Check this request again before opening another.",
                    expired.phase,
                    expired.budget.as_secs()
                ),
            );
            outcome.code = Some("timedOut".into());
            outcome
        }
    }
}

async fn remote_shell_tab_unbounded(
    app: tauri::AppHandle,
    request: RemoteShellTabRequest,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> RemoteTabOutcome {
    let (machine, team) = match &request {
        RemoteShellTabRequest::Create {
            machine_id,
            team_key,
            ..
        }
        | RemoteShellTabRequest::Attach {
            machine_id,
            team_key,
            ..
        } => (machine_id, team_key),
    };
    if let Err(error) =
        super::super::machines::authorize_terminal_machine(&app, machine, team).await
    {
        return RemoteTabOutcome::refused("accessDenied", error);
    }
    match request {
        RemoteShellTabRequest::Create {
            request_id,
            machine_id,
            team_key,
            workspace_id,
            cwd,
            name,
            launch,
            permission_mode,
            agent_key,
            prepared_workspace_handle,
            headed,
            initial_prompt,
            identity,
        } => {
            if let Err(error) = permission_mode_fits(permission_mode, launch, &agent_key) {
                return RemoteTabOutcome::refused("invalid", error);
            }
            let initial_prompt = initial_prompt
                .map(|prompt| prompt.trim().to_string())
                .filter(|prompt| !prompt.is_empty());
            if initial_prompt.is_some() && agent_key.is_none() {
                return RemoteTabOutcome::refused(
                    "invalid",
                    "a first prompt is for a saved Agent; a shell takes commands once it opens",
                );
            }
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => match host_home(&app, &machine_id).await {
                    Ok(home) => home,
                    Err(error) => return home_refusal(error),
                },
            };
            let cwd = match normalized_cwd(&cwd) {
                Ok(cwd) => cwd,
                Err(error) => return RemoteTabOutcome::refused("invalid", error),
            };
            if identity.durable_workspace
                && (agent_key.is_none() || prepared_workspace_handle.is_none() || caller.is_some())
            {
                return RemoteTabOutcome::refused(
                    "invalid",
                    "a durable workspace launch is a saved Agent's, into its prepared workspace",
                );
            }
            let home = match launch_home(
                &app,
                &team_key,
                &workspace_id,
                &cwd,
                headed,
                identity.durable_workspace,
            ) {
                Ok(home) => home,
                Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
            };
            let authority = match app
                .state::<crate::commands::local_profile::LocalProfileState>()
                .snapshot()
            {
                Ok(authority) => authority,
                Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
            };
            let receipt_owner = authority.owner_id();
            if let Some(caller) = &caller {
                if let Err(error) = caller.admit_launch(receipt_owner, &team_key).await {
                    return RemoteTabOutcome::refused("accessDenied", error);
                }
            }
            let _intent = intent_lock(
                receipt_owner,
                &team_key,
                &machine_id,
                &request_id.to_string(),
            )
            .await;
            let receipt_root = match super::super::route_to_context::resolve_ambient(&app).await {
                Ok((home, principal)) if principal == receipt_owner => {
                    crate::context_repo::path_to_user_home(
                        &home,
                        &crate::domain::Principal::from(principal),
                    )
                    .join("remote-shell-intents-v1")
                }
                Ok(_) => return RemoteTabOutcome::refused("accessDenied", "private owner changed"),
                Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
            };
            let consumed = await_blocking("read_remote_intent", {
                let app = app.clone();
                let root = receipt_root.clone();
                let team = team_key.clone();
                let machine = machine_id.clone();
                move || {
                    app.state::<DashboardLaunchState>().consumed_remote_shell(
                        &root,
                        receipt_owner,
                        &team,
                        &machine,
                        request_id,
                    )
                }
            })
            .await;
            if let Err(error) = authority.lease().validate() {
                return RemoteTabOutcome::refused("accessDenied", error);
            }
            match consumed {
                Ok(Some(existing)) => {
                    let requested = launch.unwrap_or(if agent_key.is_some() {
                        existing.launch
                    } else {
                        NativeLaunch::Shell
                    });
                    if !existing.matches_intent(
                        &cwd,
                        &workspace_id,
                        &name,
                        requested,
                        permission_mode,
                    ) || !existing.matches_agent(&agent_key, &prepared_workspace_handle)
                    {
                        return RemoteTabOutcome::refused(
                            "invalid",
                            "Request identity already belongs to another intent",
                        );
                    }
                    if existing.preparing() {
                        return finish_launch(app, existing, headed, caller, receipt_root, false)
                            .await;
                    }
                    return reconcile(app, existing, headed, caller.as_ref()).await;
                }
                Ok(None) => {}
                Err(error) => {
                    let mut out = RemoteTabOutcome::refused("consumedUnknown", error);
                    out.status = "unknown";
                    out.request_id = Some(request_id);
                    return out;
                }
            }
            let session = PtySessionId::new(format!("remote-shell-{}", Uuid::new_v4()));
            let backend =
                match pty_backend::resolve_spawn_backend(&app, &machine_id, session.as_str()) {
                    Ok(backend) => backend,
                    Err(error) => return RemoteTabOutcome::refused("unavailable", error),
                };
            let Some(target) = backend
                .admission_target()
                .cloned()
                .filter(|_| backend.is_remote())
            else {
                return RemoteTabOutcome::refused(
                    "unavailable",
                    crate::commands::machines::tab_admission_unavailable_reason(&app, &machine_id)
                        .await,
                );
            };
            if backend
                .client()
                .and_then(|client| client.private_lease())
                .filter(|lease| lease.validate().is_ok() && lease.owner_id() == receipt_owner)
                .is_none()
            {
                return RemoteTabOutcome::refused("accessDenied", "private authority unavailable");
            }
            let mut record = RemoteShellRecord {
                agent: None,
                phase: RemoteSpawnPhase::Attempted,
                handle: format!("remote-tab-{}", Uuid::new_v4()),
                owner_id: receipt_owner,
                request_id,
                machine_id,
                workspace_story_id: workspace_story_id(&app, &team_key, &workspace_id),
                team_key,
                workspace_id,
                workspace: home.workspace.clone(),
                cwd: cwd.clone(),
                name: name.clone(),
                mcp_profile_revision: None,
                mcp_inventory: Vec::new(),
                hooks: false,
                launch: launch.unwrap_or_default(),
                permission_mode,
                target: RuntimeSessionTarget {
                    runtime: target,
                    session: session.clone(),
                },
                status: "pending",
                identity: *identity,
            };
            // The session id is the machine's telemetry token for this launch, so the usage pulled
            // from the machine is attributed to its org, team and agent, as a local launch's is. A
            // shell gets no usage export, so it registers nothing.
            if agent_key.is_some() || record.launch.cli().is_some() {
                let (org, team) = parse_team_key(&record.team_key);
                crate::session_usage::register_launch_async(
                    session.as_str().to_string(),
                    org.to_string(),
                    team.map(str::to_string),
                    agent_key
                        .clone()
                        .unwrap_or_else(|| record.launch.as_str().to_string()),
                    record
                        .identity
                        .story
                        .clone()
                        .unwrap_or_else(|| record.workspace_id.clone()),
                    Some(record.cwd.clone()),
                )
                .await;
            }
            if let Err(error) = backend.revalidate(&app) {
                return RemoteTabOutcome::refused("accessDenied", error);
            }
            match (&agent_key, &prepared_workspace_handle) {
                (Some(key), Some(handle)) => {
                    let key = match crate::agents::keys::AgentKey::parse(key) {
                        Some(key) => key,
                        None => {
                            return RemoteTabOutcome::refused("invalid", "invalid saved Agent key")
                        }
                    };
                    let resolved =
                        match resolve_prepared(&app, &record, handle, caller.clone()).await {
                            Ok(resolved) => resolved,
                            Err(error) => {
                                return RemoteTabOutcome::refused("workspaceUnavailable", error)
                            }
                        };
                    let context = resolved.context;
                    if let Err(error) = scope
                        .require_launch_key(
                            &context.codesense_home,
                            &context.principal(),
                            &record.team_key,
                        )
                        .and_then(|_| {
                            scope.require_agent(&context.codesense_home, &context.principal(), &key)
                        })
                    {
                        return RemoteTabOutcome::refused("notFound", error);
                    }
                    let handle = handle.clone();
                    let prompt = initial_prompt.clone();
                    let agent = match await_blocking("capture_remote_agent", move || {
                        let mut agent = crate::agents::remote_launch::prepare(
                            &context.codesense_home,
                            &context.principal(),
                            &key,
                            context.org_slug.clone(),
                            context
                                .team_name
                                .clone()
                                .ok_or("prepared workspace team missing")?,
                            handle,
                        )?;
                        crate::agents::remote_launch::start_with_prompt(&mut agent, prompt)?;
                        Ok(agent)
                    })
                    .await
                    {
                        Ok(agent) => agent,
                        Err(error) => return RemoteTabOutcome::refused("agentUnavailable", error),
                    };
                    if launch.is_some_and(|runtime| runtime != NativeLaunch::Cli(agent.runtime)) {
                        return RemoteTabOutcome::refused(
                            "invalid",
                            "saved Agent owns its runtime; remove the launch override",
                        );
                    }
                    take_agent(&mut record, agent);
                    record.phase = RemoteSpawnPhase::Preparing;
                }
                (None, None) => {}
                _ => {
                    return RemoteTabOutcome::refused(
                        "invalid",
                        "saved Agent requires its prepared workspace and owns its MCP selection",
                    )
                }
            }
            if let Err(refusal) = agent_ready(&app, &backend, &record).await {
                return refusal;
            }
            let generates_hooks = backend
                .client()
                .is_some_and(|client| client.supports_agent_startup());
            let _captured_mcp = match crate::mcp::remote_authority::capture_launch(
                &app,
                &mut record,
                caller.as_ref(),
                generates_hooks,
            )
            .await
            {
                Ok(captured) => captured,
                Err(error) => return RemoteTabOutcome::refused("reverseMcpUnavailable", error),
            };
            if let Some(caller) = &caller {
                if let Err(error) = caller.admit_launch(record.owner_id, &record.team_key).await {
                    return RemoteTabOutcome::refused("accessDenied", error);
                }
            }
            let reserved = await_blocking("reserve_remote_intent", {
                let receipt_root = receipt_root.clone();
                let app = app.clone();
                let record = record.clone();
                move || {
                    app.state::<DashboardLaunchState>()
                        .reserve_remote_shell_durable(&receipt_root, record)
                }
            })
            .await;
            match reserved {
                Ok(super::super::dashboard_launch::RemoteShellReservation::Fresh) => {}
                Ok(super::super::dashboard_launch::RemoteShellReservation::Existing(existing)) => {
                    if !existing.matches_intent(
                        &record.cwd,
                        &record.workspace_id,
                        &record.name,
                        record.launch,
                        record.permission_mode,
                    ) || !existing.matches_capture(&record)
                    {
                        return RemoteTabOutcome::refused(
                            "invalid",
                            "Request identity already belongs to another intent",
                        );
                    }
                    if existing.preparing() {
                        return finish_launch(app, *existing, headed, caller, receipt_root, false)
                            .await;
                    }
                    return reconcile(app, *existing, headed, caller.as_ref()).await;
                }
                Err(error) => {
                    return RemoteTabOutcome::unknown(
                        &RemoteTabReference::RequestId(request_id),
                        error,
                    )
                }
            }
            finish_launch(app, record, headed, caller, receipt_root, true).await
        }
        RemoteShellTabRequest::Attach {
            machine_id,
            team_key,
            reference,
            headed,
        } => {
            let owner = match app
                .state::<crate::commands::local_profile::LocalProfileState>()
                .snapshot()
            {
                Ok(owner) => owner.owner_id(),
                Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
            };
            let root = match receipt_root_for(&app, owner).await {
                Ok(root) => root,
                Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
            };
            // SW-XS-1: a launch whose caller was answered first, under this owner,
            // once the caller may act for this team.
            if let RemoteTabReference::RequestId(id) = &reference {
                let settlement = app
                    .state::<DashboardLaunchState>()
                    .remote_launch_settlement(&(owner, team_key.clone(), machine_id.clone(), *id));
                if settlement.is_some() {
                    if let Some(caller) = &caller {
                        if let Err(error) = caller.admit_launch(owner, &team_key).await {
                            return RemoteTabOutcome::refused("accessDenied", error);
                        }
                    }
                }
                if let Some(outcome) = attach_settlement(settlement, &reference) {
                    return outcome;
                }
            }
            let restored = await_blocking("restore_remote_intent", {
                let app = app.clone();
                let root = root.clone();
                let team = team_key.clone();
                let machine = machine_id.clone();
                let reference = reference.clone();
                move || {
                    app.state::<DashboardLaunchState>()
                        .restore_remote_shell(&root, owner, &team, &machine, &reference)
                }
            })
            .await;
            let record = match restored {
                Ok(record) => record.or_else(|| {
                    app.state::<DashboardLaunchState>().remote_shell_record(
                        &team_key,
                        &machine_id,
                        &reference,
                    )
                }),
                Err(error) => return RemoteTabOutcome::unknown(&reference, error),
            };
            if let Some(record) = &record {
                if record.preparing()
                    && (record.agent.is_some() || record.launch.cli().is_some())
                    && !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
                {
                    let ctx = match scope.context(&app).await {
                        Ok(ctx) => ctx,
                        Err(error) => return RemoteTabOutcome::refused("notFound", error),
                    };
                    if let Err(error) = scope
                        .require_launch_key(&ctx.codesense_home, &ctx.principal(), &record.team_key)
                        .and_then(|_| match &record.agent {
                            Some(agent) => scope.require_agent(
                                &ctx.codesense_home,
                                &ctx.principal(),
                                &agent.manifest.agent.key,
                            ),
                            None => Ok(()),
                        })
                    {
                        return RemoteTabOutcome::refused("notFound", error);
                    }
                }
            }
            let _intent = if let Some(record) = &record {
                Some(
                    intent_lock(
                        owner,
                        &team_key,
                        &machine_id,
                        &record.request_id.to_string(),
                    )
                    .await,
                )
            } else {
                None
            };
            match record {
                Some(record) if record.preparing() => {
                    let root = match receipt_root_for(&app, record.owner_id).await {
                        Ok(root) => root,
                        Err(error) => return RemoteTabOutcome::refused("accessDenied", error),
                    };
                    finish_launch(app, record, headed, caller, root, false).await
                }
                Some(record) => {
                    let session = record.target.session.clone();
                    let outcome = reconcile(app.clone(), record, headed, caller.as_ref()).await;
                    // A tab brought back (after an app restart) is offered its mail: its
                    // machine may have connected before it was restored.
                    crate::commands::host_inbox::tab_restored(&app, session.as_str());
                    outcome
                }
                None => recover(app, machine_id, team_key, reference, headed, caller).await,
            }
        }
    }
}

/// A permission mode is a raw Claude/Codex runtime's choice: a shell asks for
/// nothing, and a saved Agent declares its own permissions.
fn permission_mode_fits(
    mode: RemotePermissionMode,
    launch: Option<NativeLaunch>,
    agent_key: &Option<String>,
) -> Result<(), &'static str> {
    if mode.is_default() {
        return Ok(());
    }
    if agent_key.is_some() {
        return Err("a saved Agent declares its own permissions; remove permissionMode");
    }
    if launch.unwrap_or_default().is_shell() {
        return Err("permissionMode applies to a claude or codex launch, not a shell");
    }
    Ok(())
}

/// What a person sees when the CLI an agent launch needs is not on the machine.
fn cli_missing(launch: NativeLaunch) -> RemoteTabOutcome {
    // A shell is always there; its refusal names the CLI a launch runs by default.
    let cli = launch
        .cli()
        .unwrap_or(crate::agents::adapter::PREFERRED_RUNTIME);
    let noun = &descriptor(cli).name;
    RemoteTabOutcome::refused(
        "agentCliMissing",
        format!("{noun} isn't installed on this machine"),
    )
}

/// SW-XS-6: the person-facing message of a launch failure that names an
/// outdated terminal server (the context sync version gate), if it is one.
fn outdated_server_message(error: &str) -> Option<String> {
    DestinationBatchError::names_outdated_server(error).then(|| {
        error
            .replace(&format!("[{TERMINAL_SERVER_OUTDATED}] "), "")
            .trim()
            .to_string()
    })
}

/// The code of a launch the layout refused because the workspace's open tabs run
/// in another folder. Nothing was started.
const CWD_CONFLICT: &str = "cwdConflict";

/// A layout error as a launch failure: a folder conflict names the folder the
/// launch asked for and what to do, after its code.
fn layout_refusal(error: LayoutError, cwd: &str) -> String {
    match error {
        LayoutError::CwdConflict(_) => format!(
            "{CWD_CONFLICT}: this workspace's tabs run in another folder, so a tab in {cwd} \
             can't join them; close them, or launch into their folder"
        ),
        other => other.to_string(),
    }
}

/// The person-facing message of a folder-conflict refusal, if it is one.
fn cwd_conflict_message(error: &str) -> Option<&str> {
    error.strip_prefix(CWD_CONFLICT)?.strip_prefix(": ")
}

/// The host's own word that a launch's CLI is missing, from a terminal server
/// that answers it only when the launch starts.
fn is_cli_missing(error: &str) -> bool {
    error.contains(codesense_host_proto::reverse_mcp::NATIVE_RUNTIME_MISSING)
}

/// Where a launch runs relative to the viewer's workspace.
#[derive(Debug, PartialEq, Eq)]
enum WorkspacePlace {
    /// The workspace does not live on this machine: nothing to set up.
    Elsewhere,
    /// Inside the workspace's worktree there: its managed handle.
    Worktree(String),
    /// The workspace lives on this machine, but its worktree isn't where the
    /// launch runs, or isn't ready: it can't be set up.
    Unready(&'static str),
}

/// The managed workspace a launch at `record.cwd` runs in, when the viewer's
/// workspace lives on this machine.
fn workspace_on_machine(app: &tauri::AppHandle, record: &RemoteShellRecord) -> WorkspacePlace {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    let row = viewer
        .get("workspaces")
        .and_then(|teams| teams.get(&record.team_key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|id| id.as_str()) == Some(&record.workspace_id))
        })
        .cloned();
    match row {
        Some(row) => workspace_location(&row, &record.machine_id, &record.cwd),
        None => WorkspacePlace::Elsewhere,
    }
}

fn workspace_location(row: &serde_json::Value, machine: &str, cwd: &str) -> WorkspacePlace {
    let Some(location) = row.get("location") else {
        return WorkspacePlace::Elsewhere;
    };
    let text = |field: &str| location.get(field).and_then(|value| value.as_str());
    if text("kind") != Some("machine") || text("machineId") != Some(machine) {
        return WorkspacePlace::Elsewhere;
    }
    let (Some(handle), Some(at)) = (text("handle"), text("cwd")) else {
        return WorkspacePlace::Unready("the workspace is still being created on this machine");
    };
    if normalized_cwd(at) != Ok(cwd.to_string()) {
        return WorkspacePlace::Unready(
            "the agent was asked to start outside the workspace's worktree",
        );
    }
    WorkspacePlace::Worktree(handle.to_string())
}

/// Before an agent starts on a machine: agents are allowed there, its CLI is
/// installed, and it starts inside its workspace's worktree. A plain shell
/// needs none of it.
async fn agent_ready(
    app: &tauri::AppHandle,
    backend: &pty_backend::ResolvedTerminalBackend,
    record: &RemoteShellRecord,
) -> Result<(), RemoteTabOutcome> {
    if record.launch.is_shell() {
        return Ok(());
    }
    if super::super::machines::authorize_remote_mcp_control(app, &record.machine_id)
        .await
        .is_err()
    {
        return Err(RemoteTabOutcome::refused(
            "agentsNotAllowed",
            "Agents aren't allowed on this machine yet",
        ));
    }
    if record.launch.is_shell() {
        return Ok(());
    }
    if !record.permission_mode.is_default() {
        match backend.client() {
            None => {
                return Err(RemoteTabOutcome::refused(
                    "unavailable",
                    "This machine's terminal server is not connected",
                ))
            }
            Some(client) if !client.supports_agent_startup() => {
                return Err(RemoteTabOutcome::refused(
                    "terminalServerOutdated",
                    "This machine's terminal server is too old for permissionMode; update the terminal server, or launch without permissionMode",
                ))
            }
            Some(_) => {}
        }
    }
    if let Some(client) = backend.client() {
        let cwd = record.cwd.clone();
        let launch = record.launch;
        let installed = await_blocking("probe_native_runtime", move || {
            client
                .native_runtime_installed(&cwd, launch)
                .map_err(|error| error.to_string())
        })
        .await;
        // An unanswered probe is no verdict: the launch still refuses a
        // missing CLI itself.
        if let Ok(Some(false)) = installed {
            return Err(cli_missing(record.launch));
        }
    }
    // SW-XS-1 (X3): no content pass here. The launch's only content work is
    // `prepare_destination`, under the destination lock in `prepare_remote_launch`.
    match workspace_on_machine(app, record) {
        WorkspacePlace::Unready(why) => Err(RemoteTabOutcome::refused(
            "agentSetupFailed",
            format!("Couldn't set up the workspace for the agent: {why}"),
        )),
        WorkspacePlace::Elsewhere | WorkspacePlace::Worktree(_) => Ok(()),
    }
}

/// A tab a machine's live layout holds, as a launch receipt names it.
#[derive(Clone)]
pub(crate) struct LiveRemoteTab {
    pub team: String,
    pub handle: String,
    pub request_id: Uuid,
    pub session: String,
}

/// `machine` connected. Its tabs already in the launch registry are reached
/// through this connection from now on. After an app restart, the remote tabs
/// this app launched that `machine` still runs come back into the registry from
/// their receipts, so they are listed and reachable by mail before any of their
/// workspaces is opened. Each team's tabs on a machine are restored once since
/// the app started, so a tab closed or dismissed later stays gone across
/// reconnects.
pub(crate) async fn restore_on_machine(app: &tauri::AppHandle, machine: &str) {
    // The (machine, team) pairs restored since the app started.
    static RESTORED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<(String, String)>>,
    > = std::sync::OnceLock::new();
    let restored_set = || {
        RESTORED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    };
    let Ok(owner) = app
        .state::<crate::commands::local_profile::LocalProfileState>()
        .snapshot()
        .map(|profile| profile.owner_id())
    else {
        return;
    };
    let Ok(root) = receipt_root_for(app, owner).await else {
        return;
    };
    let read = await_blocking("remote_tab_restore", {
        let app = app.clone();
        let machine = machine.to_string();
        move || {
            let backend = pty_backend::resolve_spawn_backend(&app, &machine, "restore")?;
            backend.revalidate(&app)?;
            let target = backend
                .admission_target()
                .cloned()
                .ok_or("remote admission unavailable")?;
            let snapshot = backend
                .domain()
                .layout_snapshot(&app_viewer(), &ClientBootId::new("remote-human"))
                .map_err(|e| e.to_string())?;
            let live: Vec<LiveRemoteTab> = snapshot
                .workspaces
                .iter()
                .flat_map(|(key, ws)| ws.tabs.iter().map(move |tab| (key, tab)))
                .filter(|(_, tab)| !tab.state.is_exited())
                .filter_map(|(key, tab)| {
                    Some(LiveRemoteTab {
                        team: key.as_str().split('|').next()?.to_string(),
                        handle: tab.tags.get("remoteHandle")?.clone(),
                        request_id: Uuid::parse_str(tab.tags.get("remoteRequestId")?).ok()?,
                        session: tab.session.as_str().to_string(),
                    })
                })
                .collect();
            Ok((target, live))
        }
    })
    .await;
    let (target, mut live) = match read {
        Ok(read) => read,
        Err(error) => {
            log::debug!(target: "remote", "restore tabs on {machine}: {error}");
            return;
        }
    };
    // Every team that may still use this machine has its registered tabs
    // reached through this connection; each is restored from its receipts
    // once, and a team not restored (a read or a check failed) is tried again
    // on a reconnect.
    let mut teams: Vec<String> = live.iter().map(|tab| tab.team.clone()).collect();
    teams.sort();
    teams.dedup();
    let (mut allowed, mut fresh) = (Vec::new(), Vec::new());
    for team in teams {
        let pair = (machine.to_string(), team.clone());
        // Claimed before the check, so two connections never both restore it.
        let first = restored_set().insert(pair.clone());
        if super::super::machines::authorize_terminal_machine(app, machine, &team)
            .await
            .is_ok()
        {
            if first {
                fresh.push(team.clone());
            }
            allowed.push(team);
        } else if first {
            restored_set().remove(&pair);
        }
    }
    live.retain(|tab| allowed.contains(&tab.team));
    let readmitted = app.state::<DashboardLaunchState>().readmit_remote_shells(
        owner,
        machine,
        &live,
        &target,
        |runtime| same_lifetime(&target, runtime),
    );
    live.retain(|tab| fresh.contains(&tab.team));
    let restored = await_blocking("remote_tab_restore", {
        let app = app.clone();
        let machine = machine.to_string();
        move || {
            // The admission now, not the one read before the team checks: a
            // reconnect meanwhile has moved it on.
            let backend = pty_backend::resolve_spawn_backend(&app, &machine, "restore")?;
            backend.revalidate(&app)?;
            let now = backend
                .admission_target()
                .cloned()
                .ok_or("remote admission unavailable")?;
            Ok::<_, String>(app.state::<DashboardLaunchState>().restore_remote_shells(
                &root,
                owner,
                &machine,
                &live,
                &now,
                |runtime| same_lifetime(&target, runtime) && same_lifetime(&now, runtime),
            ))
        }
    })
    .await;
    if readmitted > 0 || restored.is_ok_and(|n| n > 0) {
        crate::commands::dashboard_launch::emit_session_state_changed(app);
    }
}

async fn receipt_root_for(
    app: &tauri::AppHandle,
    owner: u64,
) -> Result<std::path::PathBuf, String> {
    let (home, principal) = super::super::route_to_context::resolve_ambient(app).await?;
    if principal != owner {
        return Err("private owner changed".into());
    }
    Ok(
        crate::context_repo::path_to_user_home(&home, &crate::domain::Principal::from(owner))
            .join("remote-shell-intents-v1"),
    )
}

/// The receipt gate: the prepared workspace a launch resolved must be exactly
/// the launch's directory, workspace, team and terminal server (SW-AGR-12
/// keeps it for a durable launch too).
fn receipt_gate(
    cwd: &str,
    workspace_id: &str,
    team_key: &str,
    server_id: &str,
    record: &RemoteShellRecord,
) -> Result<(), String> {
    if cwd != record.cwd
        || workspace_id != record.workspace_id
        || team_key != record.team_key
        || server_id != record.target.runtime.namespace.server_id
    {
        return Err(
            "prepared workspace does not match the exact launch directory/group/server".into(),
        );
    }
    Ok(())
}

async fn resolve_prepared(
    app: &tauri::AppHandle,
    record: &RemoteShellRecord,
    handle: &str,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
) -> Result<crate::mcp::remote_workspace_target::ResolvedWorkspace, String> {
    if let Some(caller) = &caller {
        caller
            .admit_launch(record.owner_id, &record.team_key)
            .await?;
    }
    let (org, team) = super::parse_team_key(&record.team_key);
    let ctx = match super::super::route_to_context::derive_context_for(
        app,
        org,
        team.ok_or("prepared workspace needs a team")?,
    )
    .await?
    {
        super::super::route_to_context::ContextResolution::Ok(ctx)
            if ctx.user_id == record.owner_id =>
        {
            ctx
        }
        _ => return Err("saved-Agent workspace context unavailable".into()),
    };
    let resolved = if record.identity.durable_workspace && caller.is_none() {
        crate::mcp::remote_workspace_target::resolve_durable(
            app.clone(),
            ctx,
            &record.machine_id,
            handle,
        )
        .await?
    } else {
        crate::mcp::remote_workspace_target::resolve_selected(
            app.clone(),
            ctx,
            Some(&record.machine_id),
            handle,
            caller.clone(),
            false,
        )
        .await?
    };
    if let Some(caller) = &caller {
        caller
            .admit_launch(record.owner_id, &record.team_key)
            .await?;
    }
    receipt_gate(
        &resolved.cwd,
        &resolved.workspace_id,
        &resolved.team_key,
        &resolved.server_id,
        record,
    )?;
    if !resolved.client.supports_agent_startup() {
        return Err("saved-Agent startup requires an updated remote terminal server".into());
    }
    resolved.client.require_agent_env(
        record
            .agent
            .as_ref()
            .is_some_and(|agent| !agent.startup.env.is_empty()),
    )?;
    Ok(resolved)
}

async fn finish_launch(
    app: tauri::AppHandle,
    mut record: RemoteShellRecord,
    headed: bool,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
    root: std::path::PathBuf,
    fresh: bool,
) -> RemoteTabOutcome {
    let checked = await_blocking("remote_spawn_phase", {
        let app = app.clone();
        let root = root.clone();
        let mut record = record.clone();
        move || {
            app.state::<DashboardLaunchState>()
                .refresh_remote_spawn_phase(&root, &mut record)?;
            Ok(record)
        }
    })
    .await;
    record = match checked {
        Ok(record) => record,
        Err(error) => {
            return RemoteTabOutcome::unknown(
                &RemoteTabReference::RequestId(record.request_id),
                error,
            )
        }
    };
    if !record.preparing() && (!fresh || record.agent.is_some()) {
        return reconcile(app, record, headed, caller.as_ref()).await;
    }
    let prepared = std::sync::Mutex::new(None);
    let result =
        finish_launch_inner(&app, &mut record, headed, caller.as_ref(), &root, &prepared).await;
    let prepared = prepared.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = result {
        // A marker may have become durable before an I/O error. Never offer a
        // preparation retry once a cross-process spawn attempt could exist.
        if app
            .state::<DashboardLaunchState>()
            .refresh_remote_spawn_phase(&root, &mut record)
            .is_err()
        {
            record.mark_spawn_attempted();
        }
        record.status = if record.preparing() {
            "refused"
        } else {
            "unknown"
        };
        app.state::<DashboardLaunchState>()
            .update_remote_shell(record.clone());
        let mut out = record.outcome(false);
        // Refused before anything started (the machine's single flight, or the
        // workspace's tabs in another folder): nothing runs, so nothing is listed.
        let refusal = if error.contains(codesense_pty_core::routine_arm::AGENT_RUNNING) {
            Some((
                codesense_pty_core::routine_arm::AGENT_RUNNING,
                error.clone(),
            ))
        } else {
            cwd_conflict_message(&error).map(|message| (CWD_CONFLICT, message.to_string()))
        };
        if let Some((code, message)) = refusal {
            app.state::<DashboardLaunchState>().abort(&record.handle);
            crate::commands::dashboard_launch::emit_session_state_changed(&app);
            out.status = "refused";
            out.retryable = false;
            out.handle = None;
            out.code = Some(code.into());
            out.message = Some(message);
            return out;
        }
        if is_cli_missing(&error) {
            // Refused by the host before anything was started.
            out.status = "refused";
            out.retryable = false;
            let missing = cli_missing(record.launch);
            out.code = missing.code;
            out.message = missing.message;
            return out;
        }
        if let Some(message) = outdated_server_message(&error).filter(|_| record.preparing()) {
            // SW-XS-6 version gate: nothing was sent to the machine. The UI
            // offers "Update terminal server" for this code and relaunches
            // once the server is replaced.
            out.status = "refused";
            out.retryable = false;
            out.code = Some(TERMINAL_SERVER_OUTDATED.into());
            out.message = Some(message);
            return out;
        }
        if record.preparing() && error.starts_with("newIntentRequired:") {
            out.retryable = false;
            out.code = Some("newIntentRequired".into());
            out.message = Some(error.trim_start_matches("newIntentRequired: ").into());
            return out;
        }
        out.code = Some(
            if record.preparing() {
                "preparationIncomplete"
            } else {
                "spawnUnconfirmed"
            }
            .into(),
        );
        out.message = Some(error);
        return out;
    }
    app.state::<DashboardLaunchState>()
        .update_remote_shell(record.clone());
    crate::commands::host_inbox::mail_stored(&app, &record.handle);
    let mut out = reconcile(app.clone(), record.clone(), headed, caller.as_ref()).await;
    if out.status == "alreadyLive" {
        out.status = "opened";
    }
    out.prepared = prepared;
    out
}

/// The launch phase (and the bytes it is budgeted for) a Step 2 progress
/// report enters (SW-XS-2): the diff renews its budget with the bytes being
/// hashed; the transfer and the apply are budgeted by the bytes they move,
/// never by an operation count.
fn budget_step(
    progress: &crate::agents::prepare::PrepareProgress,
) -> Option<(crate::agents::launch_budget::LaunchPhase, u64)> {
    use crate::agents::launch_budget::LaunchPhase;
    use crate::agents::prepare::PreparePhase;
    match progress.phase {
        PreparePhase::Diff => Some((LaunchPhase::Diffing, progress.bytes)),
        PreparePhase::Transfer | PreparePhase::Apply => {
            Some((LaunchPhase::Transferring, progress.total))
        }
        _ => None,
    }
}

/// Step 2's progress, into the launch's phase budget (SW-XS-2) and onto the
/// Step 2 progress event (SW-XS-7).
fn report_step_two(
    budget: Option<&crate::agents::launch_budget::LaunchBudget>,
    sink: &dyn crate::commands::worktree::EventSink,
    progress: &crate::agents::prepare::PrepareProgress,
) {
    use crate::agents::launch_budget::LaunchPhase;
    crate::agents::prepare::progress::to_sink(sink)(progress);
    let Some(budget) = budget else { return };
    match budget_step(progress) {
        Some((LaunchPhase::Diffing, bytes)) => budget.diff_progress(bytes),
        Some((phase, bytes)) => budget.enter(phase, bytes),
        None => {}
    }
}

/// What a prepared launch hands its spawn. It holds no destination lock: Step 2
/// released it (SW-XS-2), so a spawn never waits on, or blocks, a sync.
struct PreparedRemoteLaunch {
    home: LaunchHome,
    backend: pty_backend::ResolvedTerminalBackend,
    provision_required: bool,
}

async fn prepare_remote_launch(
    app: &tauri::AppHandle,
    record: &RemoteShellRecord,
    headed: bool,
    caller: Option<&crate::mcp::remote_authority::RemoteAuthority>,
    prepared: &std::sync::Mutex<Option<crate::agents::prepare::progress::PrepareSummary>>,
) -> Result<PreparedRemoteLaunch, String> {
    if let Some(caller) = caller {
        caller
            .admit_launch(record.owner_id, &record.team_key)
            .await?;
    }
    super::super::machines::authorize_terminal_machine(app, &record.machine_id, &record.team_key)
        .await?;
    let home = launch_home(
        app,
        &record.team_key,
        &record.workspace_id,
        &record.cwd,
        headed,
        record.identity.durable_workspace,
    )?;
    let backend = pty_backend::resolve_spawn_backend(
        app,
        &record.machine_id,
        record.target.session.as_str(),
    )?;
    if !backend
        .admission_target()
        .is_some_and(|target| same_lifetime(target, &record.target.runtime))
    {
        return Err(
            "newIntentRequired: original remote server lifetime changed; start a new launch".into(),
        );
    }
    backend.revalidate(app)?;
    let mut capture = record.clone();
    let generates_hooks = backend
        .client()
        .is_some_and(|client| client.supports_agent_startup());
    let mcp =
        crate::mcp::remote_authority::capture_launch(app, &mut capture, caller, generates_hooks)
            .await?;
    if !record.matches_capture(&capture) {
        return Err(
            "newIntentRequired: captured MCP configuration changed; start a new launch".into(),
        );
    }
    let budget = crate::agents::launch_budget::current();
    let mut destination_guard = None;
    let mut step_two = None;
    // SW-XS-1: a saved Agent names its prepared workspace; a plain `claude` /
    // `codex` started inside a machine workspace's worktree gets its runtime's
    // context there too. A shell, or a launch outside a workspace, gets none.
    let workspace_handle = match &record.agent {
        Some(agent) => Some(agent.workspace_handle.clone()),
        None if record.launch.is_shell() => None,
        None => match workspace_on_machine(app, record) {
            WorkspacePlace::Worktree(handle) => Some(handle),
            WorkspacePlace::Elsewhere | WorkspacePlace::Unready(_) => None,
        },
    };
    if let Some(workspace_handle) = workspace_handle {
        let resolved = resolve_prepared(app, record, &workspace_handle, caller.cloned()).await?;
        let resolved = crate::mcp::remote_destination::from_resolved(resolved);
        let ctx = super::super::content_engine::derive_resolver_context(
            resolved.context.codesense_home,
            resolved.context.user_id,
            resolved.context.org_slug,
            resolved.context.team_name,
        )
        .map_err(|e| e.to_string())?;
        let coordinator = app
            .state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
            .inner()
            .clone();
        let identity = resolved.io.identity().clone();
        let crate::content_engine::destination::DestinationIdentity::Remote(remote) = &identity
        else {
            return Err("remote destination required".into());
        };
        destination_guard = Some(
            coordinator
                .acquire_remote_destination_lock(&ctx, &resolved.source_repo, remote)
                .await,
        );
        step_two = Some((
            ctx,
            resolved.source_repo,
            resolved.io,
            record.agent.as_ref().map(|agent| {
                (
                    agent.manifest.clone(),
                    agent.name.clone(),
                    agent.workspace_files.clone(),
                )
            }),
        ));
    }
    // SW-XS-2 / SW-XS-7: Step 2 (one diff, at most one batch) runs under the
    // destination lock; the lock is released when it ends, before the spawn.
    // The batch's Apply checks every pre-hash on the machine, so there is no
    // separate verification pass.
    let provision_required = crate::agents::remote_launch::under_destination_lock(
        destination_guard,
        async {
            if let Some((ctx, source_repo, io, agent)) = step_two {
                let launch = record.launch;
                let budget = budget.clone();
                let sink = crate::commands::worktree::TauriEventSink(app.clone());
                let report = await_blocking("remote_agent_prepare", move || {
                    crate::agents::remote_launch::prepare_workspace(
                        &ctx,
                        &source_repo,
                        io.as_ref(),
                        launch,
                        agent
                            .as_ref()
                            .map(|(manifest, name, _)| (manifest, name.as_str())),
                        agent
                            .as_ref()
                            .map(|(_, _, files)| files.as_slice())
                            .unwrap_or(&[]),
                        &|progress| report_step_two(budget.as_ref(), &sink, progress),
                    )
                })
                .await?;
                *prepared.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                    crate::agents::prepare::progress::PrepareSummary::of(&report),
                );
            }
            if let Some(budget) = &budget {
                budget.enter(crate::agents::launch_budget::LaunchPhase::Provisioning, 0);
            }
            let provision_required =
                crate::mcp::remote_authority::prepare_bound(app, record, mcp).await?;
            if record.agent.is_some() {
                let client = backend.client().ok_or("remote runtime unavailable")?;
                let session = record.target.session.clone();
                let cwd = record.cwd.clone();
                let launch = record.launch;
                await_blocking("preflight_remote_agent", move || {
                    client.preflight_native_remote(&session, &cwd, launch).map_err(|error| {
                        let message = error.to_string();
                        if message.contains("private Agent settings conflict") || message.contains("private Agent settings identity changed") {
                            "newIntentRequired: private settings changed; start a new launch without overwriting them".into()
                        } else { message }
                    })
                })
                .await?;
            }
            if let Some(caller) = caller {
                caller
                    .admit_launch(record.owner_id, &record.team_key)
                    .await?;
            }
            backend.revalidate(app)?;
            Ok(provision_required)
        },
    )
    .await?;
    Ok(PreparedRemoteLaunch {
        home,
        backend,
        provision_required,
    })
}

async fn finish_launch_inner(
    app: &tauri::AppHandle,
    record: &mut RemoteShellRecord,
    headed: bool,
    caller: Option<&crate::mcp::remote_authority::RemoteAuthority>,
    root: &std::path::Path,
    prepared: &std::sync::Mutex<Option<crate::agents::prepare::progress::PrepareSummary>>,
) -> Result<(), String> {
    let preparation_record = record.clone();
    crate::agents::remote_launch::drive_prepared(
        record.agent.is_none() || record.preparing(),
        prepare_remote_launch(app, &preparation_record, headed, caller, prepared),
        |prepared| async move {
            let PreparedRemoteLaunch {
                home,
                backend,
                provision_required,
            } = prepared;
            let attempted = await_blocking("consume_remote_spawn", {
                let app = app.clone();
                let root = root.to_path_buf();
                let mut record = record.clone();
                move || {
                    let fresh = app
                        .state::<DashboardLaunchState>()
                        .begin_remote_spawn(&root, &mut record)?;
                    Ok((fresh, record))
                }
            })
            .await?;
            *record = attempted.1;
            if !attempted.0 {
                return Err("spawn was already attempted; attach to the original session".into());
            }
            if let Some(caller) = caller {
                caller
                    .admit_launch(record.owner_id, &record.team_key)
                    .await?;
            }
            backend.revalidate(app)?;
            let meta_story = home.descriptor.story_id.clone();
            let meta_workspace = home.workspace.as_str().to_string();
            let open = OpenTab {
                viewer: app_viewer(),
                workspace: home.workspace,
                session: record.target.session.clone(),
                descriptor: Some(home.descriptor),
                name: record.name.clone(),
                role: None,
                tags: remote_layout_tags(record),
                placement: Placement::Tab,
                index: None,
                activate: false,
            };
            let args = SpawnPtyArgs {
                command: String::new(),
                // SW-WA-F10: a durable remote-shell tab is a plain
                // interactive shell — no argv-direct spawn, no native-launch
                // activity binding (routines carry their own `remote_launch`
                // through a different arm_prepare path).
                command_argv: None,
                cwd: record.cwd.clone(),
                size: super::super::pty::SpawnPtySize { cols: 80, rows: 24 },
                interactive: Some(true),
                starship_preset: None,
                story_id: meta_story,
                workspace_id: Some(meta_workspace),
                epic_id: None,
                scrollback: None,
                shell_integration: Some(true),
                env_remove: None,
                account: None,
                environment_binding: None,
                native_launch: None,
            };
            if let Some(budget) = crate::agents::launch_budget::current() {
                budget.enter(crate::agents::launch_budget::LaunchPhase::Spawning, 0);
            }
            open_remote_layout_tab(
                app.clone(),
                backend,
                open,
                args,
                provision_required.then_some(record.launch),
            )
            .await
            .map_err(|e| layout_refusal(e, &record.cwd))?;
            record.status = "opened";
            Ok(())
        },
    )
    .await
}

async fn reconcile(
    app: tauri::AppHandle,
    mut record: RemoteShellRecord,
    headed: bool,
    caller: Option<&crate::mcp::remote_authority::RemoteAuthority>,
) -> RemoteTabOutcome {
    if let Some(caller) = caller {
        if caller.validate().await.is_err() || caller.check_control(&record).is_err() {
            return RemoteTabOutcome::refused(
                "accessDenied",
                "target live MCP grant is outside caller scope or unavailable",
            );
        }
    }
    let authority = match app
        .state::<crate::commands::local_profile::LocalProfileState>()
        .snapshot()
    {
        Ok(authority) if authority.owner_id() == record.owner_id => authority,
        _ => {
            return RemoteTabOutcome::refused(
                "accessDenied",
                "The original private owner is unavailable",
            )
        }
    };
    let checked = await_blocking("remote_tab_attach", {
        let app = app.clone();
        let record = record.clone();
        move || {
            let backend = pty_backend::resolve_spawn_backend(
                &app,
                &record.machine_id,
                record.target.session.as_str(),
            )?;
            let target = backend
                .admission_target()
                .ok_or("remote admission unavailable")?;
            if !same_lifetime(target, &record.target.runtime) {
                return Err("The original terminal server lifetime is unavailable".into());
            }
            backend.revalidate(&app)?;
            if matches!(record.status, "pending" | "unknown") {
                // Only this connection's already reserved session can be completed;
                // absence is not permission to create or to adopt another session.
                if let Some(client) = backend.client() {
                    let _ = client.reconcile_remote_tab(&record.target.session);
                }
            }
            let snapshot = backend
                .domain()
                .layout_snapshot(&app_viewer(), &ClientBootId::new("remote-human"))
                .map_err(|e| e.to_string())?;
            let tab = snapshot
                .workspaces
                .get(&record.workspace)
                .and_then(|ws| {
                    ws.tabs
                        .iter()
                        .find(|tab| tab.session == record.target.session)
                })
                .ok_or("The original tab is not yet confirmed; creation remains unknown")?;
            if tab.tags.get("remoteHandle") != Some(&record.handle)
                || tab.tags.get("remoteRequestId") != Some(&record.request_id.to_string())
            {
                return Err("The original tab correlation is unavailable".into());
            }
            let claim = if !tab.state.is_exited() {
                Some(
                    backend
                        .client()
                        .ok_or("remote client unavailable")?
                        .claim_writable_typed(&record.target.session),
                )
            } else {
                None
            };
            backend.revalidate(&app)?;
            Ok((target.clone(), tab.state.is_exited(), claim))
        }
    })
    .await;
    if let Err(error) = authority.lease().validate() {
        return RemoteTabOutcome::refused("accessDenied", error);
    }
    match checked {
        Ok((target, ended, claim)) => {
            record.target.runtime = target;
            match claim {
                Some(Err(
                    crate::terminal::session_server::WritableAttachmentFailure::AttachedElsewhere,
                )) => {
                    let mut out = record.outcome(false);
                    out.status = "refused";
                    out.code = Some("attachedElsewhere".into());
                    out.message = Some("This terminal is attached in another connection owned by this account. Close that attachment or disconnect that app, then Attach here; the process is preserved.".into());
                    return out;
                }
                Some(Err(error)) => {
                    let mut out = record.outcome(false);
                    out.status = "unknown";
                    out.message = Some(error.to_string());
                    return out;
                }
                _ => record.status = if ended { "ended" } else { "alreadyLive" },
            }
        }
        Err(error) => {
            let mut out = record.outcome(false);
            out.status = if record.status == "pending" {
                "pending"
            } else {
                "unknown"
            };
            out.message = Some(error);
            return out;
        }
    }
    if let Err(error) = super::super::machines::authorize_terminal_machine(
        &app,
        &record.machine_id,
        &record.team_key,
    )
    .await
    {
        return RemoteTabOutcome::refused("accessDenied", error);
    }
    if let Err(error) = launch_home(
        &app,
        &record.team_key,
        &record.workspace_id,
        &record.cwd,
        headed,
        record.identity.durable_workspace,
    ) {
        return RemoteTabOutcome::refused("accessDenied", error);
    }
    if record.workspace_story_id.is_none() {
        record.workspace_story_id =
            workspace_story_id(&app, &record.team_key, &record.workspace_id);
    }
    app.state::<DashboardLaunchState>()
        .update_remote_shell(record.clone());
    record.outcome(headed)
}

/// SW-AGR-12: the layout tag on a durable (routine) launch's tab, so a
/// recovery from the layout re-homes it the same way.
pub(crate) const TAG_REMOTE_DURABLE: &str = "remoteDurable";

fn remote_layout_tags(record: &RemoteShellRecord) -> Tags {
    let mut tags = Tags::new();
    tags.insert("remoteHandle".into(), record.handle.clone());
    tags.insert("remoteRequestId".into(), record.request_id.to_string());
    // SW-AGR-2b: whose launch it is, so a wake never types into another
    // account's session on a shared machine.
    tags.insert(
        crate::routines::wake_app::TAG_REMOTE_OWNER.into(),
        record.owner_id.to_string(),
    );
    tags.insert("remoteWorkspaceId".into(), record.workspace_id.clone());
    tags.insert(
        "remoteLaunch".into(),
        serde_json::to_string(&record.launch).expect("a launch serializes"),
    );
    if keeps_permission_mode(record.launch) && !record.permission_mode.is_default() {
        tags.insert(
            "remotePermissionMode".into(),
            serde_json::to_string(&record.permission_mode).expect("mode enum serializes"),
        );
    }
    if record.identity.if_absent {
        tags.insert(
            codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT.into(),
            "1".into(),
        );
    }
    if record.identity.durable_workspace {
        tags.insert(TAG_REMOTE_DURABLE.into(), "1".into());
    }
    if let Some(agent) = &record.agent {
        tags.insert("remoteAgentKey".into(), agent.key());
        tags.insert(
            "remotePreparedWorkspace".into(),
            agent.workspace_handle.clone(),
        );
        tags.insert(
            "remoteStartupManifest".into(),
            agent.manifest.manifest_hash.clone(),
        );
    }
    if let Some(revision) = &record.mcp_profile_revision {
        tags.insert("remoteMcpProfileRevision".into(), revision.clone());
    }
    if !record.mcp_inventory.is_empty() {
        tags.insert(
            "remoteMcpInventory".into(),
            serde_json::to_string(&record.mcp_inventory).expect("inventory serializes"),
        );
    }
    tags
}

fn remote_layout_launch(tags: &Tags) -> Result<NativeLaunch, serde_json::Error> {
    Ok(tags
        .get("remoteLaunch")
        .map(|value| serde_json::from_str(value))
        .transpose()?
        .unwrap_or_default())
}

/// Whether a tab of `launch` comes back in its permission mode: its CLI's restore tags
/// carry it.
fn keeps_permission_mode(launch: NativeLaunch) -> bool {
    launch
        .cli()
        .is_some_and(|cli| adapter(cli).restore_tags().permission_mode)
}

/// A recovered tab's permission mode; a missing tag (or a launch whose restore tags do
/// not carry it) is the default, a malformed one is refused like a malformed launch.
fn remote_layout_permission_mode(
    tags: &Tags,
    launch: NativeLaunch,
) -> Result<RemotePermissionMode, serde_json::Error> {
    if !keeps_permission_mode(launch) {
        return Ok(RemotePermissionMode::Default);
    }
    Ok(tags
        .get("remotePermissionMode")
        .map(|value| serde_json::from_str(value))
        .transpose()?
        .unwrap_or_default())
}

async fn recover(
    app: tauri::AppHandle,
    machine: String,
    team: String,
    reference: RemoteTabReference,
    headed: bool,
    caller: Option<crate::mcp::remote_authority::RemoteAuthority>,
) -> RemoteTabOutcome {
    let snapshot = await_blocking("remote_tab_reconcile", {
        let app = app.clone();
        let machine = machine.clone();
        move || {
            let backend = pty_backend::resolve_spawn_backend(&app, &machine, "reconcile")?;
            backend.revalidate(&app)?;
            let target = backend
                .admission_target()
                .cloned()
                .ok_or("remote admission unavailable")?;
            let snapshot = backend
                .domain()
                .layout_snapshot(&app_viewer(), &ClientBootId::new("remote-human"))
                .map_err(|e| e.to_string())?;
            backend.revalidate(&app)?;
            let owner = backend
                .client()
                .and_then(|client| client.private_lease())
                .ok_or("private authority unavailable")?
                .owner_id();
            Ok((target, snapshot, owner))
        }
    })
    .await;
    let Ok((target, snapshot, owner)) = snapshot else {
        return RemoteTabOutcome::unknown(
            &reference,
            "Machine unavailable; creation remains unknown",
        );
    };
    let mut matches = Vec::new();
    for (key, ws) in snapshot.workspaces {
        for tab in ws.tabs {
            let matched = match &reference {
                RemoteTabReference::Handle(handle) => tab.tags.get("remoteHandle") == Some(handle),
                RemoteTabReference::RequestId(id) => {
                    tab.tags.get("remoteRequestId") == Some(&id.to_string())
                }
            };
            if !matched {
                continue;
            }
            let (Some(handle), Some(id), Some(workspace_id), Some(cwd)) = (
                tab.tags.get("remoteHandle"),
                tab.tags
                    .get("remoteRequestId")
                    .and_then(|id| Uuid::parse_str(id).ok()),
                tab.tags.get("remoteWorkspaceId"),
                ws.descriptor
                    .worktree_path
                    .as_ref()
                    .and_then(|path| path.to_str()),
            ) else {
                continue;
            };
            // SW-AGR-12: a durable launch's tab (a saved Agent's, in its
            // prepared workspace) is re-homed as it was launched.
            let durable = tab.tags.get(TAG_REMOTE_DURABLE).map(String::as_str) == Some("1")
                && tab.tags.contains_key("remotePreparedWorkspace");
            let if_absent = tab
                .tags
                .get(codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT)
                .map(String::as_str)
                == Some("1");
            let Ok(home) = launch_home(&app, &team, workspace_id, cwd, headed, durable) else {
                continue;
            };
            if home.workspace != key
                || home.descriptor.org != ws.descriptor.org
                || home.descriptor.team != ws.descriptor.team
            {
                continue;
            }
            let Ok(launch) = remote_layout_launch(&tab.tags) else {
                continue;
            };
            let Ok(permission_mode) = remote_layout_permission_mode(&tab.tags, launch) else {
                continue;
            };
            matches.push(RemoteShellRecord {
                agent: None,
                phase: RemoteSpawnPhase::Attempted,
                handle: handle.clone(),
                owner_id: owner,
                request_id: id,
                machine_id: machine.clone(),
                team_key: team.clone(),
                workspace_id: workspace_id.clone(),
                workspace_story_id: workspace_story_id(&app, &team, workspace_id),
                workspace: key.clone(),
                cwd: cwd.into(),
                name: Some(tab.name),
                mcp_profile_revision: tab.tags.get("remoteMcpProfileRevision").cloned(),
                mcp_inventory: tab
                    .tags
                    .get("remoteMcpInventory")
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_default(),
                hooks: false,
                launch,
                permission_mode,
                target: RuntimeSessionTarget {
                    runtime: target.clone(),
                    session: tab.session,
                },
                status: "unknown",
                identity: RemoteLaunchIdentity {
                    if_absent,
                    durable_workspace: durable,
                    ..Default::default()
                },
            });
        }
    }
    if matches.len() != 1 {
        return RemoteTabOutcome::unknown(
            &reference,
            "No unique authoritative tab evidence; do not create again",
        );
    }
    let record = matches.remove(0);
    let _ = app
        .state::<DashboardLaunchState>()
        .reserve_remote_shell(record.clone());
    reconcile(app, record, headed, caller.as_ref()).await
}

/// Recheck the current viewer grouping at the actual operation boundary.
pub(crate) fn validate_remote_shell_group(
    app: &tauri::AppHandle,
    record: &RemoteShellRecord,
) -> Result<(), String> {
    let viewer = app.state::<crate::mcp::state::McpViewerState>().get();
    remote_shell_group_in(&viewer, record)
}

/// [`validate_remote_shell_group`] against the viewer state `viewer`: what a
/// reverse-MCP authority check of the launched agent rests on. A durable
/// (routine) launch passes with its workspace closed in the UI.
pub(crate) fn remote_shell_group_in(
    viewer: &serde_json::Value,
    record: &RemoteShellRecord,
) -> Result<(), String> {
    let home = home_from(
        viewer,
        &record.team_key,
        &record.workspace_id,
        &record.cwd,
        false,
        record.identity.durable_workspace,
    )?;
    if home.workspace != record.workspace {
        return Err("original workspace grouping changed".into());
    }
    Ok(())
}

pub(crate) async fn authorize_human_terminal(
    app: &tauri::AppHandle,
    reference: &RuntimeSessionReference,
) -> Result<bool, String> {
    let RuntimeSessionReference::Admitted(target) = reference else {
        return Ok(false);
    };
    if target.runtime.member.slot != super::super::machines::RuntimeMemberSlot::Remote {
        return Ok(false);
    }
    let record = app
        .state::<DashboardLaunchState>()
        .remote_shell_for_target(target)
        .ok_or("remote terminal must be explicitly attached first")?;
    super::super::machines::authorize_terminal_machine(app, &record.machine_id, &record.team_key)
        .await?;
    validate_remote_shell_group(app, &record)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use codesense_host_proto::agent_cli::AgentCli;

    /// SW-XS-7 (SHOULD 10): Step 2's progress enters the launch budget by
    /// bytes — the diff by bytes hashed, the transfer and the apply by bytes
    /// moved — never by an operation count.
    #[test]
    fn step_two_progress_is_budgeted_by_bytes() {
        use crate::agents::launch_budget::LaunchPhase;
        use crate::agents::prepare::{PreparePhase, PrepareProgress};
        let at = |phase, bytes, total| PrepareProgress {
            workspace: "w".into(),
            agent: "a".into(),
            phase,
            files: 7,
            bytes,
            total,
            note: None,
            summary: None,
            operation_id: None,
            operation_kind: None,
        };
        assert_eq!(
            budget_step(&at(PreparePhase::Diff, 5, 99)),
            Some((LaunchPhase::Diffing, 5))
        );
        assert_eq!(
            budget_step(&at(PreparePhase::Transfer, 1, 3 << 20)),
            Some((LaunchPhase::Transferring, 3 << 20))
        );
        assert_eq!(
            budget_step(&at(PreparePhase::Apply, 0, 3 << 20)),
            Some((LaunchPhase::Transferring, 3 << 20))
        );
        assert_eq!(budget_step(&at(PreparePhase::Ready, 0, 0)), None);
    }

    fn quiet_budget() -> crate::agents::launch_budget::LaunchBudget {
        crate::agents::launch_budget::LaunchBudget::new(|_| {})
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_that_never_answers_is_reported_unconfirmed_within_the_bound() {
        let request_id = Uuid::new_v4();
        let started = tokio::time::Instant::now();
        let outcome = super::within_bound(
            std::future::pending::<RemoteTabOutcome>(),
            quiet_budget(),
            RemoteTabReference::RequestId(request_id),
            None,
        )
        .await;
        assert_eq!(outcome.status, "unknown");
        assert_eq!(outcome.code.as_deref(), Some("timedOut"));
        assert_eq!(outcome.request_id, Some(request_id));
        // An open that never leaves admission is answered after admission's budget.
        assert_eq!(
            started.elapsed(),
            crate::agents::launch_budget::ADMIT_BUDGET
        );
    }

    /// SW-XS-2 AC4, through the bound the launch path uses: an open that is
    /// copying 3 MiB is answered after the 90 s transfer budget (not a flat
    /// 120 s), the answer names the phase, and progress showed Transferring.
    #[tokio::test(start_paused = true)]
    async fn a_3_mib_transfer_is_answered_after_its_90_s_budget_naming_the_phase() {
        use crate::agents::launch_budget::{current, LaunchBudget, LaunchPhase};
        let phases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = phases.clone();
        let budget = LaunchBudget::new(move |progress| seen.lock().unwrap().push(progress));
        let started = tokio::time::Instant::now();
        let outcome = super::within_bound(
            async {
                current()
                    .expect("the open runs inside its budget")
                    .enter(LaunchPhase::Transferring, 3 * 1024 * 1024);
                std::future::pending::<RemoteTabOutcome>().await
            },
            budget,
            RemoteTabReference::RequestId(Uuid::new_v4()),
            None,
        )
        .await;
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(90));
        assert_eq!(outcome.code.as_deref(), Some("timedOut"));
        assert!(
            outcome
                .message
                .as_deref()
                .is_some_and(|m| m.contains("Transferring") && m.contains("90s")),
            "{:?}",
            outcome.message
        );
        assert!(phases
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.phase == LaunchPhase::Transferring && p.budget_ms == 90_000));
    }

    #[test]
    fn launch_progress_is_one_flat_payload_keyed_by_request() {
        let id = Uuid::nil();
        let payload = super::RemoteLaunchProgressPayload {
            request_id: Some(id),
            handle: None,
            progress: crate::agents::launch_budget::LaunchProgress {
                phase: crate::agents::launch_budget::LaunchPhase::Transferring,
                bytes: Some(3),
                budget_ms: 10_000,
            },
        };
        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({"requestId": id, "phase": "transferring", "bytes": 3, "budgetMs": 10000})
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_that_answers_in_time_is_passed_through() {
        let outcome = super::within_bound(
            async { RemoteTabOutcome::refused("invalid", "no") },
            quiet_budget(),
            RemoteTabReference::Handle("remote-tab-x".into()),
            None,
        )
        .await;
        assert_eq!(outcome.status, "refused");
        assert_eq!(outcome.code.as_deref(), Some("invalid"));
    }

    use super::*;
    /// Records what a bounded open reports after its caller was answered.
    #[derive(Default)]
    struct RecordedSettlement {
        running: std::sync::atomic::AtomicBool,
        settled: std::sync::Mutex<Option<RemoteTabOutcome>>,
    }
    impl SettleLaunch for RecordedSettlement {
        fn running(&self) {
            self.running
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn settled(&self, outcome: &RemoteTabOutcome) {
            *self.settled.lock().unwrap() = Some(outcome.clone());
        }
    }

    fn opened(handle: &str, request_id: Uuid) -> RemoteTabOutcome {
        let mut outcome = RemoteTabOutcome::refused("x", "x");
        outcome.status = "opened";
        outcome.code = None;
        outcome.message = None;
        outcome.handle = Some(handle.into());
        outcome.request_id = Some(request_id);
        outcome
    }

    /// SW-XS-1 AC4 (X3): an open that outlives its caller keeps running, and when
    /// it spawns it reports the handle (`launch-settled`) instead of spawning silently.
    #[tokio::test(start_paused = true)]
    async fn xs1_an_open_that_outlives_its_caller_settles_with_its_handle() {
        let request_id = Uuid::new_v4();
        let sink = std::sync::Arc::new(RecordedSettlement::default());
        let outcome = within_bound(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(90)).await;
                opened("remote-tab-late", request_id)
            },
            quiet_budget(), // admitting: 60 s
            RemoteTabReference::RequestId(request_id),
            Some(sink.clone()),
        )
        .await;
        assert_eq!(outcome.code.as_deref(), Some("timedOut"));
        assert!(sink.running.load(std::sync::atomic::Ordering::SeqCst));
        assert!(sink.settled.lock().unwrap().is_none());
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let settled = sink
            .settled
            .lock()
            .unwrap()
            .clone()
            .expect("launch-settled");
        assert_eq!(settled.handle.as_deref(), Some("remote-tab-late"));
        assert_eq!(settled.request_id, Some(request_id));
    }

    /// An open that answers in time never reports a settlement.
    #[tokio::test(start_paused = true)]
    async fn xs1_an_open_that_answers_in_time_needs_no_settlement() {
        let request_id = Uuid::new_v4();
        let sink = std::sync::Arc::new(RecordedSettlement::default());
        let outcome = within_bound(
            async move { opened("remote-tab-fast", request_id) },
            quiet_budget(), // admitting: 60 s
            RemoteTabReference::RequestId(request_id),
            Some(sink.clone()),
        )
        .await;
        assert_eq!(outcome.status, "opened");
        assert!(!sink.running.load(std::sync::atomic::Ordering::SeqCst));
        assert!(sink.settled.lock().unwrap().is_none());
    }

    /// SW-XS-1 AC4: `attach {requestId}` while the launch still runs answers
    /// `pending`; once it settled with a handle, every attach goes on to the
    /// reserved record (which returns that handle); a final refusal is returned
    /// as is, an unknown or retryable one is reconciled. Records are per owner
    /// and expire.
    #[test]
    fn xs1_attach_by_request_id_follows_the_launch_settlement() {
        let state = DashboardLaunchState::new(1);
        let id = Uuid::new_v4();
        let key = |owner: u64, request: Uuid| {
            (
                owner,
                "acme:default".to_string(),
                "box".to_string(),
                request,
            )
        };
        let reference = RemoteTabReference::RequestId(id);
        let now = std::time::Instant::now();
        let lookup = |at| state.remote_launch_settlement_at(&key(7, id), at);
        assert!(attach_settlement(lookup(now), &reference).is_none());
        state.remote_launch_running_at(key(7, id), now);
        let pending = attach_settlement(lookup(now), &reference).unwrap();
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.request_id, Some(id));
        assert!(pending.handle.is_none());
        // Another owner never sees it.
        assert!(state
            .remote_launch_settlement_at(&key(8, id), now)
            .is_none());
        state.settle_remote_launch_at(key(7, id), opened("remote-tab-1", id), now);
        for _ in 0..2 {
            match lookup(now) {
                Some(LaunchSettlement::Settled(outcome)) => {
                    assert_eq!(outcome.handle.as_deref(), Some("remote-tab-1"))
                }
                other => panic!("expected the settled handle, got {other:?}"),
            }
            assert!(attach_settlement(lookup(now), &reference).is_none());
        }
        assert!(lookup(now + LAUNCH_SETTLEMENT_TTL + std::time::Duration::from_secs(1)).is_none());

        let settle = |outcome: RemoteTabOutcome| {
            let request = Uuid::new_v4();
            state.settle_remote_launch_at(key(7, request), outcome, now);
            attach_settlement(
                state.remote_launch_settlement_at(&key(7, request), now),
                &RemoteTabReference::RequestId(request),
            )
        };
        let answer = settle(RemoteTabOutcome::refused(
            "agentSetupFailed",
            "Couldn't set up",
        ))
        .unwrap();
        assert_eq!(answer.status, "refused");
        assert_eq!(answer.code.as_deref(), Some("agentSetupFailed"));
        let mut retryable = RemoteTabOutcome::refused("preparationIncomplete", "retry");
        retryable.retryable = true;
        assert!(settle(retryable).is_none());
        assert!(settle(RemoteTabOutcome::unknown(&reference, "no answer")).is_none());
    }

    fn receipt_record() -> RemoteShellRecord {
        RemoteShellRecord {
            workspace_story_id: None,
            agent: None, phase: RemoteSpawnPhase::Attempted,
            handle: "remote-tab-original".into(),
            owner_id: 41, request_id: Uuid::new_v4(), machine_id: "remote-machine".into(), team_key: "org:team".into(),
            workspace_id: "work".into(), workspace: WorkspaceKey::new("org:team|work"), cwd: "/home/account".into(), name: None, mcp_profile_revision: None, mcp_inventory: Vec::new(), hooks: false, launch: NativeLaunch::Shell, permission_mode: Default::default(),
            target: serde_json::from_value(serde_json::json!({"member":{"machineId":"remote-machine","slot":"remote"},"admissionRevision":"1","namespace":{"serverId":"server","epoch":"epoch","generation":"1"},"session":"original-session"})).unwrap(),
            status: "pending",
            identity: Default::default(),
        }
    }

    /// SW-AGR-12 AC1/AC2: a routine's launch into a prepared workspace homes
    /// without the viewer's row (closed in the UI, or before the frontend has
    /// published anything), keyed as the viewer keys that repo workspace, so
    /// it groups with it once it is opened. Any other launch still needs it.
    #[test]
    fn chat_story_context_resolves_filename_stems_to_real_story_ids() {
        let viewer = serde_json::json!({ "workspaces": { "org:team": [{ "id": "sv-x-fix-login", "kind": "story", "storyId": "SV-X" }] } });
        assert_eq!(
            workspace_story_id_in(&viewer, "org:team", "sv-x-fix-login").as_deref(),
            Some("SV-X")
        );
        assert_eq!(workspace_story_id_in(&viewer, "org:team", "missing"), None);
    }

    #[test]
    fn agr12_a_durable_launch_homes_without_the_viewer_row() {
        let team = "acme/api:backend";
        let cwd = "/home/dev/work/api";
        let none = serde_json::json!({});
        let refused = home_from(&none, team, "api-main", cwd, false, false).unwrap_err();
        assert!(refused.contains("not open"), "{refused}");
        let durable = home_from(&none, team, "api-main", cwd, false, true).unwrap();
        assert_eq!(durable.descriptor.kind, "generic");
        assert_eq!(
            durable.descriptor.worktree_path.as_deref(),
            Some(std::path::Path::new(cwd))
        );
        let opened = serde_json::json!({ "workspaces": { team: [
            { "id": "api-main", "kind": "generic" },
        ]}});
        let viewed = home_from(&opened, team, "api-main", cwd, false, false).unwrap();
        assert_eq!(durable.workspace, viewed.workspace);
        assert_eq!(
            home_from(&opened, team, "api-main", cwd, false, true)
                .unwrap()
                .workspace,
            viewed.workspace,
            "an open row is read as before"
        );
        // Open as a story: the row wins, so the durable record's group no
        // longer matches it and the launch is refused, never re-grouped.
        let story = serde_json::json!({ "workspaces": { team: [
            { "id": "api-main", "kind": "story", "storyId": "SW-12" },
        ]}});
        assert_ne!(
            home_from(&story, team, "api-main", cwd, false, true)
                .unwrap()
                .workspace,
            durable.workspace
        );
    }

    /// SW-AGR-12 AC3: the receipt still gates a durable launch exactly: its
    /// worktree, workspace, team and terminal server each must match.
    #[test]
    fn agr12_the_receipt_gate_still_matches_cwd_workspace_team_and_server() {
        let mut record = receipt_record();
        record.identity.durable_workspace = true;
        let (cwd, ws, team, server) = ("/home/account", "work", "org:team", "server");
        assert!(receipt_gate(cwd, ws, team, server, &record).is_ok());
        for (cwd, ws, team, server) in [
            ("/home/other", ws, team, server),
            (cwd, "elsewhere", team, server),
            (cwd, ws, "org:other", server),
            (cwd, ws, team, "replaced-server"),
        ] {
            let refused = receipt_gate(cwd, ws, team, server, &record).unwrap_err();
            assert!(refused.contains("exact"), "{refused}");
        }
    }

    /// SW-AGR-12 (review r1): a durable launch's tab is tagged so a recovery
    /// from the layout re-homes it durably, and the launched agent's
    /// reverse-MCP authority (the group check it rests on) holds with its
    /// workspace closed in the UI; a plain launch's does not.
    #[test]
    fn agr12_a_durable_tab_is_tagged_and_its_group_holds_without_the_viewer() {
        let mut record = receipt_record();
        record.workspace = crate::commands::pty_layout::workspace_key("org:team", "work");
        let none = serde_json::json!({});
        assert!(remote_shell_group_in(&none, &record).is_err());
        assert!(!remote_layout_tags(&record).contains_key(TAG_REMOTE_DURABLE));
        record.identity.durable_workspace = true;
        record.identity.if_absent = true;
        assert!(remote_shell_group_in(&none, &record).is_ok());
        let tags = remote_layout_tags(&record);
        assert_eq!(tags.get(TAG_REMOTE_DURABLE).map(String::as_str), Some("1"));
        assert_eq!(
            tags.get(codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT)
                .map(String::as_str),
            Some("1")
        );
    }

    /// SW-AGR-12: the durable mark is the app's own: never over the wire, and
    /// kept on the launch record so a restore re-homes it the same way.
    #[test]
    fn agr12_the_durable_mark_is_app_only_and_survives_the_record() {
        let mut record = receipt_record();
        record.identity.durable_workspace = true;
        let back: RemoteShellRecord =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        assert!(back.identity.durable_workspace);
        let plain: RemoteShellRecord =
            serde_json::from_value(serde_json::to_value(receipt_record()).unwrap()).unwrap();
        assert!(!plain.identity.durable_workspace);
        let wire: Result<RemoteShellTabRequest, _> = serde_json::from_value(serde_json::json!({
            "operation": "create", "requestId": Uuid::new_v4(), "machineId": "m",
            "teamKey": "org:team", "workspaceId": "work", "cwd": null, "name": null,
            "agentKey": null, "preparedWorkspaceHandle": null, "headed": false,
            "durableWorkspace": true,
        }));
        assert!(wire.is_err(), "not settable over the wire");
    }

    #[test]
    fn a_launched_remote_agent_lists_with_its_launcher_role_and_face() {
        let state = DashboardLaunchState::new(4);
        let mut record = receipt_record();
        record.name = Some("Worker 1".into());
        record.identity = RemoteLaunchIdentity {
            launched_by: Some("SW-1#lead".into()),
            role: Some("worker".into()),
            avatar: Some("builtin-funny:skeptical-goat".into()),
            story: Some("SW-1".into()),
            ..Default::default()
        };
        state.reserve_remote_shell(record).unwrap();
        let mut plain = receipt_record();
        plain.handle = "remote-tab-shell".into();
        plain.request_id = Uuid::new_v4();
        state.reserve_remote_shell(plain).unwrap();
        let rows = state.snapshot();
        let agent = rows
            .iter()
            .find(|r| r.handle == "remote-tab-original")
            .unwrap();
        assert_eq!(agent.meta.display_name.as_deref(), Some("Worker 1"));
        assert_eq!(agent.meta.launched_by.as_deref(), Some("SW-1#lead"));
        assert_eq!(agent.meta.role.as_deref(), Some("worker"));
        assert_eq!(
            agent.meta.avatar.as_deref(),
            Some("builtin-funny:skeptical-goat")
        );
        assert_eq!(agent.meta.story.as_deref(), Some("SW-1"));
        assert_eq!(
            agent.meta.team,
            Some(crate::commands::dashboard_launch::LaunchTeam {
                org: "org".into(),
                team: "team".into()
            })
        );
        // A tab a person opened carries none of it.
        let shell = rows
            .iter()
            .find(|r| r.handle == "remote-tab-shell")
            .unwrap();
        assert_eq!(shell.meta, Default::default());

        // Its roster row carries the session its tab runs under on the machine, so the
        // viewer finds its tab, its live dot and the agents it launched; and it reads
        // running while that tab runs, never errored for want of a local session.
        let snapshot = state
            .snapshot()
            .into_iter()
            .find(|r| r.handle == "remote-tab-original")
            .unwrap();
        let row = crate::commands::dashboard_launch::remote_agent_row(
            snapshot.clone(),
            true,
            Some("original-session".into()),
        );
        assert_eq!(row.session_id.as_deref(), Some("original-session"));
        assert_eq!(
            row.status,
            crate::commands::dashboard_launch::AgentStatus::Running
        );
        assert_eq!(row.meta.role.as_deref(), Some("worker"));
        let ended = crate::commands::dashboard_launch::remote_agent_row(
            snapshot,
            false,
            Some("original-session".into()),
        );
        assert_eq!(
            ended.status,
            crate::commands::dashboard_launch::AgentStatus::Done
        );
    }

    #[test]
    fn a_relaunch_finds_the_agent_by_what_it_was_launched_as() {
        use crate::commands::workspace_open::launched_as;
        let mut record = receipt_record();
        record.name = Some("Worker 1".into());
        // The workspace id is the story file's stem, which need not be the story id.
        record.workspace_id = "sw-1-fix-login".into();
        assert!(launched_as(
            &record,
            "remote-machine",
            "org:team",
            "sw-1-fix-login",
            "Worker 1"
        ));
        assert!(!launched_as(
            &record,
            "remote-machine",
            "org:team",
            "SW-1",
            "Worker 1"
        ));
        assert!(!launched_as(
            &record,
            "other",
            "org:team",
            "sw-1-fix-login",
            "Worker 1"
        ));
        assert!(!launched_as(
            &record,
            "remote-machine",
            "org:other",
            "sw-1-fix-login",
            "Worker 1"
        ));
        assert!(!launched_as(
            &record,
            "remote-machine",
            "org:team",
            "sw-1-fix-login",
            "Worker 2"
        ));
    }

    #[test]
    fn a_remote_agent_identity_survives_its_receipt() {
        let mut record = receipt_record();
        record.identity.launched_by = Some("SW-1#lead".into());
        for choice in [None, Some(None), Some(Some("reviewer".into()))] {
            record.identity.queue_role = choice;
            let json = serde_json::to_value(&record).unwrap();
            let back: RemoteShellRecord = serde_json::from_value(json).unwrap();
            assert_eq!(back.identity, record.identity);
        }
        // A receipt written before identities were kept still reads.
        let mut old = serde_json::to_value(receipt_record()).unwrap();
        old.as_object_mut().unwrap().remove("identity");
        let back: RemoteShellRecord = serde_json::from_value(old).unwrap();
        assert!(back.identity.is_empty());
    }

    #[test]
    fn a_remote_agent_starts_with_its_prompt_and_is_reachable_by_its_inbox() {
        use crate::agents::resolve::tests::{alice, draft, save, team_content, ORG, TEAM};
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        let mut definition = crate::agents::model::Agent {
            default_prompt: "Review the story.".into(),
            ..draft("reviewer", codesense_pty_core::agent_cli::AgentCli::Claude)
        };
        definition.mcp.servers.clear();
        let key = save(root.path(), &alice(), definition);
        let mut agent = crate::agents::remote_launch::prepare(
            root.path(),
            &alice(),
            &key,
            ORG.into(),
            TEAM.into(),
            "prepared".into(),
        )
        .unwrap();
        // The Agent's own default prompt rides the launch to the machine as its starting
        // prompt; a launch's own prompt (a slash command too) replaces it.
        assert_eq!(agent.startup.prompt.as_deref(), Some("Review the story."));
        crate::agents::remote_launch::start_with_prompt(&mut agent, Some("/review SW-1".into()))
            .unwrap();
        assert_eq!(agent.startup.prompt.as_deref(), Some("/review SW-1"));

        let state = DashboardLaunchState::new(4);
        let mut record = receipt_record();
        take_agent(&mut record, agent.clone());
        // A launch that names no tab gets the Agent's name; one that names it keeps it.
        assert_eq!(record.name.as_deref(), Some("Agent reviewer"));
        let mut named = receipt_record();
        named.name = Some("Worker 1".into());
        take_agent(&mut named, agent);
        assert_eq!(named.name.as_deref(), Some("Worker 1"));
        record.identity.launched_by = Some("SW-1#lead".into());
        state.reserve_remote_shell(record).unwrap();
        // Its $CODESENSE_TAB_SESSION_ID on the machine is its tab's session: it reads its
        // inbox under the remote tab's handle, and its hooks announce that inbox.
        assert_eq!(
            state
                .remote_handle_for_session("original-session")
                .as_deref(),
            Some("remote-tab-original")
        );
        // Its row names the saved Agent it runs, so the Agents panel offers its inbox, and
        // what it started with.
        let row = state
            .snapshot()
            .into_iter()
            .find(|r| r.handle == "remote-tab-original")
            .unwrap();
        assert_eq!(
            row.agent_revision.as_deref(),
            Some(format!("{key}@1").as_str())
        );
        assert_eq!(row.started_with.as_deref(), Some("/review SW-1"));
        // A tab with no saved Agent (Claude started by hand, or a plain shell) still
        // reads its inbox under its tab's handle.
        let mut shell = receipt_record();
        shell.handle = "remote-tab-shell".into();
        shell.request_id = Uuid::new_v4();
        shell.target.session = codesense_pty_core::domain::PtySessionId::new("shell-session");
        state.reserve_remote_shell(shell).unwrap();
        assert_eq!(
            state.remote_handle_for_session("shell-session").as_deref(),
            Some("remote-tab-shell")
        );
        assert_eq!(state.remote_handle_for_session("no-such-session"), None);
    }

    /// An agent that opens a remote tab over MCP is that tab's launcher in
    /// list_sessions; a tab a person opened lists with no launcher.
    #[tokio::test]
    async fn remote_startup_is_provisioned_only_after_registration_and_surfaces_provider_errors() {
        use crate::agents::resolve::tests::{alice, draft, save, team_content, ORG, TEAM};
        use crate::commands::role_startup::tests::StartupHost;
        use crate::weft::{role_inbox, role_state};
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        let mut definition = draft(
            "remote-handler",
            codesense_pty_core::agent_cli::AgentCli::Claude,
        );
        definition.role = Some("reviewer".into());
        definition.default_prompt = "/review queued work".into();
        definition.mcp.servers.clear();
        let key = save(root.path(), &alice(), definition);
        let captured = crate::agents::remote_launch::prepare(
            root.path(),
            &alice(),
            &key,
            ORG.into(),
            TEAM.into(),
            "prepared".into(),
        )
        .unwrap();
        let host = StartupHost::new(root.path().join("inbox.sqlite3"));
        let now = chrono::Utc::now().timestamp();
        role_inbox::send_role_message_commit(
            &host.store(),
            "human",
            "reviewer",
            "before launch",
            None,
            None,
            None,
            "",
            now,
            None,
            None,
            None,
        )
        .unwrap();
        let mut record = receipt_record();
        take_agent(&mut record, captured);
        crate::mcp::remote_authority::prepare_agent_startup(&host, &record, |startup| async {
            let startup = startup.unwrap();
            let prompt = startup.prompt.unwrap();
            assert!(prompt.starts_with("/review queued work\n\n"));
            assert!(prompt.contains("1 messages are pending"));
            let queue =
                role_state::role_state_read(&host.store(), Some("reviewer"), now, 0).unwrap();
            assert_eq!((queue[0].counts.pending, queue[0].counts.claimed), (1, 0));
            let registration = &queue[0].registrations[0];
            assert_eq!(registration.session_id.as_deref(), Some("original-session"));
            assert!(registration.live);
            assert!(registration.expires_at_ms >= (now + 179) * 1000);
            Ok(())
        })
        .await
        .unwrap();
        let failed: Result<(), String> =
            crate::mcp::remote_authority::prepare_agent_startup(&host, &record, |_| async {
                Err("startup provision refused".into())
            })
            .await;
        assert_eq!(failed.unwrap_err(), "startup provision refused");
    }

    #[test]
    fn an_agent_opened_remote_tab_lists_its_launcher_and_a_person_opened_one_none() {
        use crate::mcp::machine_tools::{
            resolve_launcher, with_launcher, Launcher, OpenRemoteTabParams, TabOpener,
        };
        let open = |launched_by: Option<&str>| {
            let mut value = serde_json::json!({"operation":"create","requestId":Uuid::new_v4(),
                "machineId":"remote-machine","teamKey":"org:team","workspaceId":"work","cwd":"/home/account"});
            if let Some(by) = launched_by {
                value["launchedBy"] = by.into();
            }
            serde_json::from_value::<OpenRemoteTabParams>(value).unwrap()
        };
        let list = |handle: &str, request: RemoteShellTabRequest| {
            let RemoteShellTabRequest::Create { identity, .. } = request else {
                panic!("create")
            };
            let state = DashboardLaunchState::new(4);
            let mut record = receipt_record();
            record.handle = handle.into();
            record.identity = *identity;
            state.reserve_remote_shell(record).unwrap();
            let rows = state
                .snapshot()
                .into_iter()
                .map(|s| crate::commands::dashboard_launch::session_row(s, true))
                .collect();
            crate::mcp::session_list::session_list_rows(rows, |_| None, None)
                .into_iter()
                .find(|row| row.handle == handle)
                .unwrap()
                .launched_by
        };
        // An agent on a machine that names no launcher is recorded as its own tab.
        let remote = TabOpener::RemoteTab {
            handle: "remote-tab-lead",
        };
        let params = open(None);
        let Launcher::Own(by) = resolve_launcher(remote, params.claimed_launcher()) else {
            panic!("own tab")
        };
        let request = with_launcher(params.into_request().unwrap(), Some(by));
        assert_eq!(
            list("remote-tab-new", request).as_deref(),
            Some("remote-tab-lead")
        );
        // One that names another session has it recorded, once it is confirmed running
        // and resolved to its handle.
        let params = open(Some("SW-1#lead"));
        assert_eq!(
            resolve_launcher(remote, params.claimed_launcher()),
            Launcher::Named("SW-1#lead".into())
        );
        let request = with_launcher(params.into_request().unwrap(), Some("SW-1#lead".into()));
        assert_eq!(
            list("remote-tab-other", request).as_deref(),
            Some("SW-1#lead")
        );
        // A local agent's session, once confirmed running and normalized, is listed.
        let params = open(Some("SW-1#lead"));
        assert_eq!(
            resolve_launcher(TabOpener::Local, params.claimed_launcher()),
            Launcher::Named("SW-1#lead".into())
        );
        let request = with_launcher(params.into_request().unwrap(), Some("SW-1#lead".into()));
        assert_eq!(
            list("remote-tab-local", request).as_deref(),
            Some("SW-1#lead")
        );
        // A person's open names nobody, and the tab lists with no launcher.
        let params = open(None);
        assert_eq!(
            resolve_launcher(TabOpener::Local, params.claimed_launcher()),
            Launcher::None
        );
        let request = with_launcher(params.into_request().unwrap(), None);
        assert_eq!(list("remote-tab-human", request), None);
    }

    #[test]
    fn a_first_prompt_is_never_sent_over_the_wire_identity() {
        let request: RemoteShellTabRequest = serde_json::from_value(serde_json::json!({
            "operation": "create", "requestId": Uuid::new_v4(), "machineId": "m",
            "teamKey": "org:team", "workspaceId": "w", "cwd": "/w", "name": null,
            "agentKey": null, "preparedWorkspaceHandle": null, "headed": true,
            "initialPrompt": "go"
        }))
        .unwrap();
        let RemoteShellTabRequest::Create {
            initial_prompt,
            identity,
            ..
        } = request
        else {
            panic!("create")
        };
        assert_eq!(initial_prompt.as_deref(), Some("go"));
        assert!(identity.is_empty());
        // Who launched it is the app's to say, never the wire's.
        assert!(
            serde_json::from_value::<RemoteShellTabRequest>(serde_json::json!({
                "operation": "create", "requestId": Uuid::new_v4(), "machineId": "m",
                "teamKey": "org:team", "workspaceId": "w", "cwd": "/w", "name": null,
                "agentKey": null, "preparedWorkspaceHandle": null, "headed": true,
                "identity": {"launched_by": "x"}
            }))
            .is_err()
        );
    }

    #[test]
    fn an_agent_launch_sets_up_only_the_workspace_that_lives_there() {
        let row = serde_json::json!({
            "id": "SW-1", "kind": "story", "storyId": "SW-1",
            "location": {"kind": "machine", "machineId": "box", "requestId": "r",
                "handle": "remote-workspace-h", "cwd": "/home/dev/codesense/worktrees/t/SW-1/"}
        });
        let at = "/home/dev/codesense/worktrees/t/SW-1";
        assert_eq!(
            workspace_location(&row, "box", at),
            WorkspacePlace::Worktree("remote-workspace-h".into())
        );
        // A workspace that lives elsewhere: nothing to set up.
        assert_eq!(
            workspace_location(&row, "other", at),
            WorkspacePlace::Elsewhere
        );
        let local = serde_json::json!({"id": "SW-1", "kind": "story", "cwd": at});
        assert_eq!(
            workspace_location(&local, "box", at),
            WorkspacePlace::Elsewhere
        );
        // It lives here, but not where the agent would start, or isn't made
        // yet: refused, never started unset.
        assert!(matches!(
            workspace_location(&row, "box", "/home/dev"),
            WorkspacePlace::Unready(_)
        ));
        let mut creating = row.clone();
        creating["location"]
            .as_object_mut()
            .unwrap()
            .remove("handle");
        assert!(matches!(
            workspace_location(&creating, "box", at),
            WorkspacePlace::Unready(_)
        ));
    }

    #[test]
    fn a_folder_conflict_is_a_refusal_naming_the_folder() {
        let error = layout_refusal(
            LayoutError::CwdConflict(WorkspaceKey::new("org:team|work")),
            "/home/account/other",
        );
        let message = cwd_conflict_message(&error).expect("a folder conflict");
        assert!(message.contains("/home/account/other"), "{message}");
        assert!(message.contains("another folder"), "{message}");
        let other = layout_refusal(
            LayoutError::UnknownWorkspace(WorkspaceKey::new("org:team|work")),
            "/home/account",
        );
        assert_eq!(cwd_conflict_message(&other), None);
    }

    #[test]
    fn a_missing_cli_is_refused_with_its_own_code() {
        for (launch, noun) in [
            (NativeLaunch::Cli(AgentCli::Claude), "Claude Code"),
            (NativeLaunch::Cli(AgentCli::Codex), "OpenAI Codex"),
        ] {
            let refusal = cli_missing(launch);
            assert_eq!(refusal.status, "refused");
            assert_eq!(refusal.code.as_deref(), Some("agentCliMissing"));
            assert_eq!(
                refusal.message.as_deref(),
                Some(format!("{noun} isn't installed on this machine").as_str())
            );
        }
        assert!(is_cli_missing(&format!(
            "claude {}",
            codesense_host_proto::reverse_mcp::NATIVE_RUNTIME_MISSING
        )));
        assert!(!is_cli_missing("remote login PATH unavailable"));
    }

    /// SW-XS-6: the version gate's failure reads "Update the terminal server
    /// on {machine}" and carries the code the UI's one-click update keys on.
    #[test]
    fn a_version_gate_failure_is_an_outdated_server_refusal() {
        let gate = DestinationBatchError::ServerOutdated {
            machine_id: "devbox".into(),
        }
        .to_string();
        let message = outdated_server_message(&format!("prepare agent context: {gate}")).unwrap();
        assert!(
            message.contains("Update the terminal server on devbox"),
            "{message}"
        );
        assert!(!message.contains('['), "{message}");
        assert_eq!(
            outdated_server_message("remote login PATH unavailable"),
            None
        );
        assert_eq!(TERMINAL_SERVER_OUTDATED, "terminalServerOutdated");
    }

    #[test]
    fn an_outdated_terminal_server_refuses_a_home_open_with_its_own_code() {
        let outdated = home_refusal(AccountHomeError::OutdatedServer);
        assert_eq!(outdated.status, "refused");
        assert_eq!(outdated.code.as_deref(), Some("terminalServerOutdated"));
        assert!(outdated
            .message
            .as_deref()
            .is_some_and(|message| message.contains("update the terminal server")));
        let other = home_refusal(AccountHomeError::Failed("host gone".into()));
        assert_eq!(other.code.as_deref(), Some("unavailable"));
        assert_eq!(other.message.as_deref(), Some("host gone"));
    }

    #[test]
    fn cx4_remote_output_identity_survives_without_a_retained_tab() {
        let mut record = receipt_record();
        record.launch = NativeLaunch::Cli(AgentCli::Codex);
        let expected = record.request_id.to_string();
        let state = DashboardLaunchState::new(1);
        state.reserve_remote_shell(record).unwrap();
        let snapshot = state.snapshot().pop().unwrap();
        let row = crate::commands::dashboard_launch::remote_agent_row(snapshot, false, None);
        assert!(!row.alive);
        assert!(row.provenance.is_none());
        assert_eq!(row.output_run_id.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn layout_tags_preserve_native_launch_identity_and_refuse_malformed_evidence() {
        for launch in [
            NativeLaunch::Shell,
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            let mut record = receipt_record();
            record.launch = launch;
            let tags = remote_layout_tags(&record);
            assert_eq!(tags.get("remoteHandle"), Some(&record.handle));
            assert_eq!(
                tags.get("remoteRequestId"),
                Some(&record.request_id.to_string())
            );
            assert_eq!(tags.get("remoteWorkspaceId"), Some(&record.workspace_id));
            assert_eq!(remote_layout_launch(&tags).unwrap(), launch);
            for value in ["invalid", "\"unknown-provider\""] {
                let mut malformed = tags.clone();
                malformed.insert("remoteLaunch".into(), value.into());
                assert!(
                    remote_layout_launch(&malformed).is_err(),
                    "remoteLaunch={value}"
                );
            }
            let mut legacy = tags;
            legacy.remove("remoteLaunch");
            assert_eq!(remote_layout_launch(&legacy).unwrap(), NativeLaunch::Shell);
        }
    }

    #[test]
    fn captured_profile_revision_and_inventory_survive_receipts_without_retargeting() {
        let mut record = receipt_record();
        record.mcp_profile_revision = Some("captured-profile-revision".into());
        record.mcp_inventory = vec![McpInventoryEntry {
            id: "remote-62726f77736572".into(),
            logical_name: "browser".into(),
            execution_location: "remoteWorkspace".into(),
            availability: "selected".into(),
        }];
        let bytes = serde_json::to_vec(&record).unwrap();
        let restored: RemoteShellRecord = serde_json::from_slice(&bytes).unwrap();
        assert!(record.matches_capture(&restored));
        assert_eq!(restored.outcome(false).mcp_inventory, record.mcp_inventory);
        let tags = remote_layout_tags(&record);
        assert_eq!(
            tags["remoteMcpProfileRevision"],
            "captured-profile-revision"
        );
        // SW-AGR-2b: whose launch it is, so a wake never types into another
        // account's session.
        assert_eq!(
            tags[crate::routines::wake_app::TAG_REMOTE_OWNER],
            record.owner_id.to_string()
        );
        assert_eq!(
            serde_json::from_str::<Vec<McpInventoryEntry>>(&tags["remoteMcpInventory"]).unwrap(),
            record.mcp_inventory
        );
        let mut changed = restored.clone();
        changed.mcp_profile_revision = Some("edited-profile".into());
        assert!(!record.matches_capture(&changed));
        changed = restored;
        changed.mcp_inventory[0].execution_location = "desktop".into();
        assert!(!record.matches_capture(&changed));
    }

    #[test]
    fn native_choice_is_durable_immutable_intent_with_a_shell_default() {
        let mut record = receipt_record();
        record.launch = NativeLaunch::Cli(AgentCli::Codex);
        let value = serde_json::to_value(&record).unwrap();
        let restored: RemoteShellRecord = serde_json::from_value(value.clone()).unwrap();
        assert!(restored.matches_intent(
            &record.cwd,
            &record.workspace_id,
            &record.name,
            NativeLaunch::Cli(AgentCli::Codex),
            RemotePermissionMode::Default
        ));
        assert!(!restored.matches_intent(
            &record.cwd,
            &record.workspace_id,
            &record.name,
            NativeLaunch::Cli(AgentCli::Claude),
            RemotePermissionMode::Default
        ));
        assert!(!restored.matches_intent(
            &record.cwd,
            &record.workspace_id,
            &record.name,
            NativeLaunch::Cli(AgentCli::Codex),
            RemotePermissionMode::Bypass
        ));
        let mut legacy = value;
        legacy.as_object_mut().unwrap().remove("launch");
        let restored: RemoteShellRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.launch, NativeLaunch::Shell);
        assert_eq!(restored.permission_mode, RemotePermissionMode::Default);
    }

    #[test]
    fn bypass_permission_mode_starts_the_raw_runtime_without_approvals_and_keeps_its_hooks() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        for (launch, flag) in [
            (
                NativeLaunch::Cli(AgentCli::Claude),
                "--dangerously-skip-permissions",
            ),
            (
                NativeLaunch::Cli(AgentCli::Codex),
                "--dangerously-bypass-approvals-and-sandbox",
            ),
        ] {
            let mut record = receipt_record();
            record.launch = launch;
            assert_eq!(
                record.agent_startup(),
                None,
                "default: interactive approvals"
            );
            record.permission_mode = RemotePermissionMode::Bypass;
            let startup: AgentStartup = record.agent_startup().unwrap();
            assert_eq!(startup.args, vec![flag.to_string()]);
            assert_eq!(startup.prompt, None);
            // The machine's launcher accepts it as a saved-Agent-shaped startup, so it
            // still composes the CodeSense MCP config, status hooks and inbox nudges.
            codesense_pty_core::agent_cli::validate_startup(&startup, launch).unwrap();
            // The choice survives a restart, both on disk and in the host's layout.
            let restored: RemoteShellRecord =
                serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
            assert_eq!(restored.permission_mode, RemotePermissionMode::Bypass);
            let tags = remote_layout_tags(&record);
            assert_eq!(
                remote_layout_permission_mode(&tags, launch).unwrap(),
                RemotePermissionMode::Bypass
            );
            let mut malformed = tags;
            malformed.insert("remotePermissionMode".into(), "\"yolo\"".into());
            assert!(remote_layout_permission_mode(&malformed, launch).is_err());
        }
        let mut shell = receipt_record();
        shell.permission_mode = RemotePermissionMode::Bypass;
        assert_eq!(shell.agent_startup(), None);
        assert_eq!(
            remote_layout_permission_mode(&remote_layout_tags(&receipt_record()), shell.launch)
                .unwrap(),
            RemotePermissionMode::Default
        );
        // A shell's tab never comes back in a permission mode, whatever its tags say.
        let mut tagged = remote_layout_tags(&receipt_record());
        tagged.insert("remotePermissionMode".into(), "\"bypass\"".into());
        assert_eq!(
            remote_layout_permission_mode(&tagged, NativeLaunch::Shell).unwrap(),
            RemotePermissionMode::Default
        );
    }

    #[test]
    fn permission_mode_is_only_for_a_raw_claude_or_codex_launch() {
        let saved = Some("builtin/claude".to_string());
        assert!(permission_mode_fits(RemotePermissionMode::Default, None, &saved).is_ok());
        assert!(permission_mode_fits(
            RemotePermissionMode::Bypass,
            Some(NativeLaunch::Cli(AgentCli::Claude)),
            &None
        )
        .is_ok());
        assert!(permission_mode_fits(
            RemotePermissionMode::Bypass,
            Some(NativeLaunch::Cli(AgentCli::Codex)),
            &None
        )
        .is_ok());
        assert!(permission_mode_fits(RemotePermissionMode::Bypass, None, &None).is_err());
        assert!(permission_mode_fits(
            RemotePermissionMode::Bypass,
            Some(NativeLaunch::Shell),
            &None
        )
        .is_err());
        assert!(permission_mode_fits(
            RemotePermissionMode::Bypass,
            Some(NativeLaunch::Cli(AgentCli::Claude)),
            &saved
        )
        .is_err());
    }

    #[test]
    fn preparing_intent_survives_restart_and_spawn_marker_is_cross_process_single_use() {
        let root = tempfile::tempdir().unwrap();
        let first = DashboardLaunchState::new(1);
        let mut original = receipt_record();
        original.phase = RemoteSpawnPhase::Preparing;
        first
            .reserve_remote_shell_durable(root.path(), original.clone())
            .unwrap();
        let second = DashboardLaunchState::new(1);
        let mut restored = second
            .consumed_remote_shell(
                root.path(),
                original.owner_id,
                &original.team_key,
                &original.machine_id,
                original.request_id,
            )
            .unwrap()
            .unwrap();
        assert!(restored.preparing());
        assert!(second
            .begin_remote_spawn(root.path(), &mut restored)
            .unwrap());
        assert!(!first
            .begin_remote_spawn(root.path(), &mut original)
            .unwrap());
        assert!(!original.preparing());
        let restarted = DashboardLaunchState::new(1);
        let restored = restarted
            .restore_remote_shell(
                root.path(),
                original.owner_id,
                &original.team_key,
                &original.machine_id,
                &RemoteTabReference::Handle(original.handle.clone()),
            )
            .unwrap()
            .unwrap();
        assert!(!restored.preparing());
        assert!(!restored.outcome(false).retryable);
        let mut value = serde_json::to_value(restored).unwrap();
        value.as_object_mut().unwrap().remove("phase");
        assert!(!serde_json::from_value::<RemoteShellRecord>(value)
            .unwrap()
            .preparing());
    }

    #[test]
    fn an_unreadable_receipt_does_not_block_attach_by_another_handle() {
        let root = tempfile::tempdir().unwrap();
        let original = receipt_record();
        let first = DashboardLaunchState::new(1);
        first
            .reserve_remote_shell_durable(root.path(), original.clone())
            .unwrap();
        drop(first);
        // A receipt that no longer parses: another tab's, cut short.
        let mut stale = receipt_record();
        stale.request_id = Uuid::new_v4();
        stale.handle = "remote-tab-stale".into();
        let bytes = serde_json::to_vec(&stale).unwrap();
        std::fs::write(root.path().join("stale.json"), &bytes[..bytes.len() / 2]).unwrap();
        let restarted = DashboardLaunchState::new(1);
        let lookup = |handle: &str| {
            restarted.restore_remote_shell(
                root.path(),
                original.owner_id,
                &original.team_key,
                &original.machine_id,
                &RemoteTabReference::Handle(handle.into()),
            )
        };
        let restored = lookup(&original.handle).unwrap().unwrap();
        assert_eq!(restored.request_id, original.request_id);
        assert_eq!(restored.target.session, original.target.session);
        assert!(lookup("remote-tab-stale").unwrap().is_none());
    }

    #[test]
    fn a_restart_restores_the_remote_tabs_its_machine_still_runs() {
        let root = tempfile::tempdir().unwrap();
        let original = receipt_record();
        let chat_home = tempfile::tempdir().unwrap();
        let chat = crate::commands::workspace_chat::WorkspaceChatStore::default();
        chat.install(chat_home.path());
        chat.position(
            "story:SV-X",
            &original.handle,
            original.target.session.as_str(),
        )
        .unwrap();
        chat.note_read(
            "story:SV-X",
            &original.handle,
            original.target.session.as_str(),
            4,
        )
        .unwrap();
        chat.note_told(
            "story:SV-X",
            &original.handle,
            original.target.session.as_str(),
            6,
        )
        .unwrap();
        let mut gone = receipt_record();
        gone.request_id = Uuid::new_v4();
        gone.handle = "remote-tab-gone".into();
        gone.target.session = PtySessionId::new("remote-shell-gone");
        let first = DashboardLaunchState::new(1);
        first
            .reserve_remote_shell_durable(root.path(), original.clone())
            .unwrap();
        first
            .reserve_remote_shell_durable(root.path(), gone)
            .unwrap();
        drop(first);
        let restarted = DashboardLaunchState::new(1);
        let live = [
            LiveRemoteTab {
                team: original.team_key.clone(),
                handle: original.handle.clone(),
                request_id: original.request_id,
                session: original.target.session.as_str().to_string(),
            },
            // A tab naming the same request under another handle: not its receipt.
            LiveRemoteTab {
                team: original.team_key.clone(),
                handle: "remote-tab-impostor".into(),
                request_id: original.request_id,
                session: original.target.session.as_str().to_string(),
            },
            // A tab whose receipt this app does not hold.
            LiveRemoteTab {
                team: original.team_key.clone(),
                handle: "remote-tab-stranger".into(),
                request_id: Uuid::new_v4(),
                session: "remote-shell-stranger".into(),
            },
        ];
        // The machine's admission now: the same lifetime, reconnected since.
        let mut now = original.target.runtime.clone();
        now.admission_revision = crate::commands::machines::RuntimeCounter::new(99);
        let restore = |current: bool| {
            restarted.restore_remote_shells(
                root.path(),
                original.owner_id,
                &original.machine_id,
                &live,
                &now,
                |_| current,
            )
        };
        // Another terminal server lifetime runs none of them.
        assert_eq!(restore(false), 0);
        assert!(restarted.remote_shell_by_handle(&original.handle).is_none());
        // The tab its machine still runs comes back; the one it no longer runs does not.
        assert_eq!(restore(true), 1);
        let back = restarted.remote_shell_by_handle(&original.handle).unwrap();
        assert_eq!(back.status, "alreadyLive");
        assert_eq!(
            back.target.runtime, now,
            "reached through the current admission"
        );
        assert!(restarted
            .remote_shell_by_handle("remote-tab-gone")
            .is_none());
        assert!(restarted
            .remote_shell_by_handle("remote-tab-stranger")
            .is_none());
        assert!(restarted
            .remote_shell_by_handle("remote-tab-impostor")
            .is_none());
        // Once back, it is not restored twice.
        assert_eq!(restore(true), 0);
        // A later reconnect reaches it through that admission; another lifetime,
        // or a tab under its handle for another request, leaves it as it is.
        let mut later = now.clone();
        later.admission_revision = crate::commands::machines::RuntimeCounter::new(100);
        let readmit = |live: &[LiveRemoteTab], current: bool| {
            restarted.readmit_remote_shells(
                original.owner_id,
                &original.machine_id,
                live,
                &later,
                |_| current,
            )
        };
        assert_eq!(readmit(&live, false), 0);
        let other_request = [LiveRemoteTab {
            request_id: Uuid::new_v4(),
            ..live[0].clone()
        }];
        assert_eq!(readmit(&other_request, true), 0);
        assert_eq!(
            restarted
                .remote_shell_by_handle(&original.handle)
                .unwrap()
                .target
                .runtime,
            now
        );
        assert_eq!(readmit(&live, true), 1);
        assert_eq!(
            restarted
                .remote_shell_by_handle(&original.handle)
                .unwrap()
                .target
                .runtime,
            later,
            "reached through the reconnect's admission"
        );
        let reconnected = restarted.remote_shell_by_handle(&original.handle).unwrap();
        assert_eq!(reconnected.target.session, original.target.session);
        let restored_chat = crate::commands::workspace_chat::WorkspaceChatStore::default();
        restored_chat.install(chat_home.path());
        let p = restored_chat
            .position(
                "story:SV-X",
                &reconnected.handle,
                reconnected.target.session.as_str(),
            )
            .unwrap();
        assert_eq!((p.read, p.told), (4, 6));
        assert_eq!(readmit(&live, true), 0, "already there");
        // A slower reconnect's older admission never takes it back, nor does
        // a row written from an older read.
        let older = restarted.readmit_remote_shells(
            original.owner_id,
            &original.machine_id,
            &live,
            &now,
            |_| true,
        );
        assert_eq!(older, 0);
        let mut stale = restarted.remote_shell_by_handle(&original.handle).unwrap();
        stale.target.runtime = now.clone();
        stale.status = "ended";
        restarted.update_remote_shell(stale);
        let kept = restarted.remote_shell_by_handle(&original.handle).unwrap();
        assert_eq!(kept.target.runtime, later);
        assert_eq!(kept.status, "ended");
    }

    #[test]
    fn remote_consumed_intent_survives_registry_restart_without_redispatch() {
        use super::super::super::dashboard_launch::RemoteShellReservation;
        let root = tempfile::tempdir().unwrap();
        let original = receipt_record();
        let first = DashboardLaunchState::new(1);
        let mut dispatches = 0;
        if matches!(
            first
                .reserve_remote_shell_durable(root.path(), original.clone())
                .unwrap(),
            RemoteShellReservation::Fresh
        ) {
            dispatches += 1;
        }
        drop(first);
        let restarted = DashboardLaunchState::new(1);
        // A disconnected runtime is deliberately unnecessary for receipt lookup:
        // restart restores the original unknown identity before any backend resolution.
        let disconnected = restarted
            .consumed_remote_shell(
                root.path(),
                original.owner_id,
                &original.team_key,
                &original.machine_id,
                original.request_id,
            )
            .unwrap()
            .unwrap();
        let outcome = disconnected.outcome(false);
        assert_eq!(outcome.status, "unknown");
        assert_eq!(outcome.target.unwrap().session, original.target.session);
        assert_eq!(outcome.request_id, Some(original.request_id));
        assert_eq!(outcome.handle.as_ref(), Some(&original.handle));
        let mut attempted = original.clone();
        attempted.target.session = PtySessionId::new("must-not-spawn");
        let restored = match restarted
            .reserve_remote_shell_durable(root.path(), attempted)
            .unwrap()
        {
            RemoteShellReservation::Existing(record) => record,
            RemoteShellReservation::Fresh => {
                panic!("a restart redispatched consumed intent")
            }
        };
        assert_eq!(dispatches, 1);
        assert_eq!(restored.target.session, original.target.session);
        assert_eq!(restored.handle, original.handle);
        assert_eq!(restored.status, "unknown");
        // Missing server tags cannot free the durable consumption. Corruption
        // also refuses instead of deleting the evidence and creating again.
        let path = std::fs::read_dir(root.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(path, b"partial").unwrap();
        assert!(restarted
            .reserve_remote_shell_durable(root.path(), original)
            .is_err());
    }

    #[test]
    fn remote_directory_is_data_and_never_desktop_canonicalized() {
        for invalid in [
            "relative",
            "~/dir",
            "/home/$USER",
            "/a/../b",
            "/a/./b",
            "/a\0b",
        ] {
            assert!(normalized_cwd(invalid).is_err());
        }
        assert_eq!(
            normalized_cwd("/not-on-this-mac//plain directory/"),
            Ok("/not-on-this-mac/plain directory".into())
        );
        assert_eq!(normalized_cwd("/"), Ok("/".into()));
    }
}
