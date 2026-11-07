// SPDX-License-Identifier: MIT
//! A reverse caller's authority is captured by the trusted launch, not MCP JSON.
use crate::commands::{machines::RuntimeSessionReference, pty_layout::RemoteShellRecord};
use crate::terminal::session_server::SessionServerClient;
use codesense_host_proto::agent_cli::NativeLaunch;
use rmcp::ErrorData;
use std::sync::{Arc, Weak};
use tauri::{AppHandle, Manager};

#[derive(Clone)]
enum Lifetime {
    Shell(Box<RemoteShellRecord>),
    Task {
        valid: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
        /// The task's own handle (`remote-agent-task-<id>`), which it may
        /// name as its `sessionId`.
        handle: String,
    },
    /// An agent the machine started for a routine while this app
    /// was connected: its tab session, live while its process runs.
    Routine {
        session: String,
        routine_key: String,
    },
    /// An agent that ended while this app was away, whose calls
    /// its machine queued meanwhile (`send_message`, `inbox_post`,
    /// `signal_progress`): only those, delivered once, as its own tab
    /// (`handle`, when this app launched it) or session.
    Outbox {
        session: String,
        handle: Option<String>,
    },
}

/// The only calls a finished agent's queued outbox may make.
pub(crate) const OUTBOX_TOOLS: &[&str] = &["send_message", "inbox_post", "signal_progress"];

#[derive(Clone)]
pub(crate) struct RemoteAuthority {
    app: AppHandle,
    lifetime: Lifetime,
    owner_id: u64,
    team_key: String,
    machine_id: String,
    client: Weak<SessionServerClient>,
    relay_grant: Arc<super::local_mcp_relay::config::Grant>,
}
impl RemoteAuthority {
    /// A task lifetime is independent of PTY process ancestry.
    pub(crate) fn for_task(
        app: AppHandle,
        owner_id: u64,
        resolved: &super::remote_workspace_target::ResolvedWorkspace,
        valid: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
        grant: Arc<super::local_mcp_relay::config::Grant>,
        handle: String,
    ) -> Self {
        Self {
            app,
            lifetime: Lifetime::Task { valid, handle },
            owner_id,
            team_key: resolved.team_key.clone(),
            machine_id: resolved.machine_id.clone(),
            client: Arc::downgrade(&resolved.client),
            relay_grant: grant,
        }
    }
    /// The authority of an agent `client`'s machine started for a
    /// routine of `team_key` as `session`, while this app was connected: it
    /// acts as the machine's owner, as long as its session runs.
    pub(crate) fn for_routine(
        app: AppHandle,
        owner_id: u64,
        machine_id: String,
        team_key: String,
        client: &Arc<SessionServerClient>,
        session: String,
        routine_key: String,
    ) -> Self {
        Self {
            app,
            lifetime: Lifetime::Routine {
                session,
                routine_key,
            },
            owner_id,
            team_key,
            machine_id,
            client: Arc::downgrade(client),
            relay_grant: Arc::new(super::local_mcp_relay::config::Grant::new()),
        }
    }
    /// The authority of a tab's agent `record` describes whose
    /// CodeSense MCP its machine hands back to this app on `client`, a new
    /// connection after a disconnect: the same tab, owner and team as at
    /// launch, served only while that connection is the tab's backend.
    /// The desktop-relayed servers its launch selected stay on the machine's
    /// own answers (`desktop_offline`); only CodeSense is handed back.
    pub(crate) fn for_reattached_shell(
        app: AppHandle,
        record: RemoteShellRecord,
        client: &Arc<SessionServerClient>,
    ) -> Self {
        Self {
            app,
            owner_id: record.owner_id,
            team_key: record.team_key.clone(),
            machine_id: record.machine_id.clone(),
            lifetime: Lifetime::Shell(Box::new(record)),
            client: Arc::downgrade(client),
            relay_grant: Arc::new(super::local_mcp_relay::config::Grant::new()),
        }
    }
    /// The authority of an agent of `client`'s machine that ended
    /// while this app was away: its queued calls only, as `session` (tab
    /// `handle` when this app launched it), for `team_key`.
    pub(crate) fn for_outbox(
        app: AppHandle,
        owner_id: u64,
        machine_id: String,
        team_key: String,
        client: &Arc<SessionServerClient>,
        session: String,
        handle: Option<String>,
    ) -> Self {
        Self {
            app,
            lifetime: Lifetime::Outbox { session, handle },
            owner_id,
            team_key,
            machine_id,
            client: Arc::downgrade(client),
            relay_grant: Arc::new(super::local_mcp_relay::config::Grant::new()),
        }
    }
    /// A remote agent launches terminals as its owner, like a local agent; the launch
    /// must belong to the same person.
    pub(crate) async fn admit_launch(&self, owner: u64, _team: &str) -> Result<(), String> {
        check_owner(self.owner_id, Some(owner)).map_err(|e| e.to_string())?;
        self.validate().await.map_err(|e| e.to_string())
    }
    /// A remote agent drives any of its owner's terminals, like a local agent, while its
    /// own connection is live.
    pub(crate) fn check_control(&self, target: &RemoteShellRecord) -> Result<(), String> {
        check_owner(self.owner_id, Some(target.owner_id))
            .map_err(|_| "terminal belongs to another person")?;
        let parent = self.client.upgrade().ok_or("original MCP caller retired")?;
        parent
            .private_lease()
            .ok_or("original MCP caller retired")?
            .validate()
    }
    pub(crate) async fn validate(&self) -> Result<(), ErrorData> {
        crate::commands::machines::authorize_remote_mcp_control(&self.app, &self.machine_id)
            .await
            .map_err(denied)?;
        let client = self
            .client
            .upgrade()
            .ok_or_else(|| denied("original connection ended"))?;
        let lease = client
            .private_lease()
            .ok_or_else(|| denied("private authority missing"))?;
        lease.validate().map_err(denied)?;
        if lease.owner_id() != self.owner_id {
            return Err(denied("private owner changed"));
        }
        let record = match &self.lifetime {
            Lifetime::Task { valid, .. } => {
                let valid = valid.clone();
                return crate::commands::await_blocking("remote_task_authority", move || valid())
                    .await
                    .map_err(denied);
            }
            // Ended: its owner's machine, still connected and still this
            // owner's (checked above), is all there is to check; what it may
            // call is limited in `arguments`.
            Lifetime::Outbox { .. } => return Ok(()),
            Lifetime::Routine { session, .. } => {
                let session = codesense_pty_core::domain::PtySessionId::new(session.clone());
                return crate::commands::await_blocking("routine_session_authority", move || {
                    use codesense_pty_core::domain::TerminalDomain;
                    match client
                        .live_child_pids(std::slice::from_ref(&session))
                        .map_err(|e| e.to_string())?
                        .first()
                        .copied()
                        .flatten()
                    {
                        Some(_) => Ok(()),
                        None => Err("managed session ended".to_string()),
                    }
                })
                .await
                .map_err(denied);
            }
            Lifetime::Shell(record) => record.clone(),
        };
        let app = self.app.clone();
        crate::commands::await_blocking("reverse_mcp_authority", move || {
            use codesense_pty_core::domain::TerminalDomain;
            let reference = RuntimeSessionReference::Admitted(Box::new(record.target.clone()));
            let backend =
                crate::commands::pty_backend::resolve_session_reference(&app, &reference, true)?;
            backend.revalidate(&app)?;
            let current = backend.client().ok_or("original connection unavailable")?;
            if !Arc::ptr_eq(&current, &client) {
                return Err("original connection replaced".into());
            }
            crate::commands::pty_layout::validate_remote_shell_group(&app, &record)?;
            if client
                .live_child_pids(std::slice::from_ref(&record.target.session))
                .map_err(|e| e.to_string())?
                .first()
                .copied()
                .flatten()
                .is_none()
            {
                return Err("managed session ended".into());
            }
            backend.revalidate(&app)
        })
        .await
        .map_err(denied)
    }
    pub(crate) async fn context(
        &self,
    ) -> Result<crate::commands::route_to_context::ActiveContext, ErrorData> {
        let (org, team) = self
            .team_key
            .split_once(':')
            .ok_or_else(|| denied("invalid bound team"))?;
        let result = crate::commands::route_to_context::derive_context_for(&self.app, org, team)
            .await
            .map_err(denied)?;
        self.validate().await?;
        match result {
            crate::commands::route_to_context::ContextResolution::Ok(context)
                if context.user_id == self.owner_id =>
            {
                Ok(context)
            }
            _ => Err(denied("bound context is no longer available")),
        }
    }
    pub(crate) fn team_key(&self) -> &str {
        &self.team_key
    }
    /// The machine whose terminal this caller runs in.
    pub(crate) fn machine_id(&self) -> &str {
        &self.machine_id
    }
    /// The tab this agent runs in, as `(handle, session)`; a task has none.
    pub(crate) fn caller_tab(&self) -> Option<(&str, &str)> {
        match &self.lifetime {
            Lifetime::Shell(_)
            | Lifetime::Outbox {
                handle: Some(_), ..
            } => self.caller_identity(),
            _ => None,
        }
    }
    /// Every captured session has an inbox identity, even without a visible tab handle.
    pub(crate) fn caller_identity(&self) -> Option<(&str, &str)> {
        captured_session_identity(&self.lifetime)
    }
    /// The tab session this agent runs in on its machine (its
    /// `$CODESENSE_TAB_SESSION_ID`): a launched tab's, or the session the
    /// machine started for a routine; a task has none.
    pub(crate) fn caller_session(&self) -> Option<&str> {
        match &self.lifetime {
            Lifetime::Shell(record) => Some(record.target.session.as_str()),
            Lifetime::Routine { session, .. } | Lifetime::Outbox { session, .. } => Some(session),
            Lifetime::Task { .. } => None,
        }
    }
    pub(crate) fn queue_owner(&self) -> Option<String> {
        match &self.lifetime {
            Lifetime::Routine { routine_key, .. } => Some(format!("routine:{routine_key}")),
            Lifetime::Shell(record) => {
                let launch = self
                    .app
                    .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()?;
                let bound = launch.remote_shell_for_session(record.target.session.as_str())?;
                (bound.handle == record.handle && bound.machine_id == record.machine_id)
                    .then(|| launch.queue_owner_for_session(record.target.session.as_str()))
                    .flatten()
            }
            _ => None,
        }
    }

    /// The remote agent task this caller serves (its handle); a terminal has
    /// none.
    pub(crate) fn caller_task(&self) -> Option<&str> {
        match &self.lifetime {
            Lifetime::Shell(_) | Lifetime::Routine { .. } | Lifetime::Outbox { .. } => None,
            Lifetime::Task { handle, .. } => Some(handle),
        }
    }
    /// A remote agent calls every tool a local agent can. The terminals it drives are
    /// its owner's, and every call that speaks as a session (inbox, role, progress,
    /// identity) speaks as its own tab: those arguments are bound to the authority here,
    /// before the tool reads them, so no argument can make it act as another session.
    pub(crate) fn arguments(
        &self,
        name: &str,
        args: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ErrorData> {
        if matches!(self.lifetime, Lifetime::Outbox { .. }) && !OUTBOX_TOOLS.contains(&name) {
            return Err(denied("a finished agent's queued calls only"));
        }
        bind_identity(name, args, self.caller_identity()).map_err(denied)?;
        if matches!(name, "run_in_terminal_tab" | "read_terminal_tab") {
            if let Some(handle) = args.get("handle").and_then(|value| value.as_str()) {
                if let Some(record) = self
                    .app
                    .state::<crate::commands::dashboard_launch::DashboardLaunchState>()
                    .remote_shell_by_handle(handle)
                {
                    check_owner(self.owner_id, Some(record.owner_id))?;
                    if name == "run_in_terminal_tab" {
                        self.check_control(&record).map_err(denied)?;
                    }
                }
            }
        }
        Ok(())
    }
}
fn captured_session_identity(lifetime: &Lifetime) -> Option<(&str, &str)> {
    match lifetime {
        Lifetime::Shell(record) => Some((&record.handle, record.target.session.as_str())),
        Lifetime::Outbox { session, handle } => {
            Some((handle.as_deref().unwrap_or(session), session))
        }
        Lifetime::Routine { session, .. } => Some((session, session)),
        Lifetime::Task { .. } => None,
    }
}

/// Tools whose `sessionId` names the session the call speaks as.
const SESSION_TOOLS: &[&str] = &[
    "send_message",
    "reply",
    "react",
    "read_messages",
    "who_am_i",
    "post_agent_message",
    "signal_progress",
    "signal_done",
    "ask_user",
    "report_step_result",
    "report_artifact",
    "workspace_chat_send",
    "workspace_chat_read",
    "notes_create",
    "notes_write",
    "notes_restore",
    "records_collections",
    "records_list",
    "records_read",
    "records_search",
    "records_history",
    "records_read_at",
    "records_create",
    "records_write",
    "records_set_fields",
    "records_delete",
    "records_restore",
    "agent_preview",
    "agent_create",
    "agent_update",
    "agent_delete",
    "agent_restore",
    "agent_unarchive",
    "routine_create",
    "routine_update",
    "routine_set_enabled",
    "routine_arm_on",
    "routine_refresh_on_machine",
    "routine_handover_release",
    "routine_run_now",
    "routine_cancel",
    "routine_delete",
    "routine_publish_result",
    "routine_context",
    "routine_done",
    "feedback_send",
    "list_out_of_date_terminal_servers",
    "update_terminal_servers",
    "schedule_wakeup",
    "list_wakeups",
    "cancel_wakeup",
    "wv_resolve_queue",
    "wv_mark_dispatched",
    "wv_close_task",
    "wv_post_assertion",
    "wv_coord_read",
    "wv_peer_read_file",
    "wv_escalate",
    "task_start",
    "task_update",
    "task_list",
    "task_read",
];
/// Tools whose session argument names who the call speaks as, outside the inbox:
/// `(tool, keys)`. A remote tab is bound to its own session; a remote task (no tab)
/// may not name one.
pub(super) const CLAIMED_SESSION_TOOLS: &[(&str, &[&str])] = &[
    // Its `sessionId` is the verified owner every claim stamps.
    ("wv_claim_queue", &["sessionId", "session_id"]),
    // A T3 question is raised on its `sessionId`.
    ("wv_write_approval", &["sessionId", "session_id"]),
    // The completion hook matches the packet by its `sessionId`.
    ("wv_report_handoff", &["sessionId", "session_id"]),
    // The surfaced-item guard: a window auto-closes only when checked in the session
    // the item was surfaced in.
    (
        "wv_open_t2_window",
        &["surfacedSession", "surfaced_session"],
    ),
    ("wv_check_t2_window", &["checkSession", "check_session"]),
    ("wv_close_t2_window", &["checkSession", "check_session"]),
];
/// Role tools: `instanceId` names the live instance the call speaks as.
pub(super) const ROLE_INSTANCE_TOOLS: &[&str] = &[
    "register_role",
    "claim_role_messages",
    "accept_role_message",
    "ack_role_message",
    "release_role",
];

pub(crate) fn is_identity_tool(name: &str) -> bool {
    name == "wv_write_decision"
        || SESSION_TOOLS.contains(&name)
        || ROLE_INSTANCE_TOOLS.contains(&name)
        || CLAIMED_SESSION_TOOLS.iter().any(|(tool, _)| *tool == name)
        || matches!(
            name,
            "send_role_message"
                | "inbox_post"
                | "send_to_session"
                | "mark_unread"
                | "open_workspace"
                | "open_remote_tab"
        )
}

/// Bind the arguments of a call that speaks as a session to `me`, the caller's tab
/// `(handle, session)`: its `sessionId` is its own tab session (one it names must be
/// that tab, by handle or session), a role instance is its tab session, and a role
/// message's sender is its tab (filled by the tool from the connection). A caller with
/// no tab (a remote task) has no session identity: see [`bind_task`].
pub(crate) fn bind_identity(
    name: &str,
    args: &mut serde_json::Map<String, serde_json::Value>,
    me: Option<(&str, &str)>,
) -> Result<(), String> {
    if matches!(name, "open_workspace" | "open_remote_tab") {
        let Some((handle, session)) = me else {
            return bind_task(name, args);
        };
        bind_session_keys(
            args,
            &["launchedBy", "launched_by"],
            (handle, session),
            handle,
        )?;
        if name == "open_remote_tab"
            && args.get("operation").and_then(|v| v.as_str()) == Some("attach")
        {
            args.remove("launchedBy");
        }
        return Ok(());
    }
    if let Some((_, keys)) = CLAIMED_SESSION_TOOLS.iter().find(|(tool, _)| *tool == name) {
        return bind_claimed_session(name, keys, args, me);
    }
    if name == "wv_write_decision" {
        let Some(me) = me else {
            return bind_task(name, args);
        };
        return bind_session_keys(args, &["owner"], me, me.1);
    }
    let session_tool = SESSION_TOOLS.contains(&name);
    let role_tool = ROLE_INSTANCE_TOOLS.contains(&name);
    let sender_tool = name == "send_role_message";
    if !(session_tool || role_tool || sender_tool) {
        return Ok(());
    }
    let Some((handle, session)) = me else {
        return bind_task(name, args);
    };
    if session_tool {
        bind_session_keys(
            args,
            &["sessionId", "session_id"],
            (handle, session),
            session,
        )?;
    }
    if role_tool {
        args.remove("instance_id");
        args.insert("instanceId".into(), session.into());
        if name == "register_role" {
            // The session whose life keeps the registration renewed is the caller's
            // own tab: naming another would hold a role on that session's lifetime.
            bind_session_keys(
                args,
                &["sessionId", "session_id"],
                (handle, session),
                session,
            )?;
        }
    }
    if sender_tool {
        if let Some(named) = args.remove("sender") {
            let named = named.as_str().map(str::trim).unwrap_or_default();
            if !named.is_empty() && named != session && named != handle {
                return Err(format!(
                    "an agent on a machine sends only as its own session ({handle}), not '{named}'"
                ));
            }
        }
    }
    Ok(())
}

fn bind_session_keys(
    args: &mut serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
    (handle, session): (&str, &str),
    value: &str,
) -> Result<(), String> {
    for key in keys {
        if let Some(named) = args.remove(*key) {
            let named = named.as_str().map(str::trim).unwrap_or_default();
            if !named.is_empty() && named != session && named != handle {
                return Err(format!(
                    "an agent on a machine speaks only as its own session ({session}), not '{named}'"
                ));
            }
        }
    }
    args.insert(keys[0].into(), value.into());
    Ok(())
}

/// A sessionless remote task cannot speak as a session or as the human.
fn bind_task(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let named = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            args.get(*key)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|named| !named.is_empty())
                .map(str::to_string)
        })
    };
    if ROLE_INSTANCE_TOOLS.contains(&name) {
        return Err(format!(
            "{name} acts as a live role instance, and a remote task runs in no terminal tab to be one; run the agent in a remote tab"
        ));
    }
    let keys: &[&str] = if name == "send_role_message" {
        &["sender"]
    } else {
        &["sessionId", "session_id"]
    };
    match named(keys) {
        Some(named) => Err(format!(
            "{name}: a remote task runs in no CodeSense session and cannot act as '{named}'"
        )),
        None => Err(format!(
            "{name} speaks as a CodeSense session, and a remote task runs in no terminal tab; run the agent in a remote tab"
        )),
    }
}

/// [`CLAIMED_SESSION_TOOLS`]: a tab caller's session argument (`keys`, either
/// spelling) is its own tab session (one it names must be that tab); a remote task,
/// which has no session, may not name one. A remote caller never supplies an
/// approval's `queue:` endorsement pointer: only the server derives that, from the
/// caller's own session.
fn bind_claimed_session(
    name: &str,
    keys: &[&str],
    args: &mut serde_json::Map<String, serde_json::Value>,
    me: Option<(&str, &str)>,
) -> Result<(), String> {
    if name == "wv_write_approval" {
        let forged = args
            .get("source")
            .and_then(|value| value.as_str())
            .is_some_and(|source| source.trim().starts_with("queue:"));
        if forged {
            return Err(
                "an agent on a machine can't name an approval's queue: source; it is derived from its own session".into(),
            );
        }
    }
    let mut named_any: Option<String> = None;
    for key in keys {
        if let Some(named) = args.remove(*key) {
            let named = named.as_str().map(str::trim).unwrap_or_default();
            if !named.is_empty() {
                named_any = Some(named.to_string());
            }
        }
    }
    match me {
        Some((handle, session)) => {
            if let Some(named) = named_any {
                if named != session && named != handle {
                    return Err(format!(
                        "an agent on a machine speaks only as its own session ({session}), not '{named}'"
                    ));
                }
            }
            args.insert(keys[0].into(), session.into());
            Ok(())
        }
        None => match named_any {
            Some(named) => Err(format!(
                "{name}: a remote task runs in no CodeSense session and cannot act as '{named}'"
            )),
            None if name == "wv_claim_queue" => Err(format!(
                "{name}: a remote task runs in no terminal tab, so a row it claims would have no owner to finish it; run the controller in a remote tab"
            )),
            None => Ok(()),
        },
    }
}

struct RemoteCall {
    identity: Option<(String, String)>,
    team: Option<crate::commands::dashboard_launch::LaunchTeam>,
}

tokio::task_local! {
    // Captured once per admitted call; never populated from tool arguments.
    static REMOTE_CALLER: RemoteCall;
}

/// Keep launch-captured ownership beside the remote session through inbox persistence.
pub(crate) async fn as_remote_context<F: std::future::Future>(
    identity: Option<(String, String)>,
    team: Option<crate::commands::dashboard_launch::LaunchTeam>,
    call: F,
) -> F::Output {
    REMOTE_CALLER
        .scope(RemoteCall { identity, team }, call)
        .await
}

pub(crate) fn caller_team(address: &str) -> Option<crate::commands::dashboard_launch::LaunchTeam> {
    REMOTE_CALLER
        .try_with(|context| {
            let (handle, session) = context.identity.as_ref()?;
            (address == handle || address == session)
                .then(|| context.team.clone())
                .flatten()
        })
        .ok()
        .flatten()
}

/// Inside a remote call, whether `session_id` is the caller's own (live: its authority
/// was validated for this call). `None` outside a remote call, where the local liveness
/// check applies. A remote agent never passes as another session, live or not.
pub(crate) fn remote_session_check(session_id: &str) -> Option<Result<(), String>> {
    REMOTE_CALLER
        .try_with(|context| {
            match context
                .identity
                .as_ref()
                .map(|(_, session)| session.as_str())
            {
                Some(me) if me == session_id.trim() => Ok(()),
                Some(me) => Err(format!(
                    "an agent on a machine speaks only as its own session ({me}), not '{}'",
                    session_id.trim()
                )),
                None => Err("a remote task runs in no CodeSense session".into()),
            }
        })
        .ok()
}

/// Whether a tab's agent hears its inbox notices through hooks the machine generated: a
/// launched CLI whose inbox port arms any, or one typed into a shell tab (its machine's
/// host observes it). Only such a tab's notices are pushed to its machine.
pub(crate) fn hooks_accepted(record: &RemoteShellRecord) -> bool {
    record.hooks && runs_generated_hooks(record.launch)
}

/// Whether the machine tells a terminal of this launch its inbox notices: a launched
/// CLI with any inbox port (its hooks the machine writes, or the machine's own submit),
/// or a shell's CLI typed in it.
fn runs_generated_hooks(launch: NativeLaunch) -> bool {
    launch.cli().is_none_or(|cli| {
        !matches!(
            codesense_pty_core::agent_cli::adapter(cli).inbox(),
            codesense_pty_core::agent_cli::InboxDelivery::None
        )
    })
}

/// Whether a launch's hooks are its own: the machine's server wires them (any server
/// with saved-Agent startup, before or after agent activity for every terminal) into a
/// runtime that runs them.
fn trusted_hooks(generates_hooks: bool, launch: NativeLaunch) -> bool {
    generates_hooks && runs_generated_hooks(launch)
}

/// A remote agent acts for the person who owns its machine session. A local tab
/// (`None`) is that person's too.
fn check_owner(owner: u64, target: Option<u64>) -> Result<(), ErrorData> {
    match target {
        Some(actual) if actual != owner => Err(denied("terminal belongs to another person")),
        _ => Ok(()),
    }
}

fn denied(error: impl ToString) -> ErrorData {
    ErrorData::invalid_request(format!("remote authority: {}", error.to_string()), None)
}

pub(crate) struct CapturedLaunch {
    relay_grant: Arc<super::local_mcp_relay::config::Grant>,
    server_ids: Vec<String>,
    remote_entries: Vec<codesense_host_proto::reverse_mcp::RemoteStdioEntry>,
}

/// Resolve once before durable reservation. Reattach never re-reads this profile.
///
/// `generates_hooks`: the machine's terminal server says it wires CodeSense's hooks
/// into the agents it starts (an older one does not, and then none are trusted).
pub(crate) async fn capture_launch(
    app: &AppHandle,
    record: &mut RemoteShellRecord,
    parent: Option<&RemoteAuthority>,
    generates_hooks: bool,
) -> Result<Option<CapturedLaunch>, String> {
    // Default-off keeps ordinary shells available without a remote MCP grant.
    if crate::commands::machines::authorize_remote_mcp_control(app, &record.machine_id)
        .await
        .is_err()
    {
        return if record.agent.is_some() || !record.launch.is_shell() {
            Err("Enable remote MCP permission in Machines before launching Claude/Codex".into())
        } else {
            Ok(None)
        };
    }
    let (relay_grant, profile_revision) =
        super::local_mcp_relay::capture(app, record, parent).await?;
    // Every Claude on a machine hears about its inbox through the hooks the machine's
    // host generates: in a launched Claude's own settings, and in a `claude` typed in a
    // shell there, which the host observes. Only a host that says it generates them is
    // trusted.
    record.hooks = trusted_hooks(generates_hooks, record.launch);
    record.mcp_profile_revision = profile_revision;
    record.mcp_inventory = vec![crate::commands::pty_layout::McpInventoryEntry {
        id: "codesense".into(),
        logical_name: "CodeSense".into(),
        execution_location: "desktop".into(),
        availability: "selected".into(),
    }];
    let mut server_ids = vec!["codesense".to_string()];
    let mut remote_entries = Vec::new();
    for (id, server) in relay_grant.iter() {
        let (logical_name, location) = match &server.endpoint {
            super::local_mcp_relay::config::Endpoint::Remote(entry) => {
                (entry.logical_name.clone(), "remoteWorkspace")
            }
            _ => (id.clone(), "desktop"),
        };
        record
            .mcp_inventory
            .push(crate::commands::pty_layout::McpInventoryEntry {
                id: id.clone(),
                logical_name,
                execution_location: location.into(),
                availability: "selected".into(),
            });
        match &server.endpoint {
            super::local_mcp_relay::config::Endpoint::Remote(entry) => {
                remote_entries.push(entry.clone())
            }
            _ => server_ids.push(id.clone()),
        }
    }
    if !remote_entries.is_empty() && record.launch.is_shell() {
        return Err("remote workspace MCP requires an explicit Claude or Codex launch".into());
    }
    codesense_host_proto::reverse_mcp::validate_remote_entries(&server_ids, &remote_entries)?;
    let server_ids = codesense_host_proto::reverse_mcp::selected_servers(server_ids)?;
    Ok(Some(CapturedLaunch {
        relay_grant,
        server_ids,
        remote_entries,
    }))
}

/// Provision only the immutable selection captured for this exact admitted session.
pub(crate) async fn prepare_bound(
    app: &AppHandle,
    record: &RemoteShellRecord,
    captured: Option<CapturedLaunch>,
) -> Result<bool, String> {
    let Some(CapturedLaunch {
        relay_grant,
        server_ids,
        remote_entries,
    }) = captured
    else {
        return Ok(false);
    };
    crate::commands::machines::authorize_remote_mcp_control(app, &record.machine_id).await?;
    let reference = RuntimeSessionReference::Admitted(Box::new(record.target.clone()));
    let backend = crate::commands::pty_backend::resolve_session_reference(app, &reference, true)?;
    backend.revalidate(app)?;
    let client = backend.client().ok_or("private client unavailable")?;
    let authority = RemoteAuthority {
        app: app.clone(),
        lifetime: Lifetime::Shell(Box::new(record.clone())),
        owner_id: record.owner_id,
        team_key: record.team_key.clone(),
        machine_id: record.machine_id.clone(),
        client: Arc::downgrade(&client),
        relay_grant,
    };
    let serve = Arc::new(move |server_id: String, stream: tokio::io::DuplexStream| {
        let authority = authority.clone();
        tauri::async_runtime::spawn(async move {
            if authority.validate().await.is_err() {
                return;
            }
            if server_id != "codesense" {
                let Some(server) = authority.relay_grant.get(&server_id).cloned() else {
                    return;
                };
                let valid: super::local_mcp_relay::Validate = Arc::new(move || {
                    let authority = authority.clone();
                    Box::pin(async move {
                        authority
                            .validate()
                            .await
                            .map_err(|_| "remote MCP authority retired".into())
                    })
                });
                super::local_mcp_relay::serve(server, stream, valid).await;
                return;
            }
            let handler =
                super::handler::CodesenseHandler::new_remote(authority.app.clone(), authority);
            super::socket_server::serve_stream(handler, stream).await;
        });
    });
    // A saved Agent's startup, or a raw runtime's permission mode.
    // The same pinned pre-spawn intent must reuse its private provision rather
    // than conflict with its own configuration after a lost preparation reply.
    let nonce = if record.agent.is_some() {
        blake3::hash(
            format!(
                "{}:{}:{}",
                record.handle,
                record.request_id,
                record.target.session.as_str()
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string()
    } else {
        format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        )
    };
    let session = record.target.session.clone();
    let launch = record.launch;
    prepare_agent_startup(
        &crate::commands::agent_inbox::AppInboxHost { app: app.clone() },
        record,
        |agent_startup| async move {
            crate::commands::await_blocking("prepare_reverse_mcp", move || {
                client
                    .prepare_native_remote(
                        &session,
                        nonce,
                        crate::terminal::session_server::NativeMcpSelection {
                            server_ids,
                            remote_entries,
                            agent_startup,
                        },
                        launch,
                        serve,
                    )
                    .map_err(|e| e.to_string())
            })
            .await
        },
    )
    .await?;
    Ok(true)
}

/// Prepare the exact startup provisioned on the machine, with membership established first.
pub(crate) async fn prepare_agent_startup<T, F>(
    host: &dyn crate::commands::agent_inbox::InboxHost,
    record: &RemoteShellRecord,
    provision: impl FnOnce(Option<codesense_host_proto::reverse_mcp::AgentStartup>) -> F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let team = crate::commands::dashboard_launch::launch_team_from_key(&record.team_key);
    let role = record
        .identity
        .queue_role
        .as_ref()
        .and_then(|r| r.as_deref());
    let startup = record.agent_startup();
    let prompt = startup.as_ref().and_then(|s| s.prompt.as_deref());
    crate::commands::role_startup::start(
        host,
        team.as_ref(),
        role,
        record.target.session.as_str(),
        true,
        prompt,
        |prompt| {
            let mut startup = startup.clone();
            if let Some(startup) = startup.as_mut() {
                startup.prompt = prompt;
            }
            provision(startup)
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn why_line_does_not_name_a_session_target() {
        assert!(!super::SESSION_TOOLS.contains(&"why_line"));
    }

    #[tokio::test]
    async fn remote_launch_owner_admission_precedes_preparation_and_commit() {
        let root = tempfile::tempdir().unwrap();
        let prepare_effect = root.path().join("selected-receiver");
        let spawn_effect = root.path().join("spawn-commit");
        let (lease, retire) = crate::commands::auth::private_authority::test_lease(41);
        for (owner, permitted) in [(42, false), (41, true)] {
            let result = crate::agents::remote_launch::drive_prepared(
                true,
                async {
                    lease.validate()?;
                    super::check_owner(41, Some(owner)).map_err(|e| e.to_string())?;
                    std::fs::write(&prepare_effect, b"selected").map_err(|e| e.to_string())
                },
                |()| async {
                    std::fs::write(&spawn_effect, b"committed").map_err(|e| e.to_string())
                },
            )
            .await;
            assert_eq!(result.is_ok(), permitted);
            assert_eq!(prepare_effect.exists(), permitted);
            assert_eq!(spawn_effect.exists(), permitted);
        }
        retire();
        assert!(lease.validate().is_err());
    }

    #[test]
    fn routine_cancel_binds_the_caller_without_rewriting_the_target_run() {
        for local in [false, true] {
            let mut args = serde_json::json!({"runId":"target-run"})
                .as_object()
                .unwrap()
                .clone();
            if local {
                crate::mcp::local_identity::arguments(
                    "routine_cancel",
                    &mut args,
                    Some("mine"),
                    Some("handle"),
                )
                .unwrap();
            } else {
                super::bind_identity("routine_cancel", &mut args, Some(("handle", "mine")))
                    .unwrap();
            }
            assert_eq!(args.get("sessionId"), Some(&serde_json::json!("mine")));
            assert_eq!(args["runId"], "target-run");
        }
    }

    #[test]
    fn every_identity_tool_refuses_foreign_and_unbound_callers() {
        let router = crate::mcp::handler::CodesenseHandler::served_tool_router();
        let added = [
            "workspace_chat_send",
            "workspace_chat_read",
            "notes_create",
            "notes_write",
            "notes_restore",
            "records_create",
            "records_write",
            "records_set_fields",
            "records_delete",
            "records_restore",
            "agent_create",
            "agent_update",
            "agent_delete",
            "agent_restore",
            "agent_unarchive",
            "agent_preview",
            "routine_create",
            "routine_update",
            "routine_set_enabled",
            "routine_arm_on",
            "routine_refresh_on_machine",
            "routine_handover_release",
            "routine_run_now",
            "routine_cancel",
            "routine_delete",
            "routine_publish_result",
            "feedback_send",
        ];
        for tool in added {
            assert!(
                super::is_identity_tool(tool),
                "unclassified identity tool {tool}"
            );
        }
        for tool in router.list_all() {
            let name = tool.name.as_ref();
            if !super::SESSION_TOOLS.contains(&name) {
                continue;
            }
            for key in ["sessionId", "session_id"] {
                let mut spoof = serde_json::Map::new();
                spoof.insert(key.into(), "victim".into());
                assert!(
                    super::bind_identity(name, &mut spoof.clone(), Some(("handle", "mine")))
                        .is_err(),
                    "{name}/{key}"
                );
                assert!(
                    crate::mcp::local_identity::arguments(
                        name,
                        &mut spoof,
                        Some("mine"),
                        Some("handle")
                    )
                    .is_err(),
                    "{name}/{key}"
                );
            }
            for explicit in [None, Some("mine"), Some("handle")] {
                let mut args = serde_json::Map::new();
                if let Some(id) = explicit {
                    args.insert("sessionId".into(), id.into());
                }
                super::bind_identity(name, &mut args, Some(("handle", "mine"))).unwrap();
                assert_eq!(args["sessionId"], "mine", "{name}");
            }
            assert!(
                crate::mcp::local_identity::arguments(
                    name,
                    &mut serde_json::Map::new(),
                    None,
                    None
                )
                .is_err(),
                "{name}"
            );
            assert!(
                super::bind_identity(name, &mut serde_json::Map::new(), None).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn registry_identity_fields_must_be_classified() {
        let errors: Vec<_> = crate::mcp::handler::CodesenseHandler::served_tool_router()
            .list_all()
            .into_iter()
            .filter_map(|tool| {
                super::super::identity_guard::check_tool(
                    &tool.name,
                    &serde_json::Value::Object((*tool.input_schema).clone()),
                )
                .err()
            })
            .collect();
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    #[test]
    fn weft_queue_and_decision_claims_name_only_the_verified_caller() {
        for (tool, keys) in [
            ("wv_resolve_queue", &["sessionId", "session_id"][..]),
            ("wv_mark_dispatched", &["sessionId", "session_id"][..]),
            ("wv_close_task", &["sessionId", "session_id"][..]),
            ("wv_claim_queue", &["sessionId", "session_id"][..]),
            ("wv_write_decision", &["owner"][..]),
        ] {
            assert!(super::is_identity_tool(tool), "{tool}");
            for &key in keys {
                for foreign in ["victim", "controller", "victim-run"] {
                    let mut args = args(serde_json::json!({key: foreign}));
                    assert!(
                        super::bind_identity(tool, &mut args, ME).is_err(),
                        "{tool}/{key}"
                    );
                }
                for own in [None, Some("remote-shell-me"), Some("remote-tab-me")] {
                    let mut args = serde_json::Map::new();
                    if let Some(own) = own {
                        args.insert(key.into(), own.into());
                    }
                    super::bind_identity(tool, &mut args, ME).unwrap();
                    assert_eq!(args[keys[0]], "remote-shell-me", "{tool}");
                }
                assert!(
                    crate::mcp::local_identity::arguments(
                        tool,
                        &mut args(serde_json::json!({key: "victim"})),
                        None,
                        None
                    )
                    .is_err(),
                    "{tool}"
                );
                assert!(
                    super::bind_identity(tool, &mut serde_json::Map::new(), None).is_err(),
                    "{tool}"
                );
            }
        }
    }

    #[test]
    fn launch_attribution_is_the_verified_caller() {
        for tool in ["open_workspace", "open_remote_tab"] {
            for key in ["launchedBy", "launched_by"] {
                let mut args = serde_json::Map::new();
                args.insert(key.into(), "victim".into());
                assert!(super::bind_identity(tool, &mut args, Some(("handle", "mine"))).is_err());
                assert!(crate::mcp::local_identity::arguments(
                    tool,
                    &mut serde_json::Map::new(),
                    None,
                    None
                )
                .is_err());
            }
            let mut args = serde_json::Map::new();
            super::bind_identity(tool, &mut args, Some(("handle", "mine"))).unwrap();
            assert_eq!(args["launchedBy"], "handle");
            for claimed in ["mine", "handle"] {
                let mut args = serde_json::json!({"launchedBy": claimed})
                    .as_object()
                    .unwrap()
                    .clone();
                super::bind_identity(tool, &mut args, Some(("handle", "mine"))).unwrap();
                assert_eq!(args["launchedBy"], "handle");
            }
        }
        let mut attach = serde_json::json!({"operation":"attach"})
            .as_object()
            .unwrap()
            .clone();
        super::bind_identity("open_remote_tab", &mut attach, Some(("handle", "mine"))).unwrap();
        assert!(
            !attach.contains_key("launchedBy"),
            "attach retains the existing launcher"
        );
    }

    #[test]
    fn review_r1_routine_and_handleless_outbox_keep_their_captured_identity() {
        let routine = super::Lifetime::Routine {
            session: "remote-routine".into(),
            routine_key: "org/team/controller".into(),
        };
        assert_eq!(
            super::captured_session_identity(&routine),
            Some(("remote-routine", "remote-routine"))
        );
        let outbox = super::Lifetime::Outbox {
            session: "outbox-session".into(),
            handle: None,
        };
        assert_eq!(
            super::captured_session_identity(&outbox),
            Some(("outbox-session", "outbox-session"))
        );
        let tab = super::Lifetime::Outbox {
            session: "tab-session".into(),
            handle: Some("tab-handle".into()),
        };
        assert_eq!(
            super::captured_session_identity(&tab),
            Some(("tab-handle", "tab-session"))
        );
        let task = super::Lifetime::Task {
            valid: std::sync::Arc::new(|| Ok(())),
            handle: "task".into(),
        };
        assert_eq!(super::captured_session_identity(&task), None);
        for (tool, lifetime) in [
            ("send_message", &routine),
            ("reply", &routine),
            ("read_messages", &routine),
            ("send_message", &outbox),
        ] {
            let mut args = serde_json::Map::new();
            super::bind_identity(tool, &mut args, super::captured_session_identity(lifetime))
                .unwrap();
            assert_eq!(
                args["sessionId"],
                super::captured_session_identity(lifetime).unwrap().1
            );
        }
    }

    #[test]
    fn inbox_hooks_are_accepted_from_a_cli_with_an_inbox_port_or_a_shell() {
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        let mut record: crate::commands::pty_layout::RemoteShellRecord =
            serde_json::from_value(serde_json::json!({
                "handle": "remote-tab-x", "owner_id": 41, "request_id": uuid::Uuid::new_v4(),
                "machine_id": "box", "team_key": "org:team", "workspace_id": "w",
                "workspace": "org:team|w", "cwd": "/home/dev", "name": null, "launch": "claude",
                "target": {"member":{"machineId":"box","slot":"remote"},"admissionRevision":"1",
                    "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": "s1"}
            }))
            .unwrap();
        // A terminal launched without hooks: any hook from it is forged.
        assert!(!super::hooks_accepted(&record));
        record.hooks = true;
        assert!(super::hooks_accepted(&record));
        // A shell tab: a `claude` typed there reports through the tab's own hooks.
        record.launch = NativeLaunch::Shell;
        assert!(super::hooks_accepted(&record));
        record.hooks = false;
        assert!(!super::hooks_accepted(&record));
        // A Codex's checks run the hooks its machine wrote, like a Claude's waiter.
        record.hooks = true;
        record.launch = NativeLaunch::Cli(AgentCli::Codex);
        assert!(super::hooks_accepted(&record));
        // An older server that still wires the inbox hooks keeps them trusted; one that
        // wires none has none.
        assert!(super::trusted_hooks(true, NativeLaunch::Shell));
        assert!(super::trusted_hooks(
            true,
            NativeLaunch::Cli(AgentCli::Claude)
        ));
        assert!(!super::trusted_hooks(
            false,
            NativeLaunch::Cli(AgentCli::Claude)
        ));
        assert!(super::trusted_hooks(
            true,
            NativeLaunch::Cli(AgentCli::Codex)
        ));
        assert!(!super::trusted_hooks(
            false,
            NativeLaunch::Cli(AgentCli::Codex)
        ));
        // What a captured launch records: every CLI with an inbox port, and a shell,
        // carries hooks.
        for launch in [
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
            NativeLaunch::Shell,
        ] {
            assert!(super::runs_generated_hooks(launch), "{launch:?}");
        }
    }

    fn args(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    const ME: Option<(&str, &str)> = Some(("remote-tab-me", "remote-shell-me"));

    /// Inbox, progress and identity calls from a remote agent are admitted, speaking as
    /// its own tab session whatever (or whether) they name.
    #[test]
    fn a_remote_agents_session_calls_are_bound_to_its_own_tab() {
        use super::super::handler::{
            ReadMessagesParams, ReplyParams, SendMessageParams, SignalProgressParams, WhoAmIParams,
        };
        let mut send = args(serde_json::json!({"to": "human", "text": "hi"}));
        super::bind_identity("send_message", &mut send, ME).unwrap();
        let send: SendMessageParams = serde_json::from_value(send.into()).unwrap();
        assert_eq!(send.session_id.as_deref(), Some("remote-shell-me"));
        // Its own id by session or handle, in either spelling, is accepted.
        for named in [
            serde_json::json!({"sessionId": "remote-shell-me"}),
            serde_json::json!({"session_id": "remote-tab-me"}),
            serde_json::json!({"sessionId": ""}),
        ] {
            let mut read = args(named);
            super::bind_identity("read_messages", &mut read, ME).unwrap();
            let read: ReadMessagesParams = serde_json::from_value(read.into()).unwrap();
            assert_eq!(read.session_id.as_deref(), Some("remote-shell-me"));
        }
        let mut reply = args(serde_json::json!({"messageId": 4, "text": "ok"}));
        super::bind_identity("reply", &mut reply, ME).unwrap();
        let reply: ReplyParams = serde_json::from_value(reply.into()).unwrap();
        assert_eq!(reply.session_id.as_deref(), Some("remote-shell-me"));
        let mut progress =
            args(serde_json::json!({"sessionId": "remote-shell-me", "phase": "dev"}));
        super::bind_identity("signal_progress", &mut progress, ME).unwrap();
        let progress: SignalProgressParams = serde_json::from_value(progress.into()).unwrap();
        assert_eq!(progress.session_id, "remote-shell-me");
        let mut who = args(serde_json::json!({}));
        super::bind_identity("who_am_i", &mut who, ME).unwrap();
        let who: WhoAmIParams = serde_json::from_value(who.into()).unwrap();
        assert_eq!(who.session_id.as_deref(), Some("remote-shell-me"));
    }

    /// No argument makes a remote agent speak as another session: not a session id,
    /// not a role message sender.
    #[test]
    fn a_remote_agent_cannot_name_another_session() {
        for tool in [
            "send_message",
            "reply",
            "read_messages",
            "signal_done",
            "ask_user",
        ] {
            let mut spoof = args(serde_json::json!({"sessionId": "S1-local"}));
            let refused = super::bind_identity(tool, &mut spoof, ME).unwrap_err();
            assert!(refused.contains("its own session"), "{tool}: {refused}");
        }
        let mut spoof = args(serde_json::json!({"session_id": "H1"}));
        assert!(super::bind_identity("post_agent_message", &mut spoof, ME).is_err());
        let mut sender = args(serde_json::json!({"team": {"org": "a", "team": "b"},
            "role": "dev", "text": "x", "sender": "human"}));
        assert!(super::bind_identity("send_role_message", &mut sender, ME).is_err());
    }

    /// A role message goes out under the caller's tab (the tool fills the sender from
    /// the connection), and a role instance is the caller's tab session, so a claim,
    /// accept, ack or release only ever acts on its own registration.
    #[test]
    fn a_remote_agents_role_calls_speak_as_its_own_instance() {
        use super::super::handler::{
            ClaimRoleMessagesParams, RegisterRoleParams, SendRoleMessageParams,
        };
        let mut send = args(serde_json::json!({"team": {"org": "a", "team": "b"},
            "role": "dev", "text": "x", "sender": "remote-tab-me"}));
        super::bind_identity("send_role_message", &mut send, ME).unwrap();
        let send: SendRoleMessageParams = serde_json::from_value(send.into()).unwrap();
        assert_eq!(send.sender, None);
        let mut register = args(serde_json::json!({"team": {"org": "a", "team": "b"},
            "role": "dev", "instanceId": "someone-else"}));
        super::bind_identity("register_role", &mut register, ME).unwrap();
        let register: RegisterRoleParams = serde_json::from_value(register.into()).unwrap();
        assert_eq!(register.instance_id.as_deref(), Some("remote-shell-me"));
        assert_eq!(register.session_id.as_deref(), Some("remote-shell-me"));
        let mut spoof = args(serde_json::json!({"team": {"org": "a", "team": "b"},
            "role": "dev", "sessionId": "local-other"}));
        assert!(super::bind_identity("register_role", &mut spoof, ME).is_err());
        let mut claim = args(serde_json::json!({"team": {"org": "a", "team": "b"},
            "role": "dev", "instanceId": "x", "generation": 3}));
        super::bind_identity("claim_role_messages", &mut claim, ME).unwrap();
        let claim: ClaimRoleMessagesParams = serde_json::from_value(claim.into()).unwrap();
        assert_eq!(claim.instance_id, "remote-shell-me");
        for tool in ["accept_role_message", "ack_role_message", "release_role"] {
            let mut call = args(serde_json::json!({"instanceId": "x"}));
            super::bind_identity(tool, &mut call, ME).unwrap();
            assert_eq!(call["instanceId"], "remote-shell-me", "{tool}");
        }
    }

    /// A sessionless task is refused for identity-bearing calls; ordinary reads pass.
    #[test]
    fn a_sessionless_remote_task_cannot_speak_as_the_human_and_reads_pass_untouched() {
        for tool in [
            "send_message",
            "reply",
            "read_messages",
            "send_role_message",
        ] {
            let original = args(serde_json::json!({"text": "hi"}));
            let mut call = original.clone();
            assert!(
                super::bind_identity(tool, &mut call, None).is_err(),
                "{tool}"
            );
            assert_eq!(call, original, "{tool} does not rewrite a refused call");
        }
        for tool in super::ROLE_INSTANCE_TOOLS {
            for call in [
                serde_json::json!({"role": "dev", "instanceId": "other-instance"}),
                serde_json::json!({"role": "dev"}),
            ] {
                let mut call = args(call);
                let refused = super::bind_identity(tool, &mut call, None).unwrap_err();
                assert!(refused.contains("no terminal tab"), "{tool}: {refused}");
            }
        }
        for tool in ["send_message", "reply", "read_messages"] {
            for key in ["sessionId", "session_id"] {
                let mut call = args(serde_json::json!({ key: "S1-local" }));
                let refused = super::bind_identity(tool, &mut call, None).unwrap_err();
                assert!(
                    refused.contains("cannot act as 'S1-local'"),
                    "{tool}: {refused}"
                );
            }
        }
        let mut sender = args(serde_json::json!({"sender": "H1"}));
        assert!(super::bind_identity("send_role_message", &mut sender, None).is_err());
        for tool in ["who_am_i", "signal_progress", "signal_done", "ask_user"] {
            let mut call = args(serde_json::json!({}));
            assert!(
                super::bind_identity(tool, &mut call, None).is_err(),
                "{tool}"
            );
        }
        for tool in [
            "inbox_list",
            "role_state",
            "list_sessions",
            "list_agent_updates",
        ] {
            let original = args(serde_json::json!({"sessionId": "H2", "team": null}));
            let mut call = original.clone();
            super::bind_identity(tool, &mut call, ME).unwrap();
            assert_eq!(call, original, "{tool} reads any inbox");
            super::bind_identity(tool, &mut call, None).unwrap();
        }
    }

    /// Queue and approval tools take the caller's session from its tab: a foreign
    /// `sessionId` is refused, a missing one is filled, and a remote task (no tab) may
    /// call them only without naming a session.
    #[test]
    fn queue_and_approval_tools_bind_the_callers_own_session() {
        for (tool, key) in [
            ("wv_claim_queue", "sessionId"),
            ("wv_write_approval", "sessionId"),
            ("wv_report_handoff", "sessionId"),
            ("wv_open_t2_window", "surfacedSession"),
            ("wv_check_t2_window", "checkSession"),
            ("wv_close_t2_window", "checkSession"),
        ] {
            let snake_key = match key {
                "sessionId" => "session_id",
                "surfacedSession" => "surfaced_session",
                _ => "check_session",
            };
            let mut spoof = args(serde_json::json!({"id": 1, key: "local-controller"}));
            assert!(
                super::bind_identity(tool, &mut spoof, ME).is_err(),
                "{tool}"
            );
            let mut snake = args(serde_json::json!({"id": 1, snake_key: "local-controller"}));
            assert!(
                super::bind_identity(tool, &mut snake, ME).is_err(),
                "{tool}"
            );
            let mut none = args(serde_json::json!({"id": 1}));
            super::bind_identity(tool, &mut none, ME).unwrap();
            assert_eq!(none[key], "remote-shell-me", "{tool}");
            let mut own = args(serde_json::json!({"id": 1, key: "remote-tab-me"}));
            super::bind_identity(tool, &mut own, ME).unwrap();
            assert_eq!(own[key], "remote-shell-me", "{tool}");
            let mut task_spoof = args(serde_json::json!({"id": 1, key: "S1"}));
            assert!(
                super::bind_identity(tool, &mut task_spoof, None).is_err(),
                "{tool}"
            );
        }
        // A remote task may still write an approval or a handoff that names no session...
        for tool in [
            "wv_write_approval",
            "wv_report_handoff",
            "wv_check_t2_window",
        ] {
            let original = args(serde_json::json!({"id": 1}));
            let mut task = original.clone();
            super::bind_identity(tool, &mut task, None).unwrap();
            assert_eq!(task, original, "{tool}");
        }
        // ...but never claims a row it could not finish.
        let mut claim = args(serde_json::json!({"delegateRunId": "w1"}));
        assert!(super::bind_identity("wv_claim_queue", &mut claim, None).is_err());
        // No remote caller forges an approval's queue endorsement.
        for me in [ME, None] {
            let mut forged = args(serde_json::json!({"claim": "x", "source": "queue:7"}));
            assert!(super::bind_identity("wv_write_approval", &mut forged, me).is_err());
        }
        let mut plain = args(serde_json::json!({"claim": "x", "source": "slack"}));
        super::bind_identity("wv_write_approval", &mut plain, ME).unwrap();
    }

    /// Session-keyed tools inside a remote call accept only the caller's own session
    /// (live by its authority); outside one the local liveness check applies.
    #[tokio::test]
    async fn the_remote_session_check_admits_only_the_callers_own_session() {
        assert!(super::remote_session_check("remote-shell-me").is_none());
        super::as_remote_context(
            Some(("remote-shell-me".into(), "remote-shell-me".into())),
            None,
            async {
                assert_eq!(
                    super::remote_session_check(" remote-shell-me "),
                    Some(Ok(()))
                );
                assert!(matches!(super::remote_session_check("S1"), Some(Err(_))));
            },
        )
        .await;
        super::as_remote_context(None, None, async {
            assert!(matches!(super::remote_session_check("S1"), Some(Err(_))));
        })
        .await;
    }

    #[test]
    fn a_remote_agent_reaches_its_owners_terminals_in_any_team_and_local_tabs() {
        // Local tabs on this Mac are the owner's.
        assert!(super::check_owner(41, None).is_ok());
        assert!(super::check_owner(41, Some(41)).is_ok());
        assert!(super::check_owner(41, Some(42)).is_err());
    }
}
