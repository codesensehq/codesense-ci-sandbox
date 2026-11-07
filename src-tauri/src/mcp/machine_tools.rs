// SPDX-License-Identifier: MIT
//! MCP decoding for machine services. Runtime authorization stays in the services.
use crate::commands::machines::registry::Scope;
use crate::commands::machines::{MachineDraftInput, MachineMode, MachineSignIn};
use crate::commands::pty_layout::{RemoteShellTabRequest, RemoteTabReference};
use rmcp::schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyParams {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMachinesParams {
    pub org_slug: String,
    pub team: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MachineIdParams {
    pub machine_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveMachineIdentityParams {
    pub machine_id: String,
    pub revision: String,
    pub accept: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestartMachineTerminalServerParams {
    pub machine_id: String,
    pub server_token: String,
    /// Explicit authorization to end every terminal and agent running at shutdown.
    pub confirm_end_sessions: bool,
}

/// `list_out_of_date_terminal_servers`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutOfDateServersParams {
    pub org_slug: String,
    pub team: Option<String>,
    /// Your own `$CODESENSE_TAB_SESSION_ID`: your tab is marked `isCaller`.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// What one machine's update binds to, as `list_out_of_date_terminal_servers`
/// returned it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedServer {
    pub server_id: String,
    pub epoch: String,
    /// The confirmed session count (the length of `sessionIds` when given).
    #[serde(default)]
    pub sessions: u32,
    #[serde(default)]
    pub pid: Option<u32>,
    /// The sessions confirmed ending (required: an update is never bound to
    /// a count alone).
    pub session_ids: Vec<String>,
}

impl From<ExpectedServer> for crate::commands::machines::update_all::ServerExpectation {
    fn from(expected: ExpectedServer) -> Self {
        Self {
            server_id: expected.server_id,
            epoch: expected.epoch,
            sessions: expected.sessions,
            pid: expected.pid,
            session_ids: Some(expected.session_ids),
        }
    }
}

/// `update_terminal_servers`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTerminalServersParams {
    pub org_slug: String,
    pub team: Option<String>,
    /// Which machines; or `all: true` for every out-of-date one.
    #[serde(default)]
    pub machine_ids: Option<Vec<String>>,
    #[serde(default)]
    pub all: bool,
    /// Explicit authorization to end every terminal and agent each server runs.
    pub confirm_end_sessions: bool,
    /// Each machine's `expected`, exactly as listed.
    pub expected: std::collections::BTreeMap<String, ExpectedServer>,
    /// Your own `$CODESENSE_TAB_SESSION_ID`: the outcome goes to your inbox,
    /// and your own machine is updated last.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyPathParams {
    pub key_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialParams {
    pub provider: String,
    pub account: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "method", rename_all = "camelCase", deny_unknown_fields)]
pub enum SignInParams {
    Agent,
    #[serde(rename_all = "camelCase")]
    KeyFile {
        key_path: String,
    },
    #[serde(rename_all = "camelCase")]
    KeychainPassphrase {
        key_path: String,
        credential_ref: CredentialParams,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModeParams {
    Legacy,
    PrivateAccount,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftParams {
    /// Absent: a saved machine keeps its own mode; a new one is a private account.
    #[serde(default)]
    pub mode: Option<ModeParams>,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub jump_host: Option<String>,
    pub sign_in: SignInParams,
    pub trusted_host_key_fp: Option<String>,
}
impl From<DraftParams> for MachineDraftInput {
    fn from(p: DraftParams) -> Self {
        Self {
            mode: p.mode.map(|mode| match mode {
                ModeParams::Legacy => MachineMode::Legacy,
                ModeParams::PrivateAccount => MachineMode::PrivateAccount,
            }),
            label: p.label,
            host: p.host,
            port: p.port,
            username: p.username,
            jump_host: p.jump_host,
            trusted_host_key_fp: p.trusted_host_key_fp,
            sign_in: match p.sign_in {
                SignInParams::Agent => MachineSignIn::Agent,
                SignInParams::KeyFile { key_path } => MachineSignIn::KeyFile { key_path },
                SignInParams::KeychainPassphrase {
                    key_path,
                    credential_ref,
                } => MachineSignIn::KeychainPassphrase {
                    key_path,
                    credential_ref: crate::domain::CredentialRef {
                        provider: credential_ref.provider,
                        account: credential_ref.account,
                    },
                },
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftOperationParams {
    pub machine_id: Option<String>,
    pub input: DraftParams,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ScopeParams {
    Personal,
    #[serde(rename_all = "camelCase")]
    Team {
        org_slug: String,
        team: String,
    },
}
impl From<ScopeParams> for Scope {
    fn from(p: ScopeParams) -> Self {
        match p {
            ScopeParams::Personal => Self::Personal,
            ScopeParams::Team { org_slug, team } => Self::Team { org_slug, team },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMachineParams {
    pub machine_id: Option<String>,
    pub input: DraftParams,
    pub share: ScopeParams,
    pub connect: bool,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HiddenParams {
    pub machine_id: String,
    pub hidden: bool,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutoConnectParams {
    pub machine_id: String,
    pub auto_connect: bool,
}

/// `open_remote_tab`'s two forms. MCP input arrives as the flat
/// [`OpenRemoteTabToolParams`] (its schema is the tool's published one);
/// other callers decode this directly.
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum OpenRemoteTabParams {
    #[serde(rename_all = "camelCase")]
    Create {
        request_id: String,
        machine_id: String,
        team_key: String,
        workspace_id: String,
        cwd: String,
        #[serde(default)]
        kind: RemoteKind,
        #[serde(default)]
        launch: Option<codesense_host_proto::agent_cli::NativeLaunch>,
        /// With launch `claude` or `codex` (not a saved Agent): `bypass` starts it
        /// without approval prompts (Claude `--dangerously-skip-permissions`, Codex
        /// `--dangerously-bypass-approvals-and-sandbox`), still with CodeSense's MCP
        /// server, status hooks and new-mail nudges. Omitted or `default`: the
        /// runtime's ordinary approvals.
        #[serde(default)]
        permission_mode: crate::commands::pty_layout::RemotePermissionMode,
        agent_key: Option<String>,
        prepared_workspace_handle: Option<String>,
        headed: Option<bool>,
        name: Option<String>,
        /// MCP derives the launcher from the verified connection. A supplied value
        /// must name that caller. Native app launches retain their own launcher.
        #[serde(default)]
        launched_by: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Attach {
        machine_id: String,
        team_key: String,
        handle: Option<String>,
        request_id: Option<String>,
        headed: Option<bool>,
    },
}
/// Which `open_remote_tab` form a call is.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteTabOperation {
    Create,
    Attach,
}

/// `open_remote_tab`'s input as one flat object: `operation` picks Create or
/// Attach, and a field the picked form does not take is refused.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenRemoteTabToolParams {
    /// `create` opens a new tab; `attach` reconnects to one by `handle` or
    /// `requestId`.
    pub operation: RemoteTabOperation,
    /// Create: required UUID naming the open. Attach: the create's requestId
    /// (instead of `handle`).
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub request_id: Option<Option<String>>,
    /// The configured machine.
    pub machine_id: String,
    /// The team the workspace belongs to (`org:team`).
    pub team_key: String,
    /// Create (required): the already-open workspace.
    #[serde(
        default,
        deserialize_with = "super::flat_params::present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub workspace_id: Option<String>,
    /// Create (required): the absolute existing folder on the host.
    #[serde(
        default,
        deserialize_with = "super::flat_params::present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub cwd: Option<String>,
    /// Create: `shell` (default) or `agent`.
    #[serde(
        default,
        deserialize_with = "super::flat_params::present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "RemoteKind")]
    pub kind: Option<RemoteKind>,
    /// Create: what runs in the tab.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "native_launch_schema")]
    pub launch: Option<Option<codesense_host_proto::agent_cli::NativeLaunch>>,
    /// Create, with launch `claude` or `codex` (not a saved Agent): `bypass`
    /// starts it without approval prompts (Claude `--dangerously-skip-permissions`,
    /// Codex `--dangerously-bypass-approvals-and-sandbox`), still with
    /// CodeSense's MCP server, status hooks and new-mail nudges. Omitted or
    /// `default`: the runtime's ordinary approvals.
    #[serde(
        default,
        deserialize_with = "super::flat_params::present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "crate::commands::pty_layout::RemotePermissionMode")]
    pub permission_mode: Option<crate::commands::pty_layout::RemotePermissionMode>,
    /// Create: the saved Agent to start.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub agent_key: Option<Option<String>>,
    /// Create, with `agentKey`: the verified prepared workspace.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub prepared_workspace_handle: Option<Option<String>>,
    /// Whether the tab is shown in the app; defaults to true.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<bool>")]
    pub headed: Option<Option<bool>>,
    /// Create: the tab name shown by list_sessions.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub name: Option<Option<String>>,
    /// Create: the verified MCP caller is recorded as launchedBy. A supplied value
    /// must name that caller. Native app launches retain their own launcher.
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub launched_by: Option<Option<String>>,
    /// Attach: the tab's opaque handle (instead of `requestId`).
    #[serde(
        default,
        deserialize_with = "super::flat_params::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub handle: Option<Option<String>>,
}

impl OpenRemoteTabToolParams {
    /// The form `operation` names, refusing any field that form does not take.
    pub fn into_params(self) -> Result<OpenRemoteTabParams, String> {
        use super::flat_params::{refuse_extra, require};
        let Self {
            operation,
            request_id,
            machine_id,
            team_key,
            workspace_id,
            cwd,
            kind,
            launch,
            permission_mode,
            agent_key,
            prepared_workspace_handle,
            headed,
            name,
            launched_by,
            handle,
        } = self;
        match operation {
            RemoteTabOperation::Create => {
                const FORM: &str = "create";
                refuse_extra(FORM, &[("handle", handle.is_some())])?;
                Ok(OpenRemoteTabParams::Create {
                    request_id: require(FORM, "requestId", request_id.flatten())?,
                    machine_id,
                    team_key,
                    workspace_id: require(FORM, "workspaceId", workspace_id)?,
                    cwd: require(FORM, "cwd", cwd)?,
                    kind: kind.unwrap_or_default(),
                    launch: launch.flatten(),
                    permission_mode: permission_mode.unwrap_or_default(),
                    agent_key: agent_key.flatten(),
                    prepared_workspace_handle: prepared_workspace_handle.flatten(),
                    headed: headed.flatten(),
                    name: name.flatten(),
                    launched_by: launched_by.flatten(),
                })
            }
            RemoteTabOperation::Attach => {
                const FORM: &str = "attach";
                refuse_extra(
                    FORM,
                    &[
                        ("workspaceId", workspace_id.is_some()),
                        ("cwd", cwd.is_some()),
                        ("kind", kind.is_some()),
                        ("launch", launch.is_some()),
                        ("permissionMode", permission_mode.is_some()),
                        ("agentKey", agent_key.is_some()),
                        (
                            "preparedWorkspaceHandle",
                            prepared_workspace_handle.is_some(),
                        ),
                        ("name", name.is_some()),
                        ("launchedBy", launched_by.is_some()),
                    ],
                )?;
                Ok(OpenRemoteTabParams::Attach {
                    machine_id,
                    team_key,
                    handle: handle.flatten(),
                    request_id: request_id.flatten(),
                    headed: headed.flatten(),
                })
            }
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteKind {
    #[default]
    Shell,
    Agent,
}
fn native_launch_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    let launches: Vec<&str> = std::iter::once(NativeLaunch::Shell)
        .chain(
            AgentCli::ALL
                .into_iter()
                .filter(|cli| crate::agents::adapter::desktop(*cli).supports_remote_launch)
                .map(NativeLaunch::Cli),
        )
        .map(NativeLaunch::as_str)
        .collect();
    schemars::json_schema!({"type":"string", "enum": launches, "default": NativeLaunch::Shell.as_str()})
}
impl OpenRemoteTabParams {
    /// The launcher the caller reported, blank treated as absent.
    pub(crate) fn claimed_launcher(&self) -> Option<&str> {
        match self {
            Self::Create { launched_by, .. } => launched_by
                .as_deref()
                .map(str::trim)
                .filter(|raw| !raw.is_empty()),
            Self::Attach { .. } => None,
        }
    }

    pub fn into_request(self) -> Result<RemoteShellTabRequest, &'static str> {
        match self {
            Self::Create {
                request_id,
                machine_id,
                team_key,
                workspace_id,
                cwd,
                kind,
                launch,
                permission_mode,
                agent_key,
                prepared_workspace_handle,
                headed,
                name,
                launched_by: _,
            } => {
                if matches!(kind, RemoteKind::Agent)
                    && agent_key.is_none()
                    && launch.unwrap_or_default().is_shell()
                {
                    return Err("invalid: choose launch claude or codex for a native remote agent");
                }
                Ok(RemoteShellTabRequest::Create {
                    request_id: request_id
                        .parse()
                        .map_err(|_| "invalid: requestId must be a UUID")?,
                    machine_id,
                    team_key,
                    workspace_id,
                    // An agent always names its folder; only a person's
                    // request may leave it to the host's home.
                    cwd: Some(cwd),
                    name,
                    launch,
                    permission_mode,
                    agent_key,
                    prepared_workspace_handle,
                    headed: headed.unwrap_or(true),
                    initial_prompt: None,
                    identity: Default::default(),
                })
            }
            Self::Attach {
                machine_id,
                team_key,
                handle,
                request_id,
                headed,
            } => {
                let reference = match (handle, request_id) {
                    (Some(h), None) if !h.is_empty() => RemoteTabReference::Handle(h),
                    (None, Some(id)) => RemoteTabReference::RequestId(
                        id.parse()
                            .map_err(|_| "invalid: requestId must be a UUID")?,
                    ),
                    _ => return Err("invalid: attach requires exactly one handle or requestId"),
                };
                Ok(RemoteShellTabRequest::Attach {
                    machine_id,
                    team_key,
                    reference,
                    headed: headed.unwrap_or(true),
                })
            }
        }
    }
}

/// Who is opening a remote tab over MCP, as far as the app itself knows.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TabOpener<'a> {
    /// A caller on this Mac: the socket carries no identity, so it reports its own
    /// `$CODESENSE_TAB_SESSION_ID` (or a person's request reports none).
    Local,
    /// An agent in a remote tab: the tab its connection was captured from.
    RemoteTab { handle: &'a str },
    /// A remote task with no tab of its own.
    RemoteTask,
}

/// What the new tab records as its launcher.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Launcher {
    /// Nobody: a person's request.
    None,
    /// The caller's own tab, known to the app.
    Own(String),
    /// The session the caller named, to be confirmed running and recorded by its handle.
    Named(String),
}

/// Any caller may name any session as the launcher; it is confirmed running before it
/// is recorded. Without one, an agent on a machine is recorded as its own tab.
pub(crate) fn resolve_launcher(opener: TabOpener<'_>, claimed: Option<&str>) -> Launcher {
    match (claimed, opener) {
        (Some(raw), _) => Launcher::Named(raw.into()),
        (None, TabOpener::RemoteTab { handle }) => Launcher::Own(handle.into()),
        (None, TabOpener::Local | TabOpener::RemoteTask) => Launcher::None,
    }
}

/// Record `launched_by` on a create; an attach keeps the launcher its tab already has.
pub(crate) fn with_launcher(
    mut request: RemoteShellTabRequest,
    launched_by: Option<String>,
) -> RemoteShellTabRequest {
    if let RemoteShellTabRequest::Create { identity, .. } = &mut request {
        identity.launched_by = launched_by;
    }
    request
}

fn refused(message: &str) -> serde_json::Value {
    serde_json::json!({"status":"refused","code":message.split(':').next().unwrap_or("invalid"),"message":message,"headed":false})
}

/// The form `params` name, or the `invalid` refusal `open_remote_tab` returns.
fn remote_tab_form(
    params: OpenRemoteTabToolParams,
) -> Result<OpenRemoteTabParams, serde_json::Value> {
    params.into_params().map_err(|message| refused(&message))
}

pub(crate) async fn admit_remote_tab_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    request: &RemoteShellTabRequest,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(), String> {
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return Ok(());
    }
    if let RemoteShellTabRequest::Create {
        agent_key,
        launch,
        team_key,
        ..
    } = request
    {
        if agent_key.is_some() || launch.is_some_and(|launch| !launch.is_shell()) {
            let ctx = scope.context(app).await?;
            scope.require_launch_key(&ctx.codesense_home, &ctx.principal(), team_key)?;
            if let Some(key) = agent_key {
                let (org, team) = crate::commands::pty_layout::parse_team_key(team_key);
                crate::agents::launch::find_agent_for(
                    &ctx.codesense_home,
                    &ctx.principal(),
                    key,
                    team.map(|team| (org, team)),
                    scope,
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) async fn open_remote_tab(
    app: tauri::AppHandle,
    params: OpenRemoteTabToolParams,
    caller: Option<super::remote_authority::RemoteAuthority>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> serde_json::Value {
    let params = match remote_tab_form(params) {
        Ok(params) => params,
        Err(refusal) => return refusal,
    };
    let opener = caller
        .as_ref()
        .map_or(TabOpener::Local, |caller| match caller.caller_tab() {
            Some((handle, _)) => TabOpener::RemoteTab { handle },
            None => TabOpener::RemoteTask,
        });
    let launched_by = match resolve_launcher(opener, params.claimed_launcher()) {
        Launcher::None => None,
        Launcher::Own(handle) => Some(handle),
        Launcher::Named(raw) => match super::identity_tools::running_session(&app, &raw).await {
            Ok(row) => Some(row.handle),
            Err(message) => return refused(&format!("invalid: launchedBy: {message}")),
        },
    };
    let request = match params.into_request() {
        Ok(request) => with_launcher(request, launched_by),
        Err(message) => return refused(message),
    };
    let team_key = match &request {
        RemoteShellTabRequest::Create { team_key, .. }
        | RemoteShellTabRequest::Attach { team_key, .. } => team_key.clone(),
    };
    let mut result = crate::commands::pty_layout::remote_shell_tab_bound_for(
        app.clone(),
        request,
        caller,
        scope,
    )
    .await;
    if result.headed && matches!(result.status, "opened" | "alreadyLive") {
        if let Err(message) = reveal_remote_terminal(&app, &team_key, &result).await {
            result.headed = false;
            result.code = Some("revealUnavailable".into());
            result.message = Some(message);
        }
    }
    serde_json::json!({"status":result.status,"handle":result.handle,"requestId":result.request_id,
        "code":result.code,"message":result.message,"headed":result.headed,"retryable":result.retryable,"agent":result.agent,
        "workspaceId":result.workspace_id,"cwd":result.cwd,
        "origin":result.target.map(|target|serde_json::json!({"machineId":target.runtime.member.machine_id,"serverId":target.runtime.namespace.server_id,"epoch":target.runtime.namespace.epoch}))})
}

pub(crate) async fn reveal_remote_terminal(
    app: &tauri::AppHandle,
    team_key: &str,
    outcome: &crate::commands::pty_layout::RemoteTabOutcome,
) -> Result<(), String> {
    use tauri::Emitter;
    let target = outcome
        .target
        .clone()
        .ok_or("terminal origin unavailable")?;
    let reference = crate::commands::machines::RuntimeSessionReference::Admitted(Box::new(target));
    crate::commands::pty_layout::authorize_human_terminal(app, &reference).await?;
    let backend = crate::commands::pty_backend::resolve_session_reference(app, &reference, true)?;
    backend.revalidate(app)?;
    let lease = backend
        .client()
        .and_then(|client| client.private_lease())
        .ok_or("private authority unavailable")?;
    lease.validate()?;
    app.emit("mcp:remote-terminal-reveal",serde_json::json!({"teamKey":team_key,"workspaceId":outcome.workspace_id,"outcome":outcome,"privateAuthorityEpoch":lease.epoch()})).map_err(|error|error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    /// Decodes an MCP call's arguments the way `open_remote_tab` does (flat
    /// params, then the form), checked against decoding the form directly.
    fn decode(value: serde_json::Value) -> Result<OpenRemoteTabParams, String> {
        crate::mcp::handler::decode_via_flat(value, OpenRemoteTabToolParams::into_params)
    }
    #[test]
    fn machine_mcp_drafts_reject_secret_bytes_and_unknown_fields() {
        let valid = json!({"input":{"mode":"private-account","label":"host","host":"localhost","port":22,"signIn":{"method":"keyFile","keyPath":"/keys/dev"}}});
        assert!(serde_json::from_value::<DraftOperationParams>(valid.clone()).is_ok());
        for path in ["password", "passphrase", "privateKey"] {
            let mut draft = valid.clone();
            draft["input"][path] = json!("secret");
            assert!(serde_json::from_value::<DraftOperationParams>(draft).is_err());
            let mut draft = valid.clone();
            draft["input"]["signIn"][path] = json!("secret");
            assert!(serde_json::from_value::<DraftOperationParams>(draft).is_err());
        }
    }
    #[test]
    fn public_native_launch_is_finite_and_reaches_the_existing_owner() {
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        for (launch, expected) in [
            ("shell", NativeLaunch::Shell),
            ("claude", NativeLaunch::Cli(AgentCli::Claude)),
            ("codex", NativeLaunch::Cli(AgentCli::Codex)),
        ] {
            let value = json!({"operation":"create", "requestId":uuid::Uuid::new_v4().to_string(), "machineId":"host", "teamKey":"org:team", "workspaceId":"root", "cwd":"/remote/exact", "launch":launch});
            let request = decode(value).unwrap().into_request().unwrap();
            let RemoteShellTabRequest::Create { launch, cwd, .. } = request else {
                panic!("wrong operation")
            };
            assert_eq!(launch, Some(expected));
            assert_eq!(cwd.as_deref(), Some("/remote/exact"));
        }
        // There is no MCP profile selection: desktop MCP servers come with a saved Agent.
        let value = json!({"operation":"create", "requestId":uuid::Uuid::new_v4().to_string(), "machineId":"host", "teamKey":"org:team", "workspaceId":"root", "cwd":"/remote/exact", "launch":"claude", "mcpProfile":{"profileId":"trusted"}});
        assert!(decode(value).is_err());
        let value = json!({"operation":"create", "requestId":uuid::Uuid::new_v4().to_string(), "machineId":"host", "teamKey":"org:team", "workspaceId":"root", "cwd":"/remote/exact", "launch":"arbitrary command"});
        assert!(decode(value).is_err());
    }

    #[test]
    fn permission_mode_reaches_the_launch_request_and_defaults_to_approvals() {
        use crate::commands::pty_layout::RemotePermissionMode;
        let create = |extra: serde_json::Value| {
            let mut value = json!({"operation":"create", "requestId":uuid::Uuid::new_v4().to_string(), "machineId":"host", "teamKey":"org:team", "workspaceId":"root", "cwd":"/remote/exact", "launch":"claude"});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            decode(value)
        };
        for (extra, expected) in [
            (json!({}), RemotePermissionMode::Default),
            (
                json!({"permissionMode":"default"}),
                RemotePermissionMode::Default,
            ),
            (
                json!({"permissionMode":"bypass"}),
                RemotePermissionMode::Bypass,
            ),
        ] {
            let RemoteShellTabRequest::Create {
                permission_mode, ..
            } = create(extra).unwrap().into_request().unwrap()
            else {
                panic!("create required")
            };
            assert_eq!(permission_mode, expected);
        }
        assert!(create(json!({"permissionMode":"yolo"})).is_err());
    }

    #[test]
    fn saved_remote_agent_public_request_preserves_selection_and_has_strict_hook_schema() {
        let value = json!({"operation":"create", "kind":"agent", "requestId":uuid::Uuid::new_v4(), "machineId":"host", "teamKey":"org:team", "workspaceId":"root", "cwd":"/remote/prepared", "agentKey":"builtin/claude", "preparedWorkspaceHandle":"verified"});
        let request = decode(value.clone()).unwrap().into_request().unwrap();
        let RemoteShellTabRequest::Create {
            agent_key,
            prepared_workspace_handle,
            launch,
            ..
        } = request
        else {
            panic!("create required")
        };
        assert_eq!(agent_key.as_deref(), Some("builtin/claude"));
        assert_eq!(prepared_workspace_handle.as_deref(), Some("verified"));
        assert!(launch.is_none());
        let mut injected = value;
        injected["manifest"] = json!({"entries":[]});
        assert!(decode(injected).is_err());
    }

    #[test]
    fn any_opener_may_name_any_session_as_launcher_and_a_remote_tab_defaults_to_itself() {
        let remote = TabOpener::RemoteTab {
            handle: "remote-tab-lead",
        };
        // Another session, local or remote, is named as given; whether it runs is checked
        // against the sessions running when the tab opens.
        for named in [
            "other-session",
            "SW-1#lead",
            "remote-tab-other",
            "lead-session",
        ] {
            for opener in [remote, TabOpener::RemoteTask, TabOpener::Local] {
                assert_eq!(
                    resolve_launcher(opener, Some(named)),
                    Launcher::Named(named.into())
                );
            }
        }
        assert_eq!(
            resolve_launcher(remote, None),
            Launcher::Own("remote-tab-lead".into())
        );
        assert_eq!(
            resolve_launcher(TabOpener::RemoteTask, None),
            Launcher::None
        );
        assert_eq!(resolve_launcher(TabOpener::Local, None), Launcher::None);
        // The named launcher must be running: another running session is recorded by its
        // handle, a dead one is refused.
        use crate::mcp::session_list::{SessionListRow, SessionState};
        let row = |handle: &str, session: &str, state| SessionListRow {
            id: handle.to_string(),
            team: None,
            queue_role: None,
            role_join_error: None,
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            handle: handle.into(),
            display_name: handle.into(),
            state,
            launched_by: None,
            story: None,
            workspace: None,
            role: None,
            avatar: None,
            agent: None,
            machine_id: None,
            workspace_handle: None,
            machine: None,
            unread: Some(0),
            oldest_unread_at: None,
            mail_notice: None,
            notified_at: None,
            subagents: 0,
            inbox_waiter: Default::default(),
            progress: None,
            launch_telemetry: None,
            release_qualified: None,
            release_note: None,
            deadline_at_ms: None,
            expiring: false,
            session: Some(session.into()),
            failed: false,
        };
        let rows = vec![
            row("SW-1#lead", "pty-lead", SessionState::Idle),
            row(
                "remote-tab-other",
                "remote-shell-other",
                SessionState::Running,
            ),
            row("pty-dead", "pty-dead", SessionState::Exited),
        ];
        let running = |raw: &str| {
            crate::mcp::identity_tools::running_in(&rows, raw).map(|r| r.handle.clone())
        };
        assert_eq!(running("pty-lead").unwrap(), "SW-1#lead");
        assert_eq!(running("remote-shell-other").unwrap(), "remote-tab-other");
        assert!(running("pty-dead").is_err());
        assert!(running("forged").is_err());
        // Blank reads as absent.
        let value = json!({"operation":"create","requestId":uuid::Uuid::new_v4(),"machineId":"host",
            "teamKey":"org:team","workspaceId":"root","cwd":"/home/dev","launchedBy":"  "});
        let params = decode(value).unwrap();
        assert_eq!(params.claimed_launcher(), None);
        // An attach never takes a launcher: its tab already has one.
        let attach = json!({"operation":"attach","machineId":"host","teamKey":"org:team",
            "handle":"opaque","launchedBy":"lead-session"});
        assert!(decode(attach).is_err());
        let attach = decode(
            json!({"operation":"attach","machineId":"host","teamKey":"org:team","handle":"opaque"}),
        )
        .unwrap()
        .into_request()
        .unwrap();
        assert!(matches!(
            with_launcher(attach, Some("x".into())),
            RemoteShellTabRequest::Attach { .. }
        ));
    }

    #[test]
    fn remote_mcp_create_attach_are_disjoint_and_identity_is_required() {
        let create = json!({"operation":"create","requestId":uuid::Uuid::new_v4().to_string(),"machineId":"host","teamKey":"org:team","workspaceId":"root","cwd":"/home/dev","kind":"shell"});
        assert!(matches!(
            decode(create.clone()).unwrap().into_request(),
            Ok(RemoteShellTabRequest::Create { headed: true, .. })
        ));
        for key in ["requestId", "machineId", "cwd"] {
            let mut missing = create.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(decode(missing).is_err());
        }
        let mut agent = create.clone();
        agent["kind"] = json!("agent");
        assert!(decode(agent)
            .unwrap()
            .into_request()
            .unwrap_err()
            .starts_with("invalid"));
        let attach =
            json!({"operation":"attach","machineId":"host","teamKey":"org:team","handle":"opaque"});
        assert!(decode(attach.clone()).unwrap().into_request().is_ok());
        let mut ambiguous = attach.clone();
        ambiguous["requestId"] = json!(uuid::Uuid::new_v4().to_string());
        assert!(decode(ambiguous).unwrap().into_request().is_err());
        for key in [
            "cwd",
            "workspaceId",
            "sessionId",
            "target",
            "kind",
            "launch",
            "mcpProfile",
        ] {
            let mut override_request = attach.clone();
            override_request[key] = json!("forged");
            assert!(decode(override_request).is_err());
        }
        // A form mismatch comes back as an `invalid` refusal naming the field.
        let mut mixed = attach.clone();
        mixed["cwd"] = json!("/home/dev");
        assert_eq!(
            remote_tab_form(serde_json::from_value(mixed).unwrap()).unwrap_err(),
            json!({"status":"refused","code":"invalid","message":"invalid: attach does not take cwd","headed":false})
        );
        // Every create-only field, sent with a value create would take, is
        // refused on an attach by name.
        for (key, value) in [
            ("workspaceId", json!("root")),
            ("kind", json!("shell")),
            ("launch", json!("claude")),
            ("permissionMode", json!("default")),
            ("agentKey", json!("reviewer")),
            ("preparedWorkspaceHandle", json!("prepared")),
            ("name", json!("Review")),
            ("launchedBy", json!("cs-tab")),
        ] {
            let mut create_only = attach.clone();
            create_only[key] = value;
            assert!(decode(create_only.clone()).is_err(), "{key}");
            assert_eq!(
                serde_json::from_value::<OpenRemoteTabToolParams>(create_only)
                    .unwrap()
                    .into_params()
                    .unwrap_err(),
                format!("invalid: attach does not take {key}")
            );
        }
        let mut missing = create.clone();
        missing.as_object_mut().unwrap().remove("requestId");
        assert_eq!(
            serde_json::from_value::<OpenRemoteTabToolParams>(missing)
                .unwrap()
                .into_params()
                .unwrap_err(),
            "invalid: create requires requestId"
        );
        let mut handle = create.clone();
        handle["handle"] = json!("opaque");
        assert!(decode(handle).is_err());
        assert!(
            decode(json!({"machineId":"host","teamKey":"org:team","handle":"opaque"})).is_err()
        );
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteMcpControlParams {
    pub machine_id: String,
    pub enabled: bool,
}

#[cfg(test)]
mod replacement_schema_tests {
    use super::*;

    #[test]
    fn restart_requires_confirmation_and_accepts_no_transport_authority() {
        let valid = serde_json::json!({"machineId":"box", "serverToken":"token", "confirmEndSessions":true});
        assert!(
            serde_json::from_value::<RestartMachineTerminalServerParams>(valid.clone()).is_ok()
        );
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("confirmEndSessions");
        assert!(serde_json::from_value::<RestartMachineTerminalServerParams>(missing).is_err());
        for name in ["host", "pid", "credentials", "executable", "shell"] {
            let mut injected = valid.clone();
            injected[name] = serde_json::json!("caller-controlled");
            assert!(
                serde_json::from_value::<RestartMachineTerminalServerParams>(injected).is_err()
            );
        }
    }
}

#[cfg(test)]
mod update_terminal_servers_tests {
    use super::*;

    /// The update call takes the listed `expected` as is
    /// (the spec's `{serverId, epoch, sessionIds}` included), needs
    /// `confirmEndSessions`, and refuses a key it does not know.
    #[test]
    fn agr19_update_terminal_servers_takes_exactly_its_inputs() {
        let params: UpdateTerminalServersParams = serde_json::from_value(serde_json::json!({
            "orgSlug": "acme",
            "all": true,
            "confirmEndSessions": true,
            "expected": {"box": {"serverId": "s", "epoch": "e", "sessionIds": ["a"]}},
        }))
        .unwrap();
        let expected: crate::commands::machines::update_all::ServerExpectation =
            params.expected["box"].clone().into();
        assert_eq!(expected.confirmed_count(), 1);
        assert!(
            serde_json::from_value::<UpdateTerminalServersParams>(serde_json::json!({
                "orgSlug": "acme", "all": true, "expected": {},
            }))
            .is_err(),
            "confirmEndSessions is required"
        );
        assert!(serde_json::from_value::<UpdateTerminalServersParams>(serde_json::json!({
            "orgSlug": "acme", "all": true, "confirmEndSessions": true, "expected": {}, "force": true,
        }))
        .is_err());
        // An expected bound to a count alone is refused.
        assert!(
            serde_json::from_value::<UpdateTerminalServersParams>(serde_json::json!({
                "orgSlug": "acme", "all": true, "confirmEndSessions": true,
                "expected": {"box": {"serverId": "s", "epoch": "e", "sessions": 2}},
            }))
            .is_err(),
            "sessionIds is required"
        );
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn identity_requests_carry_the_review_revision_and_no_caller_authority() {
        let valid = serde_json::json!({"machineId":"box", "revision":"reviewed", "accept":false});
        let params: ResolveMachineIdentityParams = serde_json::from_value(valid.clone()).unwrap();
        assert!(!params.accept);
        let mut forged = valid;
        forged["actor"] = "human".into();
        assert!(serde_json::from_value::<ResolveMachineIdentityParams>(forged).is_err());
    }
}
