// SPDX-License-Identifier: MIT
//! The one boundary between the app and its terminal hosts: the one
//! `codesense-pty-core` runtime, either in-process or in the session-server
//! sidecar that outlives the app. Both answer [`TerminalDomain`] — the
//! in-process [`TerminalHost`] directly, the sidecar through its
//! [`SessionServerClient`] — and nothing outside this module reaches either
//! host's state.
//!
//! [`resolve_terminal_backend`] is the single place that decides where a new
//! session goes: the sidecar, connected to or launched for this instance, so
//! the session outlives the app. Only when no sidecar can run — its binary
//! missing, its launch failing, or the one found refusing, hung, serving
//! another instance, or failing a spawn — is the runtime hosted in-process
//! (fail-open). The one exception is this instance's sidecar at another
//! protocol major (protocol 1 included): it still runs sessions the app cannot
//! reach, so new terminals are refused with "restart terminal server (ends N
//! sessions)" rather than silently opened in-process beside them, and the
//! frontend is told, so it can offer [`restart_terminal_server`].
//!
//! Each host keeps the layout of the sessions it hosts. A layout tab resolves
//! its host first and is spawned and placed there; a verb about a session goes
//! to the session's host, one about a workspace to every host holding it, and
//! the snapshot is every host's. A viewport stream is re-emitted under a
//! client-unique id ([`StreamIds`]) and detached in the host it lives in. A new
//! sidecar connection is carried across: the same sidecar lifetime gets its
//! viewports re-attached without a gap and its layout re-announced; a new one
//! ends the old one's sessions in the app and says so.
//!
//! A session belongs to the host that already has it: in-process when the
//! in-process registry has its id (the fallback host), else the sidecar — so a
//! respawn or warm handoff under the same id replaces it in that same host.
//! Only a spawn may launch the sidecar; a session verb, a read or the exit
//! reap reconnects to a sidecar that is still listening but never launches
//! one, and resize and kill use only a live connection, never waiting on a
//! reconnect. Reads that mean "anything alive" answer across both hosts.

#[cfg(test)]
mod rm4f_reservation_tests;
#[cfg(test)]
mod rm4f_support_tests;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use codesense_host_proto::{Skew, PROTO_MAJOR};
use codesense_pty_core::domain::{
    ClientBootId, PortUnavailable, PtySessionId, SubscribeResult, TeardownReason, TerminalDomain,
    TerminalDomainEvent, TerminalEventSink, TerminalText, ViewerId, ViewportId,
};
use codesense_pty_core::layout::{LayoutError, LayoutSnapshot, OpenTab, TabEntry};
use codesense_pty_core::runner::PtySession;
use codesense_pty_core::runner::PtySpec;
use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::{SessionRegistration, TerminalHost};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::agent_accounts::LaunchAccountProvenance;
use crate::commands::async_helpers::await_blocking;
use crate::commands::host_connection_manager::{
    ConnectedBackendError, ConnectedBackendSnapshot, HostConnectionManager,
};
use crate::commands::machines::LOCAL_MACHINE_ID;
use crate::commands::machines::{
    RuntimeAdmissionState, RuntimeLayoutReply, RuntimeMemberSlot, RuntimeMemberTarget,
    RuntimeNamespace,
};
use crate::commands::pty_host::{
    emit_stream, remote_replay_emitter, AppTerminalSink, AttachReservedError, StreamHost,
    StreamIds, StreamItem,
};
use crate::commands::pty_layout::app_viewer;
use crate::terminal::session_server::{
    launch_detached, terminate_sidecar, ConnectError, Continuity, Presence, RemoteSessionLedger,
    SessionServerClient, SessionServerState, SpawnDisposition, SpawnFailure, SIDECAR_BIN,
};

/// The frontend event naming an incompatible terminal server:
/// [`IncompatibleTerminalServer`].
pub const TERMINAL_SERVER_INCOMPATIBLE_EVENT: &str = "terminal-server-incompatible";

/// The frontend event saying the user tried to open a terminal and the
/// incompatible terminal server refused it: [`IncompatibleTerminalServer`]. It
/// re-offers the restart even after the user put it off.
pub const TERMINAL_SERVER_REFUSED_EVENT: &str = "terminal-server-refused";

/// The frontend event saying the terminal server restarted under the app:
/// [`TerminalServerRestarted`].
pub const TERMINAL_SERVER_RESTARTED_EVENT: &str = "terminal-server-restarted";

/// The terminal server restarted under the app, ending the `ended_sessions`
/// the app had open there (warm-pool shells not counted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalServerRestarted {
    pub ended_sessions: usize,
}

/// The frontend event saying a host's layout moved while the app could not
/// hear it (a sidecar reconnect): [`TerminalLayoutResync`]. The frontend
/// re-fetches `terminal_layout_snapshot`.
pub const TERMINAL_LAYOUT_RESYNC_EVENT: &str = "terminal-layout-resync";

/// A host's layout must be re-fetched: its lifetime `epoch` is the one now
/// serving (a new epoch means the one before is gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLayoutResync {
    pub epoch: String,
}

/// This instance's terminal server speaks another protocol major: new
/// terminals are refused until it is restarted, which ends its
/// `live_sessions` (`None` when its census predates the count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleTerminalServer {
    pub live_sessions: Option<u32>,
    pub server_protocol: u16,
    pub app_protocol: u16,
    pub reason: String,
}

impl From<&Skew> for IncompatibleTerminalServer {
    fn from(skew: &Skew) -> Self {
        Self {
            live_sessions: skew.live_sessions,
            server_protocol: skew.host_major(),
            app_protocol: PROTO_MAJOR,
            reason: skew.to_string(),
        }
    }
}

/// The in-process host, once managed.
fn in_process_host<R: Runtime>(app: &AppHandle<R>) -> Option<Arc<TerminalHost>> {
    app.try_state::<Arc<TerminalHost>>()
        .map(|host| host.inner().clone())
}

/// Whether startup has installed the in-process host dependency.
pub(crate) fn in_process_host_ready<R: Runtime>(app: &AppHandle<R>) -> bool {
    in_process_host(app).is_some()
}

/// SW-WA-F10: the in-process host (so a caller can reach its activity owner),
/// `None` before startup installs it.
pub(crate) fn in_process_host_handle<R: Runtime>(app: &AppHandle<R>) -> Option<Arc<TerminalHost>> {
    in_process_host(app)
}

/// The in-process host's identity (server id + epoch), once it is managed.
pub(crate) fn in_process_identity<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<codesense_pty_core::layout::HostIdentity, String> {
    in_process_host(app)
        .ok_or_else(|| "the in-process terminal host is not installed".to_string())?
        .identity()
        .map_err(|error| error.to_string())
}

/// Whether the in-process host is installed and positively reports that it
/// does not host `session` (an unreadable registry is not "absent").
pub(crate) fn in_process_lacks_session<R: Runtime>(app: &AppHandle<R>, session: &str) -> bool {
    in_process_host(app)
        .is_some_and(|host| host.has_session(&PtySessionId::new(session)) == Ok(false))
}

/// The local in-process backend resolved from the app's managed host.
pub(crate) fn managed_in_process_backend<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<ResolvedTerminalBackend, String> {
    local_in_process_backend(
        in_process_host(app)
            .ok_or_else(|| "the in-process terminal host is not installed".to_string())?,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalBackendResolveError {
    Local(String),
    LocalIncompatible(IncompatibleTerminalServer),
    ManagerShuttingDown,
    UnknownMachine,
    Disconnected,
    Connecting,
    Handshaking,
    Incompatible {
        machine_id: String,
        snapshot: Box<super::host_connection_manager::LastKnownHostSnapshot>,
    },
    UnsupportedLegacy {
        machine_id: String,
    },
    Unreachable {
        last_error: String,
    },
    IdentityMismatch,
}

impl std::fmt::Display for TerminalBackendResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(detail) => formatter.write_str(detail),
            Self::LocalIncompatible(incompatible) => formatter.write_str(&incompatible.reason),
            Self::ManagerShuttingDown => {
                formatter.write_str("host connection manager is shutting down")
            }
            Self::UnknownMachine => formatter.write_str("machine is not registered"),
            Self::Disconnected => formatter.write_str("machine is disconnected"),
            Self::Connecting => formatter.write_str("machine is connecting"),
            Self::Handshaking => formatter.write_str("machine is handshaking"),
            Self::Incompatible {
                machine_id,
                snapshot,
            } => write!(
                formatter,
                "Restart terminal server on {machine_id} (ends {} sessions)",
                snapshot.census.live_sessions
            ),
            Self::UnsupportedLegacy { machine_id } => write!(
                formatter,
                "Restart terminal server on {machine_id} (ends its running sessions)"
            ),
            Self::Unreachable { last_error } => {
                write!(formatter, "machine is unreachable: {last_error}")
            }
            Self::IdentityMismatch => {
                formatter.write_str("connected machine identity is inconsistent")
            }
        }
    }
}

impl std::error::Error for TerminalBackendResolveError {}

#[derive(Clone)]
enum TerminalBackendPolicy {
    LocalInProcess,
    LocalSidecar,
    RemoteAdmitted,
}

/// A sealed, identity-carrying terminal backend. Only this module constructs
/// values, so remote resolution can never collapse into the local fallback.
#[derive(Clone)]
pub struct ResolvedTerminalBackend {
    #[allow(
        dead_code,
        reason = "Retained resolved-backend identity contract; runtime validation currently uses remote_token"
    )]
    machine_id: String,
    #[allow(
        dead_code,
        reason = "Retained resolved-backend identity contract; runtime validation currently uses remote_token"
    )]
    server_id: String,
    #[allow(
        dead_code,
        reason = "Retained resolved-backend identity contract; runtime validation currently uses remote_token"
    )]
    incarnation: Option<super::host_connection_manager::ActorIncarnation>,
    domain: Arc<dyn TerminalDomain>,
    sidecar: Option<Arc<SessionServerClient>>,
    policy: TerminalBackendPolicy,
    remote_token: Option<ConnectedBackendSnapshot>,
    admission: Option<RuntimeMemberTarget>,
    spawn_predecessor: Option<super::pty_host::EffectPredecessor>,
}

impl ResolvedTerminalBackend {
    pub(crate) fn admission_target(&self) -> Option<&RuntimeMemberTarget> {
        self.admission.as_ref()
    }
    #[allow(
        dead_code,
        reason = "Retained resolved-backend identity accessor; currently no production caller"
    )]
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    #[allow(
        dead_code,
        reason = "Retained resolved-backend identity accessor; currently no production caller"
    )]
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn domain(&self) -> Arc<dyn TerminalDomain> {
        self.domain.clone()
    }

    pub(crate) fn is_remote(&self) -> bool {
        matches!(self.policy, TerminalBackendPolicy::RemoteAdmitted)
    }

    pub(crate) fn allows_local_fail_open(&self) -> bool {
        matches!(self.policy, TerminalBackendPolicy::LocalSidecar)
    }

    pub(crate) fn trusts_attribution_hooks(&self) -> bool {
        Self::client(self)
            .is_none_or(|client| SessionServerClient::supports_attribution_hook_trust(&client))
    }

    pub(crate) fn client(&self) -> Option<Arc<SessionServerClient>> {
        self.sidecar.clone()
    }

    pub(crate) fn revalidate<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        if let Some(target) = &self.admission {
            validate_member_target(app, target)?;
        }
        let Some(token) = &self.remote_token else {
            return Ok(());
        };
        app.try_state::<HostConnectionManager>()
            .ok_or_else(|| "host connection manager unavailable".to_string())?
            .revalidate_backend(token)
            .map_err(|error| map_connected_error(token.machine_id(), error).to_string())
    }
}

/// Resolve one registered machine. Only synthesized `local` may connect or
/// launch the local sidecar and fall open to the in-process host. Remote
/// machines snapshot an already admitted manager-owned client.
pub(crate) fn validate_member_target<R: Runtime>(
    app: &AppHandle<R>,
    target: &RuntimeMemberTarget,
) -> Result<(), String> {
    match target.member.slot {
        RuntimeMemberSlot::InProcess | RuntimeMemberSlot::Sidecar => {
            let state = app
                .try_state::<SessionServerState>()
                .ok_or("local runtime unavailable")?;
            if !state.admission.validate(target) {
                return Err("local runtime admission changed".into());
            }
        }
        RuntimeMemberSlot::Remote => {
            app.try_state::<HostConnectionManager>()
                .ok_or("remote runtime unavailable")?
                .capture_admitted_backend(target)
                .map_err(|error| {
                    map_connected_error(&target.member.machine_id, error).to_string()
                })?;
        }
    }
    Ok(())
}

/// Capture one existing runtime; this path never connects, launches or falls
/// back to another member when the selected runtime has gone away.
pub(crate) fn resolve_member_backend<R: Runtime>(
    app: &AppHandle<R>,
    target: &RuntimeMemberTarget,
    allow_remote: bool,
) -> Result<ResolvedTerminalBackend, String> {
    validate_member_target(app, target)?;
    let mut backend = match target.member.slot {
        RuntimeMemberSlot::InProcess => {
            local_in_process_backend(in_process_host(app).ok_or("in-process runtime unavailable")?)?
        }
        RuntimeMemberSlot::Sidecar => {
            let client = app
                .try_state::<SessionServerState>()
                .and_then(|state| state.connected())
                .ok_or("sidecar runtime unavailable")?;
            if crate::commands::machines::RuntimeNamespace::from(client.namespace())
                != target.namespace
            {
                return Err("sidecar runtime admission changed".into());
            }
            select_local_backend(Some(client), || {
                Err(TerminalBackendResolveError::Disconnected)
            })
            .map_err(|error| error.to_string())?
        }
        RuntimeMemberSlot::Remote if allow_remote => {
            let manager = app
                .try_state::<HostConnectionManager>()
                .ok_or("remote runtime unavailable")?;
            let snapshot = manager.capture_admitted_backend(target).map_err(|error| {
                map_connected_error(&target.member.machine_id, error).to_string()
            })?;
            let client = snapshot.client();
            ResolvedTerminalBackend {
                machine_id: snapshot.machine_id().into(),
                server_id: snapshot.server_id().into(),
                incarnation: Some(snapshot.incarnation()),
                domain: client.clone(),
                sidecar: Some(client),
                policy: TerminalBackendPolicy::RemoteAdmitted,
                remote_token: Some(snapshot),
                admission: None,
                spawn_predecessor: None,
            }
        }
        RuntimeMemberSlot::Remote => {
            return Err("this operation is available only on local runtime members".into())
        }
    };
    if backend.server_id != target.namespace.server_id {
        return Err("runtime server identity changed".into());
    }
    backend.admission = Some(target.clone());
    validate_member_target(app, target)?;
    Ok(backend)
}

pub(crate) fn resolve_session_reference<R: Runtime>(
    app: &AppHandle<R>,
    reference: &super::machines::RuntimeSessionReference,
    allow_remote: bool,
) -> Result<ResolvedTerminalBackend, String> {
    match reference {
        super::machines::RuntimeSessionReference::Admitted(target) => {
            resolve_member_backend(app, &target.runtime, allow_remote)
        }
        super::machines::RuntimeSessionReference::Legacy(session) => {
            let host = in_process_host(app).ok_or("in-process runtime unavailable")?;
            let client = app
                .try_state::<SessionServerState>()
                .and_then(|state| state.connected());
            let local = hosted_in_process(&host, session);
            let sidecar = match &client {
                Some(client) => client
                    .has_session(&PtySessionId::new(session))
                    .map_err(String::from)?,
                None => false,
            };
            let mut backend = match (local, sidecar) {
                (true, true) => {
                    Err("ambiguous local session: an exact runtime member is required".into())
                }
                (false, false) => Err("local session not found".into()),
                (true, false) => local_in_process_backend(host),
                (false, true) => {
                    select_local_backend(client, || Err(TerminalBackendResolveError::Disconnected))
                        .map_err(|error| error.to_string())
                }
            }?;
            let slot = if backend.sidecar.is_some() {
                RuntimeMemberSlot::Sidecar
            } else {
                RuntimeMemberSlot::InProcess
            };
            let row = app
                .try_state::<SessionServerState>()
                .ok_or("local runtime unavailable")?
                .admission
                .baseline()
                .rows
                .into_iter()
                .find(|row| row.member.slot == slot)
                .ok_or("local runtime is not admitted")?;
            let RuntimeAdmissionState::Live { namespace } = row.state else {
                return Err("local runtime is not live".into());
            };
            let target = RuntimeMemberTarget {
                member: row.member,
                admission_revision: row.revision,
                namespace,
            };
            if backend.server_id != target.namespace.server_id
                || backend.sidecar.as_ref().is_some_and(|client| {
                    RuntimeNamespace::from(client.namespace()) != target.namespace
                })
            {
                return Err("local runtime changed during session lookup".into());
            }
            validate_member_target(app, &target)?;
            backend.admission = Some(target);
            Ok(backend)
        }
    }
}

pub(crate) fn with_session_reference<T, R: Runtime>(
    app: &AppHandle<R>,
    reference: &super::machines::RuntimeSessionReference,
    allow_remote: bool,
    operation: impl FnOnce(&dyn TerminalDomain, &PtySessionId) -> Result<T, String>,
) -> Result<T, String> {
    let backend = resolve_session_reference(app, reference, allow_remote)?;
    backend.revalidate(app)?;
    let result = operation(&*backend.domain(), &PtySessionId::new(reference.session()))?;
    backend.revalidate(app)?;
    Ok(result)
}

/// The caption's prompt segment for `reference`: cwd, home and Git status,
/// each read on the host that runs the session — `git` runs next to the
/// checkout, on a remote Machine too. A remote terminal server that predates
/// the question still answers the cwd and home; its Git segment stays empty
/// until it is updated (never guessed from This Mac).
pub(crate) fn session_prompt<R: Runtime>(
    app: &AppHandle<R>,
    reference: &super::machines::RuntimeSessionReference,
) -> Result<codesense_pty_core::git_prompt::SessionPrompt, String> {
    use codesense_pty_core::git_prompt;
    let backend = resolve_session_reference(app, reference, true)?;
    backend.revalidate(app)?;
    let session = PtySessionId::new(reference.session());
    let prompt = match (&backend.sidecar, &backend.policy) {
        (Some(client), policy) => match client.session_prompt(&session) {
            Ok(Some(prompt)) => prompt,
            // A host that did not answer in time would likely sit out two
            // more questions too: report it rather than wait for them.
            Err(error) if crate::terminal::session_server::is_no_answer(&error) => {
                return Err(String::from(error));
            }
            // The host's prompt read failed (a slow `git status` on a big
            // checkout times out host-side): the caption still gets the cwd
            // and home, with no Git segment rather than nothing. A dropped
            // connection fails the cwd read below too.
            Err(error) => {
                log::debug!(target: "pty_backend", "session prompt unavailable: {}", String::from(error));
                let cwd = backend.domain.session_cwd(&session).map_err(String::from)?;
                git_prompt::SessionPrompt {
                    cwd,
                    home: if matches!(policy, TerminalBackendPolicy::RemoteAdmitted) {
                        client.account_home().ok()
                    } else {
                        dirs::home_dir()
                    },
                    git: None,
                }
            }
            Ok(None) => {
                let cwd = backend.domain.session_cwd(&session).map_err(String::from)?;
                if matches!(policy, TerminalBackendPolicy::RemoteAdmitted) {
                    git_prompt::SessionPrompt {
                        cwd,
                        home: client.account_home().ok(),
                        git: None,
                    }
                } else {
                    // An older local sidecar: its sessions are on This Mac.
                    git_prompt::session_prompt(cwd, dirs::home_dir())
                }
            }
        },
        (None, _) => {
            let cwd = backend.domain.session_cwd(&session).map_err(String::from)?;
            git_prompt::session_prompt(cwd, dirs::home_dir())
        }
    };
    backend.revalidate(app)?;
    Ok(prompt)
}

/// Off-reader registration capture. Unknown sidecar presence is not absence.
pub(crate) fn capture_local_effect_claim<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
) -> Result<super::pty_host::LocalEffectClaim, String> {
    let state = app
        .try_state::<SessionServerState>()
        .ok_or("local runtime unavailable")?;
    if matches!(state.presence(), Presence::Unknown(_)) {
        return Err("sidecar ownership is unknown".into());
    }
    let backend = resolve_session_reference(
        app,
        &super::machines::RuntimeSessionReference::Legacy(session.into()),
        false,
    )?;
    capture_resolved_local_effect_claim(app, session, backend)
}

pub(crate) fn capture_resolved_local_effect_claim<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
    backend: ResolvedTerminalBackend,
) -> Result<super::pty_host::LocalEffectClaim, String> {
    backend.revalidate(app)?;
    let target = backend
        .admission_target()
        .cloned()
        .ok_or("unadmitted local owner")?;
    app.state::<SessionServerState>()
        .admission
        .effects
        .reserve(target, session)
}

pub(crate) fn commit_local_effect_claim<T, R: Runtime>(
    app: &AppHandle<R>,
    claim: &super::pty_host::LocalEffectClaim,
    mutate: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    validate_member_target(app, &claim.target)?;
    app.state::<SessionServerState>()
        .admission
        .effects
        .commit(claim, mutate)
}

pub(crate) fn reserve_spawn_effect_claim<R: Runtime>(
    app: &tauri::AppHandle<R>,
    backend: &ResolvedTerminalBackend,
    session: &str,
    pool: bool,
) -> Result<super::pty_host::LocalEffectClaim, String> {
    let state = app.state::<SessionServerState>();
    let slot = if backend.client().is_some() {
        RuntimeMemberSlot::Sidecar
    } else {
        RuntimeMemberSlot::InProcess
    };
    let row = state
        .admission
        .baseline()
        .rows
        .into_iter()
        .find(|row| row.member.slot == slot)
        .ok_or("spawn member unavailable")?;
    let RuntimeAdmissionState::Live { namespace } = row.state else {
        return Err("spawn member not live".into());
    };
    let target = RuntimeMemberTarget {
        member: row.member,
        admission_revision: row.revision,
        namespace,
    };
    let exact = resolve_member_backend(app, &target, false)?;
    if exact.server_id != backend.server_id
        || match (&exact.sidecar, &backend.sidecar) {
            (Some(actual), Some(expected)) => !Arc::ptr_eq(actual, expected),
            (None, None) => false,
            _ => true,
        }
    {
        return Err("spawn member changed before claim reservation".into());
    }
    let (claim, prior) = state.admission.effects.reserve_spawn_with_predecessor(
        target,
        session,
        pool,
        std::time::Instant::now() + crate::terminal::session_server::EXIT_REAP_TIMEOUT,
        backend.spawn_predecessor.as_ref(),
    )?;
    // Waiting must not turn the captured connection into whatever is current.
    if let Err(error) = validate_member_target(app, &claim.target) {
        state.admission.effects.never_started(&claim);
        return Err(error);
    }
    if let Some(prior) = prior {
        // Respawn keeps the tab's binding/account intent, but never old asks or
        // old scope creation. The new claim is reserved before this move.
        state.admission.effects.commit(&claim, || {
            if let Some(launch) = app.try_state::<super::dashboard_launch::DashboardLaunchState>() {
                launch.transfer_owned(&prior, &claim);
            }
            if let Some(accounts) =
                app.try_state::<super::agent_accounts::AgentAccountRuntimeState>()
            {
                accounts.transfer_owned(&prior, &claim);
            }
            Ok(())
        })?;
    }
    Ok(claim)
}

pub fn resolve_terminal_backend<R: Runtime>(
    app: &AppHandle<R>,
    machine_id: &str,
) -> Result<ResolvedTerminalBackend, TerminalBackendResolveError> {
    if machine_id == LOCAL_MACHINE_ID {
        let sidecar = match app.try_state::<SessionServerState>() {
            Some(state) => match state.root() {
                Some(root) => local_sidecar_after(
                    app,
                    &state,
                    state.connect_or_launch(root, app_sink(app), |root| launch_sidecar(app, root)),
                )?,
                None => None,
            },
            None => None,
        };
        return select_local_backend(sidecar, || {
            in_process_host(app).ok_or_else(|| {
                TerminalBackendResolveError::Local("PTY registry unavailable".into())
            })
        });
    }

    let manager = app
        .try_state::<HostConnectionManager>()
        .ok_or(TerminalBackendResolveError::UnknownMachine)?;
    resolve_remote_backend(&manager, machine_id)
}

/// Remote selection is deliberately incapable of reaching any local host or
/// sidecar state: its only input is the manager's atomic admitted snapshot.
pub(super) fn resolve_remote_backend(
    manager: &HostConnectionManager,
    machine_id: &str,
) -> Result<ResolvedTerminalBackend, TerminalBackendResolveError> {
    let snapshot = manager
        .connected_backend(machine_id)
        .map_err(|error| map_connected_error(machine_id, error))?;
    let client = snapshot.client();
    // Native launch must retain the manager's admission revision for this exact
    // captured client. A replacement between reads must refuse, never retarget.
    let target = manager
        .target_for_namespace(client.namespace())
        .ok_or_else(|| map_connected_error(machine_id, ConnectedBackendError::IdentityMismatch))?;
    let admitted = manager
        .capture_admitted_backend(&target)
        .map_err(|error| map_connected_error(machine_id, error))?;
    if !Arc::ptr_eq(&client, &admitted.client()) {
        return Err(map_connected_error(
            machine_id,
            ConnectedBackendError::IdentityMismatch,
        ));
    }
    Ok(ResolvedTerminalBackend {
        machine_id: snapshot.machine_id().to_string(),
        server_id: snapshot.server_id().to_string(),
        incarnation: Some(snapshot.incarnation()),
        domain: client.clone(),
        sidecar: Some(client),
        policy: TerminalBackendPolicy::RemoteAdmitted,
        remote_token: Some(snapshot),
        admission: Some(target),
        spawn_predecessor: None,
    })
}

fn map_connected_error(
    machine_id: &str,
    error: ConnectedBackendError,
) -> TerminalBackendResolveError {
    if std::env::var_os("RM4C_INJECT").as_deref()
        == Some(std::ffi::OsStr::new("rm4c_bypass_frozen_mutation_guard"))
        && matches!(
            error,
            ConnectedBackendError::Incompatible { .. } | ConnectedBackendError::UnsupportedLegacy
        )
    {
        return TerminalBackendResolveError::UnknownMachine;
    }
    match error {
        ConnectedBackendError::ManagerShuttingDown => {
            TerminalBackendResolveError::ManagerShuttingDown
        }
        ConnectedBackendError::UnknownMachine => TerminalBackendResolveError::UnknownMachine,
        ConnectedBackendError::Disconnected => TerminalBackendResolveError::Disconnected,
        ConnectedBackendError::Connecting => TerminalBackendResolveError::Connecting,
        ConnectedBackendError::Handshaking => TerminalBackendResolveError::Handshaking,
        ConnectedBackendError::Incompatible { snapshot } => {
            TerminalBackendResolveError::Incompatible {
                machine_id: machine_id.to_string(),
                snapshot,
            }
        }
        ConnectedBackendError::UnsupportedLegacy => {
            TerminalBackendResolveError::UnsupportedLegacy {
                machine_id: machine_id.to_string(),
            }
        }
        ConnectedBackendError::Unreachable { last_error } => {
            TerminalBackendResolveError::Unreachable { last_error }
        }
        ConnectedBackendError::IdentityMismatch => TerminalBackendResolveError::IdentityMismatch,
    }
}

fn select_local_backend(
    sidecar: Option<Arc<SessionServerClient>>,
    in_process: impl FnOnce() -> Result<Arc<TerminalHost>, TerminalBackendResolveError>,
) -> Result<ResolvedTerminalBackend, TerminalBackendResolveError> {
    if let Some(client) = sidecar {
        return Ok(ResolvedTerminalBackend {
            machine_id: LOCAL_MACHINE_ID.into(),
            server_id: client.server_id().to_string(),
            incarnation: None,
            domain: client.clone(),
            sidecar: Some(client),
            policy: TerminalBackendPolicy::LocalSidecar,
            remote_token: None,
            admission: None,
            spawn_predecessor: None,
        });
    }
    let in_process = in_process()?;
    let server_id = in_process
        .identity()
        .map_err(|error| TerminalBackendResolveError::Local(error.to_string()))?
        .server
        .id
        .to_string();
    Ok(ResolvedTerminalBackend {
        machine_id: LOCAL_MACHINE_ID.into(),
        server_id,
        incarnation: None,
        domain: in_process,
        sidecar: None,
        policy: TerminalBackendPolicy::LocalInProcess,
        remote_token: None,
        admission: None,
        spawn_predecessor: None,
    })
}

/// This instance's terminal server as last found incompatible, if it was.
pub fn incompatible_terminal_server<R: Runtime>(
    app: &AppHandle<R>,
) -> Option<IncompatibleTerminalServer> {
    let skew = app.try_state::<SessionServerState>()?.incompatible()?;
    Some(IncompatibleTerminalServer::from(&skew))
}

/// A terminal the user asked for was refused by an incompatible terminal
/// server: tell the frontend (never for a warm-pool refill, which nobody
/// asked for).
pub fn refused_open<R: Runtime>(app: &AppHandle<R>) {
    if let Some(incompatible) = incompatible_terminal_server(app) {
        let _ = app.emit(TERMINAL_SERVER_REFUSED_EVENT, incompatible);
    }
}

/// Launch this build's sidecar for the instance rooted at `root`, handing it
/// the resources the app ships so its spawns resolve as in-process ones do, and
/// this build's emulator worker.
pub(crate) fn launch_sidecar<R: Runtime>(
    app: &AppHandle<R>,
    root: &std::path::Path,
) -> Result<(), String> {
    let binary = crate::commands::deps::resolve_sidecar_source(SIDECAR_BIN)?;
    let resources = managed_host_environment(app)
        .ok()
        .and_then(|environment| environment.resources().map(std::path::Path::to_path_buf));
    let emulator = emulator_worker().ok();
    launch_detached(&binary, root, resources.as_deref(), emulator.as_deref())
}

/// This build's emulator worker: beside the app binary once bundled; in a dev
/// build, the host artifact `scripts/build-emulator.sh` leaves in
/// `src-tauri/binaries`.
fn emulator_worker() -> Result<std::path::PathBuf, String> {
    use codesense_pty_core::emulator::WORKER_BIN;
    crate::commands::deps::resolve_sidecar_source(WORKER_BIN).or_else(|error| {
        let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("{WORKER_BIN}-{}", env!("TARGET_TRIPLE")));
        if cfg!(debug_assertions) && artifact.is_file() {
            Ok(artifact)
        } else {
            Err(error)
        }
    })
}

/// The in-process host's environment: this process's, with the resources
/// the app ships.
pub fn in_process_environment(app: &tauri::AppHandle) -> HostEnvironment {
    HostEnvironment::of_process(
        crate::commands::paths::codesense_home_dir(),
        crate::commands::paths::resolve_bundled_resource(app, "resources"),
    )
}

/// Start the in-process host's emulator worker on a thread of its own, so app
/// setup never waits for it. Without a worker the host keeps serving; the
/// reason is logged.
pub fn start_in_process_emulator(app: &tauri::AppHandle) {
    use codesense_pty_core::emulator::EmulatorConfig;
    let Some(host) = in_process_host(app) else {
        log::error!("in-process host runs without an emulator worker: no host");
        return;
    };
    let started = std::thread::Builder::new()
        .name("emulator-start".into())
        .spawn(move || {
            let config = emulator_worker().and_then(|worker| {
                let home = crate::commands::paths::codesense_home_dir()
                    .ok_or_else(|| "no home directory for the emulator runtime dir".to_string())?;
                Ok(EmulatorConfig::new(worker, home.join("emulator")))
            });
            if let Err(error) =
                config.and_then(|config| host.start_emulator(config).map_err(|e| e.to_string()))
            {
                log::error!("in-process host runs without an emulator worker: {error}");
            }
        });
    if let Err(error) = started {
        log::error!("in-process host runs without an emulator worker: {error}");
    }
}

/// The in-process host's environment, as managed at setup. Fetched before a
/// spawn consumes its one-use bindings, so its absence burns none.
pub fn managed_host_environment<R: Runtime>(app: &AppHandle<R>) -> Result<HostEnvironment, String> {
    app.try_state::<HostEnvironment>()
        .map(|environment| environment.inner().clone())
        .ok_or_else(|| "PTY host environment unavailable".to_string())
}

/// Resolve `intent` for session `session` as the in-process host runs it,
/// against `environment`, on the blocking pool (it writes the managed RC
/// wrappers).
pub async fn resolve_in_process(
    environment: HostEnvironment,
    session: &str,
    intent: SpawnIntent,
) -> Result<PtySpec, String> {
    let session = session.to_string();
    await_blocking("spawn_pty.resolve", move || {
        Ok(intent.resolve(&session, &environment))
    })
    .await
}

/// How long a restart may take — the old server's exit grace plus the new
/// one's startup — before the frontend is told it failed.
const RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a terminal-server restart failed: `message` (logged; the frontend shows
/// its own words), and whether the old server stopped — only the new one did
/// not start — so the frontend offers to try again instead of restating the
/// cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartError {
    pub old_stopped: bool,
    pub message: String,
}

impl RestartError {
    pub(crate) fn still_running(message: String) -> Self {
        Self {
            old_stopped: false,
            message,
        }
    }
}

/// End this instance's running terminal server — whatever protocol it speaks,
/// ending its sessions — and launch this build's in its place, within
/// [`RESTART_TIMEOUT`]. Returns how many sessions the ended server said it
/// ran, when it said.
pub fn restart_terminal_server(app: &tauri::AppHandle) -> Result<Option<u32>, RestartError> {
    let (done, outcome) = std::sync::mpsc::channel();
    let restarting = app.clone();
    std::thread::Builder::new()
        .name("terminal-server-restart".into())
        .spawn(move || {
            let _ = done.send(restart_now(&restarting));
        })
        .map_err(|e| RestartError::still_running(format!("start the restart: {e}")))?;
    let restarted = outcome.recv_timeout(RESTART_TIMEOUT).unwrap_or_else(|_| {
        Err(RestartError::still_running(format!(
            "the restart did not finish within {}s",
            RESTART_TIMEOUT.as_secs()
        )))
    });
    if let Err(error) = &restarted {
        log::error!(target: "pty", "terminal server restart failed (old stopped: {}): {}", error.old_stopped, error.message);
    }
    restarted
}

pub fn update_terminal_activity_server(
    app: &tauri::AppHandle,
    expected: &crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<(), RestartError> {
    let Some((state, root)) = app.try_state::<SessionServerState>().and_then(|state| {
        let root = state.root()?.to_path_buf();
        Some((state, root))
    }) else {
        return Err(RestartError::still_running(
            "The terminal server is not available".into(),
        ));
    };
    let (client, old_stopped) = state
        .update_activity_server(&root, expected, app_sink(app), terminate_sidecar, |root| {
            launch_sidecar(app, root)
        })
        .map_err(|failure| RestartError {
            old_stopped: failure.old_stopped,
            message: failure.error.to_string(),
        })?;
    backend_after(app, &state, Ok(client))
        .map(|_| ())
        .map_err(|message| RestartError {
            old_stopped,
            message,
        })
}

/// SW-AGR-19: replace This Mac's terminal server with this build's, after the
/// person confirmed ending `expected`'s sessions.
pub fn replace_local_terminal_server(
    app: &tauri::AppHandle,
    expected: &crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<(), RestartError> {
    replace_terminal_server_with(app, expected, app_sink(app), terminate_sidecar, |root| {
        launch_sidecar(app, root)
    })
}

/// [`replace_local_terminal_server`], with the sink, the end of the old
/// server and the launch of the new one given.
pub(crate) fn replace_terminal_server_with<R: Runtime>(
    app: &AppHandle<R>,
    expected: &crate::terminal::session_server::LocalActivityUpgrade,
    sink: Arc<dyn TerminalEventSink>,
    terminate: impl FnOnce(u32) -> Result<(), String>,
    launch: impl Fn(&std::path::Path) -> Result<(), String>,
) -> Result<(), RestartError> {
    let Some((state, root)) = app.try_state::<SessionServerState>().and_then(|state| {
        let root = SessionServerState::root(&state)?.to_path_buf();
        Some((state, root))
    }) else {
        return Err(RestartError::still_running(
            "The terminal server is not available".into(),
        ));
    };
    let (client, old_stopped) = state
        .replace_local_server(&root, expected, sink, terminate, launch)
        .map_err(|failure| RestartError {
            old_stopped: failure.old_stopped,
            message: failure.error.to_string(),
        })?;
    backend_after(app, &state, Ok(client))
        .map(|_| ())
        .map_err(|message| RestartError {
            old_stopped,
            message,
        })
}

/// SV-7: whether This Mac's terminal server for this instance runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalServerRun {
    /// It runs and this app is connected to it.
    Running,
    /// None runs (a terminal opened starts one).
    Stopped,
    /// One runs that this build cannot use until it is restarted.
    Incompatible,
    /// One runs but does not answer (yet).
    Unreachable,
    /// This app hosts its terminals itself: there is no server to manage.
    NotApplicable,
}

/// SV-7: This Mac's terminal server for this instance, as Settings and agents
/// see it. A count is `None` when it is unknown: the server does not run, or
/// it is older than the question (`detail` says so). `expected` is what a stop
/// or a restart binds to (its identity and running sessions, exactly as read);
/// `ends` is how many running sessions a stop or restart ends — `expected`'s
/// — `None` when unknown, never a guessed 0.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalServerOverview {
    pub state: TerminalServerRun,
    pub pid: Option<u32>,
    /// The CodeSense release it was built from, when it says.
    pub version: Option<String>,
    pub started_at_ms: Option<u64>,
    /// Its sessions, running or exited but kept.
    pub sessions: Option<u32>,
    pub running: Option<u32>,
    pub running_agents: Option<u32>,
    pub exited_kept: Option<u32>,
    /// The running sessions a stop or restart ends.
    pub ends: Option<u32>,
    /// App instances connected to it, this one included (by app, not
    /// connection).
    pub connected_instances: Option<u32>,
    /// Of them, the ones other than this app: their tabs end too on a stop.
    pub other_instances: Option<u32>,
    /// The sessions no app holds.
    pub unowned: Vec<codesense_pty_core::protocol::UnownedSession>,
    pub expected: Option<crate::terminal::session_server::LocalActivityUpgrade>,
    pub incompatible: Option<IncompatibleTerminalServer>,
    pub detail: Option<String>,
}

impl TerminalServerOverview {
    pub(crate) fn bare(state: TerminalServerRun, detail: Option<String>) -> Self {
        Self {
            state,
            pid: None,
            version: None,
            started_at_ms: None,
            sessions: None,
            running: None,
            running_agents: None,
            exited_kept: None,
            ends: None,
            connected_instances: None,
            other_instances: None,
            unowned: Vec::new(),
            expected: None,
            incompatible: None,
            detail,
        }
    }
}

/// SV-7: This Mac's terminal server for this instance, read without ever
/// launching one (the same way the background reconnect never does): a
/// server that runs but is not connected is connected to, never started.
pub fn terminal_server_overview<R: Runtime>(app: &AppHandle<R>) -> TerminalServerOverview {
    let Some(state) = app
        .try_state::<SessionServerState>()
        .filter(|state| SessionServerState::root(&state).is_some())
    else {
        return TerminalServerOverview::bare(
            TerminalServerRun::NotApplicable,
            Some("this app hosts its terminals itself".into()),
        );
    };
    let client = match state.presence() {
        Presence::Connected(client) => client,
        Presence::Absent => return TerminalServerOverview::bare(TerminalServerRun::Stopped, None),
        Presence::Unknown(why) => match reconnected_session_server(app) {
            Some(client) => client,
            None => {
                if let Some(skew) = state.incompatible() {
                    let mut overview = TerminalServerOverview::bare(
                        TerminalServerRun::Incompatible,
                        Some(skew.to_string()),
                    );
                    overview.pid = skew.host_pid;
                    overview.ends = skew.live_sessions;
                    overview.incompatible = Some(IncompatibleTerminalServer::from(&skew));
                    return overview;
                }
                // A stop on purpose is a stop even when the lost
                // connection's reconnect was mid-attempt above: ask again,
                // behind the attempts a launch holds, so a server that is
                // starting never reads Stopped and is never probed here.
                if state.user_stopped() && matches!(state.presence(), Presence::Absent) {
                    return TerminalServerOverview::bare(TerminalServerRun::Stopped, None);
                }
                return TerminalServerOverview::bare(TerminalServerRun::Unreachable, Some(why));
            }
        },
    };
    overview_of(&client)
}

/// SV-PBF-290: the terminal server's open-file budget, asked only when this app
/// is connected to it already (never connected to or started for this); `None`
/// otherwise, or from a server that predates the question.
pub fn session_server_nofile<R: Runtime>(
    app: &AppHandle<R>,
) -> Option<codesense_pty_core::nofile::Usage> {
    let state = Manager::try_state::<SessionServerState>(app)?;
    let Presence::Connected(client) = SessionServerState::presence(&state) else {
        return None;
    };
    match SessionServerClient::server_status(&client) {
        Ok(Some(status)) => status.nofile,
        Ok(None) | Err(_) => None,
    }
}

/// What the connected terminal server `client` says it holds.
fn overview_of(client: &SessionServerClient) -> TerminalServerOverview {
    let mut overview = TerminalServerOverview::bare(TerminalServerRun::Running, None);
    overview.pid = Some(client.pid());
    overview.version = client.build_stamp().map(|build| build.version.clone());
    overview.expected = client.live_session_ids().ok().map(|sessions| {
        let mut session_ids: Vec<String> = sessions
            .into_iter()
            .map(|session| session.as_str().to_string())
            .collect();
        session_ids.sort();
        crate::terminal::session_server::LocalActivityUpgrade {
            server_id: client.server_id().to_string(),
            epoch: client.epoch().to_string(),
            pid: client.pid(),
            session_ids,
        }
    });
    overview.ends = overview
        .expected
        .as_ref()
        .map(|expected| expected.session_ids.len() as u32);
    match client.server_status() {
        Ok(Some(status)) => {
            overview.started_at_ms = Some(status.started_at_ms);
            overview.sessions = Some(status.sessions);
            overview.running = Some(status.running);
            overview.running_agents = Some(status.running_agents);
            overview.exited_kept = Some(status.exited_kept);
            overview.connected_instances = Some(status.clients);
            overview.other_instances = Some(status.other_clients);
            overview.unowned = status.unowned;
        }
        Ok(None) => {
            overview.detail = Some(
                "this terminal server is older than this app: restart it to see what it holds"
                    .into(),
            );
        }
        Err(error) => overview.detail = Some(String::from(error)),
    }
    overview
}

/// SV-7: what a stop ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedTerminalServer {
    /// The running sessions it ended (as confirmed).
    pub ended: u32,
}

/// SV-7: stop This Mac's terminal server, after the person (or agent)
/// confirmed ending exactly `expected`'s sessions — the confirmed stop a
/// replacement uses, with nothing launched after it. Every session ends with
/// it: the app's tabs exit, and the next terminal opened starts a server
/// again (the warm pool does not, meanwhile).
pub fn stop_terminal_server(
    app: &tauri::AppHandle,
    expected: &crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<StoppedTerminalServer, RestartError> {
    stop_terminal_server_with(app, expected, app_sink(app), terminate_sidecar)
}

/// [`stop_terminal_server`], with the sink and the end of the server given.
pub(crate) fn stop_terminal_server_with<R: Runtime>(
    app: &AppHandle<R>,
    expected: &crate::terminal::session_server::LocalActivityUpgrade,
    sink: Arc<dyn TerminalEventSink>,
    terminate: impl FnOnce(u32) -> Result<(), String>,
) -> Result<StoppedTerminalServer, RestartError> {
    let Some((state, root)) = app.try_state::<SessionServerState>().and_then(|state| {
        let root = SessionServerState::root(&state)?.to_path_buf();
        Some((state, root))
    }) else {
        return Err(RestartError::still_running(
            "The terminal server is not available".into(),
        ));
    };
    // Before the end: the lost connection's own reconnect then finds it
    // stopped on purpose, and says nothing of a restart.
    state.mark_user_stopped(&expected.server_id, &expected.epoch);
    if let Err(failure) = state.stop_local_server(&root, expected, sink, terminate) {
        if !failure.old_stopped {
            state.clear_user_stopped();
        }
        return Err(RestartError {
            old_stopped: failure.old_stopped,
            message: failure.error.to_string(),
        });
    }
    end_lifetime(app, &state, false);
    Ok(StoppedTerminalServer {
        ended: expected.session_ids.len() as u32,
    })
}

/// SV-7: start This Mac's terminal server for this instance when none runs
/// (one that runs is only connected to), the way opening a terminal does.
pub fn start_terminal_server(app: &tauri::AppHandle) -> Result<TerminalServerOverview, String> {
    start_terminal_server_with(app, app_sink(app), |root| launch_sidecar(app, root))
}

/// [`start_terminal_server`], with the sink and the launch given.
pub(crate) fn start_terminal_server_with<R: Runtime>(
    app: &AppHandle<R>,
    sink: Arc<dyn TerminalEventSink>,
    launch: impl FnOnce(&std::path::Path) -> Result<(), String>,
) -> Result<TerminalServerOverview, String> {
    let Some((state, root)) = app.try_state::<SessionServerState>().and_then(|state| {
        let root = SessionServerState::root(&state)?.to_path_buf();
        Some((state, root))
    }) else {
        return Err("this app hosts its terminals itself".into());
    };
    state.clear_user_stopped();
    let connected = state.connect_or_launch(&root, sink, launch);
    match backend_after(app, &state, connected)? {
        Some(client) => Ok(overview_of(&client)),
        None => Err("the terminal server did not start".into()),
    }
}

/// SV-7: end `session`, a session no app holds. The server checks that as it
/// ends it: refused, and nothing ends, when an app holds it (again), or it is
/// gone.
pub fn end_unowned_terminal_session<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
) -> Result<(), String> {
    let state = app
        .try_state::<SessionServerState>()
        .ok_or("this app hosts its terminals itself")?;
    let client =
        SessionServerState::connected(&state).ok_or("the terminal server is not connected")?;
    client
        .end_unowned_session(&PtySessionId::new(session))
        .map_err(String::from)
}

/// SV-7: tell the terminal server which of its sessions this app holds —
/// the ones it opened and its background and headless runs, those restored
/// from before a relaunch included; never its layout entries, which every app
/// shares — so a relaunched app's sessions are never taken for a gone app's.
/// Only once the restore of this Mac's launches has settled (it fills the
/// launch rows), so they are claimed on this connection; the restore claims
/// as it settles. Once per connection (again on a new webview boot): until
/// the server answers a claim, each try tries again. A server older than the
/// question is not asked.
pub fn claim_held_sessions(app: &tauri::AppHandle, new_boot: bool) {
    let settled = app
        .try_state::<super::dashboard_launch::DashboardLaunchState>()
        .is_none_or(|launches| {
            launches.local_restore_state() == crate::routines::wake::Restore::Settled
        });
    claim_held_sessions_with(app, new_boot, settled, || {
        super::dashboard_launch::list_sessions_inner(app)
            .map(|rows| rows.into_iter().filter_map(|row| row.session_id).collect())
            .unwrap_or_default()
    });
}

/// [`claim_held_sessions`], with whether the launch restore settled and the
/// launch rows' sessions given.
pub(crate) fn claim_held_sessions_with<R: Runtime>(
    app: &AppHandle<R>,
    new_boot: bool,
    restore_settled: bool,
    launch_sessions: impl FnOnce() -> Vec<String>,
) {
    if !restore_settled {
        return;
    }
    let Some(state) = app.try_state::<SessionServerState>() else {
        return;
    };
    let Some(client) = SessionServerState::connected(&state) else {
        return;
    };
    let mut held = SessionServerState::opened_sessions(&state);
    held.extend(launch_sessions().into_iter().map(PtySessionId::new));
    if let Err(error) = client.claim_once(&held, new_boot) {
        log::debug!(target: "pty_backend", "claiming the app's sessions failed (tried again on the next layout read): {}", String::from(error));
    }
}

fn restart_now(app: &tauri::AppHandle) -> Result<Option<u32>, RestartError> {
    let Some((state, root)) = app.try_state::<SessionServerState>().and_then(|state| {
        let root = state.root()?.to_path_buf();
        Some((state, root))
    }) else {
        return Err(RestartError::still_running(
            "the terminal server is not available".to_string(),
        ));
    };
    match state.restart(&root, app_sink(app), terminate_sidecar, |root| {
        launch_sidecar(app, root)
    }) {
        Ok((client, ended)) => backend_after(app, &state, Ok(client))
            .map(|_| ended)
            .map_err(|message| RestartError {
                old_stopped: true,
                message,
            }),
        Err(failure) => {
            let old_stopped = failure.old_stopped;
            let message = match failure.error {
                // Still (or again) incompatible: the frontend hears of it.
                incompatible @ ConnectError::Incompatible(_) => {
                    backend_after(app, &state, Err(incompatible))
                        .err()
                        .unwrap_or_default()
                }
                other => other.to_string(),
            };
            Err(RestartError {
                old_stopped,
                message,
            })
        }
    }
}

/// The host a sidecar connection attempt leaves: the sidecar, carried across
/// from the connection before; in-process (`None`) for any failure
/// (fail-open); or, for an incompatible sidecar, an `Err` — and the frontend
/// is told, so it can offer the restart.
fn backend_after<R: Runtime>(
    app: &AppHandle<R>,
    state: &SessionServerState,
    connected: Result<Arc<SessionServerClient>, ConnectError>,
) -> Result<Option<Arc<SessionServerClient>>, String> {
    match connected {
        Ok(client) => {
            carry_across(app, state, &client);
            super::terminal_server_tray::terminal_server_changed(app);
            Ok(Some(client))
        }
        Err(ConnectError::Incompatible(skew)) => {
            log::error!(target: "pty", "session server incompatible: {skew}");
            super::terminal_server_tray::terminal_server_changed(app);
            let _ = app.emit(
                TERMINAL_SERVER_INCOMPATIBLE_EVENT,
                IncompatibleTerminalServer::from(&skew),
            );
            Err(skew.to_string())
        }
        Err(error) => {
            log::warn!(target: "pty", "session server unavailable, hosting PTYs in-process: {error}");
            Ok(None)
        }
    }
}

fn local_sidecar_after<R: Runtime>(
    app: &AppHandle<R>,
    state: &SessionServerState,
    connected: Result<Arc<SessionServerClient>, ConnectError>,
) -> Result<Option<Arc<SessionServerClient>>, TerminalBackendResolveError> {
    match connected {
        Ok(client) => {
            carry_across(app, state, &client);
            Ok(Some(client))
        }
        Err(ConnectError::Incompatible(skew)) => {
            let incompatible = IncompatibleTerminalServer::from(&skew);
            log::error!(target: "pty", "session server incompatible: {skew}");
            let _ = app.emit(TERMINAL_SERVER_INCOMPATIBLE_EVENT, incompatible.clone());
            Err(TerminalBackendResolveError::LocalIncompatible(incompatible))
        }
        Err(error) => {
            log::warn!(target: "pty", "session server unavailable, hosting PTYs in-process: {error}");
            Ok(None)
        }
    }
}

/// Resolve the host a saved Agent will actually use, including a sidecar not yet connected.
/// Call off the async reactor, as resolving a spawn can connect or launch the sidecar.
pub(crate) fn attribution_hook_trust_for_spawn<R: Runtime>(
    app: &AppHandle<R>,
    runtime: codesense_pty_core::agent_cli::AgentCli,
    session: &str,
) -> Result<bool, String> {
    if !codesense_pty_core::agent_cli::descriptor(runtime).attribution_hook_trust {
        return Ok(true);
    }
    let backend = resolve_spawn_backend(app, LOCAL_MACHINE_ID, session)?;
    Ok(ResolvedTerminalBackend::trusts_attribution_hooks(&backend))
}

/// The sidecar that should host a new session `session`, if any: the resolved
/// backend, unless the in-process registry already has that id. An
/// incompatible sidecar is an `Err`: the session opens nowhere.
pub fn resolve_spawn_backend<R: Runtime>(
    app: &AppHandle<R>,
    machine_id: &str,
    session: &str,
) -> Result<ResolvedTerminalBackend, String> {
    if machine_id != LOCAL_MACHINE_ID {
        return resolve_terminal_backend(app, machine_id).map_err(|error| error.to_string());
    }
    let in_process = in_process_host(app).ok_or_else(|| "PTY registry unavailable".to_string())?;
    if hosted_in_process(&in_process, session) {
        return local_in_process_backend(in_process);
    }
    resolve_terminal_backend(app, LOCAL_MACHINE_ID).map_err(|error| error.to_string())
}

pub(crate) fn local_in_process_backend(
    in_process: Arc<TerminalHost>,
) -> Result<ResolvedTerminalBackend, String> {
    let server_id = in_process
        .identity()
        .map_err(|error| error.to_string())?
        .server
        .id
        .to_string();
    Ok(ResolvedTerminalBackend {
        machine_id: LOCAL_MACHINE_ID.into(),
        server_id,
        incarnation: None,
        domain: in_process,
        sidecar: None,
        policy: TerminalBackendPolicy::LocalInProcess,
        remote_token: None,
        admission: None,
        spawn_predecessor: None,
    })
}

/// Register a session the in-process driver just spawned (the fail-open
/// spawn). Synchronous and potentially blocking — call it off an async worker.
pub fn register_in_process(
    app: &tauri::AppHandle,
    pty: PtySession,
    registration: SessionRegistration,
) -> Result<(), String> {
    let Some(host) = in_process_host(app) else {
        pty.discard();
        return Err("PTY registry unavailable".to_string());
    };
    host.register(pty, registration)
}

/// Which host a spawn lands in.
#[derive(Clone)]
pub(crate) enum SpawnPlacement {
    ResolveLocal,
    Exact(Box<ResolvedTerminalBackend>),
}

impl SpawnPlacement {
    pub(crate) fn remote_backend(&self) -> Option<&ResolvedTerminalBackend> {
        match self {
            Self::Exact(backend) if backend.is_remote() => Some(backend),
            _ => None,
        }
    }
}

/// The host a warm handoff of pool `member` into `session` runs in: the
/// member's, and only when `session` belongs to that same host — and, for a
/// pinned spawn, only when it is the pinned host — since a handoff replaces a
/// same-id session only within one registry. `None` sends the caller to a
/// cold spawn.
pub fn resolve_handoff_backend(
    app: &tauri::AppHandle,
    member: &str,
    session: &str,
    spawn: &ResolvedTerminalBackend,
) -> Option<Arc<dyn TerminalDomain>> {
    if spawn.is_remote() {
        return None;
    }
    let in_process = in_process_host(app)?;
    let owner = |id: &str| sidecar_owner(&in_process, id, || reconnected_session_server(app));
    let sidecar = match (owner(member), owner(session)) {
        (None, None) => None,
        (Some(member_host), Some(_)) => Some(member_host),
        _ => return None,
    };
    let admitted = match (spawn.client(), sidecar.as_ref()) {
        (Some(pinned), Some(host)) => pinned.epoch() == host.epoch(),
        (None, None) => true,
        _ => false,
    };
    if !admitted {
        return None;
    }
    match sidecar {
        Some(client) => Some(client),
        None => {
            hosting_in_process(app, session);
            Some(in_process)
        }
    }
}

/// Compensate only the popped member whose original owner is still provable.
/// The permit blocks same-ID reuse without holding an owner lock over kill.
pub(crate) fn cleanup_popped_warm<R: Runtime>(
    app: &AppHandle<R>,
    domain: &Arc<dyn TerminalDomain>,
    source: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    let exact = resolve_member_backend(app, &source.target, false)?;
    if !Arc::ptr_eq(domain, &exact.domain()) {
        return Err("popped warm member changed; original owner retained".into());
    }
    let state = app.state::<SessionServerState>();
    let _permit = state
        .admission
        .effects
        .legacy_permit(source)
        .ok_or("popped warm claim changed; original owner retained")?;
    exact.revalidate(app)?;
    domain
        .kill(&PtySessionId::new(&source.session))
        .map_err(String::from)
}

pub(crate) fn prepare_popped_warm<R: Runtime>(
    app: &AppHandle<R>,
    domain: &Arc<dyn TerminalDomain>,
    member: &str,
    reserve: impl FnOnce() -> Result<super::pty_host::LocalEffectClaim, String>,
    refill: impl FnOnce(),
) -> Result<
    (
        super::pty_host::LocalEffectClaim,
        super::pty_host::LocalEffectClaim,
    ),
    String,
> {
    let source = match capture_local_effect_claim(app, member) {
        Ok(source) => source,
        Err(error) => {
            refill();
            return Err(format!(
                "popped warm owner unresolved and retained: {error}"
            ));
        }
    };
    let prepared = resolve_member_backend(app, &source.target, false).and_then(|exact| {
        if !Arc::ptr_eq(domain, &exact.domain()) {
            Err("popped warm source backend changed".into())
        } else if source.pool_member {
            reserve()
        } else {
            Err("warm handoff lacks its original pool ownership".into())
        }
    });
    match prepared {
        Ok(target) => Ok((source, target)),
        Err(error) => {
            let cleanup = cleanup_popped_warm(app, domain, &source);
            refill();
            Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; {cleanup}"),
            })
        }
    }
}

/// The host that owns `session` for a resize or kill: like
/// [`resolve_session_backend`], but using only a live sidecar connection, so
/// a call from the command thread never waits on a reconnect.
pub fn resolve_live_session_backend(
    app: &tauri::AppHandle,
    session: &str,
) -> Result<Arc<dyn TerminalDomain>, String> {
    route(app, session, |app| {
        app.try_state::<SessionServerState>()?.connected()
    })
}

/// The app is about to host `session` in-process: any sidecar events for that
/// id are stale from now on.
pub fn hosting_in_process(app: &tauri::AppHandle, session: &str) {
    if let Some(state) = app.try_state::<SessionServerState>() {
        state.withdraw(session);
    }
}

pub(crate) fn respawn_tab_admitted<R: Runtime>(
    app: &AppHandle<R>,
    backend: ResolvedTerminalBackend,
    session: &PtySessionId,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
) -> Result<TabEntry, String> {
    respawn_tab_core(
        backend,
        session,
        |backend| backend.revalidate(app),
        |mut backend| {
            if !backend.is_remote() {
                let target = backend
                    .admission_target()
                    .ok_or("respawn member is unadmitted")?;
                let prior = app
                    .state::<SessionServerState>()
                    .admission
                    .effects
                    .drained_spawn_predecessor(
                        target,
                        session.as_str(),
                        std::time::Instant::now()
                            + crate::terminal::session_server::EXIT_REAP_TIMEOUT,
                    )?;
                validate_member_target(app, target)?;
                // Resource identity is checked and deletion serialized by its owner.
                // Unknown/pre-existing directories are deliberately not adopted.
                if super::mcp_scope::owned_creation(&prior.owner)?.is_some()
                    && !super::mcp_scope::remove_owned_scope(&prior.owner)
                {
                    return Err("respawn could not remove its exact predecessor scope".into());
                }
                backend.spawn_predecessor = Some(prior);
            }
            spawn(backend)
        },
    )
}

pub(crate) fn respawn_tab_core(
    backend: ResolvedTerminalBackend,
    session: &PtySessionId,
    revalidate: impl Fn(&ResolvedTerminalBackend) -> Result<(), String>,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
) -> Result<TabEntry, String> {
    revalidate(&backend)?;
    let domain = backend.domain();
    let result = domain
        .respawn_tab(
            session,
            Box::new(|| {
                revalidate(&backend)?;
                spawn(backend.clone())
            }),
        )
        .map_err(|error| error.to_string())?;
    revalidate(&backend)?;
    Ok(result)
}

/// A respawn in place on `host`: the host reaps the ended session first
/// (answering only once that is complete, the sidecar included); then the old
/// session's scoped MCP config is removed — the reap leaves it, so nothing can
/// remove it later — before `spawn` projects a fresh one and starts the new
/// process under the same id.
#[cfg(test)]
pub(crate) fn respawn_with_fresh_scope(
    host: &dyn TerminalDomain,
    session: &PtySessionId,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<TabEntry, String> {
    host.respawn_tab(
        session,
        Box::new(|| {
            crate::commands::mcp_scope::remove_scope_config(session.as_str());
            spawn()
        }),
    )
    .map_err(|e| e.to_string())
}

/// The host that owns `session`. Never launches the sidecar.
pub fn resolve_session_backend<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
) -> Result<Arc<dyn TerminalDomain>, String> {
    route(app, session, reconnected_session_server)
}

/// `session`'s owner under `session_server`'s lookup of the sidecar.
fn route<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
    session_server: impl FnOnce(&AppHandle<R>) -> Option<Arc<SessionServerClient>>,
) -> Result<Arc<dyn TerminalDomain>, String> {
    let _ = session_server(app);
    resolve_session_reference(
        app,
        &super::machines::RuntimeSessionReference::Legacy(session.into()),
        false,
    )
    .map(|backend| backend.domain())
}

/// The exit-time complement kill, run by each host that holds sessions: the
/// sidecar keeps its persisting sessions running after the app is gone, and
/// every session this app did not open (bar idle warm-pool shells).
pub fn reap_non_persisting(app: &tauri::AppHandle) {
    let in_process = in_process_host(app);
    reap_hosts(
        reconnected_session_server(app).as_deref(),
        in_process.as_deref(),
    );
}

/// Whether the in-process registry has `session` (membership). The host's
/// own read never fails.
fn hosted_in_process(in_process: &TerminalHost, session: &str) -> bool {
    in_process.has_session(&PtySessionId::new(session)) == Ok(true)
}

/// The sidecar, when it owns `session`: the in-process registry does not have
/// it and there is a sidecar. The sidecar is only looked up for a session the
/// in-process host does not own.
fn sidecar_owner(
    in_process: &TerminalHost,
    session: &str,
    session_server: impl FnOnce() -> Option<Arc<SessionServerClient>>,
) -> Option<Arc<SessionServerClient>> {
    if hosted_in_process(in_process, session) {
        return None;
    }
    session_server()
}

/// Where a viewport attaches: `None` for the in-process host, `Some` for the
/// sidecar. A session the in-process host does not hold goes to the sidecar;
/// when the sidecar can't be reached but may hold sessions, the attach fails
/// with that (a transport failure the pane offers Retry for), rather than
/// asking the in-process host, whose "session not found" would read as an
/// ended session while the sidecar's is still alive.
#[cfg(test)]
fn attach_owner<C>(
    hosted_in_process: bool,
    reconnected: impl FnOnce() -> Option<C>,
    presence: impl FnOnce() -> Result<Option<C>, PortUnavailable>,
) -> Result<Option<C>, PortUnavailable> {
    if hosted_in_process {
        return Ok(None);
    }
    if let Some(client) = reconnected() {
        return Ok(Some(client));
    }
    presence()
}

/// The ownership rule: `session` stays in-process when the in-process registry
/// has it, else goes to the sidecar when there is one.
#[cfg(test)]
fn route_session(
    in_process: &Arc<TerminalHost>,
    session: &str,
    session_server: impl FnOnce() -> Option<Arc<SessionServerClient>>,
) -> Arc<dyn TerminalDomain> {
    match sidecar_owner(in_process, session, session_server) {
        Some(client) => client,
        None => in_process.clone(),
    }
}

fn reap_hosts(session_server: Option<&SessionServerClient>, in_process: Option<&TerminalHost>) {
    for host in [
        session_server.map(|client| client as &dyn TerminalDomain),
        in_process.map(|host| host as &dyn TerminalDomain),
    ]
    .into_iter()
    .flatten()
    {
        if let Err(error) = host.reap_non_persisting() {
            log::warn!(target: "terminal::persistence", "exit reap failed: {error}");
        }
    }
}

/// The sidecar connection of an instance that has used one, reconnected if it
/// was lost — and carried across from the connection before.
fn reconnected_session_server<R: Runtime>(app: &AppHandle<R>) -> Option<Arc<SessionServerClient>> {
    let state = app.try_state::<SessionServerState>()?;
    let client = match state.connected() {
        Some(client) => client,
        None => state.reconnected(state.root()?)?,
    };
    carry_across(app, &state, &client);
    Some(client)
}

/// This instance's sidecar as a read that must not guess sees it, never
/// waiting: the live connection (carried across from the one before), `None`
/// when no sidecar runs, or `Err` while one may hold sessions but cannot be
/// asked — a reconnect then starts in the background.
fn sidecar_presence<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<Arc<SessionServerClient>>, PortUnavailable> {
    let Some(state) = app.try_state::<SessionServerState>() else {
        return Ok(None);
    };
    match state.presence() {
        Presence::Connected(client) => {
            carry_across(app, &state, &client);
            Ok(Some(client))
        }
        Presence::Absent => Ok(None),
        Presence::Unknown(why) => {
            reconnect_in_background(app);
            Err(PortUnavailable(why))
        }
    }
}

/// Reconnect in the background whenever the sidecar connection ends, so the
/// panes viewing its sessions never freeze silently.
pub fn reconnect_on_loss<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<SessionServerState>() {
        let app = app.clone();
        state.on_lost(Arc::new(move || reconnect_in_background(&app)));
    }
}

/// The pause after a background reconnect's first failed try; it doubles
/// after each further one, up to [`RECONNECT_PAUSE_MAX`].
const RECONNECT_PAUSE: std::time::Duration = std::time::Duration::from_secs(5);

/// The longest pause between a background reconnect's tries.
const RECONNECT_PAUSE_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// Reconnect to this instance's sidecar off the caller's thread — after a
/// lost connection, or while one runs unconnected — so the panes viewing its
/// sessions resume (their streams carried across without a gap) and reads
/// about them become known again. One runs at a time. It stops once
/// connected, or once no sidecar runs any more (its sessions then end in the
/// app). Any other failure — a sidecar that does not answer, one that is
/// incompatible (the frontend is told, so it can offer the restart), or a
/// socket something else serves — is tried again, with a growing pause, for
/// as long as it takes, so a sidecar that comes back is reconnected on its
/// own. Never launches a sidecar.
pub fn reconnect_in_background<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<SessionServerState>() else {
        return;
    };
    if !state.begin_reconnect() {
        return;
    }
    let app = app.clone();
    let started = std::thread::Builder::new()
        .name("pty-server-reconnect".into())
        .spawn(move || {
            // SV-8: the connection is lost: the menu-bar item says so, and
            // again once the reconnect settled.
            super::terminal_server_tray::terminal_server_changed(&app);
            let connected = reconnect_until_settled(&app);
            super::terminal_server_tray::terminal_server_changed(&app);
            let Some(state) = app.try_state::<SessionServerState>() else {
                return;
            };
            state.end_reconnect();
            // A loss while this reconnect was finishing found it still running
            // and was dropped: catch it now.
            if connected && state.connected().is_none() {
                reconnect_in_background(&app);
            }
        });
    if let Err(error) = started {
        log::warn!(target: "pty", "session server reconnect thread failed: {error}");
        state.end_reconnect();
    }
}

/// Reconnect until settled; whether it ended connected.
fn reconnect_until_settled<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(state) = app.try_state::<SessionServerState>() else {
        return false;
    };
    let mut announced = None;
    retry_until_settled(
        || match state.presence() {
            Presence::Connected(client) => {
                carry_across(app, &state, &client);
                Some(true)
            }
            Presence::Absent => {
                end_lost_lifetime(app, &state);
                Some(false)
            }
            Presence::Unknown(_) => {
                if reconnected_session_server(app).is_some() {
                    return Some(true);
                }
                // An incompatible sidecar is announced once, as a connect
                // announces it, so the restart prompt appears; trying goes
                // on, so one that becomes compatible reconnects on its own.
                if let Some(skew) = newly_incompatible(state.incompatible(), &mut announced) {
                    let _ = backend_after(app, &state, Err(ConnectError::Incompatible(skew)));
                }
                None
            }
        },
        std::thread::sleep,
    )
}

/// The incompatibility `found` when it is not the one already `announced`
/// (which follows `found`): another reason or another host process. A
/// changed session count alone is the same incompatibility.
fn newly_incompatible(found: Option<Skew>, announced: &mut Option<Skew>) -> Option<Skew> {
    let same = |a: &Skew, b: &Skew| a.reason == b.reason && a.host_pid == b.host_pid;
    let new = found
        .as_ref()
        .filter(|skew| !announced.as_ref().is_some_and(|before| same(before, skew)))
        .cloned();
    *announced = found;
    new
}

/// Run `attempt` until it settles (`Some`, its answer: connected, or no
/// sidecar runs), pausing between tries with a backoff that doubles from
/// [`RECONNECT_PAUSE`] up to [`RECONNECT_PAUSE_MAX`]. There is no try limit:
/// an unsettled attempt is tried again.
fn retry_until_settled(
    mut attempt: impl FnMut() -> Option<bool>,
    mut pause: impl FnMut(std::time::Duration),
) -> bool {
    let mut wait = RECONNECT_PAUSE;
    loop {
        if let Some(settled) = attempt() {
            return settled;
        }
        pause(wait);
        wait = (wait * 2).min(RECONNECT_PAUSE_MAX);
    }
}

/// No sidecar runs any more: every session the app hosted in the one it had
/// ended with it, so they end in the app — their tabs exit and their
/// bookkeeping is released — the layout is re-fetched, and the frontend is
/// told how many open sessions ended.
fn end_lost_lifetime<R: Runtime>(app: &AppHandle<R>, state: &SessionServerState) {
    // A server stopped on purpose is no surprise: its end goes unannounced.
    end_lifetime(app, state, !state.lost_server_was_stopped());
}

/// [`end_lost_lifetime`]; `announce` tells the frontend how many open
/// sessions ended (a stop the person asked for reports that itself).
fn end_lifetime<R: Runtime>(app: &AppHandle<R>, state: &SessionServerState, announce: bool) {
    // SV-8: the menu-bar item (when it shows) reads the server again.
    super::terminal_server_tray::terminal_server_changed(app);
    let (Ok(streams), Some(sink)) = (stream_ids(app), state.sink()) else {
        return;
    };
    let ended = state.take_lost();
    if ended.is_empty() {
        return;
    }
    streams.forget_host(StreamHost::Sidecar);
    for host in streams.local_sidecar_hosts() {
        streams.forget_host(host);
    }
    // New terminals are hosted in-process until a sidecar runs again.
    let epoch = in_process_host(app)
        .and_then(|host| host.identity().ok())
        .map(|identity| identity.epoch)
        .unwrap_or_default();
    if announce {
        announce_ended(app, &*sink, ended, &epoch);
    } else {
        end_sessions(&*sink, ended);
        resync_layout(app, &epoch);
    }
}

/// The app's one terminal event sink, for the sidecar's events.
pub(crate) fn app_sink<R: Runtime>(app: &AppHandle<R>) -> Arc<dyn TerminalEventSink> {
    Arc::new(AppTerminalSink::new(app.clone(), StreamHost::Sidecar))
}

/// A cross-host read's answer: `Known` when every host that may hold the
/// app's sessions answered, `Unknown` while one of them cannot be asked — the
/// sidecar unreachable, hung, incompatible, or not connected yet. Unknown is
/// never empty: a caller that would reap, tear down or re-dispatch on "not
/// running" does nothing on `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness<T> {
    Known(T),
    Unknown,
}

impl<T> Liveness<T> {
    /// The known answer, if there is one.
    pub fn known(self) -> Option<T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }

    /// The known answer, or an error saying the sessions are unknown — for a
    /// command whose caller must not read "unknown" as "none".
    pub fn known_or_err(self) -> Result<T, String> {
        self.known().ok_or_else(|| {
            "the terminal server isn't answering, so its sessions are unknown — try again"
                .to_string()
        })
    }

    fn map<U>(self, f: impl FnOnce(T) -> U) -> Liveness<U> {
        match self {
            Self::Known(value) => Liveness::Known(f(value)),
            Self::Unknown => Liveness::Unknown,
        }
    }
}

impl Liveness<bool> {
    /// Whether it may be so: known so, or unknown. Only a known "no" rules it
    /// out — for a read that must not act on a guess that something is gone.
    pub fn may_be_true(&self) -> bool {
        *self != Self::Known(false)
    }
}

impl Liveness<HashSet<String>> {
    /// Whether `session` is known to be running.
    pub fn known_alive(&self, session: &str) -> bool {
        matches!(self, Self::Known(alive) if alive.contains(session))
    }

    /// Whether `session` is known NOT to be running: every host answered and
    /// none runs it.
    pub fn known_gone(&self, session: &str) -> bool {
        matches!(self, Self::Known(alive) if !alive.contains(session))
    }
}

/// Every host the app's sessions may live in: the in-process host, and this
/// instance's sidecar — `Ok(None)` when none runs, `Err` while one may hold
/// sessions but cannot be asked. A read that must not guess is then
/// [`Liveness::Unknown`].
struct Hosts {
    in_process: Arc<TerminalHost>,
    sidecar: Result<Option<Arc<SessionServerClient>>, PortUnavailable>,
}

impl Hosts {
    /// The hosts as they are now, never waiting on the sidecar: a connection
    /// being made, or a sidecar running unconnected, is unknown (and a
    /// reconnect starts in the background).
    fn of<R: Runtime>(app: &AppHandle<R>) -> Option<Self> {
        Some(Self {
            in_process: in_process_host(app)?,
            sidecar: sidecar_presence(app),
        })
    }

    /// The hosts once a connection being made to the sidecar settles: for a
    /// read that may wait (off every hot path), such as the layout snapshot a
    /// launching or reloading webview restores its terminals from.
    fn settled<R: Runtime>(app: &AppHandle<R>) -> Option<Self> {
        let mut hosts = Self::of(app)?;
        if hosts.sidecar.is_err() {
            if let Some(client) = reconnected_session_server(app) {
                hosts.sidecar = Ok(Some(client));
            }
        }
        Some(hosts)
    }

    /// Each host's answer to `read`; an unreachable sidecar's is its `Err`.
    fn each<T>(
        &self,
        read: impl Fn(&dyn TerminalDomain) -> Result<T, PortUnavailable>,
    ) -> Vec<Result<T, PortUnavailable>> {
        let mut answers = vec![read(&*self.in_process)];
        match &self.sidecar {
            Ok(Some(client)) => answers.push(read(&**client)),
            Ok(None) => {}
            Err(unreachable) => answers.push(Err(unreachable.clone())),
        }
        answers
    }

    /// Every host's answer to `read`, or `Unknown` — logged — when one that
    /// may hold sessions cannot answer.
    fn known<T>(
        &self,
        read: impl Fn(&dyn TerminalDomain) -> Result<T, PortUnavailable>,
    ) -> Liveness<Vec<T>> {
        self.each(read)
            .into_iter()
            .collect::<Result<Vec<T>, PortUnavailable>>()
            .map_or_else(
                |error| {
                    log::warn!(target: "pty", "terminal host read failed: {error}");
                    Liveness::Unknown
                },
                Liveness::Known,
            )
    }

    /// Whether any host tracks `session`: known once a host has it, or every
    /// host answered.
    fn has_session(&self, session: &PtySessionId) -> Liveness<bool> {
        if hosted_in_process(&self.in_process, session.as_str()) {
            return Liveness::Known(true);
        }
        self.known(|host| host.has_session(session))
            .map(|answers| answers.into_iter().any(|has| has))
    }

    /// Which of `sessions` have a running child, across the hosts.
    fn alive_session_ids(&self, sessions: &[PtySessionId]) -> Liveness<HashSet<String>> {
        self.known(|host| host.alive_session_ids(sessions))
            .map(|answers| {
                answers
                    .into_iter()
                    .flatten()
                    .map(|id| id.as_str().to_string())
                    .collect()
            })
    }

    /// SW-AV-1 (INV-3): every alive, non-pool session id across the hosts — the
    /// live-PTY arm of the union roster, registry-less bare sessions included.
    /// The IN-PROCESS host's error is the
    /// caller's — a poisoned registry must SURFACE, never silently collapse the
    /// whole arm to empty (INV-3) — while a sidecar that cannot answer is logged and
    /// contributes nothing this pass.
    fn live_session_ids(&self) -> Result<HashSet<String>, String> {
        let mut ids: HashSet<String> = self
            .in_process
            .live_session_ids()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|id| id.as_str().to_string())
            .collect();
        if let Ok(Some(client)) = &self.sidecar {
            match client.live_session_ids() {
                Ok(hosted) => ids.extend(hosted.into_iter().map(|id| id.as_str().to_string())),
                Err(error) => log::warn!(target: "pty", "terminal host read failed: {error}"),
            }
        }
        Ok(ids)
    }

    /// Every host's running sessions' spawn dirs; `Unknown` when any host is
    /// busy or cannot answer.
    fn live_session_home_dirs(&self) -> Liveness<Vec<PathBuf>> {
        match self.known(|host| host.live_session_home_dirs()) {
            Liveness::Known(answers) => answers
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .map_or(Liveness::Unknown, |dirs| {
                    Liveness::Known(dirs.into_iter().flatten().collect())
                }),
            Liveness::Unknown => Liveness::Unknown,
        }
    }

    /// `(total, dead_retained)` summed over the hosts; `None` when any is busy
    /// or cannot answer.
    fn session_len_and_dead(&self) -> Option<(usize, usize)> {
        self.each(|host| host.session_len_and_dead())
            .into_iter()
            .map(|answer| answer.ok().flatten())
            .try_fold((0, 0), |(total, dead), counts| {
                counts.map(|(t, d)| (total + t, dead + d))
            })
    }

    /// Every host's layout as `viewer` sees it. A layout read must not
    /// guess: a used sidecar that cannot answer fails the read, rather than
    /// its workspaces reading as closed.
    fn layout_snapshots(
        &self,
        viewer: &ViewerId,
        boot: &ClientBootId,
    ) -> Result<Vec<LayoutSnapshot>, LayoutError> {
        let mut snapshots = vec![self.in_process.layout_snapshot(viewer, boot)?];
        if let Some(client) = self.used_sidecar()? {
            snapshots.push(client.layout_snapshot(viewer, boot)?);
        }
        Ok(snapshots)
    }

    /// The sidecar, once this app used one; `Unreachable` when it cannot be
    /// reached.
    fn used_sidecar(&self) -> Result<Option<&Arc<SessionServerClient>>, LayoutError> {
        self.sidecar
            .as_ref()
            .map(Option::as_ref)
            .map_err(|unreachable| LayoutError::Unreachable(unreachable.to_string()))
    }

    /// Apply a verb about one workspace in every host holding it. A used
    /// sidecar that cannot be asked, and may hold it, makes the verb
    /// `Unreachable`; a host without the workspace answers `UnknownWorkspace`,
    /// which stands only when no host has it; any other refusal is the answer.
    fn in_workspace_hosts(
        &self,
        verb: impl Fn(&dyn TerminalDomain) -> Result<(), LayoutError>,
    ) -> Result<(), LayoutError> {
        let sidecar = self.used_sidecar();
        let mut answers = vec![verb(&*self.in_process)];
        if let Ok(Some(client)) = &sidecar {
            answers.push(verb(&***client));
        }
        let mut unknown = None;
        let mut applied = false;
        for answer in answers {
            match answer {
                Ok(()) => applied = true,
                Err(error @ LayoutError::UnknownWorkspace(_)) => unknown = Some(error),
                Err(error) => return Err(error),
            }
        }
        sidecar?;
        match unknown {
            Some(error) if !applied => Err(error),
            _ => Ok(()),
        }
    }
}

/// Whether any host tracks `session` (membership, not liveness). Never
/// waits on the sidecar; a read it cannot answer is bounded.
pub fn has_session<R: Runtime>(app: &AppHandle<R>, session: &str) -> Liveness<bool> {
    Hosts::of(app).map_or(Liveness::Unknown, |hosts| {
        hosts.has_session(&PtySessionId::new(session))
    })
}

/// Which of `sessions` have a running child, across the hosts.
pub fn alive_session_ids<R: Runtime>(
    app: &AppHandle<R>,
    sessions: &[&str],
) -> Liveness<HashSet<String>> {
    let ids: Vec<PtySessionId> = sessions.iter().copied().map(PtySessionId::from).collect();
    Hosts::of(app).map_or(Liveness::Unknown, |hosts| hosts.alive_session_ids(&ids))
}

/// SW-AV-1 (INV-3): every alive, non-pool session id on any host — the live-PTY arm
/// of the union roster (`dashboard_list_sessions`). Includes the registry-less bare
/// sessions (root / notes / git-sync / `#` / no-MAIN-tab) the register-on-open
/// ledger missed. The in-process
/// enumeration error is surfaced (never swallowed into an empty set); a sidecar that
/// cannot answer contributes nothing this pass.
pub fn live_session_ids<R: Runtime>(app: &AppHandle<R>) -> Result<HashSet<String>, String> {
    Hosts::of(app)
        .ok_or_else(|| "PTY registry unavailable".to_string())?
        .live_session_ids()
}

/// The spawn dirs of every running session across the hosts; `Unknown` when a
/// host is busy or cannot answer.
pub fn live_session_home_dirs<R: Runtime>(app: &AppHandle<R>) -> Liveness<Vec<PathBuf>> {
    Hosts::of(app).map_or(Liveness::Unknown, |hosts| hosts.live_session_home_dirs())
}

/// [`live_session_home_dirs`] read off the async workers (a sidecar answers over
/// its socket).
pub async fn live_session_home_dirs_off_thread(app: &tauri::AppHandle) -> Liveness<Vec<PathBuf>> {
    let app = app.clone();
    await_blocking("live_session_home_dirs", move || {
        Ok(live_session_home_dirs(&app))
    })
    .await
    .unwrap_or(Liveness::Unknown)
}

/// `(total, dead_retained)` sessions over every host, for the memory sampler.
pub fn session_len_and_dead<R: Runtime>(app: &AppHandle<R>) -> Option<(usize, usize)> {
    Hosts::of(app)?.session_len_and_dead()
}

/// The working directory `session` last reported, from its owning host.
pub fn session_cwd<R: Runtime>(app: &AppHandle<R>, session: &str) -> Option<PathBuf> {
    resolve_session_backend(app, session)
        .ok()?
        .session_cwd(&PtySessionId::new(session))
        .ok()
        .flatten()
}

/// What a person looking at `session` sees, as plain text from its emulator
/// (see [`codesense_pty_core::domain::visible_text`]): the alternate screen's
/// rows while it is active, otherwise the whole normal buffer the emulator
/// keeps. Read from the owning host on the blocking pool; an unknown session
/// is a clean `Err`.
pub async fn visible_text<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
) -> Result<TerminalText, String> {
    let app = app.clone();
    let session = PtySessionId::new(session);
    await_blocking("pty.visible_text", move || {
        read_visible_text(&app, &session, u32::MAX)
    })
    .await
}

/// Only the rows on `session`'s screen now (its current frame, no scrollback).
pub async fn screen_text<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
) -> Result<TerminalText, String> {
    let app = app.clone();
    let session = PtySessionId::new(session);
    await_blocking("pty.screen_text", move || {
        let host = resolve_session_backend(&app, session.as_str())?;
        codesense_pty_core::domain::screen_text(host.as_ref(), &session).map_err(String::from)
    })
    .await
}

fn read_visible_text<R: Runtime>(
    app: &AppHandle<R>,
    session: &PtySessionId,
    lines: u32,
) -> Result<TerminalText, String> {
    let host = resolve_session_backend(app, session.as_str())?;
    codesense_pty_core::domain::visible_text(host.as_ref(), session, lines).map_err(String::from)
}

/// The app's viewport streams.
fn stream_ids<R: Runtime>(app: &AppHandle<R>) -> Result<tauri::State<'_, StreamIds>, String> {
    app.try_state::<StreamIds>()
        .ok_or_else(|| "PTY streams unavailable".to_string())
}

/// Attach `viewport` to `session` in its owning host. The result names the
/// stream by its client-unique id, the one `pty-output-stream-{id}` carries.
#[cfg(test)]
pub fn subscribe<R: Runtime>(
    app: &AppHandle<R>,
    session: &str,
    viewport: &ViewportId,
) -> Result<SubscribeResult, String> {
    let in_process = in_process_host(app).ok_or_else(|| "PTY registry unavailable".to_string())?;
    let streams = stream_ids(app)?;
    let id = PtySessionId::new(session);
    let (host, side): (Arc<dyn TerminalDomain>, StreamHost) = match attach_owner(
        hosted_in_process(&in_process, session),
        || reconnected_session_server(app),
        || sidecar_presence(app),
    )
    .map_err(String::from)?
    {
        Some(client) => {
            let side = StreamHost::LocalSidecar(client.namespace().clone());
            streams.record_origin(side.clone(), &client);
            (client, side)
        }
        None => (in_process, StreamHost::InProcess),
    };
    let mut attached = host.subscribe(&id, viewport)?;
    attached.stream_id = StreamId(streams.attach(side, &id, viewport, &attached));
    Ok(attached)
}

/// Attach `viewport` to the referenced session. `stream`, when given, is an id
/// from `reserve_terminal_stream` the frontend already listens on; without it
/// the stream gets a fresh id.
#[cfg(test)]
pub(crate) fn subscribe_reference<R: Runtime>(
    app: &AppHandle<R>,
    reference: &super::machines::RuntimeSessionReference,
    viewport: &ViewportId,
    stream: Option<u64>,
) -> Result<SubscribeResult, String> {
    subscribe_reference_authorized(app, reference, viewport, stream, false)
}

/// How many times a subscribe is tried when its stream overflowed before
/// the reply.
const SUBSCRIBE_OVERFLOW_ATTEMPTS: u32 = 3;

pub(crate) fn subscribe_reference_authorized<R: Runtime>(
    app: &AppHandle<R>,
    reference: &super::machines::RuntimeSessionReference,
    viewport: &ViewportId,
    stream: Option<u64>,
    allow_remote: bool,
) -> Result<SubscribeResult, String> {
    let backend = resolve_session_reference(app, reference, allow_remote)?;
    backend.revalidate(app)?;
    let session = PtySessionId::new(reference.session());
    let streams = stream_ids(app)?;
    let host = if let Some(client) = backend.client() {
        let host = if backend.is_remote() {
            StreamHost::remote_namespace(client.namespace())
        } else {
            StreamHost::LocalSidecar(client.namespace().clone())
        };
        streams.record_origin(host.clone(), &client);
        host
    } else {
        StreamHost::InProcess
    };
    let mut attempts = 0;
    let (mut attached, local) = loop {
        attempts += 1;
        let attached = backend.domain.subscribe(&session, viewport)?;
        if let Err(error) = backend.revalidate(app) {
            let _ = TerminalDomain::unsubscribe(backend.domain.as_ref(), attached.stream_id);
            return Err(error);
        }
        let remote = attached.stream_id;
        let local = match stream {
            Some(reserved) => streams.attach_reserved(
                reserved,
                host.clone(),
                &session,
                viewport,
                &attached,
                &mut remote_replay_emitter(
                    app,
                    backend.client().and_then(|client| client.private_lease()),
                ),
            ),
            None => Ok(streams.attach(host.clone(), &session, viewport, &attached)),
        };
        match local {
            Ok(local) => break (attached, local),
            // The stream overflowed before the reply: its snapshot no longer
            // meets the stream. Subscribe again, a bounded number of times.
            Err(AttachReservedError::Overflowed) if attempts < SUBSCRIBE_OVERFLOW_ATTEMPTS => {
                let _ = TerminalDomain::unsubscribe(backend.domain.as_ref(), remote);
            }
            Err(error) => {
                let _ = TerminalDomain::unsubscribe(backend.domain.as_ref(), remote);
                return Err(error.to_string());
            }
        }
    };
    let remote = attached.stream_id;
    if let Err(error) = backend.revalidate(app) {
        streams.detach(local);
        let _ = TerminalDomain::unsubscribe(backend.domain.as_ref(), remote);
        return Err(error);
    }
    attached.stream_id = StreamId(local);
    Ok(attached)
}

/// Detach the viewport stream `local` (a client-unique id) in the host it
/// lives in. A no-op for a stream already gone.
pub fn unsubscribe<R: Runtime>(app: &AppHandle<R>, local: u64) -> Result<(), String> {
    let Some((side, stream)) = stream_ids(app)?.detach(local) else {
        return Ok(());
    };
    match side {
        StreamHost::InProcess => {
            if let Some(host) = in_process_host(app) {
                host.unsubscribe(stream)?;
            }
        }
        // A lost connection already detached its viewports in the sidecar.
        side @ (StreamHost::Sidecar | StreamHost::LocalSidecar(_) | StreamHost::Remote { .. }) => {
            if let Some(client) = stream_ids(app)?.origin(&side) {
                SessionServerClient::unsubscribe(&client, stream)?;
            }
        }
    }
    Ok(())
}

/// Every host's layout as the app's viewer sees it, once a connection being
/// made to the sidecar settles.
pub fn layout_snapshots<R: Runtime>(
    app: &AppHandle<R>,
    boot: &ClientBootId,
) -> Result<Vec<LayoutSnapshot>, String> {
    let mut layouts = Hosts::settled(app)
        .ok_or_else(|| "PTY registry unavailable".to_string())?
        .layout_snapshots(&app_viewer(), boot)
        .map_err(|e| e.to_string())?;
    if let Some(manager) = app.try_state::<HostConnectionManager>() {
        layouts.extend(manager.verified_remote_layouts(&app_viewer(), boot));
    }
    Ok(layouts)
}

pub(crate) fn admitted_layout_snapshots<R: Runtime>(
    app: &AppHandle<R>,
    boot: &ClientBootId,
) -> Result<Vec<RuntimeLayoutReply>, String> {
    admitted_layout_snapshots_of(app, boot, true)
}

/// [`admitted_layout_snapshots`], of every admitted host (`machines`) or of this Mac's
/// own runtimes only.
pub(crate) fn admitted_layout_snapshots_of<R: Runtime>(
    app: &AppHandle<R>,
    boot: &ClientBootId,
    machines: bool,
) -> Result<Vec<RuntimeLayoutReply>, String> {
    let local = app
        .try_state::<SessionServerState>()
        .ok_or("local runtime unavailable")?;
    let mut rows = local.admission.baseline().rows;
    let manager = app
        .try_state::<HostConnectionManager>()
        .filter(|_| machines);
    let private_leases = if let Some(manager) = &manager {
        let (baseline, leases) = manager.admission_baseline_with_private_leases();
        rows.extend(baseline.rows);
        leases
    } else {
        Default::default()
    };
    Ok(collect_admitted_layout_replies(
        rows,
        private_leases,
        |target| {
            resolve_member_backend(app, target, true).and_then(|backend| {
                let snapshot = backend
                    .domain
                    .layout_snapshot(&app_viewer(), boot)
                    .map_err(|error| error.to_string())?;
                if snapshot.server_id.as_str() != target.namespace.server_id
                    || snapshot.epoch != target.namespace.epoch
                {
                    return Err("snapshot does not belong to admitted runtime".into());
                }
                validate_member_target(app, target)?;
                if let Some(token) = &backend.remote_token {
                    let manager = app.state::<HostConnectionManager>();
                    manager
                        .cache_verified_layout(token, snapshot.clone())
                        .map_err(|error| {
                            map_connected_error(&target.member.machine_id, error).to_string()
                        })?;
                    manager.note_remote_layout(&target.member.machine_id, &snapshot);
                }
                Ok(snapshot)
            })
        },
        || {
            let mut current = local.admission.baseline().rows;
            if let Some(manager) = &manager {
                current.extend(manager.admission_baseline().rows);
            }
            current
        },
        |target| validate_member_target(app, target),
    ))
}

fn collect_admitted_layout_replies(
    rows: Vec<super::machines::RuntimeAdmissionRow>,
    private_leases: std::collections::BTreeMap<
        String,
        super::auth::private_authority::PrivateAuthorityLease,
    >,
    mut read: impl FnMut(&RuntimeMemberTarget) -> Result<LayoutSnapshot, String>,
    current_rows: impl FnOnce() -> Vec<super::machines::RuntimeAdmissionRow>,
    validate: impl Fn(&RuntimeMemberTarget) -> Result<(), String>,
) -> Vec<RuntimeLayoutReply> {
    let private_current = |row: &super::machines::RuntimeAdmissionRow| {
        row.private_authority_epoch.as_ref().is_none_or(|epoch| {
            private_leases
                .get(&row.member.machine_id)
                .is_some_and(|lease| lease.epoch() == *epoch && lease.validate().is_ok())
        })
    };
    let captured_rows = rows.clone();
    let mut replies: Vec<_> = rows
        .into_iter()
        .filter(&private_current)
        .filter_map(|row| match row.state {
            RuntimeAdmissionState::Removed => None,
            RuntimeAdmissionState::Historical { historical } => {
                Some(RuntimeLayoutReply::Historical {
                    private_authority_epoch: row.private_authority_epoch,
                    member: row.member,
                    admission_revision: row.revision,
                    frozen: historical,
                })
            }
            RuntimeAdmissionState::Unavailable => Some(RuntimeLayoutReply::Unavailable {
                member: row.member,
                admission_revision: row.revision,
            }),
            RuntimeAdmissionState::Live { namespace } => {
                let target = RuntimeMemberTarget {
                    member: row.member,
                    admission_revision: row.revision,
                    namespace,
                };
                let snapshot = read(&target);
                Some(match snapshot {
                    Ok(snapshot) => RuntimeLayoutReply::Live {
                        private_authority_epoch: row.private_authority_epoch,
                        target,
                        snapshot: Box::new(snapshot),
                    },
                    Err(_) => RuntimeLayoutReply::Unavailable {
                        member: target.member,
                        admission_revision: target.admission_revision,
                    },
                })
            }
        })
        .collect();
    // An earlier member can be revoked while a later member's snapshot blocks.
    // Never relabel that old reply with the replacement's revision/namespace.
    let current = current_rows();
    replies.retain(|reply| {
        let (member, revision) = match reply {
            RuntimeLayoutReply::Live { target, .. } => {
                if validate(target).is_err() {
                    return false;
                }
                (&target.member, target.admission_revision)
            }
            RuntimeLayoutReply::Historical {
                member,
                admission_revision,
                ..
            }
            | RuntimeLayoutReply::Unavailable {
                member,
                admission_revision,
            } => (member, *admission_revision),
        };
        captured_rows
            .iter()
            .find(|row| &row.member == member && row.revision == revision)
            .is_some_and(|captured| {
                private_current(captured) && current.iter().any(|row| row == captured)
            })
    });
    replies
}

/// Apply a verb about `workspace` in every host holding it.
pub fn in_workspace_hosts<R: Runtime>(
    app: &AppHandle<R>,
    verb: impl Fn(&dyn TerminalDomain) -> Result<(), LayoutError>,
) -> Result<(), String> {
    Hosts::of(app)
        .ok_or_else(|| "PTY registry unavailable".to_string())?
        .in_workspace_hosts(verb)
        .map_err(|e| e.to_string())
}

/// Open a layout tab: its host is resolved first — the sidecar in sidecar
/// mode, else in-process — and `spawn` hosts the session on exactly that host
/// (it is handed the sidecar, or `None` for in-process), so the tab is placed
/// where its session runs, atomically. An incompatible sidecar refuses it.
pub fn open_tab(
    app: &tauri::AppHandle,
    machine_id: &str,
    request: OpenTab,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
) -> Result<TabEntry, String> {
    let backend =
        resolve_spawn_backend(app, machine_id, request.session.as_str()).inspect_err(|_| {
            if machine_id == LOCAL_MACHINE_ID {
                refused_open(app)
            }
        })?;
    open_tab_resolved(
        backend,
        request,
        spawn,
        |backend| backend.revalidate(app),
        || {
            let in_process =
                in_process_host(app).ok_or_else(|| "PTY registry unavailable".to_string())?;
            let backend = local_in_process_backend(in_process.clone())?;
            Ok((in_process, backend))
        },
    )
}

fn open_tab_resolved(
    backend: ResolvedTerminalBackend,
    request: OpenTab,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
    revalidate: impl Fn(&ResolvedTerminalBackend) -> Result<(), String>,
    local_fallback: impl FnOnce() -> Result<(Arc<TerminalHost>, ResolvedTerminalBackend), String>,
) -> Result<TabEntry, String> {
    revalidate(&backend)?;
    open_tab_on(backend, request, spawn, revalidate, local_fallback)
}

/// Open a layout tab on the resolved host: the sidecar when there is one,
/// else in-process; `spawn` is handed that same host. A sidecar that cannot
/// take the tab's reservation (its connection lost before the spawn ran)
/// falls open: the tab opens in-process instead, its spawn pinned there.
fn open_tab_on(
    backend: ResolvedTerminalBackend,
    request: OpenTab,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
    revalidate: impl Fn(&ResolvedTerminalBackend) -> Result<(), String>,
    local_fallback: impl FnOnce() -> Result<(Arc<TerminalHost>, ResolvedTerminalBackend), String>,
) -> Result<TabEntry, String> {
    open_tab_on_typed(backend, request, spawn, revalidate, local_fallback)
        .map_err(|error| error.to_string())
}

fn open_tab_on_typed(
    backend: ResolvedTerminalBackend,
    request: OpenTab,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), String>,
    revalidate: impl Fn(&ResolvedTerminalBackend) -> Result<(), String>,
    local_fallback: impl FnOnce() -> Result<(Arc<TerminalHost>, ResolvedTerminalBackend), String>,
) -> Result<TabEntry, LayoutError> {
    let spawn = std::cell::RefCell::new(Some(spawn));
    let run = |placement: ResolvedTerminalBackend| match spawn.borrow_mut().take() {
        Some(spawn) => revalidate(&placement).and_then(|()| spawn(placement)),
        None => Err("open_tab: the tab's spawn already ran".to_string()),
    };
    let domain = backend.domain();
    match domain.open_tab(request.clone(), Box::new(|| run(backend.clone()))) {
        // Only a connection that is gone: its reservations went with it,
        // so the tab cannot be left behind there too.
        Err(LayoutError::Unreachable(why))
            if may_retry_local_after_abort(&backend, spawn.borrow().is_some()) =>
        {
            log::warn!(target: "pty", "session server could not open the tab, opening it in-process: {why}");
        }
        opened => return opened,
    }
    let (in_process, local) = local_fallback().map_err(LayoutError::Unreachable)?;
    in_process.open_tab(request, Box::new(|| run(local)))
}

fn may_retry_local_after_abort(backend: &ResolvedTerminalBackend, spawn_unconsumed: bool) -> bool {
    spawn_unconsumed
        && backend.allows_local_fail_open()
        && backend.client().is_some_and(|client| !client.is_alive())
}

/// The frontend event naming the viewport streams a reconnect could not carry
/// across: [`TerminalStreamsLost`]. Each pane viewing one re-subscribes
/// (snapshot + resume from its watermark), so it never freezes silently.
pub const TERMINAL_STREAMS_LOST_EVENT: &str = "terminal-streams-lost";

/// The viewport streams, by their client-unique ids, that no longer carry
/// output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStreamsLost {
    pub streams: Vec<u64>,
}

/// Carry the app across a new sidecar connection. The same sidecar lifetime
/// resumed: the viewports the lost connection attached are re-attached, each
/// stream continuing without a gap — one that cannot be is named to the
/// frontend, whose pane re-subscribes; the sessions that left it meanwhile end
/// in the app; and a layout that moved meanwhile is re-fetched. A new
/// lifetime: every session the app hosted in the old one ended with it, so
/// they end in the app, the layout is re-fetched, and the frontend is told
/// how many ended.
pub(crate) fn carry_across<R: Runtime>(
    app: &AppHandle<R>,
    state: &SessionServerState,
    client: &Arc<SessionServerClient>,
) {
    recover_local_effects(app, state, client);
    let (Ok(streams), Some(sink)) = (stream_ids(app), state.sink()) else {
        return;
    };
    let Some(change) = state.take_change() else {
        return;
    };
    match change {
        Continuity::Resumed { layout_moved } => {
            end_missed_sessions(state, client, &*sink);
            let emit = &mut |to: u64, item: StreamItem<'_>| emit_stream(app, to, item);
            let mut lost = Vec::new();
            let replacement = StreamHost::LocalSidecar(client.namespace().clone());
            streams.record_origin(replacement.clone(), client);
            for previous in streams.local_sidecar_hosts() {
                let StreamHost::LocalSidecar(namespace) = &previous else {
                    continue;
                };
                if namespace == client.namespace() {
                    continue;
                }
                if namespace.server_id != client.server_id() || namespace.epoch != client.epoch() {
                    streams.forget_host(previous);
                    continue;
                }
                lost.extend(streams.resume_from_to(
                    previous,
                    replacement.clone(),
                    || {
                        state
                            .connected()
                            .is_some_and(|current| Arc::ptr_eq(&current, client))
                    },
                    |session, viewport| client.subscribe(session, viewport),
                    |stream| {
                        let _ = client.unsubscribe(stream);
                    },
                    emit,
                ));
            }
            if !lost.is_empty() {
                let _ = app.emit(
                    TERMINAL_STREAMS_LOST_EVENT,
                    TerminalStreamsLost { streams: lost },
                );
            }
            if layout_moved {
                resync_layout(app, client.epoch());
            }
        }
        Continuity::Restarted { ended } => {
            for host in streams.local_sidecar_hosts() {
                if host != StreamHost::LocalSidecar(client.namespace().clone()) {
                    streams.forget_host(host);
                }
            }
            announce_ended(app, &*sink, ended, client.epoch());
        }
    }
}

fn recover_local_effects<R: Runtime>(
    app: &AppHandle<R>,
    state: &SessionServerState,
    client: &Arc<SessionServerClient>,
) {
    let Some(target) = state
        .admission
        .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
    else {
        return;
    };
    let retained: Vec<_> = state
        .admission
        .effects
        .claims(RuntimeMemberSlot::Sidecar)
        .into_iter()
        .filter(|old| old.target != target)
        .collect();
    if retained.is_empty() {
        return;
    }
    let retained_ids: Vec<_> = retained
        .iter()
        .map(|old| PtySessionId::new(&old.session))
        .collect();
    // The roster query intentionally excludes unattached warm members.
    let Ok(live) = client.alive_session_ids(&retained_ids) else {
        return;
    };
    for old in retained {
        let session = PtySessionId::new(&old.session);
        let same_lifetime = old.target.namespace.server_id == target.namespace.server_id
            && old.target.namespace.epoch == target.namespace.epoch;
        let presence = if same_lifetime {
            client.has_session(&session)
        } else {
            Ok(false)
        };
        recover_observed_local_effect(
            app,
            state,
            client,
            &target,
            old,
            presence,
            live.contains(&session),
        );
    }
}

pub(crate) fn recover_observed_local_effect<R: Runtime>(
    app: &AppHandle<R>,
    state: &SessionServerState,
    client: &Arc<SessionServerClient>,
    target: &RuntimeMemberTarget,
    mut old: super::pty_host::LocalEffectClaim,
    mut presence: Result<bool, PortUnavailable>,
    was_live: bool,
) {
    let session = PtySessionId::new(&old.session);
    if validate_member_target(app, target).is_err() {
        return;
    }
    if presence.is_ok() {
        match state
            .admission
            .effects
            .associate_terminal(&state.admission, &old, target)
        {
            Ok(Some(cleanup)) => {
                let _ = super::pty_host::settle_terminal_cleanup(app, cleanup);
                return;
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
    if presence == Ok(true) && was_live {
        let creation = match super::mcp_scope::owned_creation(&old) {
            Ok(creation) => creation,
            Err(_) => return,
        };
        if validate_member_target(app, target).is_err() {
            return;
        }
        let moved =
            state
                .admission
                .effects
                .transfer(&state.admission, &old, target.clone(), |new| {
                    // The sole fallible metadata comparison comes first, so no
                    // dashboard/ask/account record moves on incomplete proof.
                    if let Some(creation) = &creation {
                        super::mcp_scope::transfer_checked_creation(creation, &old, new)?;
                    }
                    if let Some(launch) =
                        app.try_state::<super::dashboard_launch::DashboardLaunchState>()
                    {
                        launch.transfer_owned(&old, new);
                    }
                    if let Some(asks) = app.try_state::<crate::mcp::state::AskUserRegistry>() {
                        asks.transfer_owned(&old, new);
                    }
                    if let Some(accounts) =
                        app.try_state::<super::agent_accounts::AgentAccountRuntimeState>()
                    {
                        accounts.transfer_owned(&old, new);
                    }
                    Ok(())
                });
        if let Ok(next) = moved {
            old = next;
        }
        // The new reader may have observed terminal ingress before the
        // old claim could move, including a refused transfer. Recheck
        // off-lock and retain the original claim if it did not move. Settle
        // exact records, but never invent the missing ingress/drain receipt.
        match client.alive_session_ids(std::slice::from_ref(&session)) {
            Ok(live) if live.contains(&session) => return,
            Ok(_) => presence = client.has_session(&session),
            Err(_) => return,
        }
        if validate_member_target(app, target).is_err() {
            return;
        }
    }
    // Unknown is never absence or continuity.
    if let Ok(present) = presence {
        // Missing/ended sessions settle only their retained original
        // owner. This is not an Exit ingress/drain receipt and cannot
        // authorize reusing an ID whose final event was missed.
        state.admission.effects.retire_claim(&old);
        let event = if present {
            TerminalDomainEvent::Exit {
                session,
                status: None,
                pool_member: false,
                requested: false,
                incarnation: 0,
            }
        } else {
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Expire,
            }
        };
        if let Ok(Some((audit, released))) =
            super::pty_host::settle_owned_app_event(app, &old, &event, false)
        {
            super::agent_audit::append(audit, &released.handle, Some(&old.session), released.kind);
        }
    }
}

/// Tell the frontend a host's layout moved while the app could not hear it:
/// `epoch` is the lifetime now serving.
fn resync_layout<R: Runtime>(app: &AppHandle<R>, epoch: &str) {
    let _ = app.emit(
        TERMINAL_LAYOUT_RESYNC_EVENT,
        TerminalLayoutResync {
            epoch: epoch.to_string(),
        },
    );
}

/// Reattach retained viewports after the exact same remote server lifetime
/// reconnects. `current` is checked around every blocking subscribe so an
/// attempt superseded by Retry/detach cannot publish its replacement stream.
pub(crate) fn resume_remote_streams<R: Runtime>(
    app: &AppHandle<R>,
    client: &Arc<SessionServerClient>,
    previous: &crate::terminal::session_server::EventNamespace,
    mut current: impl FnMut() -> bool,
) {
    let Some(streams) = app.try_state::<StreamIds>() else {
        return;
    };
    if !current() {
        return;
    }
    resync_layout(app, client.epoch());
    let prior_host = StreamHost::remote_namespace(previous);
    let host = StreamHost::remote_namespace(client.namespace());
    streams.record_origin(host.clone(), client);
    let mut emit = remote_replay_emitter(app, client.private_lease());
    let lost = resume_remote_streams_core(
        &streams,
        prior_host,
        host,
        &mut current,
        |session, viewport| client.subscribe(session, viewport),
        |stream| {
            let _ = client.unsubscribe(stream);
        },
        &mut emit,
    );
    if !lost.is_empty() {
        let _ = app.emit(
            TERMINAL_STREAMS_LOST_EVENT,
            TerminalStreamsLost { streams: lost },
        );
    }
}

fn resume_remote_streams_core<E>(
    streams: &StreamIds,
    previous: StreamHost,
    host: StreamHost,
    current: impl FnMut() -> bool,
    subscribe: impl FnMut(&PtySessionId, &ViewportId) -> Result<SubscribeResult, E>,
    unsubscribe: impl FnMut(StreamId),
    emit: &mut crate::commands::pty_host::Emit<'_>,
) -> Vec<u64> {
    streams.resume_from_to(previous, host, current, subscribe, unsubscribe, emit)
}

/// Retire exactly one verified remote lifetime after atomically acquiring the
/// manager's one-shot priority permit. No stream mutation, session ending, or
/// frontend event is allowed before that permit is consumed. The old
/// per-machine ledger is already frozen, so the emitted batch cannot claim
/// another host's work.
pub(crate) fn retire_remote_lifetime<R: Runtime>(
    app: &AppHandle<R>,
    sink: &dyn crate::terminal::session_server::NamespacedTerminalEventSink,
    namespace: &crate::terminal::session_server::EventNamespace,
    ledger: RemoteSessionLedger,
    take_priority_permit: impl FnOnce() -> bool,
) -> bool {
    let Some(streams) = app.try_state::<StreamIds>() else {
        return false;
    };
    retire_remote_lifetime_core(&streams, sink, namespace, ledger, take_priority_permit)
}

/// Presentation-independent effects, shared by the app adapter and headless
/// admission tests. The stream table owns the atomic permit/effects boundary.
pub(crate) fn retire_remote_lifetime_core(
    streams: &StreamIds,
    sink: &dyn crate::terminal::session_server::NamespacedTerminalEventSink,
    namespace: &crate::terminal::session_server::EventNamespace,
    ledger: RemoteSessionLedger,
    take_priority_permit: impl FnOnce() -> bool,
) -> bool {
    let host = StreamHost::remote_namespace(namespace);
    let Some(viewed) = streams.retire_lifetime_if(host.clone(), take_priority_permit) else {
        return false;
    };
    let mut ended: std::collections::HashMap<PtySessionId, bool> =
        ledger.take_all().into_iter().collect();
    for session in viewed {
        ended.entry(session).or_insert(false);
    }
    for (session, pool_member) in ended {
        for event in [
            TerminalDomainEvent::Exit {
                session: session.clone(),
                status: None,
                pool_member,
                requested: false,
                incarnation: 0,
            },
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Expire,
            },
        ] {
            let _ = sink.emit(crate::terminal::session_server::NamespacedTerminalEvent {
                namespace: namespace.clone(),
                event,
            });
        }
    }
    true
}

/// A sidecar lifetime ended under the app, with `ended`, the sessions the app
/// hosted there: they end in the app, the layout is re-fetched (`epoch` now
/// serving), and the frontend is told how many open sessions ended.
fn announce_ended<R: Runtime>(
    app: &AppHandle<R>,
    sink: &dyn TerminalEventSink,
    ended: Vec<(PtySessionId, bool)>,
    epoch: &str,
) {
    let open = ended.iter().filter(|(_, pool_member)| !pool_member).count();
    end_sessions(sink, ended);
    resync_layout(app, epoch);
    let _ = app.emit(
        TERMINAL_SERVER_RESTARTED_EVENT,
        TerminalServerRestarted {
            ended_sessions: open,
        },
    );
}

/// End in the app the sessions that ended in the same sidecar lifetime while
/// the app could not hear it. One that left the sidecar's registry ends and is
/// removed. One whose child exited but which the sidecar retains (so its
/// terminal stays readable) is still a member there, so the sidecar is asked to
/// emit its exit again: it reaches the app with its real status, as the lost
/// original would have. Only sessions whose exit the app has not had are
/// named; a repeat of one it has is dropped on arrival anyway. A sidecar that
/// predates the question leaves such a session reading as it did.
pub(crate) fn end_missed_sessions(
    state: &SessionServerState,
    client: &SessionServerClient,
    sink: &dyn TerminalEventSink,
) {
    end_sessions(
        sink,
        state.take_ended(|session| client.has_session(session) == Ok(false)),
    );
    let unexited = state.unexited_sessions();
    if !unexited.is_empty() {
        if let Err(error) = client.redeliver_exits(&unexited) {
            log::debug!(target: "pty_backend", "exits of retained sessions not redelivered: {}", String::from(error));
        }
    }
}

/// End `sessions` in the app — their exit and their leaving the registry were
/// never pushed to it — so their tabs exit and their bookkeeping is released.
fn end_sessions(sink: &dyn TerminalEventSink, sessions: Vec<(PtySessionId, bool)>) {
    for (session, pool_member) in sessions {
        // The sidecar ended with them: nothing here asked for these exits,
        // and their real status went with it.
        let _ = sink.emit(TerminalDomainEvent::Exit {
            session: session.clone(),
            status: None,
            pool_member,
            requested: false,
            incarnation: 0,
        });
        let _ = sink.emit(TerminalDomainEvent::SessionRemoved {
            session,
            reason: TeardownReason::Expire,
        });
    }
}

/// Spawn `intent` in the sidecar, which resolves it against its own
/// environment, staging the launch provenance exactly as the in-process spawn
/// does (and dropping it again on failure). A failure that leaves the session
/// known not to run in the sidecar returns `may_host_elsewhere`, so the caller
/// hosts it in-process.
pub async fn spawn_on_session_server(
    app: &tauri::AppHandle,
    backend: ResolvedTerminalBackend,
    intent: SpawnIntent,
    registration: SessionRegistration,
    provenance: Option<LaunchAccountProvenance>,
    effect_claim: Option<super::pty_host::LocalEffectClaim>,
) -> Result<(), SpawnFailure> {
    let app = app.clone();
    let session_id = registration.session_id.clone();
    await_blocking("spawn_pty", move || {
        let staged = app.state::<crate::commands::agent_accounts::AgentAccountRuntimeState>()
            .stage_owned_provenance(&session_id, provenance, effect_claim.clone());
        Ok(staged
            .map_err(|message| SpawnFailure {
                // No send was attempted: unlike a confirmed cancel, there
                // cannot be an unobserved final event from this reservation.
                message: {
                    if let Some(claim) = &effect_claim {
                        app.state::<SessionServerState>().admission.effects.never_started(claim);
                    }
                    message
                },
                may_host_elsewhere: true,
                disposition: SpawnDisposition::NeverStarted,
            })
            .and_then(|()| {
                send_on_exact_backend(
                    &backend,
                    |backend| backend.revalidate(&app),
                    |client| client.spawn(intent, registration),
                )
                .inspect_err(|failure| {
                    if failure.may_host_elsewhere {
                        if let Some(claim) = &effect_claim {
                            app.state::<crate::commands::agent_accounts::AgentAccountRuntimeState>().take_owned_provenance(claim);
                            app.state::<SessionServerState>().admission.effects.spawn_failed(claim, failure.disposition);
                        }
                    }
                })
            })
            .map(|pid| {
                log::info!(target: "pty", "spawn_pty: {session_id} hosted by the session server (child pid {pid:?})");
            }))
    })
    .await
    .unwrap_or_else(|message| {
        // The spawn task itself failed: whether it reached the sidecar is
        // unknown, so it is not retried elsewhere.
        Err(SpawnFailure {
            message,
            may_host_elsewhere: false,
            disposition: SpawnDisposition::Unknown,
        })
    })
}

/// Revalidate the opaque backend against the exact admitted client immediately
/// before its send. Nothing capable of writing to the transport may run between
/// these two operations.
fn send_on_exact_backend<T>(
    backend: &ResolvedTerminalBackend,
    revalidate: impl FnOnce(&ResolvedTerminalBackend) -> Result<(), String>,
    send: impl FnOnce(Arc<SessionServerClient>) -> Result<T, SpawnFailure>,
) -> Result<T, SpawnFailure> {
    let client = backend.client().ok_or_else(|| SpawnFailure {
        message: "resolved terminal backend has no session-server client".into(),
        may_host_elsewhere: false,
        disposition: SpawnDisposition::NeverStarted,
    })?;
    revalidate(backend).map_err(|message| SpawnFailure {
        message,
        may_host_elsewhere: false,
        disposition: SpawnDisposition::NeverStarted,
    })?;
    send(client)
}

/// Preserve the admitted placement captured by the trusted launch adapter.
pub(crate) fn open_tab_pinned(
    app: &tauri::AppHandle,
    backend: ResolvedTerminalBackend,
    request: OpenTab,
    spawn: impl FnOnce(ResolvedTerminalBackend) -> Result<(), SpawnFailure>,
) -> Result<TabEntry, LayoutError> {
    if !backend.is_remote() {
        return Err(LayoutError::Spawn(
            "remote launch requires an admitted remote backend".into(),
        ));
    }
    backend.revalidate(app).map_err(LayoutError::Unreachable)?;
    let client = backend
        .client()
        .ok_or_else(|| LayoutError::Unreachable("remote client unavailable".into()))?;
    client.open_remote_tab(request, || {
        backend.revalidate(app).map_err(|message| SpawnFailure {
            message,
            may_host_elsewhere: false,
            disposition: SpawnDisposition::NeverStarted,
        })?;
        spawn(backend)
    })
}

#[cfg(test)]
mod tests {
    #[path = "hook_trust_respawn_tests.rs"]
    mod hook_trust_respawn_tests;
    use super::*;
    use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
    use codesense_session_server::testing::{
        alive, registration, sh, sh_spec, wait_until, RecordingSink, TestServer,
    };
    use std::sync::Mutex;

    #[test]
    fn private_multi_member_reply_retirement_drops_live_and_historical_without_hiding_local() {
        use super::super::machines::{
            FrozenRuntimeLayout, RuntimeAdmissionRow, RuntimeHistoryReason, RuntimeMemberAddress,
        };
        let layout = || LayoutSnapshot {
            schema: codesense_pty_core::layout::LAYOUT_SCHEMA,
            server_id: codesense_pty_core::domain::ServerId::new("server"),
            server_name: "sentinel".into(),
            epoch: "epoch".into(),
            rev: 1,
            workspaces: Default::default(),
            viewer: Default::default(),
        };
        for historical in [false, true] {
            let (lease, revoke) = super::super::auth::private_authority::test_lease(1);
            let namespace =
                crate::terminal::session_server::mint_event_namespace("server", "epoch").unwrap();
            let mut private =
                RuntimeAdmissionRow::live(RuntimeMemberAddress::remote("private"), 1, &namespace);
            private.private_authority_epoch = Some(lease.epoch());
            if historical {
                private.state = RuntimeAdmissionState::Historical {
                    historical: Box::new(FrozenRuntimeLayout {
                        last_namespace: Some((&namespace).into()),
                        server_id: "server".into(),
                        epoch: "epoch".into(),
                        snapshot: layout(),
                        reason: RuntimeHistoryReason::Unreachable,
                    }),
                };
            }
            let local = RuntimeAdmissionRow::live(
                RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
                2,
                &namespace,
            );
            let rows = vec![private, local];
            let leases = std::collections::BTreeMap::from([("private".into(), lease)]);
            // The production collector serializes private rows with their exact stamp.
            let before = collect_admitted_layout_replies(
                rows.clone(),
                leases.clone(),
                |_| Ok(layout()),
                || rows.clone(),
                |_| Ok(()),
            );
            assert!(matches!(&before[0],
                RuntimeLayoutReply::Live { private_authority_epoch: Some(epoch), .. }
                | RuntimeLayoutReply::Historical { private_authority_epoch: Some(epoch), .. }
                if epoch == "1"));
            let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
            std::thread::scope(|scope| {
                let captured = rows.clone();
                let collecting = scope.spawn(move || {
                    collect_admitted_layout_replies(
                        captured.clone(),
                        leases,
                        |target| {
                            if target.member.machine_id == LOCAL_MACHINE_ID {
                                entered_tx.send(()).unwrap();
                                release_rx.recv().unwrap();
                            }
                            Ok(layout())
                        },
                        || captured.clone(),
                        |_| Ok(()),
                    )
                });
                entered_rx.recv().unwrap();
                revoke();
                release_tx.send(()).unwrap();
                let after = collecting.join().unwrap();
                assert_eq!(after.len(), 1);
                assert!(
                    matches!(&after[0], RuntimeLayoutReply::Live { target, private_authority_epoch: None, .. } if target.member.machine_id == LOCAL_MACHINE_ID)
                );
            });
            // A retained private stamp with missing proof never becomes legacy.
            let missing = collect_admitted_layout_replies(
                rows.clone(),
                Default::default(),
                |_| Ok(layout()),
                || rows.clone(),
                |_| Ok(()),
            );
            assert_eq!(missing.len(), 1);
        }
    }

    #[test]
    fn rm4c_frozen_mutation_refusal() {
        let snapshot = super::super::host_connection_manager::LastKnownHostSnapshot {
            layout: None,
            census: super::super::host_connection_manager::CensusSnapshot { live_sessions: 4 },
            server_id: "verified-server".into(),
            epoch: "old-epoch".into(),
            rev: 9,
            shutdown_for_replacement: false,
            host_pid: 0,
        };
        let error = map_connected_error(
            "build-box",
            ConnectedBackendError::Incompatible {
                snapshot: Box::new(snapshot),
            },
        );
        assert!(matches!(
            error,
            TerminalBackendResolveError::Incompatible { ref machine_id, .. }
                if machine_id == "build-box"
        ));
        assert!(!matches!(error, TerminalBackendResolveError::Local(_)));
    }

    fn test_backend(
        in_process: &Arc<TerminalHost>,
        sidecar: Option<Arc<SessionServerClient>>,
    ) -> ResolvedTerminalBackend {
        if let Some(client) = sidecar {
            select_local_backend(Some(client), || panic!("sidecar selected")).unwrap()
        } else {
            local_in_process_backend(in_process.clone()).unwrap()
        }
    }

    fn test_remote_backend(
        machine_id: &str,
        client: Arc<SessionServerClient>,
    ) -> ResolvedTerminalBackend {
        ResolvedTerminalBackend {
            machine_id: machine_id.into(),
            server_id: client.server_id().into(),
            incarnation: None,
            domain: client.clone(),
            sidecar: Some(client),
            policy: TerminalBackendPolicy::RemoteAdmitted,
            remote_token: None,
            admission: None,
            spawn_predecessor: None,
        }
    }

    pub(super) fn in_process(runtime: &tokio::runtime::Runtime) -> Arc<TerminalHost> {
        let host = Arc::new(TerminalHost::new(
            codesense_pty_core::layout::ServerIdentity::ephemeral("test"),
            Arc::new(RecordingSink::default()),
            runtime.handle().clone(),
        ));
        host.attach_echo_emulator();
        host
    }

    /// Register `reg` running `script` in `host`; returns its child's pid.
    fn register(
        runtime: &tokio::runtime::Runtime,
        host: &TerminalHost,
        script: &str,
        reg: SessionRegistration,
    ) -> u32 {
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec(&reg.session_id, script)))
            .unwrap();
        let pid = pty.pid().unwrap();
        host.register(pty, reg).unwrap();
        pid
    }

    fn sid(id: &str) -> PtySessionId {
        PtySessionId::new(id)
    }

    /// A viewport attaches in-process for a session the app hosts, to the
    /// sidecar otherwise, and fails (never "session not found") while a sidecar
    /// that may hold the session can't be reached.
    #[test]
    fn an_attach_to_an_unreachable_sidecar_fails_instead_of_reading_as_ended() {
        let unreachable = || {
            Err(PortUnavailable(
                "the session server runs but is not connected".into(),
            ))
        };
        assert_eq!(
            attach_owner(true, || Some("sidecar"), unreachable),
            Ok(None)
        );
        assert_eq!(
            attach_owner(false, || Some("sidecar"), unreachable),
            Ok(Some("sidecar"))
        );
        let failed = attach_owner::<&str>(false, || None, unreachable).unwrap_err();
        assert!(
            !failed
                .0
                .contains(codesense_pty_core::domain::SESSION_NOT_FOUND),
            "{failed}"
        );
        assert_eq!(attach_owner::<&str>(false, || None, || Ok(None)), Ok(None));
        assert_eq!(
            attach_owner(false, || None, || Ok(Some("sidecar"))),
            Ok(Some("sidecar"))
        );
    }

    /// SW-RT-6d (AC-2): the sidecar hosts new sessions by default — one is
    /// launched when none runs — and they are hosted in-process only when no
    /// sidecar can run (its launch fails).
    #[test]
    fn the_sidecar_hosts_by_default_and_falls_open_only_when_none_can_run() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let root = codesense_session_server::testing::short_root();
        let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };

        let cannot_run = SessionServerState::at(Some(root.path().to_path_buf()));
        let failed = cannot_run.connect_or_launch(root.path(), sink(), |_| {
            Err("codesense-session-server: binary missing".to_string())
        });
        assert!(matches!(
            backend_after(app.handle(), &cannot_run, failed),
            Ok(None)
        ));

        let launched = std::cell::RefCell::new(None);
        let state = SessionServerState::at(Some(root.path().to_path_buf()));
        let connected = state.connect_or_launch(root.path(), sink(), |root| {
            *launched.borrow_mut() = Some(TestServer::start_at(root));
            Ok(())
        });
        assert!(launched.borrow().is_some(), "none ran: one was launched");
        assert!(matches!(
            backend_after(app.handle(), &state, connected),
            Ok(Some(_))
        ));
    }

    /// A lost connection is re-established on its own, with nobody asking:
    /// the panes viewing the sidecar's sessions resume.
    #[test]
    fn a_lost_connection_reconnects_on_its_own() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        reconnect_on_loss(app.handle());
        let state = app.state::<SessionServerState>();
        let lost = state.connected().unwrap();
        lost.sever();
        assert!(wait_until(|| state
            .connected()
            .is_some_and(|again| !Arc::ptr_eq(&again, &lost))));
    }

    /// The sidecar the app hosted sessions in is gone and none runs: they end
    /// in the app — their tabs exit, their bookkeeping is released — and the
    /// frontend is told how many open sessions ended.
    #[test]
    fn a_gone_sidecars_sessions_end_in_the_app() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, sink) = app_reemitting(in_process(&runtime), &sidecar);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        sidecar.track("gone-with-it");
        client
            .spawn(sh("sleep 60; :"), registration("gone-with-it", None, false))
            .unwrap();
        let told = heard(&app, TERMINAL_SERVER_RESTARTED_EVENT.to_string());
        client.sever();
        assert!(wait_until(|| state.connected().is_none()));
        end_lost_lifetime(app.handle(), &state);
        assert_eq!(sink.others.exit("gone-with-it"), Some(None));
        assert!(wait_until(|| told
            .lock()
            .unwrap()
            .contains("\"endedSessions\":1")));
    }

    /// SV-7: a running terminal server's overview, read without launching
    /// anything: what it holds, the app instances connected (by app, not
    /// connection) and what a stop or restart binds to.
    #[test]
    fn sv7_the_overview_reads_a_running_server() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        sidecar.track("held");
        client
            .spawn(sh("sleep 60"), registration("held", None, false))
            .unwrap();
        let _another_connection = sidecar_client(&sidecar);

        let overview = terminal_server_overview(app.handle());
        assert_eq!(overview.state, TerminalServerRun::Running);
        assert_eq!(overview.pid, Some(std::process::id()));
        assert_eq!(
            (overview.sessions, overview.running, overview.exited_kept),
            (Some(1), Some(1), Some(0))
        );
        assert_eq!(overview.running_agents, Some(0));
        // One app's two connections are one instance.
        assert_eq!(
            (overview.connected_instances, overview.other_instances),
            (Some(1), Some(0))
        );
        assert!(overview.started_at_ms.is_some());
        let expected = overview.expected.expect("what a stop binds to");
        assert_eq!(expected.session_ids, ["held"]);
        assert_eq!(expected.server_id, client.server_id());
        assert_eq!(expected.epoch, client.epoch());
        assert!(overview.unowned.is_empty());
        assert_eq!(overview.detail, None);
    }

    /// SV-7: the app claims what it holds only once the restore of its
    /// launches settled — the restored launch rows are claimed then, on this
    /// connection — never before (nothing latched while it runs).
    #[test]
    fn sv7_restored_launches_are_claimed_once_the_restore_settles() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        // The previous app process opened it and is gone past the grace.
        sidecar.spawn_as_gone_app("previous-run", "restored-run", "sleep 60");
        assert!(wait_until(|| sidecar.host().connection_count() == 1));
        sidecar
            .host()
            .advance_clock(std::time::Duration::from_secs(5 * 60));
        let unowned = |client: &SessionServerClient| -> Vec<String> {
            client
                .server_status()
                .unwrap()
                .unwrap()
                .unowned
                .into_iter()
                .map(|left| left.session.as_str().to_string())
                .collect()
        };
        assert_eq!(unowned(&client), ["restored-run"], "no app holds it yet");

        claim_held_sessions_with(app.handle(), false, false, || {
            panic!("no launch rows are read while the restore runs")
        });
        assert!(!client.claim_answered(), "nothing claimed while it runs");

        let read = std::cell::Cell::new(false);
        claim_held_sessions_with(app.handle(), false, true, || {
            read.set(true);
            vec!["restored-run".into()]
        });
        assert!(read.get(), "the restored launch rows are read");
        assert!(client.claim_answered(), "claimed once it settled");
        assert!(unowned(&client).is_empty(), "this app holds it now");
        assert!(
            client
                .end_unowned_session(&PtySessionId::new("restored-run"))
                .is_err(),
            "End is refused: an app holds it"
        );
    }

    /// SV-7: an app that hosts its terminals itself has no terminal server to
    /// manage: the overview says so, never "stopped" (nothing would start).
    #[test]
    fn sv7_in_process_hosting_has_no_server_to_manage() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::NotApplicable
        );
        app.manage(SessionServerState::at(None));
        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::NotApplicable
        );
    }

    /// SV-PBF-291: only a server that is gone reads Stopped after a stop on
    /// purpose; one that still runs unconnected (stopped on purpose or not)
    /// keeps reading Unreachable, so a real outage is never masked.
    #[test]
    fn a_running_unconnected_server_never_reads_stopped() {
        let root = codesense_session_server::testing::short_root();
        let server = TestServer::start_at(root.path());
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(StreamIds::default());
        let state = SessionServerState::at(Some(root.path().to_path_buf()));
        app.manage(state);
        let state = app.state::<SessionServerState>();

        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::Unreachable
        );
        state.mark_user_stopped("some-server", "some-epoch");
        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::Unreachable
        );
        drop(server);
    }

    /// SV-PBF-291: an overview read while a terminal opens after a Stop does
    /// not get in the launch's way (it takes no host lock of its own) and
    /// never reads Stopped for the server that is starting: it waits behind
    /// the launch and reads Running.
    #[test]
    fn an_overview_during_a_launch_after_a_stop_does_not_stop_the_launch() {
        let root = codesense_session_server::testing::short_root();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(StreamIds::default());
        app.manage(SessionServerState::at(Some(root.path().to_path_buf())));
        let state = app.state::<SessionServerState>();
        state.mark_user_stopped("stopped-server", "stopped-epoch");

        let server = std::cell::RefCell::new(None);
        let overview = std::cell::RefCell::new(None);
        let client = state
            .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), |root| {
                let handle = app.handle().clone();
                *overview.borrow_mut() = Some(std::thread::spawn(move || {
                    terminal_server_overview(&handle).state
                }));
                // The overview reads while the launch holds the attempts.
                std::thread::sleep(std::time::Duration::from_millis(300));
                *server.borrow_mut() = Some(TestServer::start_at(root));
                Ok(())
            })
            .expect("the launch is not stopped by the overview");
        assert!(client.is_alive());
        assert_eq!(
            overview.borrow_mut().take().unwrap().join().unwrap(),
            TerminalServerRun::Running
        );
    }

    /// SV-7: with no terminal server running, the overview says so and
    /// launches nothing; Start launches one, after which it reads running.
    #[test]
    fn sv7_the_overview_never_launches_and_start_does() {
        let root = codesense_session_server::testing::short_root();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(StreamIds::default());
        let state = SessionServerState::at(Some(root.path().to_path_buf()));
        app.manage(state);

        let overview = terminal_server_overview(app.handle());
        assert_eq!(overview.state, TerminalServerRun::Stopped);
        assert!(overview.expected.is_none());
        assert!(
            !codesense_pty_core::protocol::socket_path(root.path()).exists(),
            "a status read never launches a server"
        );

        let started = std::cell::RefCell::new(None);
        let overview =
            start_terminal_server_with(app.handle(), Arc::new(RecordingSink::default()), |root| {
                *started.borrow_mut() = Some(TestServer::start_at(root));
                Ok(())
            })
            .unwrap();
        assert!(started.borrow().is_some(), "Start launched one");
        assert_eq!(overview.state, TerminalServerRun::Running);
        assert_eq!(overview.sessions, Some(0));
        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::Running
        );
    }

    /// SV-7: a real Stop of a running terminal server: it ends — its session
    /// ends and its exit reaches the app — with no "restarted" notice, even
    /// as the lost connection's reconnect finds it gone; nothing starts it
    /// again, the warm pool included, until a person does.
    #[test]
    fn sv7_stop_ends_the_server_and_its_sessions_quietly() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = codesense_session_server::testing::short_root();
        let sidecar = TestServer::start_stoppable_at(root.path());
        let (app, sink) = app_reemitting(in_process(&runtime), &sidecar);
        reconnect_on_loss(app.handle());
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        sidecar.track("ended-by-stop");
        client
            .spawn(
                sh("sleep 60; :"),
                registration("ended-by-stop", None, false),
            )
            .unwrap();
        let told = heard(&app, TERMINAL_SERVER_RESTARTED_EVENT.to_string());
        let expected = terminal_server_overview(app.handle())
            .expected
            .expect("what a stop binds to");

        let stopped = stop_terminal_server_with(
            app.handle(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| {
                sidecar.stop();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(stopped.ended, 1);
        assert!(wait_until(|| sink.others.exit("ended-by-stop").is_some()));
        assert!(state.connected().is_none());
        assert_eq!(
            terminal_server_overview(app.handle()).state,
            TerminalServerRun::Stopped
        );
        assert!(!crate::commands::pty_host::pool_may_spawn(app.handle()));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(told.lock().unwrap().is_empty(), "no restart notice");
        assert!(
            !codesense_pty_core::protocol::socket_path(root.path()).exists(),
            "nothing started it again"
        );

        let started = std::cell::RefCell::new(None);
        start_terminal_server_with(app.handle(), Arc::new(RecordingSink::default()), |root| {
            *started.borrow_mut() = Some(TestServer::start_at(root));
            Ok(())
        })
        .unwrap();
        assert!(crate::commands::pty_host::pool_may_spawn(app.handle()));
    }

    /// SV-7: Restart of a running (compatible) terminal server, end to end: it
    /// stops — its session ends — and a new one serves in its place.
    #[test]
    fn sv7_restart_replaces_a_running_server() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = codesense_session_server::testing::short_root();
        let old = TestServer::start_stoppable_at(root.path());
        let (app, sink) = app_reemitting(in_process(&runtime), &old);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        old.track("ended-by-restart");
        client
            .spawn(
                sh("sleep 60; :"),
                registration("ended-by-restart", None, false),
            )
            .unwrap();
        let before = terminal_server_overview(app.handle());
        let expected = before.expected.clone().unwrap();

        let replacement = std::sync::Mutex::new(None);
        replace_terminal_server_with(
            app.handle(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| {
                old.stop();
                Ok(())
            },
            |root| {
                *replacement.lock().unwrap() = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .unwrap();
        assert!(wait_until(|| sink
            .others
            .exit("ended-by-restart")
            .is_some()));
        let after = terminal_server_overview(app.handle());
        assert_eq!(after.state, TerminalServerRun::Running);
        assert_eq!(after.sessions, Some(0));
        assert_ne!(after.expected.unwrap().epoch, expected.epoch);
        assert!(replacement.lock().unwrap().is_some());
    }

    /// SV-7: the MCP tools refuse without their confirmation, a stop without
    /// `expected` or when the other instances connected changed since the
    /// review, a restart of a running server without `expected`; a stop's
    /// result carries what it ended and what was confirmed to end.
    #[test]
    fn sv7_the_mcp_tools_refuse_unconfirmed_and_report_what_ended() {
        use crate::mcp::terminal_server_tools::{
            self as tools, EndTerminalServerParams, TerminalServerExpected,
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = codesense_session_server::testing::short_root();
        let sidecar = TestServer::start_stoppable_at(root.path());
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        sidecar.track("by-mcp");
        client
            .spawn(sh("sleep 60; :"), registration("by-mcp", None, false))
            .unwrap();
        let reviewed = terminal_server_overview(app.handle()).expected.unwrap();
        let expected = || TerminalServerExpected {
            server_id: reviewed.server_id.clone(),
            epoch: reviewed.epoch.clone(),
            pid: reviewed.pid,
            session_ids: reviewed.session_ids.clone(),
            other_instances: Some(0),
        };
        let params =
            |confirm: bool, expected: Option<TerminalServerExpected>| EndTerminalServerParams {
                confirm_end_sessions: confirm,
                expected,
                reviewed_incompatible: None,
            };
        let status = tools::status(app.handle());
        assert_eq!(status["state"], "running");
        assert_eq!(status["ends"], "ends 1 running session");
        assert_eq!(status["expected"]["otherInstances"], 0);
        let never = |_: u32| -> Result<(), String> { panic!("nothing ends") };

        let refused = tools::stop(
            app.handle(),
            params(false, Some(expected())),
            Arc::new(RecordingSink::default()),
            never,
        );
        assert!(refused.unwrap_err().contains("confirmEndSessions"));
        let refused = tools::stop(
            app.handle(),
            params(true, None),
            Arc::new(RecordingSink::default()),
            never,
        );
        assert!(refused.unwrap_err().contains("expected is required"));
        // Another instance connected since the review: its tabs would end too.
        let mut stale = expected();
        stale.other_instances = Some(1);
        let refused = tools::stop(
            app.handle(),
            params(true, Some(stale)),
            Arc::new(RecordingSink::default()),
            never,
        );
        assert!(refused
            .unwrap_err()
            .contains("changed since you read the status (now 0"));
        let refused = tools::restart(
            app.handle(),
            params(true, None),
            Arc::new(RecordingSink::default()),
            never,
            |_| panic!("nothing starts"),
            || panic!("not incompatible"),
        );
        assert!(refused.unwrap_err().contains("expected is required"));

        let stopped = tools::stop(
            app.handle(),
            params(true, Some(expected())),
            Arc::new(RecordingSink::default()),
            |_| {
                sidecar.stop();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(stopped["ended"], 1);
        assert_eq!(stopped["ends"], "ends 1 running session");
        assert_eq!(stopped["otherInstances"], 0);
    }

    /// A read about one session may be true unless it is known false.
    #[test]
    fn only_a_known_no_rules_a_session_out() {
        assert!(Liveness::Known(true).may_be_true());
        assert!(Liveness::<bool>::Unknown.may_be_true());
        assert!(!Liveness::Known(false).may_be_true());
    }

    /// A layout tab whose sidecar cannot take its reservation (the connection
    /// lost before the spawn ran) opens in-process instead, its spawn pinned
    /// there; once the spawn ran on the sidecar, a failure is final.
    #[test]
    fn a_tab_the_sidecar_cannot_reserve_opens_in_process() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        client.sever();
        assert!(wait_until(|| client.is_idle(&sid("x")).is_err()));
        let entry = open_tab_on(
            test_backend(&local, Some(client)),
            tab_request("fell-open"),
            |host| {
                assert!(host.client().is_none(), "pinned in-process");
                register(
                    &runtime,
                    &local,
                    "sleep 60",
                    registration("fell-open", None, false),
                );
                Ok(())
            },
            |_| Ok(()),
            || Ok((local.clone(), test_backend(&local, None))),
        )
        .unwrap();
        assert_eq!(entry.session, sid("fell-open"));
        assert_eq!(placed(&*local), [sid("fell-open")]);
        local.kill(&sid("fell-open")).unwrap();
    }

    #[test]
    fn stale_checks_before_and_after_reservation_leave_no_orphan() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);

        let before = open_tab_resolved(
            test_backend(&local, None),
            tab_request("stale-before"),
            |_| panic!("spawn must not run before check one"),
            |_| Err("stale before reserve".into()),
            || panic!("stale exact placement never falls open"),
        );
        assert_eq!(before.unwrap_err(), "stale before reserve");
        assert!(placed(&*local).is_empty());

        let checks = std::cell::Cell::new(0);
        let after = open_tab_resolved(
            test_backend(&local, None),
            tab_request("stale-after"),
            |_| panic!("spawn must not run after failed check two"),
            |_| {
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    Ok(())
                } else {
                    Err("stale after reserve".into())
                }
            },
            || panic!("stale exact placement never falls open"),
        );
        assert_eq!(after.unwrap_err(), "stale after reserve");
        assert!(
            placed(&*local).is_empty(),
            "failed FinishTab releases reservation"
        );

        open_tab_resolved(
            test_backend(&local, None),
            tab_request("stale-after"),
            |_| {
                register(
                    &runtime,
                    &local,
                    "sleep 60",
                    registration("stale-after", None, false),
                );
                Ok(())
            },
            |_| Ok(()),
            || panic!("in-process placement does not retry"),
        )
        .unwrap();
        assert_eq!(placed(&*local), [sid("stale-after")]);
        local.kill(&sid("stale-after")).unwrap();
    }

    /// A viewport a reconnect cannot carry across is named to the frontend,
    /// whose pane re-subscribes — never left silently frozen.
    #[test]
    fn a_viewport_that_cannot_be_carried_across_is_named_to_the_frontend() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        sidecar.track("viewed");
        client
            .spawn(sh("sleep 60; :"), registration("viewed", None, false))
            .unwrap();
        let local = subscribe(app.handle(), "viewed", &ViewportId::new("webview"))
            .unwrap()
            .stream_id
            .0;
        let lost = heard(&app, TERMINAL_STREAMS_LOST_EVENT.to_string());
        client.sever();
        assert!(wait_until(|| state.connected().is_none()));
        // While the app is away its session leaves, so the viewport cannot be
        // re-attached.
        sidecar.host().terminal().kill(&sid("viewed")).unwrap();
        assert!(wait_until(|| sidecar
            .host()
            .terminal()
            .has_session(&sid("viewed"))
            == Ok(false)));
        let again = state.reconnected(sidecar.root()).expect("reconnected");
        carry_across(app.handle(), &state, &again);
        assert!(wait_until(|| lost
            .lock()
            .unwrap()
            .contains(&format!("\"streams\":[{local}]"))));
    }

    /// The spawn host is resolved before the one-use launch bindings are
    /// consumed, so a spawn an incompatible sidecar refuses burns no token.
    #[test]
    fn the_spawn_host_resolves_before_the_bindings_are_consumed() {
        let spawn = include_str!("pty.rs")
            .split("pub(crate) async fn spawn_pty_inner")
            .nth(1)
            .unwrap();
        let at = |needle: &str| {
            spawn
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} in spawn_pty_inner"))
        };
        let host = at("resolve_spawn_backend(");
        for consume in [
            "validate_environment_binding(",
            "consume_resolved_launch_binding(",
            "consume_environment_binding(",
        ] {
            assert!(host < at(consume), "resolve_spawn_backend before {consume}");
        }
    }

    #[test]
    fn backend_resolution_has_no_legacy_signature_or_implicit_remote_fallback() {
        let source = include_str!("pty_backend.rs");
        assert!(
            !source.contains("pub fn resolve_terminal_backend(\n    app: &tauri::AppHandle,\n)")
        );
        assert!(source.contains("resolve_terminal_backend(app, LOCAL_MACHINE_ID)"));
        assert!(!source.contains(&["into_local_spawn_", "client"].concat()));

        let remote_only = source
            .split("fn resolve_remote_backend(")
            .nth(1)
            .unwrap()
            .split("fn select_local_backend(")
            .next()
            .unwrap();
        for forbidden in [
            "SessionServerState",
            "TerminalHost",
            "connect_or_launch",
            "launch_sidecar",
            "LOCAL_MACHINE_ID",
        ] {
            assert!(
                !remote_only.contains(forbidden),
                "remote resolver cannot consult {forbidden}"
            );
        }

        let startup = include_str!("../lib.rs");
        assert!(startup.contains("commands::machines::LOCAL_MACHINE_ID"));
    }

    fn sidecar_client(sidecar: &TestServer) -> Arc<SessionServerClient> {
        SessionServerState::default()
            .connect_or_launch(sidecar.root(), Arc::new(RecordingSink::default()), |_| {
                Err("no launch".into())
            })
            .unwrap()
    }

    #[test]
    fn healthy_local_sidecar_wins_before_in_process_is_consulted() {
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        let consulted = std::cell::Cell::new(0);
        let resolved = select_local_backend(Some(client.clone()), || {
            consulted.set(consulted.get() + 1);
            Err(TerminalBackendResolveError::Local("must not run".into()))
        })
        .unwrap();
        assert_eq!(consulted.get(), 0);
        assert_eq!(resolved.server_id(), client.server_id());
        assert!(matches!(
            resolved.policy,
            TerminalBackendPolicy::LocalSidecar
        ));
    }

    #[test]
    fn absent_or_ordinary_failed_local_sidecar_selects_in_process_only_then() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let consulted = std::cell::Cell::new(0);
        for absent_reason in ["missing state", "missing root", "ordinary sidecar failure"] {
            let resolved = select_local_backend(None, || {
                consulted.set(consulted.get() + 1);
                Ok(local.clone())
            })
            .unwrap();
            assert_eq!(
                resolved.server_id(),
                local.identity().unwrap().server.id.to_string(),
                "{absent_reason}"
            );
            assert!(matches!(
                resolved.policy,
                TerminalBackendPolicy::LocalInProcess
            ));
        }
        assert_eq!(consulted.get(), 3);

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state = SessionServerState::default();
        assert!(local_sidecar_after(
            app.handle(),
            &state,
            Err(ConnectError::Refused("ordinary refusal".into()))
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn incompatible_local_sidecar_is_structured_and_emits_once() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let heard = heard(&app, TERMINAL_SERVER_INCOMPATIBLE_EVENT.to_string());
        let state = SessionServerState::default();
        let skew = Skew {
            reason: codesense_host_proto::SkewReason::ProtoMajor {
                peer: PROTO_MAJOR - 1,
            },
            live_sessions: Some(3),
            host_pid: Some(7),
        };
        let error = match local_sidecar_after(
            app.handle(),
            &state,
            Err(ConnectError::Incompatible(skew)),
        ) {
            Err(error) => error,
            Ok(_) => panic!("incompatible sidecar must be refused"),
        };
        assert!(matches!(
            error,
            TerminalBackendResolveError::LocalIncompatible(IncompatibleTerminalServer {
                live_sessions: Some(3),
                server_protocol,
                app_protocol: PROTO_MAJOR,
                ..
            }) if server_protocol == PROTO_MAJOR - 1
        ));
        assert_eq!(heard.lock().unwrap().matches("serverProtocol").count(), 1);
    }

    /// Both hosts live: an id the in-process registry has stays in-process
    /// (so a respawn displaces it there), any other goes to the sidecar; with
    /// no sidecar everything is in-process. Verbs reach the owning host.
    #[test]
    fn a_session_belongs_to_the_host_that_has_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        register(
            &runtime,
            &local,
            "sleep 60",
            registration("fallback", None, false),
        );

        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("hosted");
        // A trailing command keeps the shell alive as the parent of `sleep`.
        client
            .spawn(sh("sleep 60; :"), registration("hosted", None, false))
            .unwrap();

        let owner = |id: &str| match sidecar_owner(&local, id, || Some(client.clone())) {
            None => "in-process",
            Some(_) => "sidecar",
        };
        assert_eq!(owner("fallback"), "in-process");
        assert_eq!(owner("hosted"), "sidecar");
        assert_eq!(owner("new"), "sidecar", "a new id goes to the sidecar");
        assert!(sidecar_owner(&local, "hosted", || None).is_none());

        let verbs = route_session(&local, "hosted", || Some(client.clone()));
        assert!(
            wait_until(|| verbs.is_idle(&sid("hosted")) == Ok(false)),
            "the sidecar answers for its session"
        );
        verbs.kill(&sid("hosted")).unwrap();
        assert!(wait_until(|| sidecar
            .host()
            .terminal()
            .has_session(&sid("hosted"))
            == Ok(false)));
        route_session(&local, "fallback", || Some(client))
            .kill(&sid("fallback"))
            .unwrap();
        assert_eq!(local.has_session(&sid("fallback")), Ok(false));
    }

    /// The exit reap runs in each host: the sidecar's and the in-process
    /// registry's non-persisting sessions both go.
    #[test]
    fn the_exit_reap_runs_in_every_host() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let local_pid = register(
            &runtime,
            &local,
            "sleep 60",
            registration("local-plain", None, false),
        );

        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("hosted-plain");
        let hosted_pid = client
            .spawn(sh("sleep 60"), registration("hosted-plain", None, false))
            .unwrap()
            .unwrap();

        reap_hosts(Some(&client), Some(&local));
        assert!(wait_until(|| !alive(local_pid) && !alive(hosted_pid)));
        assert_eq!(local.has_session(&sid("local-plain")), Ok(false));
        assert_eq!(
            sidecar.host().terminal().has_session(&sid("hosted-plain")),
            Ok(false)
        );
    }

    /// A registration bound to `workspace`, anchored at `home` (a worktree
    /// session), with shell integration so its OSC 7 cwd is tracked.
    fn anchored(id: &str, workspace: &str, home: &std::path::Path) -> SessionRegistration {
        let mut reg = registration(id, None, false);
        reg.meta.workspace_id = Some(workspace.to_string());
        reg.home_cwd = Some(home.to_path_buf());
        reg.osc_enabled = true;
        reg
    }

    /// The "anything alive" reads answer across both hosts, so a worktree
    /// holding a live sidecar agent is never read as dormant and reaped, and a
    /// reloaded webview re-attaches both hosts' tabs.
    #[test]
    fn reads_answer_across_both_hosts() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let local_home = tempfile::tempdir().unwrap();
        let hosted_home = tempfile::tempdir().unwrap();
        register(
            &runtime,
            &local,
            "sleep 60",
            anchored("local-agent", "ws-local", local_home.path()),
        );
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("hosted-agent");
        client
            .spawn(
                sh("printf '\\033]7;file://host/tmp/hosted-cwd\\007'; sleep 60"),
                anchored("hosted-agent", "ws-hosted", hosted_home.path()),
            )
            .unwrap();

        let hosts = Hosts {
            in_process: local.clone(),
            sidecar: Ok(Some(client.clone())),
        };
        let dirs = hosts
            .live_session_home_dirs()
            .known()
            .expect("both hosts answer");
        assert!(dirs.contains(&local_home.path().to_path_buf()));
        assert!(
            dirs.contains(&hosted_home.path().to_path_buf()),
            "the sidecar agent's worktree is live: {dirs:?}"
        );
        let ids = [sid("local-agent"), sid("hosted-agent"), sid("gone")];
        assert_eq!(
            hosts.alive_session_ids(&ids),
            Liveness::Known(HashSet::from([
                "local-agent".to_string(),
                "hosted-agent".to_string()
            ]))
        );
        assert_eq!(
            hosts.has_session(&sid("hosted-agent")),
            Liveness::Known(true)
        );
        assert_eq!(hosts.has_session(&sid("gone")), Liveness::Known(false));
        assert_eq!(hosts.session_len_and_dead(), Some((2, 0)));
        assert!(local.live_child_pids(&ids).unwrap()[0].is_some());
        let hosted_pids = client.live_child_pids(&ids).unwrap();
        assert!(hosted_pids[1].is_some() && hosted_pids[2].is_none());
        assert!(wait_until(|| client.session_cwd(&sid("hosted-agent"))
            == Ok(Some(PathBuf::from("/tmp/hosted-cwd")))));
        local.kill(&sid("local-agent")).unwrap();
    }

    /// SW-RT-6d (AC-6): unknown is not empty. With a sidecar that may hold
    /// sessions but cannot be asked, every read that would say "not running"
    /// is `Unknown` — never the in-process answer alone — while what the
    /// in-process host knows for itself still stands. With no sidecar running
    /// at all, the in-process answer is the whole, known answer.
    #[test]
    fn an_unreachable_sidecar_makes_the_reads_unknown_not_empty() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let home = tempfile::tempdir().unwrap();
        register(
            &runtime,
            &local,
            "sleep 60",
            anchored("local-agent", "ws-local", home.path()),
        );
        let cut_off = Hosts {
            in_process: local.clone(),
            sidecar: Err(PortUnavailable("the session server is unreachable".into())),
        };
        let ids = [sid("local-agent"), sid("maybe-hosted")];
        assert_eq!(cut_off.live_session_home_dirs(), Liveness::Unknown);
        assert_eq!(cut_off.alive_session_ids(&ids), Liveness::Unknown);
        assert_eq!(cut_off.has_session(&sid("maybe-hosted")), Liveness::Unknown);
        assert_eq!(
            cut_off.has_session(&sid("local-agent")),
            Liveness::Known(true),
            "the in-process host knows its own"
        );
        assert!(local.live_child_pids(&ids[..1]).unwrap()[0].is_some());
        assert_eq!(cut_off.session_len_and_dead(), None);

        let none_running = Hosts {
            in_process: local.clone(),
            sidecar: Ok(None),
        };
        assert_eq!(
            none_running.live_session_home_dirs(),
            Liveness::Known(vec![home.path().to_path_buf()])
        );
        assert_eq!(
            none_running.alive_session_ids(&ids),
            Liveness::Known(HashSet::from(["local-agent".to_string()]))
        );
        assert_eq!(
            none_running.has_session(&sid("maybe-hosted")),
            Liveness::Known(false)
        );
        local.kill(&sid("local-agent")).unwrap();
    }

    /// SW-AV-1 (INV-3): `live_session_ids` is the live-PTY arm of the union roster.
    /// It spans BOTH hosts (an in-process bare session AND a sidecar-hosted one) and
    /// KEEPS registry-less bare sessions (no workspace). The in-process host's error is the
    /// caller's, so with the sidecar unreachable the in-process set still surfaces
    /// (never collapsed to empty); the sidecar's contribution is simply absent.
    #[test]
    fn live_session_ids_spans_hosts_and_keeps_bare_sessions() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        // Registry-less bare sessions: no workspace binding — the union roster
        // keeps them.
        register(
            &runtime,
            &local,
            "sleep 60",
            registration("bare-local", None, false),
        );
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("bare-hosted");
        client
            .spawn(sh("sleep 60; :"), registration("bare-hosted", None, false))
            .unwrap();

        let hosts = Hosts {
            in_process: local.clone(),
            sidecar: Ok(Some(client.clone())),
        };
        // Spans both hosts (the sidecar child needs a moment to come alive).
        assert!(wait_until(|| hosts.live_session_ids()
            == Ok(HashSet::from([
                "bare-local".to_string(),
                "bare-hosted".to_string()
            ]))));
        // In-process error is the caller's: an unreachable sidecar is tolerated,
        // the in-process set still surfaces (never collapsed to empty).
        let cut_off = Hosts {
            in_process: local.clone(),
            sidecar: Err(PortUnavailable("the session server is unreachable".into())),
        };
        assert_eq!(
            cut_off.live_session_ids(),
            Ok(HashSet::from(["bare-local".to_string()]))
        );

        client.kill(&sid("bare-hosted")).unwrap();
        local.kill(&sid("bare-local")).unwrap();
    }

    /// A mock app with both hosts managed, the sidecar connected.
    fn app_with_hosts(
        local: Arc<TerminalHost>,
        sidecar: &TestServer,
    ) -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state = SessionServerState::default();
        let identity = local.identity().unwrap();
        state
            .admission
            .register_in_process(
                &crate::terminal::session_server::mint_event_namespace(
                    identity.server.id.as_str(),
                    &identity.epoch,
                )
                .unwrap(),
            )
            .unwrap();
        app.manage(local);
        state
            .connect_or_launch(sidecar.root(), Arc::new(RecordingSink::default()), |_| {
                Err("no launch".into())
            })
            .unwrap();
        app.manage(state);
        app
    }

    fn admitted_reference<R: Runtime>(
        app: &AppHandle<R>,
        slot: RuntimeMemberSlot,
        session: &str,
    ) -> crate::commands::machines::RuntimeSessionReference {
        let row = app
            .state::<SessionServerState>()
            .admission
            .baseline()
            .rows
            .into_iter()
            .find(|row| row.member.slot == slot)
            .unwrap();
        let RuntimeAdmissionState::Live { namespace } = row.state else {
            panic!("live member")
        };
        crate::commands::machines::RuntimeSessionReference::Admitted(Box::new(
            crate::commands::machines::RuntimeSessionTarget {
                runtime: RuntimeMemberTarget {
                    member: row.member,
                    admission_revision: row.revision,
                    namespace,
                },
                session: sid(session),
            },
        ))
    }

    #[test]
    fn rm5aa_legacy_local_lookup_rejects_ambiguous_session() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        register(
            &runtime,
            &local,
            "sleep 60",
            registration("collision", None, false),
        );
        let sidecar = TestServer::start();
        let app = app_with_hosts(local.clone(), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        sidecar.track("collision");
        client
            .spawn(sh("sleep 60"), registration("collision", None, false))
            .unwrap();
        let calls = std::cell::Cell::new(0);
        let result = with_session_reference(
            app.handle(),
            &crate::commands::machines::RuntimeSessionReference::Legacy("collision".into()),
            false,
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("ambiguous"));
        assert_eq!(calls.get(), 0);
        assert!(local.has_session(&sid("collision")).unwrap());
        assert!(client.has_session(&sid("collision")).unwrap());
        local.kill(&sid("collision")).unwrap();
    }

    #[test]
    fn rm5aa_local_session_verbs_target_exact_member_with_colliding_ids() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let local = Arc::new(TerminalHost::new(
            sidecar.host().terminal().identity().unwrap().server,
            Arc::new(RecordingSink::default()),
            runtime.handle().clone(),
        ));
        let app = app_with_hosts(local.clone(), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        let local_identity = local.identity().unwrap();
        assert_eq!(local_identity.server.id.as_str(), client.server_id());
        assert_ne!(local_identity.epoch, client.epoch());
        let replies =
            admitted_layout_snapshots(app.handle(), &ClientBootId::new("shared-id")).unwrap();
        assert_eq!(
            replies
                .iter()
                .filter(|reply| matches!(reply, RuntimeLayoutReply::Live { .. }))
                .count(),
            2
        );
        assert_eq!(
            app.state::<SessionServerState>()
                .admission
                .baseline()
                .rows
                .len(),
            2
        );
        sidecar.track("collision");
        let local_pid = std::cell::Cell::new(0);
        let sidecar_pid = std::cell::Cell::new(0);
        local
            .open_tab(
                tab_request("collision"),
                Box::new(|| {
                    local_pid.set(register(
                        &runtime,
                        &local,
                        "sleep 60",
                        registration("collision", None, false),
                    ));
                    Ok(())
                }),
            )
            .unwrap();
        client
            .open_tab(
                tab_request("collision"),
                Box::new(|| {
                    client
                        .spawn(sh("sleep 60"), registration("collision", None, false))
                        .map(|pid| sidecar_pid.set(pid.unwrap()))
                        .map_err(|failure| failure.message)
                }),
            )
            .unwrap();
        let workspace = tab_request("collision").workspace;
        for (slot, name) in [
            (RuntimeMemberSlot::InProcess, "local-only"),
            (RuntimeMemberSlot::Sidecar, "sidecar-only"),
        ] {
            let crate::commands::machines::RuntimeSessionReference::Admitted(reference) =
                admitted_reference(app.handle(), slot, "collision")
            else {
                unreachable!()
            };
            let target = crate::commands::machines::RuntimeWorkspaceTarget {
                runtime: reference.runtime,
                workspace: workspace.clone(),
            };
            runtime
                .block_on(crate::commands::pty_layout::in_workspace_hosts(
                    app.handle().clone(),
                    "partition-test",
                    crate::commands::machines::RuntimeWorkspaceReference::Admitted(Box::new(
                        target.clone(),
                    )),
                    move |domain, workspace| {
                        domain.set_descriptor(
                            workspace,
                            codesense_pty_core::layout::WorkspaceDescriptor {
                                display_name: name.into(),
                                ..Default::default()
                            },
                        )
                    },
                ))
                .unwrap();
            let mut stale = target;
            stale.runtime.admission_revision =
                crate::commands::machines::RuntimeCounter::new(u64::MAX);
            assert!(runtime
                .block_on(crate::commands::pty_layout::in_workspace_hosts(
                    app.handle().clone(),
                    "stale-partition-test",
                    crate::commands::machines::RuntimeWorkspaceReference::Admitted(Box::new(stale)),
                    |_, _| panic!("stale target must not invoke a workspace effect"),
                ))
                .is_err());
        }
        assert_eq!(
            local
                .layout_snapshot(&app_viewer(), &ClientBootId::new("partition"))
                .unwrap()
                .workspaces[&workspace]
                .descriptor
                .display_name,
            "local-only"
        );
        assert_eq!(
            client
                .layout_snapshot(&app_viewer(), &ClientBootId::new("partition"))
                .unwrap()
                .workspaces[&workspace]
                .descriptor
                .display_name,
            "sidecar-only"
        );
        for (slot, expected) in [
            (RuntimeMemberSlot::InProcess, local_pid.get()),
            (RuntimeMemberSlot::Sidecar, sidecar_pid.get()),
        ] {
            let reference = admitted_reference(app.handle(), slot, "collision");
            let actual =
                with_session_reference(app.handle(), &reference, false, |host, session| {
                    Ok(host.live_child_pids(std::slice::from_ref(session))?)
                })
                .unwrap();
            assert_eq!(actual, vec![Some(expected)]);
        }
        let reference = admitted_reference(app.handle(), RuntimeMemberSlot::Sidecar, "collision");
        with_session_reference(app.handle(), &reference, false, |host, session| {
            Ok(host.kill(session)?)
        })
        .unwrap();
        assert!(local.has_session(&sid("collision")).unwrap());
        local.kill(&sid("collision")).unwrap();
    }

    #[test]
    fn rm5aa_workspace_reveal_keeps_recorded_owner_across_peer_collision_and_rebind() {
        use crate::commands::machines::RuntimeSessionReference;
        use crate::commands::{dashboard_launch::DashboardLaunchState, workspace_open};
        use codesense_pty_core::layout::{Placement, WorkspaceDescriptor};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let local = in_process(&runtime);
        let app = app_with_hosts(local.clone(), &sidecar);
        app.manage(DashboardLaunchState::new(4));
        let launch = app.state::<DashboardLaunchState>();
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        let session = "reveal-collision";
        let workspace = codesense_pty_core::domain::WorkspaceKey::new("acme:team|reveal");
        let mut open = tab_request(session);
        open.workspace = workspace.clone();
        open.placement = Placement::Background;
        open.activate = false;
        open.descriptor = Some(WorkspaceDescriptor {
            display_name: "reveal".into(),
            kind: "story".into(),
            org: Some("acme".into()),
            team: Some("team".into()),
            ..Default::default()
        });
        struct LocalCleanup(Arc<TerminalHost>, PtySessionId);
        impl Drop for LocalCleanup {
            fn drop(&mut self) {
                let _ = self.0.kill(&self.1);
            }
        }
        let _cleanup = LocalCleanup(local.clone(), sid(session));
        local
            .open_tab(
                open.clone(),
                Box::new(|| {
                    register(
                        &runtime,
                        &local,
                        "sleep 60",
                        registration(session, None, false),
                    );
                    Ok(())
                }),
            )
            .unwrap();
        sidecar.track(session);
        client
            .open_tab(
                open,
                Box::new(|| {
                    client
                        .spawn(sh("sleep 60"), registration(session, None, false))
                        .map(|_| ())
                        .map_err(|failure| failure.message)
                }),
            )
            .unwrap();
        let RuntimeSessionReference::Admitted(reference) =
            admitted_reference(app.handle(), RuntimeMemberSlot::Sidecar, session)
        else {
            unreachable!()
        };
        let owner = state
            .admission
            .effects
            .reserve(reference.runtime, session)
            .unwrap();
        let RuntimeSessionReference::Admitted(peer_reference) =
            admitted_reference(app.handle(), RuntimeMemberSlot::InProcess, session)
        else {
            unreachable!()
        };
        let peer = state
            .admission
            .effects
            .reserve(peer_reference.runtime, session)
            .unwrap();
        assert!(
            state.admission.effects.legacy_permit(&owner).is_none(),
            "ambiguous legacy events stay forbidden"
        );
        launch.begin("owned-reveal").unwrap();
        launch
            .bind_owned("owned-reveal", session, None, None, Some(owner.clone()))
            .unwrap();
        let boot = ClientBootId::new("reveal-test");
        let peer_before = local.layout_snapshot(&app_viewer(), &boot).unwrap();
        let peer_process = local.live_child_pids(&[sid(session)]).unwrap();
        let owner_before = client.layout_snapshot(&app_viewer(), &boot).unwrap();
        let captured =
            workspace_open::capture_workspace_reveal_owner(app.handle(), "owned-reveal").unwrap();
        assert_eq!(captured, owner);
        let passive = workspace_open::complete_workspace_reveal(
            app.handle(),
            "owned-reveal",
            false,
            &captured,
            admitted_layout_snapshots(app.handle(), &boot).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(passive["focus"], false);
        assert_eq!(
            client.layout_snapshot(&app_viewer(), &boot).unwrap(),
            owner_before
        );
        let focused = workspace_open::complete_workspace_reveal(
            app.handle(),
            "owned-reveal",
            true,
            &captured,
            admitted_layout_snapshots(app.handle(), &boot).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(focused["runtimeTarget"]["member"]["slot"], "sidecar");
        let focused_layout = client.layout_snapshot(&app_viewer(), &boot).unwrap();
        assert_eq!(
            focused_layout.workspaces[&workspace].tabs[0].placement,
            Placement::Tab
        );
        assert_eq!(focused_layout.viewer.active[&workspace], sid(session));
        assert_eq!(
            local.layout_snapshot(&app_viewer(), &boot).unwrap(),
            peer_before
        );

        // Hold the real snapshot result across an actual registry rebind.
        let held = admitted_layout_snapshots(app.handle(), &boot).unwrap();
        assert!(launch.abort("owned-reveal"));
        launch.begin("owned-reveal").unwrap();
        launch
            .bind_owned("owned-reveal", session, None, None, Some(peer.clone()))
            .unwrap();
        assert!(workspace_open::complete_workspace_reveal(
            app.handle(),
            "owned-reveal",
            true,
            &captured,
            held
        )
        .is_err());
        assert!(workspace_open::validate_workspace_reveal_owner(
            app.handle(),
            "owned-reveal",
            &captured
        )
        .is_err());
        assert_eq!(
            client.layout_snapshot(&app_viewer(), &boot).unwrap(),
            focused_layout
        );
        assert_eq!(
            local.layout_snapshot(&app_viewer(), &boot).unwrap(),
            peer_before
        );

        assert!(launch.abort("owned-reveal"));
        launch.begin("owned-reveal").unwrap();
        launch
            .bind_owned("owned-reveal", session, None, None, Some(owner.clone()))
            .unwrap();
        client.sever();
        assert!(wait_until(|| !client.is_alive()));
        assert_eq!(launch.resolve_owned_pty("owned-reveal"), Some(owner));
        assert!(
            workspace_open::capture_workspace_reveal_owner(app.handle(), "owned-reveal").is_err()
        );
        assert!(workspace_open::complete_workspace_reveal(
            app.handle(),
            "owned-reveal",
            true,
            &captured,
            admitted_layout_snapshots(app.handle(), &boot).unwrap()
        )
        .is_err());
        assert_eq!(
            local.layout_snapshot(&app_viewer(), &boot).unwrap(),
            peer_before
        );
        assert_eq!(
            local.live_child_pids(&[sid(session)]).unwrap(),
            peer_process
        );

        assert!(launch.abort("owned-reveal"));
        launch.begin("owned-reveal").unwrap();
        launch.bind("owned-reveal", session, None).unwrap();
        assert!(
            workspace_open::capture_workspace_reveal_owner(app.handle(), "owned-reveal").is_err()
        );
        assert!(launch.abort("owned-reveal"));
        launch.begin("owned-reveal").unwrap();
        launch
            .bind_owned("owned-reveal", session, None, None, Some(peer))
            .unwrap();
        launch.mark_headless(
            "owned-reveal",
            Arc::default(),
            Arc::default(),
            Arc::default(),
        );
        assert!(
            workspace_open::capture_workspace_reveal_owner(app.handle(), "owned-reveal").is_err()
        );
        assert_eq!(
            local.layout_snapshot(&app_viewer(), &boot).unwrap(),
            peer_before
        );
    }

    #[test]
    fn rm5aa_historical_or_stale_target_performs_zero_side_effects() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let app = app_with_hosts(in_process(&runtime), &sidecar);
        let mut reference =
            admitted_reference(app.handle(), RuntimeMemberSlot::InProcess, "missing");
        let crate::commands::machines::RuntimeSessionReference::Admitted(target) = &mut reference
        else {
            unreachable!()
        };
        target.runtime.namespace.generation =
            crate::commands::machines::RuntimeCounter::new(u64::MAX);
        let calls = std::cell::Cell::new(0);
        assert!(
            with_session_reference(app.handle(), &reference, false, |_, _| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn rm5aa_used_unreachable_sidecar_preserves_in_process_layout() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let local = in_process(&runtime);
        let app = app_with_hosts(local.clone(), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        client.sever();
        assert!(wait_until(|| !client.is_alive()));
        let replies = admitted_layout_snapshots(app.handle(), &ClientBootId::new("test")).unwrap();
        assert!(replies.iter().any(|reply| matches!(reply, RuntimeLayoutReply::Live { target, snapshot, .. }
            if target.member.slot == RuntimeMemberSlot::InProcess && snapshot.epoch == local.identity().unwrap().epoch)));
        assert!(!replies.iter().any(
            |reply| matches!(reply, RuntimeLayoutReply::Live { target, .. }
            if target.member.slot == RuntimeMemberSlot::Sidecar)
        ));
    }

    /// An MCP interactive session hosted by the sidecar is addressable: the
    /// liveness guard (`require_live_session`'s `has_session`) sees it, and
    /// the submit path reaches it through the resolved host.
    #[test]
    fn a_sidecar_session_is_addressable_through_the_boundary() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let app = app_with_hosts(in_process(&runtime), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        sidecar.track("mcp-1");
        client
            .spawn(
                sh("printf '$ '; while read line; do printf 'GOT=%s\\n$ ' \"$line\"; done"),
                registration("mcp-1", None, false),
            )
            .unwrap();
        let handle = app.handle();
        assert_eq!(has_session(handle, "mcp-1"), Liveness::Known(true));
        assert_eq!(has_session(handle, "forged"), Liveness::Known(false));
        let host = resolve_session_backend(handle, "mcp-1").unwrap();
        assert_eq!(
            host.submit_shell_line(&sid("mcp-1"), "hello", 100, 5000)
                .map(|outcome| outcome.submitted),
            Ok(true)
        );
        assert!(wait_until(|| sidecar.text("mcp-1").contains("GOT=hello")));
        assert_eq!(
            alive_session_ids(handle, &["mcp-1", "forged"]),
            Liveness::Known(HashSet::from(["mcp-1".to_string()]))
        );
        // SW-AV-1 (INV-3): the union roster's live-PTY arm sees the sidecar session
        // through the same app boundary.
        assert_eq!(
            live_session_ids(handle),
            Ok(HashSet::from(["mcp-1".to_string()]))
        );
        // Readers get the sidecar's emulator text (over `ReadText`), at the
        // session's stream position.
        let text = runtime.block_on(visible_text(handle, "mcp-1")).unwrap();
        assert!(text.text.contains("GOT=hello"), "{text:?}");
        assert_eq!(
            text.seq,
            host.read_terminal_text(
                &sid("mcp-1"),
                1,
                codesense_pty_core::emulator::TextMode::Normal
            )
            .unwrap()
            .seq
        );
        assert!(runtime.block_on(visible_text(handle, "forged")).is_err());
        let tail = codesense_pty_core::domain::visible_text(&*host, &sid("mcp-1"), 5).unwrap();
        assert_eq!(tail.seq, text.seq);
        assert!(tail.text.contains("GOT=hello"));
        assert!(codesense_pty_core::domain::visible_text(&*host, &sid("forged"), 5).is_err());
    }

    /// A background reconnect announces an incompatible sidecar once, not on
    /// every try nor when only its session count changed, and again only
    /// after it was not found incompatible.
    #[test]
    fn a_background_reconnect_announces_an_incompatible_sidecar_once() {
        let skew = || Skew {
            reason: codesense_host_proto::SkewReason::ProtoMajor {
                peer: PROTO_MAJOR - 1,
            },
            live_sessions: None,
            host_pid: None,
        };
        let mut announced = None;
        assert_eq!(
            newly_incompatible(Some(skew()), &mut announced),
            Some(skew())
        );
        assert_eq!(newly_incompatible(Some(skew()), &mut announced), None);
        let recounted = Skew {
            live_sessions: Some(3),
            ..skew()
        };
        assert_eq!(
            newly_incompatible(Some(recounted), &mut announced),
            None,
            "a changed session count is the same incompatibility"
        );
        assert_eq!(newly_incompatible(None, &mut announced), None);
        assert_eq!(
            newly_incompatible(Some(skew()), &mut announced),
            Some(skew())
        );
    }

    /// A background reconnect to a sidecar that runs but does not answer
    /// keeps trying well past the dozen tries it once gave up after, its pause
    /// doubling to a cap; it stops as soon as a try settles.
    #[test]
    fn a_background_reconnect_keeps_trying_until_it_settles() {
        let mut tries = 0;
        let mut pauses = Vec::new();
        let connected = retry_until_settled(
            || {
                tries += 1;
                (tries == 40).then_some(true)
            },
            |pause| pauses.push(pause),
        );
        assert!(connected);
        assert_eq!(tries, 40);
        let secs: Vec<u64> = pauses.iter().map(|pause| pause.as_secs()).collect();
        assert_eq!(&secs[..4], [5, 10, 20, 30]);
        assert!(secs[4..].iter().all(|secs| *secs == 30));
        assert!(!retry_until_settled(|| Some(false), |_| unreachable!()));
    }

    /// The worktree reap's liveness read, through the app's boundary, holds a
    /// live sidecar agent's worktree, so the reap never takes it.
    #[test]
    fn the_reap_sees_a_live_sidecar_agents_worktree() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let app = app_with_hosts(in_process(&runtime), &sidecar);
        let home = tempfile::tempdir().unwrap();
        sidecar.track("agent");
        app.state::<SessionServerState>()
            .connected()
            .unwrap()
            .spawn(sh("sleep 60"), anchored("agent", "ws", home.path()))
            .unwrap();
        let dirs = live_session_home_dirs(app.handle())
            .known()
            .expect("every host answers");
        assert_eq!(dirs, [home.path().to_path_buf()]);
    }

    /// Re-emits `host`'s viewport output through the app's own re-emit path,
    /// as the app's sink does, and records every other event.
    struct Reemit {
        app: AppHandle<tauri::test::MockRuntime>,
        host: StreamHost,
        others: Arc<RecordingSink>,
    }

    impl TerminalEventSink for Reemit {
        fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            match event {
                TerminalDomainEvent::StreamOutput { stream, data, seq } => {
                    crate::commands::pty_host::reemit_stream_item(
                        &self.app,
                        self.host.clone(),
                        stream,
                        StreamItem::Output { data: &data, seq },
                    )
                }
                other => self.others.emit(other),
            }
        }
    }

    /// A mock app with `local` in-process and the app's stream ids, connected
    /// to `sidecar` through a re-emitting sink.
    fn app_reemitting(
        local: Arc<TerminalHost>,
        sidecar: &TestServer,
    ) -> (tauri::App<tauri::test::MockRuntime>, Arc<Reemit>) {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(local);
        app.manage(StreamIds::default());
        let sink = Arc::new(Reemit {
            app: app.handle().clone(),
            host: StreamHost::Sidecar,
            others: Arc::new(RecordingSink::default()),
        });
        let state = SessionServerState::at(Some(sidecar.root().to_path_buf()));
        let sink_app = app.handle().clone();
        let sink_events = sink.others.clone();
        state.admission.set_sink_factory(Arc::new(move |namespace| {
            Arc::new(Reemit {
                app: sink_app.clone(),
                host: StreamHost::LocalSidecar(namespace.clone()),
                others: sink_events.clone(),
            })
        }));
        state
            .connect_or_launch(sidecar.root(), sink.clone(), |_| Err("no launch".into()))
            .unwrap();
        app.manage(state);
        (app, sink)
    }

    /// Every payload the frontend is sent on `event`, as its raw text.
    fn heard(app: &tauri::App<tauri::test::MockRuntime>, event: String) -> Arc<Mutex<String>> {
        use tauri::Listener;
        let heard = Arc::new(Mutex::new(String::new()));
        let into = heard.clone();
        app.listen_any(event, move |event| {
            into.lock().unwrap().push_str(event.payload())
        });
        heard
    }

    fn tab_request(session: &str) -> OpenTab {
        OpenTab {
            viewer: app_viewer(),
            workspace: codesense_pty_core::domain::WorkspaceKey::new("root:acme/app"),
            session: sid(session),
            descriptor: Some(codesense_pty_core::layout::WorkspaceDescriptor {
                display_name: "app".into(),
                kind: "root".into(),
                ..Default::default()
            }),
            name: None,
            role: None,
            tags: Default::default(),
            placement: codesense_pty_core::layout::Placement::Tab,
            index: None,
            activate: true,
        }
    }

    /// The sessions `host`'s layout holds.
    fn placed(host: &dyn TerminalDomain) -> Vec<PtySessionId> {
        host.layout_snapshot(&app_viewer(), &ClientBootId::new("boot"))
            .unwrap()
            .workspaces
            .values()
            .flat_map(|ws| ws.tabs.iter().map(|tab| tab.session.clone()))
            .collect()
    }

    /// With a sidecar, a layout tab's session is spawned on the sidecar and
    /// its tab placed there — in the sidecar's layout, absent from the
    /// in-process one; without one, both happen in-process. Layout and session
    /// never split.
    #[test]
    fn a_tab_is_placed_on_the_host_its_session_runs_on() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("hosted-tab");
        let entry = open_tab_on(
            test_backend(&local, Some(client.clone())),
            tab_request("hosted-tab"),
            |host| {
                host.client()
                    .expect("the spawn is handed the sidecar")
                    .spawn(sh("sleep 60; :"), registration("hosted-tab", None, false))
                    .map(|_| ())
                    .map_err(|failure| failure.message)
            },
            |_| Ok(()),
            || panic!("healthy sidecar does not fall open"),
        )
        .unwrap();
        assert_eq!(entry.session, sid("hosted-tab"));
        assert_eq!(placed(sidecar.host().terminal()), [sid("hosted-tab")]);
        assert!(placed(&*local).is_empty(), "not in the in-process layout");

        open_tab_on(
            test_backend(&local, None),
            tab_request("local-tab"),
            |host| {
                assert!(
                    host.client().is_none(),
                    "the spawn is handed the in-process host"
                );
                register(
                    &runtime,
                    &local,
                    "sleep 60",
                    registration("local-tab", None, false),
                );
                Ok(())
            },
            |_| Ok(()),
            || panic!("in-process placement does not retry"),
        )
        .unwrap();
        assert_eq!(placed(&*local), [sid("local-tab")]);
        assert_eq!(placed(sidecar.host().terminal()), [sid("hosted-tab")]);
        local.kill(&sid("local-tab")).unwrap();
    }

    /// A start through the layout — a visible tab, or a headless agent out of
    /// sight — leaves its session in the host's layout: every placement a user
    /// or MCP start uses is layout membership, the keep-alive input of SW-RT-2's
    /// quit reap.
    #[test]
    fn every_placement_a_start_uses_lands_in_the_layout() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        for (session, placement) in [
            ("shown", codesense_pty_core::layout::Placement::Tab),
            (
                "headless",
                codesense_pty_core::layout::Placement::Background,
            ),
        ] {
            let request = OpenTab {
                placement,
                activate: placement == codesense_pty_core::layout::Placement::Tab,
                ..tab_request(session)
            };
            open_tab_on(
                test_backend(&local, None),
                request,
                |_| {
                    register(
                        &runtime,
                        &local,
                        "sleep 60",
                        registration(session, None, false),
                    );
                    Ok(())
                },
                |_| Ok(()),
                || panic!("in-process placement does not retry"),
            )
            .unwrap();
        }
        let mut held = placed(&*local);
        held.sort();
        assert_eq!(held, [sid("headless"), sid("shown")]);
        local.kill(&sid("shown")).unwrap();
        local.kill(&sid("headless")).unwrap();
    }

    /// The in-process host's viewport output reaches the frontend under the
    /// stream's client-unique id, through the app's one re-emit path.
    #[test]
    fn in_process_output_is_reemitted_under_its_client_id() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(StreamIds::default());
        let local = Arc::new(TerminalHost::new(
            codesense_pty_core::layout::ServerIdentity::ephemeral("test"),
            Arc::new(Reemit {
                app: app.handle().clone(),
                host: StreamHost::InProcess,
                others: Arc::new(RecordingSink::default()),
            }),
            runtime.handle().clone(),
        ));
        local.attach_echo_emulator();
        app.manage(local.clone());
        register(
            &runtime,
            &local,
            "read a; printf 'GOT=%s\\n' \"$a\"; sleep 60",
            registration("echo", None, false),
        );
        let attached = subscribe(app.handle(), "echo", &ViewportId::new("webview")).unwrap();
        let stream = heard(&app, format!("pty-output-stream-{}", attached.stream_id.0));
        local.write(&sid("echo"), b"x\r").unwrap();
        assert!(wait_until(|| stream.lock().unwrap().contains("GOT=x")));
        local.kill(&sid("echo")).unwrap();
    }

    /// A verb about one workspace applies in every host holding it: a
    /// workspace one host holds answers `Ok`; one no host holds is unknown —
    /// or unreachable while a used sidecar, which may hold it, cannot be
    /// asked; another refusal stands. A layout read never guesses past an
    /// unreachable sidecar either.
    #[test]
    fn a_workspace_verb_reaches_every_host_holding_it() {
        use codesense_pty_core::layout::{GridState, WorkspaceDescriptor};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        sidecar.track("hosted-ws");
        open_tab_on(
            test_backend(&local, Some(client.clone())),
            tab_request("hosted-ws"),
            |host| {
                host.client()
                    .unwrap()
                    .spawn(sh("sleep 60; :"), registration("hosted-ws", None, false))
                    .map(|_| ())
                    .map_err(|failure| failure.message)
            },
            |_| Ok(()),
            || panic!("healthy sidecar does not retry"),
        )
        .unwrap();
        let hosts = Hosts {
            in_process: local.clone(),
            sidecar: Ok(Some(client.clone())),
        };
        let held = codesense_pty_core::domain::WorkspaceKey::new("root:acme/app");
        let renamed = WorkspaceDescriptor {
            display_name: "renamed".into(),
            kind: "root".into(),
            ..Default::default()
        };
        assert_eq!(
            hosts.in_workspace_hosts(|host| host.set_descriptor(&held, renamed.clone())),
            Ok(())
        );
        let snapshot = client
            .layout_snapshot(&app_viewer(), &ClientBootId::new("boot"))
            .unwrap();
        assert_eq!(
            snapshot.workspaces[&held].descriptor.display_name,
            "renamed"
        );
        let absent = codesense_pty_core::domain::WorkspaceKey::new("nowhere");
        assert_eq!(
            hosts.in_workspace_hosts(|host| host.close_workspace(&absent)),
            Err(LayoutError::UnknownWorkspace(absent.clone()))
        );
        let bad_grid = GridState {
            paned: vec![sid("not-a-tab")],
            ..Default::default()
        };
        assert!(matches!(
            hosts.in_workspace_hosts(|host| host.set_grid(&held, bad_grid.clone())),
            Err(LayoutError::InvalidGrid(_))
        ));

        let cut_off = Hosts {
            in_process: local.clone(),
            sidecar: Err(PortUnavailable("the session server is unreachable".into())),
        };
        assert!(matches!(
            cut_off.in_workspace_hosts(|host| host.close_workspace(&held)),
            Err(LayoutError::Unreachable(_))
        ));
        assert!(
            matches!(
                cut_off.in_workspace_hosts(|_| Ok(())),
                Err(LayoutError::Unreachable(_))
            ),
            "applied in-process only: the sidecar's copy is unknown"
        );
        assert!(matches!(
            cut_off.layout_snapshots(&app_viewer(), &ClientBootId::new("boot")),
            Err(LayoutError::Unreachable(_))
        ));
        assert_eq!(
            hosts
                .layout_snapshots(&app_viewer(), &ClientBootId::new("boot"))
                .unwrap()
                .len(),
            2
        );
    }

    /// A sealed placement carries exactly the client selected for reservation;
    /// only a local sidecar policy may fall open after an aborted reservation.
    #[test]
    fn a_pinned_spawn_lands_on_its_host_or_nowhere() {
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        let pinned = select_local_backend(Some(client.clone()), || panic!("not local")).unwrap();
        assert!(Arc::ptr_eq(&pinned.client().unwrap(), &client));
        assert!(pinned.allows_local_fail_open());
        assert!(!pinned.is_remote());
        assert!(
            !may_retry_local_after_abort(&pinned, true),
            "live exact client keeps original error"
        );
        client.sever();
        assert!(wait_until(|| !client.is_alive()));
        assert!(may_retry_local_after_abort(&pinned, true));
        assert!(
            !may_retry_local_after_abort(&pinned, false),
            "consumed spawn never retries"
        );

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = local_in_process_backend(in_process(&runtime)).unwrap();
        assert!(local.client().is_none());
        assert!(!local.allows_local_fail_open());
    }

    #[test]
    fn stale_exact_client_at_send_boundary_writes_zero_bytes() {
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        let backend = select_local_backend(Some(client), || panic!("not local")).unwrap();
        let sends = std::cell::Cell::new(0);

        let failure = send_on_exact_backend(
            &backend,
            |_| Err("exact client generation changed".into()),
            |_| {
                sends.set(sends.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(failure.message, "exact client generation changed");
        assert!(
            !failure.may_host_elsewhere,
            "remote failure cannot fall local"
        );
        assert_eq!(sends.get(), 0, "stale boundary emits no request bytes");
    }

    #[test]
    fn open_tab_final_send_revalidation_releases_reservation_without_fallback() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let sidecar = TestServer::start();
        let client = sidecar_client(&sidecar);
        let backend = test_backend(&local, Some(client));
        let sends = std::cell::Cell::new(0);
        let fallbacks = std::cell::Cell::new(0);

        let failure = open_tab_resolved(
            backend,
            tab_request("stale-at-send"),
            |placement| {
                send_on_exact_backend(
                    &placement,
                    |_| Err("stale at actual send".into()),
                    |_| {
                        sends.set(sends.get() + 1);
                        Ok(())
                    },
                )
                .map_err(|failure| failure.message)
            },
            |_| Ok(()),
            || {
                fallbacks.set(fallbacks.get() + 1);
                Ok((local.clone(), test_backend(&local, None)))
            },
        )
        .unwrap_err();

        assert_eq!(failure, "stale at actual send");
        assert_eq!(sends.get(), 0);
        assert_eq!(fallbacks.get(), 0);
        assert!(placed(sidecar.host().terminal()).is_empty());
        assert!(placed(&*local).is_empty());
    }

    #[test]
    fn sealed_placements_keep_identical_session_ids_on_independent_hosts() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let a = TestServer::start();
        let b = TestServer::start();
        let client_a = sidecar_client(&a);
        let client_b = sidecar_client(&b);
        a.track("same-session");
        b.track("same-session");

        for client in [client_a, client_b] {
            open_tab_on(
                test_backend(&local, Some(client.clone())),
                tab_request("same-session"),
                |placement| {
                    let selected = placement.client().expect("sealed sidecar placement");
                    assert!(Arc::ptr_eq(&selected, &client));
                    selected
                        .spawn(sh("sleep 60; :"), registration("same-session", None, false))
                        .map(|_| ())
                        .map_err(|failure| failure.message)
                },
                |_| Ok(()),
                || panic!("a sealed placement cannot cross hosts or fall local"),
            )
            .unwrap();
        }

        assert_eq!(placed(a.host().terminal()), [sid("same-session")]);
        assert_eq!(placed(b.host().terminal()), [sid("same-session")]);
        assert!(placed(&*local).is_empty());
    }

    #[test]
    fn real_respawn_targets_only_the_selected_remote_with_an_identical_session_id() {
        let a = TestServer::start();
        let b = TestServer::start();
        let client_a = sidecar_client(&a);
        let client_b = sidecar_client(&b);
        let viewer = app_viewer();
        let boot = ClientBootId::new("respawn-proof");
        for client in [&client_a, &client_b] {
            client.layout_snapshot(&viewer, &boot).unwrap();
        }
        for (server, client) in [(&a, &client_a), (&b, &client_b)] {
            server.track("same-respawn");
            client
                .open_tab(
                    tab_request("same-respawn"),
                    Box::new(|| {
                        client
                            .spawn(sh("exit 0"), registration("same-respawn", None, false))
                            .map(|_| ())
                            .map_err(|failure| failure.message)
                    }),
                )
                .unwrap();
        }
        let exited = |client: &SessionServerClient| {
            client
                .layout_snapshot(&viewer, &boot)
                .ok()
                .and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .values()
                        .flat_map(|workspace| &workspace.tabs)
                        .find(|tab| tab.session == sid("same-respawn"))
                        .map(|tab| tab.state.is_exited())
                })
                .unwrap_or(false)
        };
        assert!(wait_until(|| exited(&client_a) && exited(&client_b)));

        let backend_a = test_remote_backend("remote-a", client_a.clone());
        let _backend_b = test_remote_backend("remote-b", client_b.clone());
        respawn_tab_core(
            backend_a,
            &sid("same-respawn"),
            |_| Ok(()),
            |placement| {
                let selected = placement.client().unwrap();
                assert!(Arc::ptr_eq(&selected, &client_a));
                selected
                    .spawn(sh("sleep 60; :"), registration("same-respawn", None, false))
                    .map(|_| ())
                    .map_err(|failure| failure.message)
            },
        )
        .unwrap();

        assert!(wait_until(
            || client_a.has_session(&sid("same-respawn")) == Ok(true)
        ));
        assert!(exited(&client_b), "the identical B session was untouched");
    }

    #[test]
    fn remote_warm_route_reaches_cold_exact_send_without_touching_local_pool() {
        let server = TestServer::start();
        let client = sidecar_client(&server);
        let backend = test_remote_backend("remote-warm", client.clone());
        let acquire = std::cell::Cell::new(0);
        let refill = std::cell::Cell::new(0);
        let kill = std::cell::Cell::new(0);
        let local_spawn = std::cell::Cell::new(0);
        let remote_send = std::cell::Cell::new(0);

        let placement = SpawnPlacement::Exact(Box::new(backend));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let served_warm = runtime
            .block_on(crate::commands::pty::attach_warm_from_args_core(
                &placement,
                || async {
                    acquire.set(acquire.get() + 1);
                    refill.set(refill.get() + 1);
                    kill.set(kill.get() + 1);
                    local_spawn.set(local_spawn.get() + 1);
                    Ok(true)
                },
                || async {
                    let SpawnPlacement::Exact(selected) = &placement else {
                        panic!("remote cold send lost exact placement")
                    };
                    assert!(Arc::ptr_eq(&selected.client().unwrap(), &client));
                    remote_send.set(remote_send.get() + 1);
                    Ok(())
                },
            ))
            .unwrap();

        assert!(!served_warm);
        assert_eq!(remote_send.get(), 1);
        assert_eq!(acquire.get(), 0);
        assert_eq!(refill.get(), 0);
        assert_eq!(kill.get(), 0);
        assert_eq!(local_spawn.get(), 0);
    }

    #[test]
    fn remote_retryable_spawn_failure_never_enters_local_fallback() {
        let server = TestServer::start();
        let backend = test_remote_backend("remote-failure", sidecar_client(&server));
        let failure = SpawnFailure {
            message: "remote refused after reservation abort".into(),
            may_host_elsewhere: true,
            disposition: SpawnDisposition::CancelledUndrained,
        };
        let local_spawn = std::cell::Cell::new(0);
        let pool = std::cell::Cell::new(0);

        if crate::commands::pty::may_fall_open_after_spawn(&backend, &failure, true) {
            local_spawn.set(local_spawn.get() + 1);
            pool.set(pool.get() + 1);
        }

        assert_eq!(local_spawn.get(), 0);
        assert_eq!(pool.get(), 0);
    }

    /// A viewport stream is known to the frontend by a client-unique id, and
    /// detaching it goes to the host it lives in — never tried in-process
    /// first. A second detach is a no-op.
    #[test]
    fn a_viewport_detaches_in_its_own_host() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = in_process(&runtime);
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(local.clone(), &sidecar);
        register(
            &runtime,
            &local,
            "sleep 60",
            registration("local-v", None, false),
        );
        sidecar.track("hosted-v");
        app.state::<SessionServerState>()
            .connected()
            .unwrap()
            .spawn(sh("sleep 60; :"), registration("hosted-v", None, false))
            .unwrap();
        let viewport = ViewportId::new("webview");
        let handle = app.handle();
        let in_proc = subscribe(handle, "local-v", &viewport).unwrap().stream_id.0;
        let hosted = subscribe(handle, "hosted-v", &viewport)
            .unwrap()
            .stream_id
            .0;
        assert_ne!(in_proc, hosted);
        let streams = app.state::<StreamIds>();
        unsubscribe(handle, hosted).unwrap();
        assert!(streams.attached_on(StreamHost::Sidecar).is_empty());
        assert_eq!(streams.attached_on(StreamHost::InProcess).len(), 1);
        unsubscribe(handle, in_proc).unwrap();
        assert!(streams.attached_on(StreamHost::InProcess).is_empty());
        assert_eq!(unsubscribe(handle, in_proc), Ok(()), "already gone");
        local.kill(&sid("local-v")).unwrap();
    }

    /// `subscribe_terminal(stream: Some(reserved))`: the viewport binds the id
    /// the frontend reserved and already listens on, and live output lands on
    /// that channel. An id never reserved is refused and leaves no viewport
    /// attached; without an id the viewport gets a fresh one.
    #[test]
    fn subscribe_binds_the_reserved_stream_and_output_lands_on_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, _sink) = app_reemitting(in_process(&runtime), &sidecar);
        let client = app.state::<SessionServerState>().connected().unwrap();
        sidecar.track("rs");
        client
            .spawn(
                sh("printf 'READY\\n'; read a; printf 'GOT=%s\\n' \"$a\"; sleep 60"),
                registration("rs", None, false),
            )
            .unwrap();
        assert!(wait_until(|| sidecar.text("rs").contains("READY")));
        let reference = admitted_reference(app.handle(), RuntimeMemberSlot::Sidecar, "rs");
        let streams = app.state::<StreamIds>();

        let unreserved = streams.reserve() + 1000;
        assert!(subscribe_reference(
            app.handle(),
            &reference,
            &ViewportId::new("webview"),
            Some(unreserved),
        )
        .is_err());
        let host = StreamHost::LocalSidecar(client.namespace().clone());
        assert!(
            streams.attached_on(host.clone()).is_empty(),
            "a refused bind leaves no viewport attached"
        );

        let reserved = streams.reserve();
        let on_reserved = heard(&app, format!("pty-output-stream-{reserved}"));
        let attached = subscribe_reference(
            app.handle(),
            &reference,
            &ViewportId::new("webview"),
            Some(reserved),
        )
        .unwrap();
        assert_eq!(attached.stream_id.0, reserved);
        assert!(attached.snapshot.vt.contains("READY"));
        let fresh = subscribe_reference(app.handle(), &reference, &ViewportId::new("other"), None)
            .unwrap()
            .stream_id
            .0;
        assert_ne!(fresh, reserved);
        let on_fresh = heard(&app, format!("pty-output-stream-{fresh}"));

        client.write(&sid("rs"), b"x\r").unwrap();
        assert!(wait_until(|| on_reserved.lock().unwrap().contains("GOT=x")));
        assert!(wait_until(|| on_fresh.lock().unwrap().contains("GOT=x")));
        assert_eq!(streams.attached_on(host).len(), 2);
        client.kill(&sid("rs")).unwrap();
    }

    /// The same sidecar lifetime reached again after a lost connection: a
    /// viewport whose session printed while the app was away cannot carry on
    /// from the new stream's snapshot, so it is reported lost (its display is
    /// rebuilt from a fresh subscribe) and carries nothing more; one that
    /// missed nothing is re-attached under the same id and carries on —
    /// nothing missing, nothing twice.
    #[test]
    fn a_resumed_sidecar_reattaches_or_rebuilds_its_viewports_without_a_gap() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sidecar = TestServer::start();
        let (app, sink) = app_reemitting(in_process(&runtime), &sidecar);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        sidecar.track("re");
        client
            .spawn(
                sh(
                    "printf 'BEFORE\\n'; read a; printf 'AFTER=%s\\n' \"$a\"; sleep 0.3; \
                    printf 'GAP\\n'; read b; printf 'LAST=%s\\n' \"$b\"; sleep 60",
                ),
                registration("re", None, false),
            )
            .unwrap();
        assert!(wait_until(|| sidecar.text("re").contains("BEFORE")));
        let attached = subscribe(app.handle(), "re", &ViewportId::new("webview")).unwrap();
        let original_target = state
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
            .unwrap();
        let exact_reference = admitted_reference(app.handle(), RuntimeMemberSlot::Sidecar, "re");
        let delayed = subscribe_reference(
            app.handle(),
            &exact_reference,
            &ViewportId::new("late-unsubscribe"),
            None,
        )
        .unwrap();
        let streams = app.state::<StreamIds>();
        let (old_origin, old_wire) = streams.detach(delayed.stream_id.0).unwrap();
        let old_unsubscribe_client = streams.origin(&old_origin).unwrap();
        assert!(Arc::ptr_eq(&old_unsubscribe_client, &client));
        assert!(attached.snapshot.vt.contains("BEFORE"));
        let stream = heard(&app, format!("pty-output-stream-{}", attached.stream_id.0));
        client.write(&sid("re"), b"x\r").unwrap();
        assert!(wait_until(|| stream.lock().unwrap().contains("AFTER=x")));

        // Retain the same viewport under the verified remote namespace. This
        // second app table lets this test drive the remote production resume
        // wrapper over the very same real protocol connection.
        let remote_app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("remote mock app");
        remote_app.manage(StreamIds::default());
        let server_id = client.server_id().to_string();
        let remote_host = StreamHost::remote_namespace(client.namespace());
        let remote_viewport = ViewportId::new("remote-webview");
        let remote_initial = client.subscribe(&sid("re"), &remote_viewport).unwrap();
        let remote_local = remote_app.state::<StreamIds>().attach(
            remote_host.clone(),
            &sid("re"),
            &remote_viewport,
            &remote_initial,
        );
        let remote_output = heard(&remote_app, format!("pty-output-stream-{remote_local}"));
        let remote_resync = heard(&remote_app, TERMINAL_LAYOUT_RESYNC_EVENT.to_string());

        sidecar.track("gone");
        client
            .open_tab(
                tab_request("gone"),
                Box::new(|| {
                    client
                        .spawn(sh("sleep 60; :"), registration("gone", None, false))
                        .map(|_| ())
                        .map_err(|failure| failure.message)
                }),
            )
            .unwrap();
        let resync = heard(&app, TERMINAL_LAYOUT_RESYNC_EVENT.to_string());
        let lost = heard(&app, TERMINAL_STREAMS_LOST_EVENT.to_string());
        client.sever();
        assert!(wait_until(|| state.connected().is_none()));
        assert!(wait_until(|| sidecar.text("re").contains("GAP")));
        // While the app is away a placed session leaves, which moves the
        // layout too.
        sidecar.host().terminal().kill(&sid("gone")).unwrap();
        assert!(wait_until(|| sidecar
            .host()
            .terminal()
            .has_session(&sid("gone"))
            == Ok(false)));
        let again = state.reconnected(sidecar.root()).expect("reconnected");
        carry_across(app.handle(), &state, &again);
        let replacement_target = state
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, again.namespace())
            .unwrap();
        assert_eq!(original_target.member, replacement_target.member);
        assert_eq!(
            original_target.namespace.epoch,
            replacement_target.namespace.epoch
        );
        assert_ne!(
            original_target.namespace.generation,
            replacement_target.namespace.generation
        );
        assert!(replacement_target.admission_revision > original_target.admission_revision);
        assert!(validate_member_target(app.handle(), &original_target).is_err());
        assert!(validate_member_target(app.handle(), &replacement_target).is_ok());
        let replacement_reference =
            admitted_reference(app.handle(), RuntimeMemberSlot::Sidecar, "re");
        let new_subscription = subscribe_reference(
            app.handle(),
            &replacement_reference,
            &ViewportId::new("late-unsubscribe"),
            None,
        )
        .unwrap();
        assert!(old_unsubscribe_client.unsubscribe(old_wire).is_err());
        assert!(streams
            .attached_on(StreamHost::LocalSidecar(again.namespace().clone()))
            .iter()
            .any(|(local, _, _)| *local == new_subscription.stream_id.0));
        assert!(Arc::ptr_eq(
            &streams
                .origin(&StreamHost::LocalSidecar(again.namespace().clone()))
                .unwrap(),
            &again
        ));
        assert_eq!(again.server_id(), server_id);
        assert_eq!(again.epoch(), client.epoch());
        resume_remote_streams(remote_app.handle(), &again, client.namespace(), || true);
        assert!(wait_until(|| remote_resync
            .lock()
            .unwrap()
            .contains(again.epoch())));
        // Its exit reaches the app: the one carried across, or — when the
        // child's exit is emitted only after the new connection listens — the
        // child's own.
        assert!(
            sink.others.exit("gone").is_some(),
            "it left while the app was away"
        );
        assert_eq!(sink.others.exit("re"), None, "a live session carries on");
        assert!(
            wait_until(|| resync.lock().unwrap().contains(again.epoch())),
            "the moved layout is re-fetched"
        );
        assert!(
            wait_until(|| lost
                .lock()
                .unwrap()
                .contains(&attached.stream_id.0.to_string())),
            "the viewport that missed output is reported lost"
        );
        again.write(&sid("re"), b"y\r").unwrap();
        assert!(wait_until(|| sidecar.text("re").contains("LAST=y")));
        let sent = stream.lock().unwrap().clone();
        assert_eq!(sent.matches("AFTER=x").count(), 1, "{sent}");
        for missed in ["BEFORE", "GAP", "LAST=y"] {
            assert!(
                !sent.contains(missed),
                "{missed} in the lost stream: {sent}"
            );
        }
        let remote_sent = remote_output.lock().unwrap().clone();
        assert!(!remote_sent.contains("GAP"), "{remote_sent}");

        // Late output from the retired connection is ignored.
        remote_app.state::<StreamIds>().deliver_output(
            remote_host.clone(),
            remote_initial.stream_id,
            "late-old".into(),
            999,
            &mut |to, data, seq| {
                emit_stream(remote_app.handle(), to, StreamItem::Output { data, seq })
            },
        );
        assert!(!remote_output.lock().unwrap().contains("late-old"));

        // A viewport that missed nothing carries on across a resume, and the
        // exact production core also merges output that the replacement
        // reader receives after Subscribe but before the stream is repointed.
        let streams = remote_app.state::<StreamIds>();
        let early_host = StreamHost::remote_namespace(again.namespace());
        let current = again.subscribe(&sid("re"), &remote_viewport).unwrap();
        let remote_local =
            streams.attach(early_host.clone(), &sid("re"), &remote_viewport, &current);
        let remote_output = heard(&remote_app, format!("pty-output-stream-{remote_local}"));
        let lost = resume_remote_streams_core(
            &streams,
            early_host.clone(),
            early_host.clone(),
            || true,
            |session, viewport| {
                let subscribed = again.subscribe(session, viewport)?;
                streams.deliver_output(
                    early_host.clone(),
                    subscribed.stream_id,
                    "EARLY".into(),
                    subscribed.snapshot.seq + 5,
                    &mut |to, data, seq| {
                        emit_stream(remote_app.handle(), to, StreamItem::Output { data, seq })
                    },
                );
                Ok::<_, PortUnavailable>(subscribed)
            },
            |stream| {
                again.unsubscribe(stream).unwrap();
            },
            &mut |to, item| emit_stream(remote_app.handle(), to, item),
        );
        assert!(lost.is_empty());
        assert_eq!(remote_output.lock().unwrap().matches("EARLY").count(), 1);

        // A real subscribe is held between the production core's current
        // checks. Retry/detach wins; the allocated replacement is released.
        let retained = again.subscribe(&sid("re"), &remote_viewport).unwrap();
        let raced_local = remote_app.state::<StreamIds>().attach(
            remote_host,
            &sid("re"),
            &remote_viewport,
            &retained,
        );
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let current = std::sync::atomic::AtomicBool::new(true);
        let unsubscribed = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            let streams = remote_app.state::<StreamIds>();
            let detach_streams = remote_app.state::<StreamIds>();
            let current_ref = &current;
            let unsubscribed_ref = &unsubscribed;
            let again_ref = &again;
            scope.spawn(move || {
                let raced_host = StreamHost::remote_namespace(again_ref.namespace());
                let lost = resume_remote_streams_core(
                    &streams,
                    raced_host.clone(),
                    raced_host,
                    || current_ref.load(std::sync::atomic::Ordering::SeqCst),
                    |session, viewport| {
                        let subscribed = again_ref.subscribe(session, viewport);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        subscribed
                    },
                    |stream| {
                        again_ref.unsubscribe(stream).unwrap();
                        unsubscribed_ref.lock().unwrap().push(stream);
                    },
                    &mut |_, _| true,
                );
                assert!(lost.is_empty());
            });
            entered_rx.recv().unwrap();
            current.store(false, std::sync::atomic::Ordering::SeqCst);
            assert!(detach_streams.detach(raced_local).is_some());
            release_tx.send(()).unwrap();
        });
        assert_eq!(unsubscribed.lock().unwrap().len(), 1);
        again.unsubscribe(retained.stream_id).unwrap();
    }

    /// A new sidecar lifetime behind the instance's socket: every session the
    /// app hosted in the old one ended with it — viewed or not — so its tab
    /// exits and its bookkeeping is released; the layout is re-fetched, and
    /// the frontend is told how many open sessions ended.
    #[test]
    fn a_restarted_sidecar_ends_every_session_the_app_hosted_there() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let first = TestServer::start();
        let (app, sink) = app_reemitting(in_process(&runtime), &first);
        let state = app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        for (id, pool_member) in [("viewed", false), ("unviewed", false), ("pooled", true)] {
            first.track(id);
            client
                .spawn(sh("sleep 60; :"), registration(id, None, pool_member))
                .unwrap();
        }
        subscribe(app.handle(), "viewed", &ViewportId::new("webview")).unwrap();
        let told = heard(&app, TERMINAL_SERVER_RESTARTED_EVENT.to_string());
        let resync = heard(&app, TERMINAL_LAYOUT_RESYNC_EVENT.to_string());

        client.sever();
        assert!(wait_until(|| state.connected().is_none()));
        let second = TestServer::start();
        let next = state.reconnected(second.root()).expect("the new lifetime");
        carry_across(app.handle(), &state, &next);
        assert_eq!(sink.others.exit("viewed"), Some(None));
        assert_eq!(sink.others.exit("unviewed"), Some(None), "viewed or not");
        assert!(sink.others.events().iter().any(|event| matches!(
            event,
            TerminalDomainEvent::Exit { session, pool_member: true, .. } if session.as_str() == "pooled"
        )), "a pool member's exit reaches the pool");
        assert!(wait_until(|| resync.lock().unwrap().contains(next.epoch())));
        assert!(sink.others.events().iter().any(|event| matches!(
            event,
            TerminalDomainEvent::SessionRemoved { session, reason: TeardownReason::Expire }
                if session.as_str() == "viewed"
        )));
        assert!(
            wait_until(|| told.lock().unwrap().contains("\"endedSessions\":2")),
            "the open sessions, not the pool's"
        );
        assert!(app
            .state::<StreamIds>()
            .attached_on(StreamHost::Sidecar)
            .is_empty());
    }

    #[test]
    fn remote_retirement_without_stream_state_does_not_consume_permit_or_ledger() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let sink = codesense_session_server::testing::RecordingSink::default();
        let ledger = RemoteSessionLedger::default();
        ledger.record("retained", false);
        let namespace =
            crate::terminal::session_server::EventNamespace::for_test("server", "old", 1);
        assert!(!retire_remote_lifetime(
            app.handle(),
            &sink,
            &namespace,
            ledger.clone(),
            || panic!("missing streams must fail before the permit")
        ));
        assert_eq!(ledger.take_all(), [(sid("retained"), false)]);
        assert!(sink.events().is_empty());
    }

    #[test]
    fn remote_retirement_core_denied_permit_preserves_streams_and_ledger() {
        let streams = StreamIds::default();
        let sink = codesense_session_server::testing::RecordingSink::default();
        let ledger = RemoteSessionLedger::default();
        ledger.record("retained", true);
        let namespace =
            crate::terminal::session_server::EventNamespace::for_test("server", "old", 1);
        let host = StreamHost::remote_namespace(&namespace);
        streams.attach(
            host.clone(),
            &sid("retained"),
            &ViewportId::new("test"),
            &SubscribeResult {
                snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
                stream_id: StreamId(1),
                story_id: None,
                workspace_id: None,
                epic_id: None,
            },
        );
        assert!(!retire_remote_lifetime_core(
            &streams,
            &sink,
            &namespace,
            ledger.clone(),
            || false
        ));
        assert_eq!(streams.attached_on(host).len(), 1);
        assert_eq!(ledger.take_all(), [(sid("retained"), true)]);
        assert!(sink.events().is_empty());
    }

    #[test]
    fn changed_remote_epoch_retires_exact_lifetime_once_and_leaves_other_host_live() {
        #[derive(Default)]
        struct NamespaceSink(Mutex<Vec<crate::terminal::session_server::NamespacedTerminalEvent>>);
        impl crate::terminal::session_server::NamespacedTerminalEventSink for NamespaceSink {
            fn emit(
                &self,
                event: crate::terminal::session_server::NamespacedTerminalEvent,
            ) -> Result<(), PortUnavailable> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
        }
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(StreamIds::default());
        let streams = app.state::<StreamIds>();
        let subscribed = |stream| SubscribeResult {
            snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
            stream_id: StreamId(stream),
            story_id: None,
            workspace_id: None,
            epic_id: None,
        };
        let old_namespace =
            crate::terminal::session_server::EventNamespace::for_test("server-a", "old", 73);
        let old = StreamHost::remote_namespace(&old_namespace);
        let sibling_generation = StreamHost::remote_namespace(
            &crate::terminal::session_server::EventNamespace::for_test("server-a", "old", 72),
        );
        let other = StreamHost::remote("server-b", "old");
        let viewport = ViewportId::new("webview");
        streams.attach(old.clone(), &sid("viewed"), &viewport, &subscribed(1));
        let sibling_local = streams.attach(
            sibling_generation.clone(),
            &sid("viewed"),
            &viewport,
            &subscribed(1),
        );
        let other_local = streams.attach(other.clone(), &sid("viewed"), &viewport, &subscribed(1));
        let ledger = RemoteSessionLedger::default();
        ledger.record("viewed", false);
        ledger.record("unviewed", false);
        let restarted = heard(&app, TERMINAL_SERVER_RESTARTED_EVENT.to_string());
        let resync = heard(&app, TERMINAL_LAYOUT_RESYNC_EVENT.to_string());
        let sink = NamespaceSink::default();

        let duplicate_permits = std::sync::atomic::AtomicUsize::new(0);
        assert!(!retire_remote_lifetime(
            app.handle(),
            &sink,
            &old_namespace,
            ledger.clone(),
            || false,
        ));
        assert_eq!(streams.attached_on(old.clone()).len(), 1);
        assert!(restarted.lock().unwrap().is_empty());

        assert!(retire_remote_lifetime(
            app.handle(),
            &sink,
            &old_namespace,
            ledger.clone(),
            || true,
        ));
        assert!(streams.attached_on(old.clone()).is_empty());
        assert_eq!(streams.attached_on(sibling_generation.clone()).len(), 1);
        assert_eq!(streams.attached_on(other.clone()).len(), 1);
        assert!(restarted.lock().unwrap().is_empty());
        assert!(resync.lock().unwrap().is_empty());
        let first_restarted = restarted.lock().unwrap().clone();
        let first_resync = resync.lock().unwrap().clone();

        assert!(!retire_remote_lifetime(
            app.handle(),
            &sink,
            &old_namespace,
            ledger,
            || {
                duplicate_permits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                true
            },
        ));
        assert_eq!(
            duplicate_permits.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(*restarted.lock().unwrap(), first_restarted);
        assert_eq!(*resync.lock().unwrap(), first_resync);
        assert!(retire_remote_lifetime(
            app.handle(),
            &sink,
            &crate::terminal::session_server::EventNamespace::for_test("server-zero", "old", 74,),
            RemoteSessionLedger::default(),
            || true,
        ));
        assert!(restarted.lock().unwrap().is_empty());
        let ledger_only = RemoteSessionLedger::default();
        ledger_only.record("ledger-only", false);
        assert!(retire_remote_lifetime(
            app.handle(),
            &sink,
            &crate::terminal::session_server::EventNamespace::for_test("server-ledger", "old", 75,),
            ledger_only,
            || true,
        ));
        let emitted = sink.0.lock().unwrap();
        assert!(emitted
            .iter()
            .all(|event| event.namespace.generation() >= 73));
        assert!(emitted.iter().any(|event| matches!(
            &event.event,
            TerminalDomainEvent::Exit { session, .. } if session.as_str() == "ledger-only"
        )));
        drop(emitted);
        let mut sent = Vec::new();
        streams.deliver_output(old, StreamId(1), "late".into(), 4, &mut |to, data, _| {
            sent.push((to, data.to_string()));
            true
        });
        streams.deliver_output(other, StreamId(1), "other".into(), 5, &mut |to, data, _| {
            sent.push((to, data.to_string()));
            true
        });
        streams.deliver_output(
            sibling_generation,
            StreamId(1),
            "sibling".into(),
            6,
            &mut |to, data, _| {
                sent.push((to, data.to_string()));
                true
            },
        );
        assert_eq!(
            sent,
            [
                (other_local, "other".into()),
                (sibling_local, "sibling".into())
            ]
        );
    }

    /// A refused open re-offers the restart with the server's census only
    /// while the terminal server is incompatible; otherwise nothing is said.
    #[test]
    fn a_refused_open_reoffers_the_restart_while_incompatible() {
        use codesense_host_proto::{read_hello, write_frame, Census, Hello, PROTO_MAJOR};
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let told = heard(&app, TERMINAL_SERVER_REFUSED_EVENT.to_string());
        app.manage(SessionServerState::default());
        refused_open(app.handle());
        assert!(told.lock().unwrap().is_empty(), "nothing is incompatible");

        let root = codesense_session_server::testing::short_root();
        let listener = std::os::unix::net::UnixListener::bind(
            codesense_pty_core::protocol::socket_path(root.path()),
        )
        .unwrap();
        // A future major is refused.
        let unsupported = Hello {
            proto_major: PROTO_MAJOR + 1,
            instance_id: codesense_pty_core::protocol::instance_id(root.path()),
            ..Hello::default()
        };
        let host = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            write_frame(&mut &stream, &unsupported.frame().unwrap()).unwrap();
            write_frame(&mut &stream, &Census { live_sessions: 3 }.frame()).unwrap();
            // Held until the client's hello is read, as a host does.
            let _ = read_hello(&mut &stream);
        });
        assert!(app
            .state::<SessionServerState>()
            .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), |_| {
                Err("no launch".into())
            })
            .is_err());
        host.join().unwrap();
        refused_open(app.handle());
        assert!(wait_until(|| told
            .lock()
            .unwrap()
            .contains("\"liveSessions\":3")));
    }

    /// A sidecar at another protocol major is never fallen back from: the
    /// resolution fails with the restart it needs, and the frontend is told
    /// the census count it would end. Any other failure hosts in-process,
    /// silently.
    #[test]
    fn only_an_incompatible_sidecar_blocks_the_fallback() {
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let told = Arc::new(Mutex::new(Vec::<String>::new()));
        let heard = told.clone();
        app.listen_any(TERMINAL_SERVER_INCOMPATIBLE_EVENT, move |event| {
            heard.lock().unwrap().push(event.payload().to_string());
        });
        let state = SessionServerState::default();
        let skew = codesense_host_proto::Skew {
            reason: codesense_host_proto::SkewReason::ProtoMajor { peer: 3 },
            live_sessions: Some(3),
            host_pid: Some(7),
        };
        match backend_after(app.handle(), &state, Err(ConnectError::Incompatible(skew))) {
            Err(message) => assert!(
                message.starts_with("restart terminal server (ends 3 sessions)"),
                "{message}"
            ),
            Ok(_) => panic!("an incompatible sidecar must not fall open"),
        }
        for error in [
            ConnectError::Unreachable("gone".into()),
            ConnectError::Refused("foreign".into()),
        ] {
            assert!(matches!(
                backend_after(app.handle(), &state, Err(error)),
                Ok(None)
            ));
        }
        assert!(wait_until(|| told.lock().unwrap().len() == 1));
        let payload = told.lock().unwrap()[0].clone();
        for field in [
            "\"liveSessions\":3".to_string(),
            "\"serverProtocol\":3".to_string(),
            format!("\"appProtocol\":{PROTO_MAJOR}"),
            "\"reason\":\"restart terminal server (ends 3 sessions)".to_string(),
        ] {
            assert!(payload.contains(&field), "{field} in {payload}");
        }
    }

    /// A real later-member wire reply holds collection while the captured earlier
    /// in-process row is revoked through shutdown (in-process cannot be reinstalled).
    #[test]
    fn snapshot_reply_is_bound_to_original_member_generation() {
        use crate::terminal::session_server::mint_event_namespace;
        use crate::terminal::session_server::tests::{answer, mock_sidecar, no_launch};
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::protocol::Request;
        use codesense_session_server::testing::short_root;
        let root = short_root();
        let (reached, waiting) = std::sync::mpsc::sync_channel(1);
        let (resume, release) = std::sync::mpsc::sync_channel(1);
        let release = Mutex::new(release);
        let mock = mock_sidecar(root.path(), 1, move |_, stream, rid| {
            reached.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(codesense_session_server::testing::WAIT)
                .unwrap();
            answer(
                stream,
                rid,
                Err("controlled later-member snapshot failure".into()),
            );
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let local = Arc::new(TerminalHost::new(
            ServerIdentity::ephemeral("held-layout"),
            Arc::new(RecordingSink::default()),
            runtime.handle().clone(),
        ));
        let identity = local.identity().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = SessionServerState::default();
        let old = mint_event_namespace(identity.server.id.as_str(), &identity.epoch).unwrap();
        state.admission.register_in_process(&old).unwrap();
        state
            .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
            .unwrap();
        app.manage(local);
        app.manage(state);
        let handle = app.handle().clone();
        let pending = std::thread::spawn(move || {
            admitted_layout_snapshots(
                &handle,
                &codesense_pty_core::domain::ClientBootId::new("held"),
            )
        });
        waiting
            .recv_timeout(codesense_session_server::testing::WAIT)
            .unwrap();
        let state = app.state::<SessionServerState>();
        state.admission.shutdown();
        resume.send(()).unwrap();
        let replies = pending.join().unwrap().unwrap();
        assert!(!replies.iter().any(|reply| matches!(reply, RuntimeLayoutReply::Live { target, .. } if target.member.slot == RuntimeMemberSlot::InProcess)),
            "earlier captured reply cannot survive revocation during later-member wait");
        state.close_test_connection();
        assert!(matches!(
            mock.join().unwrap().as_slice(),
            [Request::LayoutSnapshot { .. }]
        ));
    }
}
