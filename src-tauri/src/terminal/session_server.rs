// SPDX-License-Identifier: MIT
//! The app's client of the `codesense-session-server` sidecar: the long-lived
//! process that hosts the same `codesense-pty-core` runtime out of process, so
//! a session's child and its terminal outlive the app.
//!
//! The client answers the same [`TerminalDomain`] surface the in-process
//! [`TerminalHost`](codesense_pty_core::TerminalHost) does, so the app reaches
//! either host through one seam. One live connection carries the app's
//! requests, its keystrokes (raw bytes on an input channel per session) and
//! the sidecar's pushed runtime events. Events are handed, in order, to the
//! app's one terminal event sink — the same sink the in-process registry feeds
//! — so `pty-output-stream-{id}`, `pty-exit` (with the real exit
//! status), OSC 133 dispatch, and the teardown bookkeeping behave identically
//! whichever host runs the session. Replies carry the runtime's error strings
//! verbatim. After a lost connection a later request may reconnect; events
//! pushed in between are not replayed.
//!
//! The socket and the sidecar's singleton lock derive from the instance's
//! app-data root, and the handshake verifies the sidecar serves that same
//! instance at this protocol major and hosts terminals: a sidecar belonging to
//! another root, or one without terminals, is never used. A sidecar at another
//! major — including a protocol-1 one, which never answers a framed hello — is
//! not fallen back from silently: it is reported as
//! [`ConnectError::Incompatible`], naming the sessions a restart would end, and
//! [`SessionServerState::restart`] ends it and launches this build's.
//!
//! The sidecar's terminal layout lives with its sessions and every layout verb
//! crosses the socket. Each connection learns the sidecar's identity from its
//! hello — its id, this lifetime's `epoch` and its layout `rev` — so a
//! reconnect tells a sidecar that carried on (same epoch) from one that
//! restarted and lost its sessions ([`Continuity`]).

mod executor;
mod reverse_mcp;
pub(crate) use reverse_mcp::RoutineBinder;
pub(crate) mod inbox_wait;
pub(crate) mod routine_arm;
pub(crate) mod session_wake;
pub(crate) mod strays;
use crate::commands::auth::private_authority::PrivateAuthorityLease;
use codesense_host_proto::agent_cli::NativeLaunch;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use codesense_host_proto::{
    read_frame, read_host_greeting, write_frame, Capability, Frame, Hello, HostGreeting, Skew,
};
use codesense_pty_core::domain::{
    ClientBootId, DebugDump, PortUnavailable, PtySessionId, SubmitOutcome, SubscribeResult,
    TerminalDomain, TerminalDomainEvent, TerminalEventSink, TerminalText, ViewerId, ViewportId,
    WorkspaceKey,
};
use codesense_pty_core::emulator::TextMode;
use codesense_pty_core::layout::{
    CloseMode, CloseOutcome, GridState, LayoutError, LayoutSnapshot, OpenTab, Placement, TabEntry,
    TagPatch, ViewHint, WorkspaceDescriptor,
};
use codesense_pty_core::protocol::{
    call_once, client_hello, host_pid, host_running, instance_id, socket_path, Inbound, Ingress,
    Inputs, Message, Reply, Request, UsageSpoolRead,
};
use codesense_pty_core::remote_manifest::{
    ManifestEntry, ManifestPart, ManifestProgress, ManifestReply, ManifestRequest,
};
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::{SessionMeta, SessionRegistration};
use serde::{Deserialize, Serialize};

use crate::commands::host_connection::{AdmittedRemoteTransport, RemoteCloseHandle};
use crate::commands::machines::{
    RuntimeAdmissionBaseline, RuntimeAdmissionRow, RuntimeAdmissionState, RuntimeCounter,
    RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
};

/// Immutable client-owned provenance for one admitted transport connection.
///
/// The server identity and epoch are verified during admission. The generation
/// is minted locally and is never reused, so two connections to the same
/// server lifetime cannot publish under the same authority.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNamespace {
    pub server_id: String,
    pub epoch: String,
    generation: u64,
}

/// One decoded remote event stamped exactly once at the admitted connection's
/// reader boundary. Core/domain events and host-proto remain deliberately
/// unaware of transport provenance.
#[derive(Debug, Clone, Serialize)]
pub struct NamespacedTerminalEvent {
    pub namespace: EventNamespace,
    pub event: TerminalDomainEvent,
}

pub(crate) trait NamespacedTerminalEventSink: Send + Sync {
    fn emit(&self, event: NamespacedTerminalEvent) -> Result<(), PortUnavailable>;

    /// Private recipients must explicitly preserve the authority stamp through
    /// their final consumer. A legacy sink cannot silently discard it.
    fn emit_private(
        &self,
        _event: NamespacedTerminalEvent,
        _lease: &PrivateAuthorityLease,
    ) -> Result<(), PortUnavailable> {
        Err(PortUnavailable(
            "private event recipient unavailable".into(),
        ))
    }
}

#[cfg(test)]
impl NamespacedTerminalEventSink for codesense_session_server::testing::RecordingSink {
    fn emit_private(
        &self,
        envelope: NamespacedTerminalEvent,
        lease: &PrivateAuthorityLease,
    ) -> Result<(), PortUnavailable> {
        // This test recipient commits only to its in-memory recording; no I/O
        // or reentrant callbacks run under the authority gate.
        lease
            .commit(|| TerminalEventSink::emit(self, envelope.event))
            .map_err(PortUnavailable)?
    }

    fn emit(&self, envelope: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        TerminalEventSink::emit(self, envelope.event)
    }
}

impl EventNamespace {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub(crate) fn for_test(server_id: &str, epoch: &str, generation: u64) -> Self {
        Self {
            server_id: server_id.into(),
            epoch: epoch.into(),
            generation,
        }
    }
}

static NEXT_CONNECTION_GENERATION: AtomicU64 = AtomicU64::new(0);

fn mint_connection_generation(counter: &AtomicU64) -> Result<u64, ConnectError> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| ConnectError::Refused("connection generation space exhausted".into()))
}

pub(crate) fn mint_event_namespace(
    server_id: &str,
    epoch: &str,
) -> Result<EventNamespace, ConnectError> {
    #[cfg(test)]
    if std::env::var("RM4E_INJECT").as_deref() == Ok("rm4e_drop_server_namespace") {
        return Ok(EventNamespace {
            server_id: "rm4e-mutant".into(),
            epoch: "rm4e-mutant".into(),
            generation: 0,
        });
    }
    Ok(EventNamespace {
        server_id: server_id.into(),
        epoch: epoch.into(),
        generation: mint_connection_generation(&NEXT_CONNECTION_GENERATION)?,
    })
}

type AdmissionObserver = Arc<dyn Fn(RuntimeAdmissionRow) + Send + Sync>;
type LocalSinkFactory = Arc<dyn Fn(&EventNamespace) -> Arc<dyn TerminalEventSink> + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalCandidatePhase {
    Pending,
    Lost,
    Installed,
    Retired,
}

struct LocalCandidate {
    phase: LocalCandidatePhase,
    client: Weak<SessionServerClient>,
}

#[derive(Default)]
struct LocalAdmissionCore {
    closing: bool,
    revision: u64,
    rows: std::collections::BTreeMap<RuntimeMemberSlot, RuntimeAdmissionRow>,
    candidates: HashMap<EventNamespace, LocalCandidate>,
    observers: Vec<AdmissionObserver>,
}

impl LocalAdmissionCore {
    fn prune_terminal_candidates(&mut self) {
        // Namespaces are never reused. Missing candidates already refuse installation
        // and ignore late loss callbacks, so terminal records need no tombstone.
        self.candidates
            .retain(|_, candidate| match candidate.phase {
                LocalCandidatePhase::Pending => candidate.client.strong_count() != 0,
                LocalCandidatePhase::Installed => true,
                LocalCandidatePhase::Lost | LocalCandidatePhase::Retired => false,
            });
    }

    fn publish(
        &mut self,
        slot: RuntimeMemberSlot,
        state: RuntimeAdmissionState,
    ) -> Result<RuntimeAdmissionRow, ConnectError> {
        let Some(revision) = self.revision.checked_add(1) else {
            self.closing = true;
            return Err(ConnectError::Refused(
                "local admission revision space exhausted".into(),
            ));
        };
        self.revision = revision;
        let row = RuntimeAdmissionRow {
            private_authority_epoch: None,
            member: RuntimeMemberAddress::local(slot),
            revision: RuntimeCounter::new(revision),
            state,
        };
        self.rows.insert(slot, row.clone());
        Ok(row)
    }
}

/// Serializes local authority independently of connection I/O and reconnect
/// attempts. Callbacks always run after the publication mutex is released.
#[derive(Default)]
pub struct LocalRuntimeAdmission {
    core: Mutex<LocalAdmissionCore>,
    pub(crate) effects: crate::commands::pty_host::LocalEffectOwners,
    sink_factory: Mutex<Option<LocalSinkFactory>>,
}

impl LocalRuntimeAdmission {
    pub(crate) fn set_sink_factory(&self, factory: LocalSinkFactory) {
        *lock(&self.sink_factory) = Some(factory);
    }

    pub(crate) fn target_for_namespace(
        &self,
        slot: RuntimeMemberSlot,
        namespace: &EventNamespace,
    ) -> Option<RuntimeMemberTarget> {
        let core = lock(&self.core);
        if core.closing {
            return None;
        }
        let row = core.rows.get(&slot)?;
        match &row.state {
            RuntimeAdmissionState::Live { namespace: current } if current == &namespace.into() => {
                Some(RuntimeMemberTarget {
                    member: row.member.clone(),
                    admission_revision: row.revision,
                    namespace: current.clone(),
                })
            }
            _ => None,
        }
    }
    pub(crate) fn register_in_process(
        &self,
        namespace: &EventNamespace,
    ) -> Result<(), ConnectError> {
        let (row, observers) = {
            let mut core = lock(&self.core);
            if core.closing || core.rows.contains_key(&RuntimeMemberSlot::InProcess) {
                return Err(ConnectError::Refused(
                    "in-process runtime is already registered or closing".into(),
                ));
            }
            let row = core.publish(
                RuntimeMemberSlot::InProcess,
                RuntimeAdmissionState::Live {
                    namespace: namespace.into(),
                },
            )?;
            (row, core.observers.clone())
        };
        Self::notify(row, observers);
        Ok(())
    }

    pub(crate) fn subscribe(&self, observer: AdmissionObserver) {
        lock(&self.core).observers.push(observer);
    }

    pub(crate) fn baseline(&self) -> RuntimeAdmissionBaseline {
        RuntimeAdmissionBaseline {
            rows: lock(&self.core).rows.values().cloned().collect(),
        }
    }

    fn is_installed(&self, client: &Arc<SessionServerClient>) -> bool {
        let core = lock(&self.core);
        !core.closing
            && client.is_alive()
            && core
                .candidates
                .get(client.namespace())
                .is_some_and(|candidate| {
                    candidate.phase == LocalCandidatePhase::Installed
                        && candidate
                            .client
                            .upgrade()
                            .is_some_and(|current| Arc::ptr_eq(&current, client))
                })
    }

    fn notify(row: RuntimeAdmissionRow, observers: Vec<AdmissionObserver>) {
        for observer in observers {
            observer(row.clone());
        }
    }

    fn register_candidate(&self, client: &Arc<SessionServerClient>) -> Result<(), ConnectError> {
        let mut core = lock(&self.core);
        core.prune_terminal_candidates();
        if core.closing {
            return Err(ConnectError::Refused(
                "local runtime is shutting down".into(),
            ));
        }
        core.candidates.insert(
            client.namespace().clone(),
            LocalCandidate {
                phase: LocalCandidatePhase::Pending,
                client: Arc::downgrade(client),
            },
        );
        Ok(())
    }

    fn install(
        &self,
        client: &Arc<SessionServerClient>,
        install: impl FnOnce(),
    ) -> Result<(), ConnectError> {
        let (row, observers) = {
            let mut core = lock(&self.core);
            let valid = core
                .candidates
                .get(client.namespace())
                .is_some_and(|candidate| {
                    candidate.phase == LocalCandidatePhase::Pending
                        && candidate
                            .client
                            .upgrade()
                            .is_some_and(|registered| Arc::ptr_eq(&registered, client))
                });
            if core.closing || !valid || !client.is_alive() {
                return Err(ConnectError::Refused(
                    "sidecar connection was lost before installation".into(),
                ));
            }
            let row = core.publish(
                RuntimeMemberSlot::Sidecar,
                RuntimeAdmissionState::Live {
                    namespace: client.namespace().into(),
                },
            )?;
            for (namespace, candidate) in &mut core.candidates {
                candidate.phase = if namespace == client.namespace() {
                    LocalCandidatePhase::Installed
                } else {
                    LocalCandidatePhase::Retired
                };
            }
            core.prune_terminal_candidates();
            install();
            (row, core.observers.clone())
        };
        Self::notify(row, observers);
        Ok(())
    }

    fn lost(&self, namespace: &EventNamespace) {
        let publication = {
            let mut core = lock(&self.core);
            let Some(candidate) = core.candidates.get_mut(namespace) else {
                return;
            };
            let installed = candidate.phase == LocalCandidatePhase::Installed;
            candidate.phase = LocalCandidatePhase::Lost;
            core.prune_terminal_candidates();
            if !installed {
                return;
            }
            core.publish(
                RuntimeMemberSlot::Sidecar,
                RuntimeAdmissionState::Unavailable,
            )
            .ok()
            .map(|row| (row, core.observers.clone()))
        };
        if let Some((row, observers)) = publication {
            Self::notify(row, observers);
        }
    }

    pub(crate) fn validate(&self, target: &RuntimeMemberTarget) -> bool {
        self.commit_current(target, || Ok(())).is_ok()
    }

    /// Metadata-only commit: never wait, query a client, touch the filesystem,
    /// or call a callback that emits while this admission guard is held.
    pub(crate) fn commit_current<T>(
        &self,
        target: &RuntimeMemberTarget,
        commit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let core = lock(&self.core);
        let current = !core.closing
            && core
                .rows
                .get(&target.member.slot)
                .is_some_and(|row| row.matches(target))
            && (target.member.slot != RuntimeMemberSlot::Sidecar
                || core.candidates.iter().any(|(namespace, candidate)| {
                    crate::commands::machines::RuntimeNamespace::from(namespace) == target.namespace
                        && candidate.phase == LocalCandidatePhase::Installed
                        && candidate
                            .client
                            .upgrade()
                            .is_some_and(|client| client.is_alive())
                }));
        if !current {
            return Err("runtime admission changed before effect commit".into());
        }
        commit()
    }

    fn revoke_sidecar(&self) {
        let publication = {
            let mut core = lock(&self.core);
            for candidate in core.candidates.values_mut() {
                candidate.phase = LocalCandidatePhase::Retired;
            }
            core.prune_terminal_candidates();
            core.publish(
                RuntimeMemberSlot::Sidecar,
                RuntimeAdmissionState::Unavailable,
            )
            .ok()
            .map(|row| (row, core.observers.clone()))
        };
        if let Some((row, observers)) = publication {
            Self::notify(row, observers);
        }
    }

    pub(crate) fn shutdown(&self) {
        let publications = {
            let mut core = lock(&self.core);
            if core.closing {
                return;
            }
            core.closing = true;
            for candidate in core.candidates.values_mut() {
                candidate.phase = LocalCandidatePhase::Retired;
            }
            core.prune_terminal_candidates();
            let slots: Vec<_> = core.rows.keys().copied().collect();
            let rows: Vec<_> = slots
                .into_iter()
                .filter_map(|slot| core.publish(slot, RuntimeAdmissionState::Unavailable).ok())
                .collect();
            rows.into_iter()
                .map(|row| (row, core.observers.clone()))
                .collect::<Vec<_>>()
        };
        for (row, observers) in publications {
            Self::notify(row, observers);
        }
    }
}

/// The protocol minor that added the respawn verb (`BeginRespawn` /
/// `FinishRespawn`).
pub const RESPAWN_PROTO_MINOR: u16 = 1;

/// A respawn refused by a terminal server older than the respawn verb.
pub const RESPAWN_NEEDS_UPDATED_SERVER: &str =
    "Restart needs the updated terminal server — restart it from Restart terminal server…";

/// The protocol minor that added a saved Agent's own variables on a spawn and in its startup
/// (`AgentEnvV1`).
pub const AGENT_ENV_PROTO_MINOR: u16 = 18;

/// A launch of a saved Agent that sets environment variables, refused by a terminal server that
/// would run it without them.
pub const AGENT_ENV_NEEDS_UPDATED_SERVER: &str =
    "This agent sets environment variables, and this terminal server is too old to set them — update or restart the terminal server, then launch again";

/// The sidecar's `externalBin` base name.
pub const SIDECAR_BIN: &str = "codesense-session-server";

/// How long the handshake may take before the sidecar counts as unusable.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on one request write, so a wedged sidecar cannot stall a
/// keystroke — or the app's exit-time reap — for long.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Pending protocol writes are bounded independently by message count and
/// encoded bytes. A slow or wedged connector can never grow app memory without
/// bound, and producers never wait for queue capacity.
const WRITE_QUEUE_MESSAGES: usize = 64;
// The protocol limit excludes its four-byte length prefix. Admit any legal
// frame, including a base64-encoded MAX_FILE_BYTES WorkspaceFile write, while
// keeping the total queued and in-flight bytes bounded by one maximum frame.
const WRITE_QUEUE_BYTES: usize = codesense_host_proto::frame::MAX_FRAME_BYTES + 4;

/// How long a request waits for its reply, beyond any budget it carries.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a read about the hosted sessions waits for its reply. Reads run
/// on hot paths (a liveness guard, a reap, the memory sampler), so a hung
/// sidecar costs each one this long, and the read is then unknown.
const READ_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the exit-time reap waits for the sidecar's answer.
pub(crate) const EXIT_REAP_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a freshly launched sidecar gets to start listening.
const STARTUP_WAIT: Duration = Duration::from_secs(5);

/// After a failed launch, don't launch again for this long; terminals open
/// in-process meanwhile instead of each paying the startup wait.
const LAUNCH_BACKOFF: Duration = Duration::from_secs(30);

/// After a failed reconnect, don't try again for this long, so a wedged
/// sidecar costs one handshake timeout, not one per keystroke.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why a connection to the sidecar was not made.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing is listening (no socket, or a stale one), or a launch failed:
    /// launching may help.
    Unreachable(String),
    /// Something answered but would not complete the handshake: launching
    /// cannot help, though a later try may.
    Refused(String),
    /// Something completed the handshake but is not this instance's terminal
    /// host (another instance's sidecar, or one that hosts no terminals):
    /// launching cannot help; what serves the socket may change.
    Foreign(String),
    /// This instance's sidecar speaks another protocol major: it must be
    /// restarted, ending its live sessions, before it can be used.
    Incompatible(Skew),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(message) | Self::Refused(message) | Self::Foreign(message) => {
                f.write_str(message)
            }
            Self::Incompatible(skew) => skew.fmt(f),
        }
    }
}

/// Why a request failed.
enum CallError {
    /// Positively refused before this request entered the writer queue.
    NotSent(String),
    /// A runtime rejection; registration/background setup may have begun.
    Failed(String),
    /// It was sent but its reply was lost; whether it ran is unknown.
    Lost(String),
}

impl From<CallError> for String {
    fn from(error: CallError) -> Self {
        match error {
            CallError::NotSent(message) | CallError::Failed(message) | CallError::Lost(message) => {
                message
            }
        }
    }
}

impl From<CallError> for PortUnavailable {
    fn from(error: CallError) -> Self {
        PortUnavailable(error.into())
    }
}

/// Delivery certainty for a shell line. A lost reply never permits replay.
#[derive(Debug)]
pub(crate) enum InputDeliveryError {
    NotSent(String),
    Unknown(String),
}
impl From<InputDeliveryError> for PortUnavailable {
    fn from(error: InputDeliveryError) -> Self {
        match error {
            InputDeliveryError::NotSent(message) | InputDeliveryError::Unknown(message) => {
                Self(message)
            }
        }
    }
}

/// A reply extractor for [`SessionServerClient::ask`]: `pattern`'s `value`, or
/// the unexpected reply back.
macro_rules! reply {
    ($pattern:pat => $value:expr) => {
        |reply| match reply {
            $pattern => Ok($value),
            other => Err(other),
        }
    };
}

/// Why a restart did not leave a usable sidecar: `error`, and whether the old
/// sidecar was already gone (the new one did not start) or still runs.
#[derive(Debug)]
pub struct RestartFailure {
    pub old_stopped: bool,
    pub error: ConnectError,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WritableAttachmentFailure {
    AttachedElsewhere,
    Unavailable(String),
}

impl std::fmt::Display for WritableAttachmentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AttachedElsewhere => f.write_str("attached elsewhere"),
            Self::Unavailable(message) => f.write_str(message),
        }
    }
}

/// Why a spawn in the sidecar did not host the session.
#[derive(Debug)]
pub struct SpawnFailure {
    pub message: String,
    /// The session is known not to run in the sidecar, so the caller may host
    /// it elsewhere. `false` when it was deliberately cancelled, or when it may
    /// still be running there.
    pub may_host_elsewhere: bool,
    pub disposition: SpawnDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnDisposition {
    NeverStarted,
    RejectedUndrained,
    CancelledUndrained,
    Unknown,
}

/// One bidirectional byte transport consumed by the one session-server client.
///
/// The protocol and client deliberately know nothing about how these halves
/// were opened. Local connections supply a Unix socket; RM-4's remote adapter
/// will supply the stdout/stdin halves returned by the pinned SSH connector.
struct SessionServerTransport {
    private_lease: Option<PrivateAuthorityLease>,
    reader: BufReader<Box<dyn TransportReader>>,
    writer: Box<dyn TransportWriter>,
    interrupt: Arc<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    observation: Arc<Rm4fObservation>,
}

impl SessionServerTransport {
    fn new(
        reader: impl TransportReader + 'static,
        writer: impl TransportWriter + 'static,
        interrupt: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        #[cfg(test)]
        let observation = Arc::new(Rm4fObservation::default());
        #[cfg(test)]
        let reader = Rm4fReader {
            inner: reader,
            observation: observation.clone(),
        };
        #[cfg(test)]
        let writer = Rm4fWriter {
            inner: writer,
            observation: observation.clone(),
            input_position: None,
        };
        Self {
            private_lease: None,
            reader: BufReader::new(Box::new(reader)),
            writer: Box::new(writer),
            interrupt: Arc::new(interrupt),
            #[cfg(test)]
            observation,
        }
    }
}

/// A readable byte-stream half. A connector may remove admission-only read
/// limits once the greeting is complete.
pub(crate) trait TransportReader: Read + Send {
    fn set_read_timeout(&mut self, _timeout: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }
}

impl TransportReader for UnixStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

impl TransportReader for std::process::ChildStdout {}

/// A byte-stream writer whose transport can be closed explicitly. Closing is
/// separate from dropping because the read half may otherwise keep a cloned
/// full-duplex handle alive and leave the peer blocked.
pub(crate) trait TransportWriter: Write + Send {
    fn close(&mut self) -> std::io::Result<()>;
    #[cfg(test)]
    fn rm4f_frame_written(&self, _frame: &Frame) {}
    #[cfg(test)]
    fn rm4f_frame_started(&mut self, _frame: &Frame) {}
}

/// A per-connection, opt-in ledger. Successful transport operations are observed
/// independently of enqueue success; replies are recorded before waking callers.
#[cfg(test)]
#[derive(Default)]
struct Rm4fObservation {
    enabled: AtomicBool,
    snapshot: Mutex<Rm4fSnapshot>,
}

#[cfg(test)]
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub(crate) struct Rm4fSnapshot {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub input_payload_bytes: u64,
    pub completed_frames: u64,
    pub requests: Vec<Rm4fRequest>,
    pub replies: Vec<Rm4fReply>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Rm4fRequest {
    pub rid: u64,
    pub kind: String,
    pub session: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Rm4fReply {
    pub rid: u64,
    pub transport_success: bool,
    pub success: bool,
    pub error: Option<String>,
    pub reply: Option<Reply>,
}

#[cfg(test)]
impl Rm4fObservation {
    fn update(&self, update: impl FnOnce(&mut Rm4fSnapshot)) {
        if self.enabled.load(Ordering::Acquire) {
            update(&mut lock(&self.snapshot));
        }
    }
}

#[cfg(test)]
struct Rm4fReader<T> {
    inner: T,
    observation: Arc<Rm4fObservation>,
}

#[cfg(test)]
impl<T: Read> Read for Rm4fReader<T> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.observation.update(|s| s.bytes_read += count as u64);
        Ok(count)
    }
}

#[cfg(test)]
impl<T: TransportReader> TransportReader for Rm4fReader<T> {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }
}

#[cfg(test)]
struct Rm4fWriter<T> {
    inner: T,
    observation: Arc<Rm4fObservation>,
    input_position: Option<(usize, usize)>,
}

#[cfg(test)]
impl<T: Write> Write for Rm4fWriter<T> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(buffer)?;
        // Count the successfully written intersection with the codec's payload,
        // including a partial write whose frame later fails.
        let input = self.input_position.as_mut().map_or(0, |(position, start)| {
            let bytes =
                (*position + count).saturating_sub(*start) - position.saturating_sub(*start);
            *position += count;
            bytes
        });
        self.observation.update(|s| {
            s.bytes_written += count as u64;
            s.input_payload_bytes += input as u64;
        });
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
impl<T: TransportWriter> TransportWriter for Rm4fWriter<T> {
    fn close(&mut self) -> std::io::Result<()> {
        self.inner.close()
    }
    fn rm4f_frame_started(&mut self, frame: &Frame) {
        self.input_position = if frame.kind == codesense_host_proto::Kind::INPUT {
            frame
                .input_parts()
                .ok()
                .map(|(_, data)| (0, frame.encode().len() - data.len()))
        } else {
            None
        };
    }
    fn rm4f_frame_written(&self, frame: &Frame) {
        self.observation.update(|s| {
            s.completed_frames += 1;
            if frame.kind == codesense_host_proto::Kind::INPUT {
                if let Ok((rid, _)) = frame.input_parts() {
                    s.requests.push(Rm4fRequest {
                        rid,
                        kind: "input".into(),
                        session: None,
                    });
                }
            }
            if frame.kind == codesense_host_proto::Kind::CONTROL {
                if let Ok(Message::Call { rid, request }) = frame.decode_json::<Message>() {
                    let session = match &request {
                        Request::ReserveTab { request } => {
                            Some(request.session.as_str().to_owned())
                        }
                        Request::FinishTab { session, .. }
                        | Request::HasSession { session }
                        | Request::Kill { session } => Some(session.as_str().to_owned()),
                        Request::Spawn { registration, .. } => {
                            Some(registration.session_id.clone())
                        }
                        _ => None,
                    };
                    let kind = match request {
                        Request::ReserveTab { .. } => "reserve_tab".into(),
                        Request::FinishTab {
                            spawned: Err(_), ..
                        } => "finish_tab_error".into(),
                        Request::FinishTab { .. } => "finish_tab_success".into(),
                        Request::Spawn { .. } => "spawn".into(),
                        Request::HasSession { .. } => "has_session".into(),
                        Request::Kill { .. } => "kill".into(),
                        Request::LiveSessionIds => "live_session_ids".into(),
                        // SW-XS-6: wire-level call counting, without the
                        // payload in the kind.
                        Request::WorkspaceManifest { .. } => "workspace_manifest".into(),
                        Request::WorkspaceFile { .. } => "workspace_file".into(),
                        Request::WorkspaceContextBatch { request } => {
                            use codesense_pty_core::context_batch::BatchOperation as Op;
                            match request.operation {
                                Op::InspectSelected { .. } => "batch_inspect",
                                Op::Begin { .. } => "batch_begin",
                                Op::Chunk { .. } => "batch_chunk",
                                Op::Seal => "batch_seal",
                                Op::Apply => "batch_apply",
                                Op::Status => "batch_status",
                                Op::Finish { .. } => "batch_finish",
                            }
                            .into()
                        }
                        other => format!("{other:?}"),
                    };
                    s.requests.push(Rm4fRequest { rid, kind, session });
                }
            }
        });
    }
}

impl TransportWriter for UnixStream {
    fn close(&mut self) -> std::io::Result<()> {
        self.shutdown(std::net::Shutdown::Both)
    }
}

#[cfg(test)]
#[path = "session_server/rm4f_observation_tests.rs"]
mod rm4f_observation_tests;
impl TransportWriter for std::process::ChildStdin {
    fn close(&mut self) -> std::io::Result<()> {
        // `close_writer` drops this owned pipe immediately after this hook.
        // The connector's process guard remains a separate lifecycle owner.
        Ok(())
    }
}

pub(crate) struct AdmittedSessionTransport {
    transport: SessionServerTransport,
    hello: Hello,
    remote_close: Option<RemoteCloseHandle>,
}

struct ByteBudget {
    used: AtomicU64,
    capacity: u64,
}

impl ByteBudget {
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<BytePermit, WriteBackpressure> {
        let bytes = bytes as u64;
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                return Err(WriteBackpressure::Bytes {
                    requested: bytes,
                    used,
                    capacity: self.capacity,
                });
            };
            if next > self.capacity {
                return Err(WriteBackpressure::Bytes {
                    requested: bytes,
                    used,
                    capacity: self.capacity,
                });
            }
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(BytePermit {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(current) => used = current,
            }
        }
    }
}

struct BytePermit {
    budget: Arc<ByteBudget>,
    bytes: u64,
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct QueuedWrite {
    frame: Frame,
    _permit: BytePermit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteBackpressure {
    Bytes {
        requested: u64,
        used: u64,
        capacity: u64,
    },
    Messages {
        capacity: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteEnqueueError {
    Backpressure(WriteBackpressure),
    Closed,
    InvalidFrame(String),
}

#[derive(Clone)]
pub(crate) struct WorkerCompletion(Arc<(Mutex<bool>, Condvar)>);

impl WorkerCompletion {
    fn complete(&self) {
        let (done, changed) = &*self.0;
        *lock(done) = true;
        changed.notify_all();
    }

    pub(crate) fn wait(&self) {
        let (done, changed) = &*self.0;
        let mut done = lock(done);
        while !*done {
            done = changed
                .wait(done)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let (done, changed) = &*self.0;
        let done = lock(done);
        if *done {
            return true;
        }
        let (done, _) = changed
            .wait_timeout_while(done, timeout, |done| !*done)
            .unwrap_or_else(|error| error.into_inner());
        *done
    }

    #[cfg(test)]
    pub(crate) fn pending() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    #[cfg(test)]
    pub(crate) fn mark_done(&self) {
        self.complete();
    }
}

struct CompleteWorkerOnDrop(WorkerCompletion);

impl Drop for CompleteWorkerOnDrop {
    fn drop(&mut self) {
        self.0.complete();
    }
}

pub(crate) struct WriteMailbox {
    private_lease: std::sync::OnceLock<PrivateAuthorityLease>,
    sender: Mutex<Option<SyncSender<QueuedWrite>>>,
    closing: AtomicBool,
    budget: Arc<ByteBudget>,
    interrupt: Arc<dyn Fn() + Send + Sync>,
    completion: WorkerCompletion,
}

impl WriteMailbox {
    pub(crate) fn start(
        writer: Box<dyn TransportWriter>,
        interrupt: Arc<dyn Fn() + Send + Sync>,
        alive: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, std::io::Error> {
        Self::start_with(writer, interrupt, alive, |run| {
            std::thread::Builder::new()
                .name("pty-server-writer".into())
                .spawn(run)
                .map(|_| ())
        })
    }

    fn start_with(
        mut writer: Box<dyn TransportWriter>,
        interrupt: Arc<dyn Fn() + Send + Sync>,
        alive: Arc<AtomicBool>,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), std::io::Error>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let (sender, receiver) = mpsc::sync_channel(WRITE_QUEUE_MESSAGES);
        let completion = WorkerCompletion(Arc::new((Mutex::new(false), Condvar::new())));
        let mailbox = Arc::new(Self {
            private_lease: std::sync::OnceLock::new(),
            sender: Mutex::new(Some(sender)),
            closing: AtomicBool::new(false),
            budget: Arc::new(ByteBudget {
                used: AtomicU64::new(0),
                capacity: WRITE_QUEUE_BYTES as u64,
            }),
            interrupt,
            completion: completion.clone(),
        });
        let worker_mailbox = Arc::downgrade(&mailbox);
        spawn(Box::new(move || {
            let _complete = CompleteWorkerOnDrop(completion);
            while let Ok(write) = receiver.recv() {
                #[cfg(test)]
                writer.rm4f_frame_started(&write.frame);
                if write_frame(&mut writer, &write.frame)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    alive.store(false, Ordering::SeqCst);
                    if let Some(mailbox) = worker_mailbox.upgrade() {
                        mailbox.close();
                    }
                    break;
                }
                #[cfg(test)]
                writer.rm4f_frame_written(&write.frame);
            }
            let _ = writer.close();
        }))?;
        Ok(mailbox)
    }

    pub(crate) fn try_send(&self, frame: &Frame) -> Result<(), WriteEnqueueError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(WriteEnqueueError::Closed);
        }
        let mut bytes = Vec::new();
        write_frame(&mut bytes, frame)
            .map_err(|error| WriteEnqueueError::InvalidFrame(error.to_string()))?;
        let permit = self
            .budget
            .reserve(bytes.len())
            .map_err(WriteEnqueueError::Backpressure)?;
        if self.closing.load(Ordering::Acquire) {
            return Err(WriteEnqueueError::Closed);
        }
        let sender = lock(&self.sender).clone();
        let Some(sender) = sender else {
            return Err(WriteEnqueueError::Closed);
        };
        let write = QueuedWrite {
            frame: frame.clone(),
            _permit: permit,
        };
        let enqueue = || sender.try_send(write);
        let result = match self.private_lease.get() {
            Some(lease) => lease
                .commit(enqueue)
                .map_err(|_| WriteEnqueueError::Closed)?,
            None => enqueue(),
        };
        match result {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(WriteEnqueueError::Backpressure(
                WriteBackpressure::Messages {
                    capacity: WRITE_QUEUE_MESSAGES,
                },
            )),
            Err(TrySendError::Disconnected(_)) => Err(WriteEnqueueError::Closed),
        }
    }

    pub(crate) fn close(&self) {
        if !self.closing.swap(true, Ordering::AcqRel) {
            (self.interrupt)();
        }
        lock(&self.sender).take();
    }

    pub(crate) fn completion(&self) -> WorkerCompletion {
        self.completion.clone()
    }
}

fn open_socket(root: &Path) -> Result<SessionServerTransport, ConnectError> {
    let socket = socket_path(root);
    let stream = UnixStream::connect(&socket)
        .map_err(|e| ConnectError::Unreachable(format!("connect {socket:?}: {e}")))?;
    let refused = |e: std::io::Error| ConnectError::Refused(format!("{socket:?}: {e}"));
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(refused)?;
    let reader = stream.try_clone().map_err(refused)?;
    let interrupt = stream.try_clone().map_err(refused)?;
    reader
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(refused)?;
    Ok(SessionServerTransport::new(reader, stream, move || {
        let _ = interrupt.shutdown(std::net::Shutdown::Both);
    }))
}

/// SV-7: this app process's identity on every connection it makes to a
/// terminal server, so the server can tell which sessions a connected app
/// opened.
pub(crate) fn app_client_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| format!("app-{}-{}", std::process::id(), uuid::Uuid::new_v4()))
}

/// Open a connection to the sidecar of the instance rooted at `root` and
/// complete the handshake: the sidecar must serve that instance at this
/// protocol major and host terminals. Returns the stream, its reader, and the
/// sidecar's hello.
fn handshake(root: &Path) -> Result<AdmittedSessionTransport, ConnectError> {
    let socket = socket_path(root);
    let mut transport = open_socket(root)?;
    let refused = |e: std::io::Error| ConnectError::Refused(format!("{socket:?}: {e}"));
    let expected = instance_id(root);
    let mut ours = client_hello(
        expected.clone(),
        env!("CARGO_PKG_VERSION"),
        &crate::commands::pty_layout::app_viewer(),
    );
    ours.client_id = Some(app_client_id().to_string());
    let (greeting, census) = read_host_greeting(&mut transport.reader)
        .map_err(|error| ConnectError::Refused(format!("{socket:?}: handshake: {error}")))?;
    let hello = match greeting {
        HostGreeting::Supported { hello } => hello,
        HostGreeting::Unsupported(hello) => {
            return Err(ConnectError::Incompatible(Skew {
                reason: hello
                    .skew_reason()
                    .expect("an unsupported greeting has a skew"),
                live_sessions: Some(census.live_sessions),
                host_pid: Some(hello.pid).filter(|pid| *pid != 0),
            }))
        }
    };
    if hello.instance_id != expected {
        return Err(ConnectError::Foreign(format!(
            "{socket:?} is served for instance {:?}, not {expected:?}",
            hello.instance_id
        )));
    }
    if !hello.supports(&Capability::Terminals) {
        return Err(ConnectError::Foreign(format!(
            "{socket:?} does not host terminals"
        )));
    }
    write_frame(
        &mut transport.writer,
        &ours.frame().map_err(ConnectError::Refused)?,
    )
    .map_err(refused)?;
    transport
        .reader
        .get_mut()
        .set_read_timeout(None)
        .map_err(refused)?;
    Ok(AdmittedSessionTransport {
        transport,
        hello,
        remote_close: None,
    })
}

type Pending = Mutex<HashMap<u64, mpsc::Sender<Result<Reply, String>>>>;

/// Told when a connection to the sidecar ends.
pub type OnLost = Arc<dyn Fn() + Send + Sync>;

/// Where a connection finds its [`OnLost`] hook as it ends.
pub(crate) type LostHook = Arc<Mutex<Option<OnLost>>>;

/// The app's own account of its sessions in the sidecar, kept across
/// connections.
#[derive(Default)]
pub(crate) struct Ledger {
    /// Sessions withdrawn from the sidecar: ids the app hosts itself while it
    /// uses a sidecar (a spawn that fell open, or one hosted in-process while
    /// the sidecar was unreachable), and a lost spawn being cancelled there.
    /// The sidecar's events for such an id are stale and never delivered.
    withdrawn: Mutex<HashSet<String>>,
    /// Sessions this app hosts in the sidecar — each id with whether it is a
    /// warm-pool member — from their spawn or handoff until they leave its
    /// registry: what a sidecar restart ends.
    /// Each entry is stamped with the spawn or handoff that recorded it, so
    /// an entry replaced meanwhile is never taken for the one checked.
    hosted: Mutex<HashMap<String, (bool, u64)>>,
    /// The last stamp given to a hosted entry.
    stamps: AtomicU64,
    /// Sessions whose spawn is awaiting its reply: not yet known to be
    /// running, or ended.
    spawning: Mutex<HashSet<String>>,
    /// Bumped as a sidecar lifetime ends: a spawn that saw another
    /// generation when it started was taken with its lifetime.
    generation: AtomicU64,
    /// Sessions whose natural exit this app has delivered while they stay in
    /// the sidecar's registry (retained, their terminal readable). The
    /// sidecar repeats a retained session's exit for a client that may have
    /// missed it; a repeat of an exit delivered here — the same process, by
    /// sidecar lifetime and incarnation — is dropped, however late it
    /// arrives, even after this app opened the id again (which only marks
    /// the entry no longer current). An entry goes when the session leaves
    /// the registry.
    exited: Mutex<HashMap<String, Delivered>>,
}

/// How many delivered exits a [`Ledger`] remembers per session id.
const DELIVERED_EXITS_KEPT: usize = 8;

/// The exits a [`Ledger`] delivered for one session id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Delivered {
    /// The processes whose exits they were, by sidecar lifetime (epoch) and
    /// incarnation, oldest first, at most [`DELIVERED_EXITS_KEPT`].
    processes: std::collections::VecDeque<(String, u64)>,
    /// No process was opened under the id since the last of them: the id's
    /// exit is had.
    current: bool,
}

/// Opaque per-machine ownership retained across connections of one verified
/// remote lifetime and frozen when that lifetime changes.
#[derive(Clone, Default)]
pub(crate) struct RemoteSessionLedger(Arc<Ledger>);

impl RemoteSessionLedger {
    #[cfg(test)]
    pub(crate) fn rm4f_snapshot(&self) -> Vec<(PtySessionId, bool)> {
        let _spawning = lock(&self.0.spawning);
        let hosted = lock(&self.0.hosted);
        let withdrawn = lock(&self.0.withdrawn);
        hosted
            .iter()
            .filter(|(session, _)| !withdrawn.contains(session.as_str()))
            .map(|(session, (pool_member, _))| (PtySessionId::new(session), *pool_member))
            .collect()
    }

    pub(crate) fn take_all(&self) -> Vec<(PtySessionId, bool)> {
        self.0.take_all()
    }

    #[cfg(test)]
    pub(crate) fn record(&self, session: &str, pool_member: bool) {
        let mut hosted = lock(&self.0.hosted);
        self.0.record(&mut hosted, session, pool_member);
    }
}

#[cfg(test)]
#[test]
fn rm4f_ledger_snapshot_does_not_drain_or_advance_generation() {
    let ledger = RemoteSessionLedger::default();
    ledger.record("owned", false);
    let generation = ledger.0.generation.load(Ordering::SeqCst);
    let expected = vec![(PtySessionId::new("owned"), false)];
    assert_eq!(ledger.rm4f_snapshot(), expected);
    assert_eq!(ledger.rm4f_snapshot(), expected);
    assert_eq!(ledger.0.generation.load(Ordering::SeqCst), generation);
    assert_eq!(ledger.take_all(), expected);
    assert!(ledger.rm4f_snapshot().is_empty());
}

/// The session an event is about, if any.
fn event_session(event: &TerminalDomainEvent) -> Option<&str> {
    match event {
        TerminalDomainEvent::MachineStats { .. } => None,
        TerminalDomainEvent::Activity { snapshot } => Some(&snapshot.session),
        TerminalDomainEvent::Wake { outcome } => Some(outcome.session.as_str()),
        TerminalDomainEvent::InboxAck { session, .. }
        | TerminalDomainEvent::InboxRelease { session, .. }
        | TerminalDomainEvent::InboxUncertain { session, .. } => Some(session.as_str()),
        TerminalDomainEvent::InboxWaiter { state } => Some(state.session.as_str()),
        TerminalDomainEvent::Output { session, .. }
        | TerminalDomainEvent::PromptMarker { session, .. }
        | TerminalDomainEvent::CwdChanged { session, .. }
        | TerminalDomainEvent::Exit { session, .. }
        | TerminalDomainEvent::SessionRemoved { session, .. } => Some(session.as_str()),
        TerminalDomainEvent::StreamOutput { .. }
        | TerminalDomainEvent::StreamResized { .. }
        | TerminalDomainEvent::WorktreeLive { .. }
        | TerminalDomainEvent::LayoutChanged { .. } => None,
    }
}

/// Why a call failed when the session server took longer than the call's
/// wait to answer it.
const NO_ANSWER: &str = "the session server did not answer in time";

/// Whether `error` is a call the session server did not answer in time (a
/// wedged or very slow host), rather than a refusal or a closed connection.
pub(crate) fn is_no_answer(error: &PortUnavailable) -> bool {
    error.0 == NO_ANSWER
}

/// A handshaken connection to this instance's sidecar. Dropping the last
/// handle closes the connection; the sidecar keeps every session running.
pub struct SessionServerClient {
    machine_stats_watch: AtomicBool,
    machine_stats_control: Mutex<()>,
    machine_stats_unsupported: AtomicBool,
    machine_stats: Mutex<Option<(Instant, codesense_host_proto::machine_stats::MachineStats)>>,
    executor_streams: executor::Streams,
    reverse_streams: Arc<reverse_mcp::ReverseStreams>,
    private_lease: Option<PrivateAuthorityLease>,
    #[cfg(test)]
    observation: Arc<Rm4fObservation>,
    #[cfg(test)]
    rm4f_skip_interrupted_reservation_abort: AtomicBool,
    root: PathBuf,
    writer: Arc<WriteMailbox>,
    /// The sessions' input channels. Held while an input frame is sent, so a
    /// channel's closing is ordered after every write already on it.
    inputs: Mutex<Inputs>,
    pending: Arc<Pending>,
    /// SW-XS-6: where a streaming call's parts go, by `rid`, ahead of its
    /// reply (both arrive in order on the one reader).
    manifest_parts: Mutex<HashMap<u64, mpsc::Sender<Box<ManifestPart>>>>,
    alive: Arc<AtomicBool>,
    ledger: Arc<Ledger>,
    next_rid: AtomicU64,
    /// The sidecar's hello on this connection.
    hello: Hello,
    /// Client-owned authority for every event decoded by this connection.
    namespace: EventNamespace,
    /// The newest layout revision this connection has seen: the hello's, then
    /// each change pushed.
    rev: Arc<AtomicU64>,
    /// SV-7: this connection already told the sidecar what the app holds.
    claimed: AtomicBool,
}

impl Ledger {
    /// Forget and return the hosted sessions `ended` picks (it runs outside
    /// the ledger's locks: it may ask the sidecar), except those the app now
    /// hosts itself (withdrawn), which are only forgotten. A spawn still
    /// awaiting its reply is left to that reply. Lock order: `spawning`, then
    /// `hosted`, then `withdrawn`.
    fn take_ended(&self, ended: impl Fn(&PtySessionId) -> bool) -> Hosted {
        let candidates: Vec<(String, (bool, u64))> = {
            let spawning = lock(&self.spawning);
            lock(&self.hosted)
                .iter()
                .filter(|(session, _)| !spawning.contains(session.as_str()))
                .map(|(session, entry)| (session.clone(), *entry))
                .collect()
        };
        let gone: Vec<(String, (bool, u64))> = candidates
            .into_iter()
            .filter(|(session, _)| ended(&PtySessionId::new(session.as_str())))
            .collect();
        let _spawning = lock(&self.spawning);
        let mut hosted = lock(&self.hosted);
        let withdrawn = lock(&self.withdrawn);
        gone.into_iter()
            // Only the very entry checked: a respawn recorded meanwhile —
            // still awaiting its reply or already answered — stands.
            .filter(|(session, entry)| {
                let same = hosted.get(session) == Some(entry);
                if same {
                    hosted.remove(session);
                }
                same
            })
            .filter(|(session, _)| !withdrawn.contains(session.as_str()))
            .map(|(session, (pool_member, _))| (PtySessionId::new(session), pool_member))
            .collect()
    }

    /// The old lifetime ended: forget and return every session hosted in it,
    /// spawns in flight included (they went with it), except those the app
    /// now hosts itself; spawns still awaiting their reply see the new
    /// generation and leave the ledger alone.
    fn take_all(&self) -> Hosted {
        let _spawning = lock(&self.spawning);
        let mut hosted = lock(&self.hosted);
        let withdrawn = lock(&self.withdrawn);
        self.generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.exited).clear();
        hosted
            .drain()
            .filter(|(session, _)| !withdrawn.contains(session.as_str()))
            .map(|(session, (pool_member, _))| (PtySessionId::new(session), pool_member))
            .collect()
    }

    /// Every session this app opened in the sidecar that has not left it,
    /// warm-pool members and spawns awaiting their reply included.
    fn opened_sessions(&self) -> Vec<PtySessionId> {
        let spawning = lock(&self.spawning);
        let hosted = lock(&self.hosted);
        hosted
            .keys()
            .chain(
                spawning
                    .iter()
                    .filter(|session| !hosted.contains_key(*session)),
            )
            .map(|session| PtySessionId::new(session.as_str()))
            .collect()
    }

    /// The hosted sessions known to have started in the sidecar whose exit
    /// the app has not had: neither a spawn still awaiting its reply, nor one
    /// the app now hosts itself, nor one already delivered exited. Lock
    /// order: `spawning`, then `hosted`, then `withdrawn`, then `exited`.
    fn unexited_sessions(&self) -> Vec<PtySessionId> {
        let spawning = lock(&self.spawning);
        let hosted = lock(&self.hosted);
        let withdrawn = lock(&self.withdrawn);
        let exited = lock(&self.exited);
        hosted
            .keys()
            .filter(|session| {
                !spawning.contains(session.as_str())
                    && !withdrawn.contains(session.as_str())
                    && !exited.get(session.as_str()).is_some_and(|d| d.current)
            })
            .map(|session| PtySessionId::new(session.as_str()))
            .collect()
    }

    /// Record `session` as hosted, under a fresh stamp; its previous entry.
    /// A new process takes the id: the exit delivered for the id is no longer
    /// its current one, though a late repeat of it is still dropped. Lock
    /// order: `hosted`, then `exited`.
    fn record(
        &self,
        hosted: &mut HashMap<String, (bool, u64)>,
        session: &str,
        pool_member: bool,
    ) -> Option<(bool, u64)> {
        let stamp = self.stamps.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(delivered) = lock(&self.exited).get_mut(session) {
            delivered.current = false;
        }
        hosted.insert(session.to_string(), (pool_member, stamp))
    }

    /// Whether `event`, read from the sidecar lifetime `epoch`, reaches the
    /// app: always, except a repeat of the exit of the very process (that
    /// lifetime and incarnation) already delivered while its session stayed
    /// in the registry. Any other process's exit is delivered, whatever its
    /// incarnation (a handed-off pool shell's predates later spawns). A
    /// removal ends the session's record. A requested exit follows its
    /// session's removal and a warm-pool member's is never repeated, so
    /// neither is recorded; nor is an exit with no incarnation (from a host
    /// that never repeats one).
    fn deliver_once(&self, epoch: &str, event: &TerminalDomainEvent) -> bool {
        match event {
            TerminalDomainEvent::SessionRemoved { session, .. } => {
                lock(&self.exited).remove(session.as_str());
                true
            }
            TerminalDomainEvent::Exit {
                session,
                pool_member: false,
                requested: false,
                incarnation,
                ..
            } if *incarnation != 0 => {
                let mut exited = lock(&self.exited);
                let delivered = exited.entry(session.as_str().to_string()).or_default();
                let process = (epoch.to_string(), *incarnation);
                if delivered.processes.contains(&process) {
                    return false;
                }
                if delivered.processes.len() == DELIVERED_EXITS_KEPT {
                    delivered.processes.pop_front();
                }
                delivered.processes.push_back(process);
                delivered.current = true;
                true
            }
            _ => true,
        }
    }
}

impl Drop for SessionServerClient {
    fn drop(&mut self) {
        self.writer.close();
    }
}

pub(crate) struct NativeMcpSelection {
    pub agent_startup: Option<codesense_host_proto::reverse_mcp::AgentStartup>,
    pub server_ids: Vec<String>,
    pub remote_entries: Vec<codesense_host_proto::reverse_mcp::RemoteStdioEntry>,
}

/// The terminal server's usage spool could not be read: it predates the request, or the read failed.
pub(crate) enum UsageReadError {
    /// The terminal server predates the usage spool.
    OutdatedServer,
    Failed(String),
}

/// Why a machine's account home could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountHomeError {
    /// The terminal server predates the account-home request.
    OutdatedServer,
    Failed(String),
}

impl std::fmt::Display for AccountHomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutdatedServer => f.write_str(
                "This machine's terminal server is too old to open in its home folder; update the terminal server, or open in a folder",
            ),
            Self::Failed(error) => f.write_str(error),
        }
    }
}

/// Why an export operation did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportCallError {
    /// Refused or failed; the machine reported or nothing was sent.
    Refused(String),
    /// Sent, but its reply was lost: the machine may have acted on it.
    Lost(String),
}

impl std::fmt::Display for ExportCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(error) | Self::Lost(error) => f.write_str(error),
        }
    }
}

/// A batch control call's default deadline.
pub(crate) const BATCH_CALL_TIMEOUT: Duration = Duration::from_secs(65);
/// Marks a batch call whose reply never came back.
const REPLY_LOST: &str = "[replyLost]";
/// Whether a batch call's error means its reply was lost (the machine may
/// still be working on it), not that it was refused.
pub(crate) fn is_reply_lost(error: &str) -> bool {
    error.starts_with(REPLY_LOST)
}
#[cfg(test)]
thread_local! {
    /// Forces a batch Apply's per-call deadline and total retry budget on
    /// this thread (tests only).
    pub(crate) static APPLY_WAIT_OVERRIDE: std::cell::Cell<Option<(Duration, Duration)>> =
        const { std::cell::Cell::new(None) };
}

/// How long a manifest call may go without a part or its reply. Each part
/// extends it: a cold cache that is still hashing keeps the call alive.
pub(crate) const MANIFEST_IDLE: Duration = Duration::from_secs(30);

/// SW-XS-8: the file bytes one manifest streamed, assembled per path.
#[derive(Debug, Default)]
pub(crate) struct ManifestContents {
    pub(crate) files: std::collections::BTreeMap<String, Vec<u8>>,
    open: std::collections::BTreeMap<String, Vec<u8>>,
    broken: Option<String>,
}
impl ManifestContents {
    fn take(&mut self, piece: codesense_pty_core::remote_manifest::ManifestContent) {
        let buffer = self.open.entry(piece.path.clone()).or_default();
        if buffer.len() as u64 != piece.offset {
            self.broken
                .get_or_insert_with(|| format!("{} arrived out of order", piece.path));
            return;
        }
        buffer.extend_from_slice(&piece.bytes);
        if piece.last {
            let bytes = self.open.remove(&piece.path).unwrap_or_default();
            self.files.insert(piece.path, bytes);
        }
    }
    /// Every file the reply says was sent, whole.
    fn finish(self, read: &[String]) -> Result<Self, String> {
        if let Some(broken) = self.broken {
            return Err(format!(
                "the manifest's file contents broke in transit: {broken}"
            ));
        }
        if !self.open.is_empty() || read.iter().any(|path| !self.files.contains_key(path)) {
            return Err("the manifest lost file contents in transit".into());
        }
        Ok(self)
    }
}

/// Why a workspace manifest could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManifestCallError {
    /// The terminal server predates `ContextManifestV1`. Nothing was sent.
    OutdatedServer,
    Failed(String),
}

impl From<&str> for ManifestCallError {
    fn from(error: &str) -> Self {
        Self::Failed(error.into())
    }
}

/// Why a managed workspace's checkout could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceHeadError {
    /// The terminal server predates the workspace head request.
    OutdatedServer,
    Failed(String),
}

impl From<&str> for WorkspaceHeadError {
    fn from(error: &str) -> Self {
        Self::Failed(error.into())
    }
}

impl std::fmt::Display for WorkspaceHeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutdatedServer => f.write_str(
                "This machine's terminal server is too old to bring work home; update the terminal server, then try again",
            ),
            Self::Failed(error) => f.write_str(error),
        }
    }
}

impl SessionServerClient {
    #[cfg(test)]
    pub(crate) fn remote_ledger_for_test(&self) -> RemoteSessionLedger {
        RemoteSessionLedger(self.ledger.clone())
    }

    pub(crate) fn server_pid(&self) -> u32 {
        self.hello.pid
    }

    pub(crate) fn supports_replacement(&self) -> bool {
        self.hello
            .supports(&codesense_host_proto::Capability::ShutdownForReplacement)
    }

    /// Manager control-plane liveness probe. The caller owns the exact response
    /// deadline; unlike ordinary reads this must not inherit READ_TIMEOUT.
    pub(crate) fn heartbeat(&self, timeout: Duration) -> Result<(), PortUnavailable> {
        self.ask(
            Request::LiveSessionIds,
            timeout,
            reply!(Reply::Sessions { .. } => ()),
        )
    }

    /// Exact fail-closed handoff from RM-4 admission. Callers cannot supply
    /// transport halves or a Hello independently.
    pub(crate) async fn from_remote_admitted(
        admitted: AdmittedRemoteTransport,
        sink: Arc<dyn NamespacedTerminalEventSink>,
        ledger: Arc<Ledger>,
        lost: Option<LostHook>,
    ) -> Result<ManagedRemoteClient, ConnectError> {
        let (reader, writer, hello, close, private_lease) =
            admitted.into_client_parts().into_session_bridge();
        let root = PathBuf::from(&hello.instance_id);
        let publication = RemotePublicationGate::new_private(sink, lost, private_lease.clone());
        let gated_lost: LostHook = Arc::new(Mutex::new(Some({
            let publication = publication.clone();
            Arc::new(move || publication.lost()) as OnLost
        })));
        let interrupt = {
            let close = close.clone();
            move || close.begin_close()
        };
        let mut transport = SessionServerTransport::new(reader, writer, interrupt);
        transport.private_lease = private_lease;
        let admitted = AdmittedSessionTransport {
            transport,
            hello,
            remote_close: Some(close.clone()),
        };
        match Self::from_admitted_transport_inner(
            &root,
            admitted,
            PumpEventSink::Namespaced(publication.clone()),
            ledger,
            Some(gated_lost),
            None,
        ) {
            Ok(client) => Ok(ManagedRemoteClient {
                client,
                close,
                publication,
            }),
            Err(error) => {
                close.close_awaited().await;
                Err(error)
            }
        }
    }

    /// Connect to the sidecar of the instance rooted at `root`, verify it
    /// serves that instance, and start handing its events to `sink`; `lost`
    /// is told when the connection ends.
    #[cfg(test)]
    pub(crate) fn connect(
        root: &Path,
        sink: Arc<dyn TerminalEventSink>,
        ledger: Arc<Ledger>,
        lost: Option<LostHook>,
    ) -> Result<Arc<Self>, ConnectError> {
        Self::from_admitted_transport(root, handshake(root)?, sink, ledger, lost)
    }

    /// Start the one client over an already-admitted byte transport.
    /// Admission remains connector-owned: callers must verify instance,
    /// protocol capabilities, and any connector-specific identity before
    /// handing the transport to this constructor.
    #[cfg(test)]
    pub(crate) fn from_admitted_transport(
        root: &Path,
        admitted: AdmittedSessionTransport,
        sink: Arc<dyn TerminalEventSink>,
        ledger: Arc<Ledger>,
        lost: Option<LostHook>,
    ) -> Result<Arc<Self>, ConnectError> {
        Self::from_admitted_transport_inner(
            root,
            admitted,
            PumpEventSink::Legacy(sink),
            ledger,
            lost,
            None,
        )
    }

    fn from_admitted_transport_inner(
        root: &Path,
        admitted: AdmittedSessionTransport,
        sink: PumpEventSink,
        ledger: Arc<Ledger>,
        lost: Option<LostHook>,
        local_admission: Option<Arc<LocalRuntimeAdmission>>,
    ) -> Result<Arc<Self>, ConnectError> {
        let AdmittedSessionTransport {
            transport,
            hello,
            remote_close,
        } = admitted;
        let is_remote = remote_close.is_some();
        let SessionServerTransport {
            private_lease,
            reader,
            writer,
            interrupt,
            #[cfg(test)]
            observation,
        } = transport;
        let namespace = mint_event_namespace(&hello.server_id, &hello.epoch)?;
        let sink = local_admission
            .as_ref()
            .and_then(|owner| lock(&owner.sink_factory).clone())
            .map(|factory| PumpEventSink::Legacy(factory(&namespace)))
            .unwrap_or(sink);
        let alive = Arc::new(AtomicBool::new(true));
        let writer = WriteMailbox::start(writer, interrupt, alive.clone()).map_err(|error| {
            ConnectError::Refused(format!("start the session-server writer: {error}"))
        })?;
        if let Some(lease) = &private_lease {
            let _ = writer.private_lease.set(lease.clone());
        }
        if let Some(close) = &remote_close {
            close.register_writer(writer.clone());
        }
        let client = Arc::new(Self {
            machine_stats: Mutex::default(),
            machine_stats_watch: AtomicBool::new(false),
            machine_stats_control: Mutex::new(()),
            machine_stats_unsupported: AtomicBool::new(false),
            reverse_streams: Arc::default(),
            executor_streams: Default::default(),
            private_lease,
            #[cfg(test)]
            observation,
            #[cfg(test)]
            rm4f_skip_interrupted_reservation_abort: AtomicBool::new(false),
            root: root.to_path_buf(),
            writer,
            inputs: Mutex::default(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            manifest_parts: Mutex::default(),
            alive,
            ledger: ledger.clone(),
            next_rid: AtomicU64::new(1),
            rev: Arc::new(AtomicU64::new(hello.rev)),
            claimed: AtomicBool::new(false),
            namespace: namespace.clone(),
            hello,
        });
        let pending = client.pending.clone();
        let alive = client.alive.clone();
        let rev = client.rev.clone();
        let inputs_of = Arc::downgrade(&client);
        if let Some(owner) = &local_admission {
            owner.register_candidate(&client)?;
        }
        let local_loss = local_admission.map(|owner| {
            let namespace = namespace.clone();
            Arc::new(move || owner.lost(&namespace)) as OnLost
        });
        std::thread::Builder::new()
            .name("pty-server-client".into())
            .spawn(move || {
                let connection = Connection {
                    pending: &pending,
                    alive: &alive,
                    rev: &rev,
                    ledger: &ledger,
                    client: &inputs_of,
                    namespace: &namespace,
                    is_remote,
                };
                if let Some(local_loss) = local_loss {
                    let lost = Arc::new(Mutex::new(Some(Arc::new(move || {
                        local_loss();
                        if let Some(callback) = lost.as_ref().and_then(|hook| lock(hook).clone()) {
                            off_the_reader("pty-local-reconnect", move || callback());
                        }
                    }) as OnLost)));
                    pump(reader, connection, &sink, Some(lost));
                } else {
                    pump(reader, connection, &sink, lost);
                }
            })
            .map_err(|e| ConnectError::Refused(format!("start the session-server reader: {e}")))?;
        client
            .done(Request::Listen, HANDSHAKE_TIMEOUT)
            .map_err(|e| ConnectError::Refused(format!("listen to the session server: {e}")))?;
        Ok(client)
    }

    #[cfg(test)]
    pub(crate) fn prepare_reverse_mcp(
        &self,
        session: &PtySessionId,
        nonce: String,
        serve: Arc<dyn Fn(tokio::io::DuplexStream) + Send + Sync>,
    ) -> Result<(), PortUnavailable> {
        self.prepare_reverse_mcp_servers(
            session,
            nonce,
            codesense_host_proto::reverse_mcp::default_servers(),
            Arc::new(move |_, stream| serve(stream)),
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_reverse_mcp_servers(
        &self,
        session: &PtySessionId,
        nonce: String,
        server_ids: Vec<String>,
        serve: Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync>,
    ) -> Result<(), PortUnavailable> {
        self.prepare_reverse_mcp_launch(
            session,
            nonce,
            NativeMcpSelection {
                server_ids,
                remote_entries: Vec::new(),
                agent_startup: None,
            },
            Default::default(),
            serve,
        )
    }

    /// Status for every agent this host runs, typed by hand or launched. An
    /// older host still answers activity for its launches, but its shells
    /// stay dark: it needs an update.
    pub(crate) fn supports_activity(&self) -> bool {
        self.hello.supports(&Capability::ActivityV2)
    }

    pub(crate) fn supports_attribution_hook_trust(&self) -> bool {
        self.hello.supports(&Capability::AttributionHookTrustV1)
    }

    pub(crate) fn require_spend_holds(&self) -> Result<(), String> {
        require_spend_hold_capability(&self.hello)
    }

    pub(crate) fn supports_agent_startup(&self) -> bool {
        self.hello.supports(&Capability::AgentStartup)
    }

    /// Whether this terminal server sets a saved Agent's own variables (`AgentEnvV1`).
    pub(crate) fn supports_agent_env(&self) -> bool {
        self.hello.proto_minor >= AGENT_ENV_PROTO_MINOR
            && self.hello.supports(&Capability::AgentEnvV1)
    }

    /// Refuses a launch whose agent sets variables (`sets_env`) when this terminal server
    /// would drop them; a launch that sets none goes anywhere.
    pub(crate) fn require_agent_env(&self, sets_env: bool) -> Result<(), String> {
        if sets_env && !self.supports_agent_env() {
            return Err(AGENT_ENV_NEEDS_UPDATED_SERVER.to_string());
        }
        Ok(())
    }

    pub(crate) fn preflight_native_remote(
        &self,
        session: &PtySessionId,
        cwd: &str,
        launch: NativeLaunch,
    ) -> Result<(), PortUnavailable> {
        if !self.supports_agent_startup() {
            return Err(PortUnavailable(
                "saved-Agent native preflight requires an updated terminal server".into(),
            ));
        }
        self.private_lease
            .as_ref()
            .ok_or_else(|| PortUnavailable("private admission required".into()))?
            .validate()
            .map_err(PortUnavailable)?;
        self.done(
            Request::PreflightNativeRemote {
                session: session.clone(),
                cwd: cwd.into(),
                launch,
            },
            Duration::from_secs(30),
        )
    }

    pub(crate) fn prepare_native_remote(
        &self,
        session: &PtySessionId,
        nonce: String,
        selection: NativeMcpSelection,
        launch: NativeLaunch,
        serve: Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync>,
    ) -> Result<(), PortUnavailable> {
        if !self.hello.supports(&Capability::NativeRemoteLaunch) {
            return Err(PortUnavailable(
                "session-bound remote launch unavailable; update this machine's terminal server"
                    .into(),
            ));
        }
        self.prepare_reverse_mcp_launch(session, nonce, selection, launch, serve)
    }

    fn prepare_reverse_mcp_launch(
        &self,
        session: &PtySessionId,
        nonce: String,
        selection: NativeMcpSelection,
        launch: NativeLaunch,
        serve: Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync>,
    ) -> Result<(), PortUnavailable> {
        let NativeMcpSelection {
            server_ids,
            remote_entries,
            agent_startup,
        } = selection;
        if agent_startup.is_some() && !self.supports_agent_startup() {
            return Err(PortUnavailable(
                "saved-Agent startup requires an updated remote terminal server".into(),
            ));
        }
        self.require_agent_env(
            agent_startup
                .as_ref()
                .is_some_and(|agent| !agent.env.is_empty()),
        )
        .map_err(PortUnavailable)?;
        if let Some(agent) = &agent_startup {
            codesense_pty_core::agent_cli::validate_startup(agent, launch)
                .map_err(PortUnavailable)?;
        }
        codesense_host_proto::reverse_mcp::validate_remote_entries(&server_ids, &remote_entries)
            .map_err(PortUnavailable)?;
        if !remote_entries.is_empty() && !self.hello.supports(&Capability::RemoteStdioMcp) {
            return Err(PortUnavailable(
                "remote stdio MCP unavailable; update this machine's terminal server".into(),
            ));
        }
        let server_ids = codesense_host_proto::reverse_mcp::selected_servers(server_ids)
            .map_err(PortUnavailable)?;
        if server_ids.len() > 1 && !self.hello.supports(&Capability::ReverseMcpServers) {
            return Err(PortUnavailable(
                "selected MCP servers unavailable on this server".into(),
            ));
        }
        if !self.hello.supports(&Capability::ReverseMcp) {
            return Err(PortUnavailable(
                "reverse MCP unavailable on this server".into(),
            ));
        }
        self.private_lease
            .as_ref()
            .ok_or_else(|| PortUnavailable("private admission required".into()))?
            .validate()
            .map_err(PortUnavailable)?;
        // The stream pumps need an async runtime; callers prepare from one.
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PortUnavailable("reverse MCP needs an async runtime".into()))?;
        self.reverse_streams
            .bind(
                session.as_str(),
                nonce.clone(),
                server_ids.clone(),
                serve,
                runtime,
            )
            .map_err(PortUnavailable)?;
        let result = self.done(
            Request::PrepareReverseMcp {
                session: session.clone(),
                binding_nonce: nonce,
                server_ids,
                remote_entries,
                launch,
                agent_startup,
            },
            HANDSHAKE_TIMEOUT,
        );
        if result.is_err() {
            self.release_reverse_mcp(session);
        }
        result
    }

    pub(crate) fn release_reverse_mcp(&self, session: &PtySessionId) {
        self.reverse_streams.unbind(session.as_str());
        let _ = self.post(Request::ReleaseReverseMcp {
            session: session.clone(),
        });
    }
    /// The sidecar's process id.
    pub(crate) fn pid(&self) -> u32 {
        self.hello.pid
    }

    /// This sidecar lifetime's epoch.
    pub fn epoch(&self) -> &str {
        &self.hello.epoch
    }

    pub(crate) fn server_id(&self) -> &str {
        &self.hello.server_id
    }

    /// SV-7: tell the sidecar the app holds `sessions` (they are attributed
    /// to it from now on); `false` from one that predates the question.
    pub(crate) fn claim_sessions(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<bool, PortUnavailable> {
        if !self.hello.supports(&Capability::ServerStatusV1) {
            return Ok(false);
        }
        self.done(
            Request::ClaimSessions {
                sessions: sessions.to_vec(),
            },
            READ_TIMEOUT,
        )
        .map(|()| true)
    }

    /// SV-7: tell the sidecar the app holds `sessions`, once per connection
    /// (`again` asks anew): until a claim is answered, each call tries again.
    pub(crate) fn claim_once(
        &self,
        sessions: &[PtySessionId],
        again: bool,
    ) -> Result<(), PortUnavailable> {
        if self.claimed.load(Ordering::SeqCst) && !again {
            return Ok(());
        }
        self.claim_sessions(sessions)?;
        self.claimed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// SV-7: whether a claim on this connection has been answered.
    #[cfg(test)]
    pub(crate) fn claim_answered(&self) -> bool {
        self.claimed.load(Ordering::SeqCst)
    }

    /// SV-7: end `session` if, checked as it ends, no app holds it.
    pub(crate) fn end_unowned_session(
        &self,
        session: &PtySessionId,
    ) -> Result<(), PortUnavailable> {
        if !self.hello.supports(&Capability::ServerStatusV1) {
            return Err(PortUnavailable(
                "this terminal server is older than this app: restart it first".into(),
            ));
        }
        self.done(
            Request::EndUnownedSession {
                session: session.clone(),
            },
            READ_TIMEOUT,
        )
    }

    pub(crate) fn supports_machine_stats(&self) -> bool {
        self.hello.supports(&Capability::MachineStatsV1)
            && !self.machine_stats_unsupported.load(Ordering::Acquire)
    }
    pub(crate) fn cached_machine_stats(
        &self,
    ) -> Option<codesense_host_proto::machine_stats::MachineStats> {
        lock(&self.machine_stats)
            .as_ref()
            .map(|(_, sample)| sample.clone())
    }
    pub(crate) fn read_machine_stats(
        &self,
    ) -> Result<Option<codesense_host_proto::machine_stats::MachineStats>, PortUnavailable> {
        if !self.supports_machine_stats() {
            return Ok(None);
        }
        if let Some((at, sample)) = lock(&self.machine_stats).as_ref() {
            if at.elapsed() < Duration::from_secs(30) {
                return Ok(Some(sample.clone()));
            }
        }
        self.request_machine_stats(Request::MachineStats)
    }
    pub(crate) fn set_machine_stats_watching(&self, watch: bool) {
        self.machine_stats_watch.store(watch, Ordering::Release);
    }
    pub(crate) fn sync_machine_stats_watch(
        &self,
    ) -> Result<Option<codesense_host_proto::machine_stats::MachineStats>, PortUnavailable> {
        if !self.supports_machine_stats() {
            return Ok(None);
        }
        let _control = lock(&self.machine_stats_control);
        self.request_machine_stats(Request::MachineStatsWatch {
            watch: self.machine_stats_watch.load(Ordering::Acquire),
        })
    }
    fn request_machine_stats(
        &self,
        request: Request,
    ) -> Result<Option<codesense_host_proto::machine_stats::MachineStats>, PortUnavailable> {
        let sample = match self.ask(
            request,
            READ_TIMEOUT,
            reply!(Reply::MachineStats { sample } => sample),
        ) {
            Ok(sample) => sample,
            Err(error)
                if error.to_string().contains("unknown")
                    && error.to_string().contains("request") =>
            {
                self.machine_stats_unsupported
                    .store(true, Ordering::Release);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if let Some(sample) = &sample {
            *lock(&self.machine_stats) = Some((Instant::now(), sample.clone()));
        }
        Ok(sample)
    }

    /// SV-7: what this terminal server holds; `None` from one that predates
    /// the question, which is never sent it.
    pub(crate) fn server_status(
        &self,
    ) -> Result<Option<codesense_pty_core::protocol::ServerStatus>, PortUnavailable> {
        if !self.hello.supports(&Capability::ServerStatusV1) {
            return Ok(None);
        }
        self.ask(
            Request::ServerStatus,
            READ_TIMEOUT,
            reply!(Reply::ServerStatus { status } => Some(*status)),
        )
    }

    pub(crate) fn namespace(&self) -> &EventNamespace {
        &self.namespace
    }

    /// The newest layout revision this connection has seen.
    fn rev(&self) -> u64 {
        self.rev.load(Ordering::SeqCst)
    }

    /// Cut the connection, as a crash or a sleep would.
    #[cfg(test)]
    pub(crate) fn sever(&self) {
        self.writer.close();
    }

    pub(crate) fn private_lease(&self) -> Option<PrivateAuthorityLease> {
        self.private_lease.clone()
    }

    /// Called with the manager's own state lock already acquired. The effect
    /// only mutates local state; never acquire manager/auth locks inside it.
    pub(crate) fn commit_private<T>(&self, effect: impl FnOnce() -> T) -> Result<T, String> {
        match &self.private_lease {
            Some(lease) => lease.commit(effect),
            None => Ok(effect()),
        }
    }

    /// Whether the connection is still up.
    pub fn is_alive(&self) -> bool {
        self.private_lease
            .as_ref()
            .is_none_or(|lease| lease.validate().is_ok())
            && self.alive.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn rm4f_observe(&self) {
        self.observation.enabled.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn rm4f_snapshot(&self) -> Rm4fSnapshot {
        lock(&self.observation.snapshot).clone()
    }

    #[cfg(test)]
    pub(crate) fn rm4f_skip_abort(&self, enabled: bool) {
        self.rm4f_skip_interrupted_reservation_abort
            .store(enabled, Ordering::Release);
    }

    pub(crate) fn supports_selected_context_batch(&self) -> bool {
        self.hello.supports(&Capability::SelectedContextBatchV1)
    }

    /// The version the host announced, with its installed build as semver
    /// build metadata when it has one.
    pub(crate) fn announced_version(&self) -> &str {
        &self.hello.agent_version
    }

    /// The protocol minor the host speaks.
    pub(crate) fn proto_minor(&self) -> u16 {
        self.hello.proto_minor
    }

    /// SW-AGR-19: the build the host announced (hello minor 16), if any.
    pub(crate) fn build_stamp(&self) -> Option<&codesense_host_proto::BuildStamp> {
        self.hello.build.as_ref()
    }

    /// How many sessions the host runs now, if it can say.
    pub(crate) fn live_session_count(&self) -> Option<u32> {
        self.ask(
            Request::LiveSessionIds,
            READ_TIMEOUT,
            reply!(Reply::Sessions { sessions } => sessions),
        )
        .ok()
        .map(|sessions| u32::try_from(sessions.len()).unwrap_or(u32::MAX))
    }

    /// SW-AGR-19: the sessions the host runs now, sorted, if it can say.
    pub(crate) fn live_session_id_list(&self) -> Option<Vec<String>> {
        self.ask(
            Request::LiveSessionIds,
            READ_TIMEOUT,
            reply!(Reply::Sessions { sessions } => sessions),
        )
        .ok()
        .map(|sessions| {
            let mut ids: Vec<String> = sessions
                .into_iter()
                .map(|id| id.as_str().to_string())
                .collect();
            ids.sort();
            ids
        })
    }

    /// SW-XS-6: the streamed manifest and the extended batch Apply.
    pub(crate) fn supports_context_manifest(&self) -> bool {
        self.hello.supports(&Capability::ContextManifestV1)
    }

    /// SW-XS-7: the manifest reports a named workspace's exclude block.
    pub(crate) fn supports_manifest_workspace_exclude(&self) -> bool {
        self.hello.supports(&Capability::ManifestWorkspaceExcludeV1)
    }

    /// SW-XS-8: the manifest also streams the bytes of named files.
    pub(crate) fn supports_manifest_read(&self) -> bool {
        self.hello.supports(&Capability::ManifestReadV1)
    }

    /// SW-XS-26: whether the host decodes a zstd context batch.
    pub(crate) fn supports_context_batch_zstd(&self) -> bool {
        self.hello.supports(&Capability::ContextBatchZstdV1)
    }

    /// SW-XS-6: one `WorkspaceManifest` call. The host streams parts on the
    /// call's `rid`, then replies; this waits as long as parts keep arriving
    /// (a cold cache hashes for a while) and gives up after [`MANIFEST_IDLE`]
    /// of silence. A host without `ContextManifestV1` is refused before
    /// anything is sent: there is no per-file fallback.
    pub(crate) fn workspace_manifest(
        &self,
        request: ManifestRequest,
        progress: &mut dyn FnMut(&ManifestProgress),
    ) -> Result<(ManifestReply, Vec<ManifestEntry>, ManifestContents), ManifestCallError> {
        if !self.supports_context_manifest()
            || (!request.read.is_empty() && !self.supports_manifest_read())
        {
            return Err(ManifestCallError::OutdatedServer);
        }
        let lease = || -> Result<(), ManifestCallError> {
            self.private_lease()
                .ok_or("private authority unavailable")?
                .validate()
                .map_err(ManifestCallError::Failed)
        };
        lease()?;
        let rid = self.rid();
        let frame = Frame::control(&Message::Call {
            rid,
            request: Request::WorkspaceManifest {
                request: Box::new(request),
            },
        })
        .map_err(ManifestCallError::Failed)?;
        let (parts_tx, parts) = mpsc::channel();
        lock(&self.manifest_parts).insert(rid, parts_tx);
        let (tx, replies) = mpsc::channel();
        lock(&self.pending).insert(rid, tx);
        let forget = || {
            lock(&self.pending).remove(&rid);
            lock(&self.manifest_parts).remove(&rid);
        };
        if !self.is_alive() {
            forget();
            return Err(ManifestCallError::Failed(
                "the session server connection is closed".into(),
            ));
        }
        if let Err(error) = self.send(&frame) {
            forget();
            return Err(ManifestCallError::Failed(String::from(error)));
        }
        let mut entries = Vec::new();
        let mut contents = ManifestContents::default();
        let mut take = |part: Box<ManifestPart>,
                        entries: &mut Vec<ManifestEntry>,
                        contents: &mut ManifestContents| {
            progress(&part.progress);
            entries.extend(part.entries);
            for piece in part.contents {
                contents.take(piece);
            }
        };
        let mut quiet_since = Instant::now();
        let result = loop {
            while let Ok(part) = parts.try_recv() {
                take(part, &mut entries, &mut contents);
                quiet_since = Instant::now();
            }
            match replies.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => {
                    // Every part was delivered before its reply.
                    while let Ok(part) = parts.try_recv() {
                        take(part, &mut entries, &mut contents);
                    }
                    break result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) if quiet_since.elapsed() < MANIFEST_IDLE => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    forget();
                    return Err(ManifestCallError::Failed(
                        "the session server stopped answering the manifest".into(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    forget();
                    return Err(ManifestCallError::Failed(
                        "the session server connection closed before answering".into(),
                    ));
                }
            }
        };
        lock(&self.manifest_parts).remove(&rid);
        lease()?;
        match result {
            Ok(Reply::WorkspaceManifest { result }) => {
                if result.entries != entries.len() as u64 {
                    return Err(ManifestCallError::Failed(
                        "the manifest lost entries in transit".into(),
                    ));
                }
                let contents = contents
                    .finish(&result.read)
                    .map_err(ManifestCallError::Failed)?;
                Ok((*result, entries, contents))
            }
            Ok(_) => Err(ManifestCallError::Failed(
                "unexpected workspace manifest reply".into(),
            )),
            Err(error) => Err(ManifestCallError::Failed(error)),
        }
    }

    /// The host account's home directory, as the host itself reads it. An
    /// older terminal server that predates the request is refused before
    /// anything is sent to it.
    pub(crate) fn account_home(&self) -> Result<PathBuf, AccountHomeError> {
        if !self.hello.supports(&Capability::AccountHome) {
            return Err(AccountHomeError::OutdatedServer);
        }
        self.ask(
            Request::AccountHome,
            READ_TIMEOUT,
            reply!(Reply::Cwd { cwd } => cwd),
        )
        .map_err(|error| AccountHomeError::Failed(error.to_string()))?
        .ok_or_else(|| {
            AccountHomeError::Failed("This machine has no home folder for its account".into())
        })
    }

    /// The terminal server's usage spool past `after`, at most `limit` records. A server that predates
    /// the question is refused before anything is sent to it.
    pub(crate) fn usage_read(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<UsageSpoolRead, UsageReadError> {
        if !self.hello.supports(&Capability::UsageSpoolV1) {
            return Err(UsageReadError::OutdatedServer);
        }
        self.ask(
            Request::UsageRead {
                after_seq: after,
                limit: limit.min(u32::MAX as usize) as u32,
            },
            READ_TIMEOUT,
            reply!(Reply::UsageRead { read } => *read),
        )
        .map_err(|error| UsageReadError::Failed(error.to_string()))
    }

    /// `session`'s prompt segment (cwd, account home, Git checkout status),
    /// read on the host that runs it. `None` from a terminal server that
    /// predates the question, which is never sent it.
    pub(crate) fn session_prompt(
        &self,
        session: &PtySessionId,
    ) -> Result<Option<codesense_pty_core::git_prompt::SessionPrompt>, PortUnavailable> {
        if !self.hello.supports(&Capability::SessionPrompt) {
            return Ok(None);
        }
        self.ask(
            Request::SessionPrompt {
                session: session.clone(),
            },
            // The host bounds its own `git status`; this only covers the hop.
            Duration::from_secs(8),
            reply!(Reply::Prompt { prompt } => prompt),
        )
        .map(|prompt| Some(*prompt))
    }

    /// Whether `launch`'s CLI is installed for the host account, read from
    /// `cwd` with its login PATH. `None` from a terminal server that predates
    /// the question: the launch itself still refuses a missing CLI.
    pub(crate) fn native_runtime_installed(
        &self,
        cwd: &str,
        launch: NativeLaunch,
    ) -> Result<Option<bool>, PortUnavailable> {
        if !self.hello.supports(&Capability::NativeRuntimeProbe) {
            return Ok(None);
        }
        self.ask(
            Request::ProbeNativeRuntime {
                cwd: cwd.into(),
                launch,
            },
            Duration::from_secs(10),
            reply!(Reply::Flag { value } => value),
        )
        .map(Some)
    }

    pub(crate) fn workspace_context_batch(
        &self,
        request: codesense_pty_core::context_batch::BatchRequest,
    ) -> Result<codesense_pty_core::context_batch::BatchReply, String> {
        self.workspace_context_batch_within(request, BATCH_CALL_TIMEOUT)
    }

    /// One batch call that may take `wait` (an Apply scales with its batch).
    /// A reply that never came back says so ([`is_reply_lost`]): the machine
    /// may still be working on it.
    pub(crate) fn workspace_context_batch_within(
        &self,
        request: codesense_pty_core::context_batch::BatchRequest,
        wait: Duration,
    ) -> Result<codesense_pty_core::context_batch::BatchReply, String> {
        if !self.supports_selected_context_batch() {
            return Err("selected context batching was not negotiated".into());
        }
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::WorkspaceContextBatch {
                request: Box::new(request),
            },
            wait,
        ) {
            Ok(Reply::WorkspaceContextBatch { result }) => Ok(*result),
            Ok(_) => Err("unexpected selected context batch reply".into()),
            Err(CallError::Lost(message)) => Err(format!("{REPLY_LOST} {message}")),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    /// One export operation. A reply that never came back is told apart from
    /// a refusal: the machine may have acted on it.
    pub(crate) fn workspace_export(
        &self,
        request: codesense_pty_core::remote_export::ExportRequest,
    ) -> Result<codesense_pty_core::remote_export::ExportReply, ExportCallError> {
        let lease = || -> Result<(), ExportCallError> {
            self.private_lease()
                .ok_or_else(|| ExportCallError::Refused("private authority unavailable".into()))?
                .validate()
                .map_err(ExportCallError::Refused)
        };
        lease()?;
        let timeout = if matches!(
            request.operation,
            codesense_pty_core::remote_export::ExportOperation::Prepare
        ) {
            Duration::from_secs(30 * 60 + 30)
        } else {
            Duration::from_secs(65)
        };
        let result = match self.call(
            Request::WorkspaceExport {
                request: Box::new(request),
            },
            timeout,
        ) {
            Ok(Reply::WorkspaceExport { result }) => Ok(*result),
            Ok(_) => Err(ExportCallError::Refused(
                "unexpected workspace export reply".into(),
            )),
            Err(CallError::Lost(message)) => Err(ExportCallError::Lost(message)),
            Err(error) => Err(ExportCallError::Refused(String::from(error))),
        };
        lease()?;
        result
    }

    /// The commit and branch a managed workspace's checkout is on. A terminal
    /// server that predates the question is refused by name, so the caller can
    /// say to update it.
    pub(crate) fn workspace_head(
        &self,
        target: codesense_pty_core::remote_files::FileTarget,
    ) -> Result<codesense_pty_core::remote_export::WorkspaceHead, WorkspaceHeadError> {
        if !self.hello.supports(&Capability::WorkspaceHead) {
            return Err(WorkspaceHeadError::OutdatedServer);
        }
        let lease = || -> Result<(), WorkspaceHeadError> {
            self.private_lease()
                .ok_or("private authority unavailable")?
                .validate()
                .map_err(WorkspaceHeadError::Failed)
        };
        lease()?;
        let result = match self.call(
            Request::WorkspaceHead {
                target: Box::new(target),
            },
            Duration::from_secs(65),
        ) {
            Ok(Reply::WorkspaceHead { head }) => Ok(head),
            Ok(_) => Err(WorkspaceHeadError::Failed(
                "unexpected workspace head reply".into(),
            )),
            Err(error) => Err(WorkspaceHeadError::Failed(String::from(error))),
        };
        lease()?;
        result
    }

    /// Whether this terminal server answers the workspace editor's file
    /// operations (`List`, `ReadHead`, `ListFiles`). An older server refuses
    /// them as unknown, so callers fall back to the operations it does know.
    pub(crate) fn supports_workspace_editor(&self) -> bool {
        self.hello.supports(&Capability::WorkspaceEditorV1)
    }

    pub(crate) fn workspace_file(
        &self,
        request: codesense_pty_core::remote_files::FileRequest,
    ) -> Result<codesense_pty_core::remote_files::FileReply, String> {
        // SW-XS-2: a lock-free read the machine answered "busy" changed
        // nothing; it is retried a few times with backoff before failing.
        let retries = if request.operation.is_read() { 3 } else { 0 };
        let mut backoff = Duration::from_millis(50);
        for attempt in 0..=retries {
            self.private_lease()
                .ok_or("private authority unavailable")?
                .validate()?;
            let result = match self.call(
                Request::WorkspaceFile {
                    request: Box::new(request.clone()),
                },
                Duration::from_secs(65),
            ) {
                Ok(Reply::WorkspaceFile { result }) => Ok(*result),
                Ok(_) => Err("unexpected workspace file reply".into()),
                Err(error) => Err(String::from(error)),
            };
            self.private_lease()
                .ok_or("private authority unavailable")?
                .validate()?;
            match result {
                Err(error)
                    if attempt < retries
                        && error.contains(codesense_pty_core::remote_files::WORKSPACE_BUSY) =>
                {
                    std::thread::sleep(backoff);
                    backoff *= 2;
                }
                result => return result,
            }
        }
        unreachable!("the last attempt returns")
    }

    pub(crate) fn reconcile_remote_workspace(
        &self,
        request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    ) -> Result<Option<codesense_pty_core::remote_workspace::PreparedWorkspace>, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::ReconcileRemoteWorkspace { request },
            Duration::from_secs(35),
        ) {
            Ok(Reply::RemoteWorkspaceReceipt { result }) => Ok(result.map(|value| *value)),
            Ok(_) => Err("unexpected workspace reconciliation reply".into()),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    pub(crate) fn inspect_workspace_repository(
        &self,
        request: codesense_pty_core::remote_workspace::InspectRepository,
    ) -> Result<codesense_pty_core::remote_workspace::RepositoryKnowledge, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::InspectWorkspaceRepository { request },
            Duration::from_secs(35),
        ) {
            Ok(Reply::WorkspaceRepositoryKnowledge { knowledge }) => Ok(knowledge),
            Ok(_) => Err("unexpected repository inspection reply".into()),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    pub(crate) fn forget_missing_workspace(
        &self,
        request: codesense_pty_core::remote_workspace::ForgetMissingWorkspace,
    ) -> Result<(), String> {
        if !self.hello.supports(&Capability::ForgetMissingWorkspaceV1) {
            return Err(format!(
                "[{}] workspace cleanup requires a newer terminal server",
                crate::content_engine::destination::TERMINAL_SERVER_OUTDATED
            ));
        }
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::ForgetMissingWorkspace { request },
            Duration::from_secs(60),
        ) {
            Ok(Reply::Done) => Ok(()),
            Ok(_) => Err("unexpected workspace cleanup reply".into()),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    /// SW-WS-1: tear down a managed worktree on this machine. Idempotent — a
    /// missing worktree answers `removed: false` with a note, not an error,
    /// so the move's boot reaper can retry safely.
    pub(crate) fn remove_worktree(
        &self,
        request: codesense_pty_core::remote_workspace::RemoveWorktree,
    ) -> Result<codesense_pty_core::remote_workspace::RemoveWorktreeResult, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::RemoveWorktree { request },
            Duration::from_secs(120),
        ) {
            Ok(Reply::WorktreeRemoved { result }) => Ok(result),
            Ok(_) => Err("unexpected worktree removal reply".into()),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    /// SW-WS-1 Pass-2 Flag 2: `git status --porcelain` for a managed worktree
    /// on this machine. Called by `move_workspace` pre-flight when the
    /// source worktree lives on a remote machine.
    pub(crate) fn check_worktree_dirty_remote(
        &self,
        request: codesense_pty_core::remote_workspace::CheckWorktreeDirtyRemote,
    ) -> Result<codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = match self.call(
            Request::CheckWorktreeDirtyRemote { request },
            Duration::from_secs(60),
        ) {
            Ok(Reply::WorktreeDirtyRemote { status }) => Ok(status),
            Ok(_) => Err("unexpected dirty-check reply".into()),
            Err(error) => Err(String::from(error)),
        };
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    pub(crate) fn upload_workspace_bundle(
        &self,
        chunk: codesense_pty_core::remote_workspace::BundleChunk,
    ) -> Result<u64, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        match self.call(
            Request::UploadWorkspaceBundle { chunk },
            Duration::from_secs(30),
        ) {
            Ok(Reply::WorkspaceBundleProgress { progress }) => Ok(progress.next_offset),
            Ok(_) => Err("unexpected bundle progress reply".into()),
            Err(error) => Err(String::from(error)),
        }
    }

    /// SW-XS-9: whether this machine takes the pipelined bundle upload.
    pub(crate) fn supports_bundle_pipeline(&self) -> bool {
        self.hello.supports(&Capability::BundlePipelineV1)
    }

    /// SW-XS-9: whether this machine can move a workspace to a newer base.
    pub(crate) fn supports_workspace_base_update(&self) -> bool {
        self.hello.supports(&Capability::WorkspaceBaseUpdateV1)
    }

    fn validated<T>(&self, call: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        let result = call();
        self.private_lease()
            .ok_or("private authority unavailable")?
            .validate()?;
        result
    }

    /// SW-XS-9: which chunks of a pipelined upload the machine holds.
    pub(crate) fn bundle_upload_status(
        &self,
        request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    ) -> Result<codesense_pty_core::remote_workspace::BundleUploadStatus, String> {
        self.validated(|| {
            match self.call(
                Request::BundleUploadStatus { request },
                Duration::from_secs(65),
            ) {
                Ok(Reply::BundleUploadStatus { status }) => Ok(status),
                Ok(_) => Err("unexpected bundle status reply".into()),
                Err(error) => Err(String::from(error)),
            }
        })
    }

    /// SW-XS-9: flush the complete upload once on the machine and verify it.
    pub(crate) fn seal_workspace_bundle(
        &self,
        request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    ) -> Result<(), String> {
        let wait = Duration::from_secs(65 + request.bundle.size / (64 * 1024 * 1024));
        self.validated(
            || match self.call(Request::SealWorkspaceBundle { request }, wait) {
                Ok(Reply::Done) => Ok(()),
                Ok(_) => Err("unexpected bundle seal reply".into()),
                Err(error) => Err(String::from(error)),
            },
        )
    }

    /// SW-XS-9: move a prepared workspace to a newer base commit.
    pub(crate) fn update_workspace_base(
        &self,
        request: codesense_pty_core::remote_workspace::UpdateWorkspaceBase,
    ) -> Result<codesense_pty_core::remote_workspace::WorkspaceBaseUpdate, String> {
        self.validated(|| {
            match self.call(
                Request::UpdateWorkspaceBase {
                    request: Box::new(request),
                },
                Duration::from_secs(30 * 60 + 10),
            ) {
                Ok(Reply::WorkspaceBaseUpdated { result }) => Ok(*result),
                Ok(_) => Err("unexpected workspace base reply".into()),
                Err(error) => Err(String::from(error)),
            }
        })
    }

    /// SW-XS-9: a pipe that keeps several chunks of `request`'s upload in
    /// flight on this connection; acks are matched by chunk index.
    pub(crate) fn bundle_chunk_pipe(
        &self,
        request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    ) -> BundleChunkPipe<'_> {
        let (tx, rx) = mpsc::channel();
        BundleChunkPipe {
            client: self,
            request,
            tx,
            rx,
            in_flight: HashMap::new(),
        }
    }

    pub(crate) fn prepare_remote_workspace(
        &self,
        request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    ) -> codesense_pty_core::remote_workspace::PreparedWorkspace {
        use codesense_pty_core::remote_workspace::PreparedWorkspace;
        if self
            .private_lease()
            .is_none_or(|lease| lease.validate().is_err())
        {
            return PreparedWorkspace::failure("refused", "private authority unavailable");
        }
        match self.call(
            Request::PrepareRemoteWorkspace { request },
            Duration::from_secs(30 * 60 + 10),
        ) {
            Ok(Reply::RemoteWorkspacePrepared { result }) => *result,
            Ok(_) => PreparedWorkspace::failure("unknown", "unexpected workspace reply"),
            Err(CallError::NotSent(error)) => PreparedWorkspace::failure("unknown", error),
            Err(error) => PreparedWorkspace::failure("unknown", String::from(error)),
        }
    }

    /// An identity unique to this connection for the life of the process
    /// (unlike its address, which a later connection can reuse).
    pub(crate) fn connection_generation(&self) -> u64 {
        self.namespace.generation()
    }

    fn rid(&self) -> u64 {
        self.next_rid.fetch_add(1, Ordering::Relaxed)
    }

    fn call(&self, request: Request, wait: Duration) -> Result<Reply, CallError> {
        let rid = self.rid();
        let frame = Frame::control(&Message::Call { rid, request }).map_err(CallError::NotSent)?;
        self.exchange(rid, wait, || self.send(&frame))
    }

    /// Wait for call `rid`'s reply once `send` has sent it.
    fn exchange(
        &self,
        rid: u64,
        wait: Duration,
        send: impl FnOnce() -> Result<(), CallError>,
    ) -> Result<Reply, CallError> {
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(rid, tx);
        let forget = || {
            lock(&self.pending).remove(&rid);
        };
        if !self.is_alive() {
            forget();
            return Err(CallError::NotSent(
                "the session server connection is closed".into(),
            ));
        }
        if let Err(error) = send() {
            forget();
            return Err(error);
        }
        match rx.recv_timeout(wait) {
            Ok(result) => {
                if let Some(lease) = &self.private_lease {
                    lease.validate().map_err(CallError::Lost)?;
                }
                result.map_err(CallError::Failed)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                forget();
                Err(CallError::Lost(NO_ANSWER.into()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CallError::Lost(
                "the session server connection closed before answering".into(),
            )),
        }
    }

    /// Send `request` without waiting for its reply. Requests on the one
    /// connection are read in order; a resize is applied as it is read.
    fn post(&self, request: Request) -> Result<(), PortUnavailable> {
        let frame = Frame::control(&Message::Call {
            rid: self.rid(),
            request,
        })
        .map_err(PortUnavailable)?;
        Ok(self.send(&frame)?)
    }

    /// Write one request frame. A frame too large to send is refused before
    /// any of it is written (`Failed`, the connection stays up); any other
    /// write error may have torn the frame, which would corrupt every later
    /// request, so it ends the connection (`Lost`: a partial frame may have
    /// reached the sidecar).
    fn send(&self, frame: &Frame) -> Result<(), CallError> {
        match self.writer.try_send(frame) {
            Ok(()) => Ok(()),
            Err(WriteEnqueueError::InvalidFrame(message)) => Err(CallError::NotSent(format!(
                "send to the session server: {message}"
            ))),
            Err(WriteEnqueueError::Backpressure(pressure)) => Err(CallError::NotSent(format!(
                "session server write backpressure: {pressure:?}"
            ))),
            Err(WriteEnqueueError::Closed) => {
                self.alive.store(false, Ordering::SeqCst);
                Err(CallError::NotSent(
                    "the session server connection is closed".into(),
                ))
            }
        }
    }

    fn done(&self, request: Request, wait: Duration) -> Result<(), PortUnavailable> {
        Ok(self.call(request, wait).map(|_| ())?)
    }

    /// Call `request` and `take` the value of its reply.
    fn ask<T>(
        &self,
        request: Request,
        wait: Duration,
        take: impl FnOnce(Reply) -> Result<T, Reply>,
    ) -> Result<T, PortUnavailable> {
        take(self.call(request, wait)?)
            .map_err(|other| PortUnavailable(format!("unexpected session server reply {other:?}")))
    }

    fn flag(&self, request: Request, wait: Duration) -> Result<bool, PortUnavailable> {
        self.ask(request, wait, reply!(Reply::Flag { value } => value))
    }

    /// Call a layout verb and `take` the value of its reply. The host layout's
    /// refusal comes back whole; a request that could not be asked or answered
    /// is [`LayoutError::Unreachable`].
    fn layout<T>(
        &self,
        request: Request,
        take: impl FnOnce(Reply) -> Result<T, Reply>,
    ) -> Result<T, LayoutError> {
        match self.call(request, REPLY_TIMEOUT) {
            Ok(Reply::LayoutRefused { error }) => Err(error),
            Ok(reply) => take(reply).map_err(|other| {
                LayoutError::Unreachable(format!("unexpected session server reply {other:?}"))
            }),
            Err(error) => Err(LayoutError::Unreachable(error.into())),
        }
    }

    fn layout_done(&self, request: Request) -> Result<(), LayoutError> {
        self.layout(request, reply!(Reply::Done => ()))
    }

    /// The remote shell's typed reservation/spawn path. An uncertain spawn must
    /// never be sent as a known failure to FinishTab, which releases its tags.
    pub(crate) fn open_remote_tab(
        &self,
        request: OpenTab,
        spawn: impl FnOnce() -> Result<(), SpawnFailure>,
    ) -> Result<TabEntry, LayoutError> {
        if !self.hello.supports(&Capability::RemoteTabRecovery) {
            return Err(LayoutError::Spawn(
                "remote tab recovery capability unavailable".into(),
            ));
        }
        let session = request.session.clone();
        self.layout_done(Request::ReserveTab { request })?;
        match spawn() {
            Ok(()) => self.layout(
                Request::FinishTab {
                    session,
                    spawned: Ok(()),
                },
                reply!(Reply::Tab { entry } => *entry),
            ),
            Err(failure) if failure.disposition == SpawnDisposition::Unknown => {
                // A successful reconciliation preserves server-owned tags, but
                // the original call still reports its uncertain delivery honestly.
                let _ = self.reconcile_remote_tab(&session);
                Err(LayoutError::SpawnUnknown(failure.message))
            }
            Err(failure) => self.layout(
                Request::FinishTab {
                    session,
                    spawned: Err(failure.message),
                },
                reply!(Reply::Tab { entry } => *entry),
            ),
        }
    }

    pub(crate) fn reconcile_remote_tab(
        &self,
        session: &PtySessionId,
    ) -> Result<TabEntry, LayoutError> {
        self.layout(
            Request::ReconcileTab {
                session: session.clone(),
            },
            reply!(Reply::Tab { entry } => *entry),
        )
    }

    /// Agent submit may write text/Enter without observing confirmation. Keep
    /// that uncertainty distinct from a request known never to have been sent.
    pub(crate) fn submit_agent_line_typed(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<SubmitOutcome, InputDeliveryError> {
        match self.call(
            Request::SubmitLine {
                session: session.clone(),
                line: line.into(),
                quiet_ms,
                confirm_ms,
                timeout_ms,
                max_nudges,
            },
            REPLY_TIMEOUT + Duration::from_millis(timeout_ms),
        ) {
            Ok(Reply::Flag { value: true }) => Ok(SubmitOutcome { submitted: true }),
            Ok(Reply::Flag { value: false }) => Err(InputDeliveryError::Unknown(
                "agent input was not confirmed; it may have been submitted".into(),
            )),
            Ok(_) => Err(InputDeliveryError::Unknown(
                "unexpected agent submission reply".into(),
            )),
            Err(CallError::NotSent(message)) => Err(InputDeliveryError::NotSent(message)),
            // The host typed nothing: the agent's prompt never took input.
            Err(CallError::Failed(message))
                if message.starts_with(codesense_pty_core::session_wake::AGENT_INPUT_NOT_READY) =>
            {
                Err(InputDeliveryError::NotSent(message))
            }
            Err(CallError::Lost(message) | CallError::Failed(message)) => {
                Err(InputDeliveryError::Unknown(message))
            }
        }
    }

    /// The shell protocol writes once after readiness; preserve transport certainty.
    pub(crate) fn submit_shell_line_typed(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        timeout_ms: u64,
    ) -> Result<SubmitOutcome, InputDeliveryError> {
        match self.call(
            Request::SubmitShellLine {
                session: session.clone(),
                line: line.to_string(),
                quiet_ms,
                timeout_ms,
            },
            REPLY_TIMEOUT + Duration::from_millis(timeout_ms),
        ) {
            Ok(Reply::Flag { value }) => Ok(SubmitOutcome { submitted: value }),
            Ok(_) => Err(InputDeliveryError::Unknown(
                "unexpected shell submission reply".into(),
            )),
            Err(CallError::NotSent(message)) => Err(InputDeliveryError::NotSent(message)),
            Err(CallError::Lost(message) | CallError::Failed(message)) => {
                Err(InputDeliveryError::Unknown(message))
            }
        }
    }

    /// Claim the session's writable attachment on this exact connection.
    /// Output subscriptions are independent; repeated claims by this client succeed.
    #[cfg(test)]
    pub fn claim_writable(&self, session: &PtySessionId) -> Result<(), PortUnavailable> {
        self.claim_writable_typed(session)
            .map_err(|error| PortUnavailable(error.to_string()))
    }

    pub(crate) fn claim_writable_typed(
        &self,
        session: &PtySessionId,
    ) -> Result<(), WritableAttachmentFailure> {
        if !self.hello.supports(&Capability::WritableAttachment) {
            return Err(WritableAttachmentFailure::Unavailable(
                "writable attachment capability unavailable".into(),
            ));
        }
        match self.call(
            Request::ClaimWritable {
                session: session.clone(),
            },
            REPLY_TIMEOUT,
        ) {
            Ok(Reply::Done) => Ok(()),
            Ok(Reply::WritableBusy) => Err(WritableAttachmentFailure::AttachedElsewhere),
            Ok(other) => Err(WritableAttachmentFailure::Unavailable(format!(
                "unexpected writable claim reply {other:?}"
            ))),
            Err(error) => Err(WritableAttachmentFailure::Unavailable(error.into())),
        }
    }

    /// Release input ownership without ending the process or its output streams.
    /// Hiding a display must not call this; the connection also releases on close.
    pub fn release_writable(&self, session: &PtySessionId) -> Result<(), PortUnavailable> {
        if !self.hello.supports(&Capability::WritableAttachment) {
            return Err(PortUnavailable(
                "writable attachment capability unavailable".into(),
            ));
        }
        self.ask(
            Request::ReleaseWritable {
                session: session.clone(),
            },
            REPLY_TIMEOUT,
            reply!(Reply::Done => ()),
        )
    }

    /// Write `data` to `session` as raw bytes on its input channel, opening
    /// the channel on its first write.
    fn write_input(&self, session: &PtySessionId, data: &[u8]) -> Result<(), CallError> {
        let rid = self.rid();
        self.exchange(rid, REPLY_TIMEOUT, || {
            let mut inputs = lock(&self.inputs);
            let frames = inputs
                .write(session, rid, data, || self.rid())
                .map_err(CallError::Failed)?;
            frames.iter().try_for_each(|frame| self.send(frame))
        })
        .map(|_| ())
    }

    /// Write raw input bytes (a single key) with the same delivery certainty as a
    /// submitted line: a write known never to have left is `NotSent`; one whose
    /// acknowledgement was lost may have been typed and is `Unknown`.
    pub(crate) fn write_input_typed(
        &self,
        session: &PtySessionId,
        data: &[u8],
    ) -> Result<(), InputDeliveryError> {
        let written = self.write_input(session, data);
        if written.is_err() {
            // A refused write (an unknown or gone session, or its backed-up
            // input) leaves no channel open for it; the next write reopens.
            self.close_input(session, None);
        }
        written.map_err(|error| match error {
            CallError::NotSent(message) => InputDeliveryError::NotSent(message),
            CallError::Lost(message) | CallError::Failed(message) => {
                InputDeliveryError::Unknown(message)
            }
        })
    }

    /// `session`'s input channel, if it has one open.
    fn input_channel(&self, session: &PtySessionId) -> Option<u32> {
        lock(&self.inputs).channel(session)
    }

    /// `session` is gone: close its input channel, if it has one — only
    /// `channel`, when given, so a respawn's newer channel stays open. Ordered
    /// after every write already sent on it.
    fn close_input(&self, session: &PtySessionId, channel: Option<u32>) {
        let mut inputs = lock(&self.inputs);
        match inputs.close(session, channel, || self.rid()) {
            Ok(Some(frame)) => {
                if let Err(error) = self.send(&frame) {
                    log::warn!(target: "pty", "close the input of {session}: {}", String::from(error));
                }
            }
            Ok(None) => {}
            Err(error) => log::warn!(target: "pty", "close the input of {session}: {error}"),
        }
    }

    /// Spawn and register a session in the sidecar, which resolves `intent`
    /// against its own environment; returns the child's pid.
    /// If the reply is lost the sidecar may already run the child, or run it
    /// later, so the spawn is cancelled there over a fresh connection; only a
    /// confirmed cancel lets the caller host it elsewhere, so the command
    /// never runs twice.
    pub fn spawn(
        &self,
        intent: SpawnIntent,
        registration: SessionRegistration,
    ) -> Result<Option<u32>, SpawnFailure> {
        self.spawn_with_timeout(intent, registration, REPLY_TIMEOUT)
    }

    fn spawn_with_timeout(
        &self,
        intent: SpawnIntent,
        registration: SessionRegistration,
        timeout: Duration,
    ) -> Result<Option<u32>, SpawnFailure> {
        if let Err(message) = self.require_agent_env(!intent.agent_env.is_empty()) {
            return Err(SpawnFailure {
                message,
                may_host_elsewhere: false,
                disposition: SpawnDisposition::NeverStarted,
            });
        }
        let session = registration.session_id.clone();
        // A new session of the id gets a channel of its own: the one before's
        // input closes first, so its leaving can never close the new one's.
        self.close_input(&PtySessionId::new(session.as_str()), None);
        // Hosted here again: its events are live, from the very first.
        lock(&self.ledger.withdrawn).remove(&session);
        // Hosted from before the request — its removal may be pushed before
        // the reply arrives — and spawning until the reply, so no carry-across
        // reads it as ended meanwhile.
        let pool_member = registration.is_pool_member;
        let ((prior, mine), generation) = {
            let mut spawning = lock(&self.ledger.spawning);
            spawning.insert(session.clone());
            let mut hosted = lock(&self.ledger.hosted);
            let prior = self.ledger.record(&mut hosted, &session, pool_member);
            let mine = hosted.get(&session).copied();
            ((prior, mine), self.ledger.generation.load(Ordering::SeqCst))
        };
        let _spawning = Spawning {
            ledger: &self.ledger,
            session: &session,
        };
        // The ledger entry of this spawn, while its lifetime still runs.
        let settle = |entry: Option<(bool, u64)>| {
            let _spawning = lock(&self.ledger.spawning);
            let mut hosted = lock(&self.ledger.hosted);
            if self.ledger.generation.load(Ordering::SeqCst) == generation {
                match entry {
                    Some(entry) => hosted.insert(session.clone(), entry),
                    None => hosted.remove(&session),
                };
            }
        };
        let failure = |message: String, may_host_elsewhere: bool, disposition| SpawnFailure {
            message,
            may_host_elsewhere,
            disposition,
        };
        let outcome = match self.call(
            Request::Spawn {
                intent,
                registration,
            },
            timeout,
        ) {
            Ok(Reply::Spawned { pid }) => return Ok(pid),
            Ok(Reply::Cancelled) => failure(
                format!("spawn_pty: session {session} was closed while it was opening"),
                false,
                SpawnDisposition::CancelledUndrained,
            ),
            Ok(other) => failure(
                format!("unexpected session server reply {other:?}"),
                false,
                SpawnDisposition::Unknown,
            ),
            Err(CallError::NotSent(message)) => {
                failure(message, true, SpawnDisposition::NeverStarted)
            }
            Err(CallError::Failed(message)) => {
                failure(message, true, SpawnDisposition::RejectedUndrained)
            }
            Err(CallError::Lost(message)) => {
                // Withdrawn before the cancel is sent, so none of the teardown
                // the cancel causes reaches the app.
                lock(&self.ledger.withdrawn).insert(session.clone());
                let cancelled = cancel_spawn_over_fresh_connection(&self.root, &session);
                if !cancelled {
                    // It may still run there: it stays hosted.
                    lock(&self.ledger.withdrawn).remove(&session);
                    settle(mine);
                    return Err(failure(message, false, SpawnDisposition::Unknown));
                }
                // The cancel ended any session of the id there.
                settle(None);
                return Err(failure(message, true, SpawnDisposition::CancelledUndrained));
            }
        };
        // Not hosted by this spawn: an older session of the id stays as it was.
        settle(prior);
        Err(outcome)
    }
}

/// The one protocol client together with authority to normally close and reap
/// the SSH process that carries it.
pub(crate) struct ManagedRemoteClient {
    client: Arc<SessionServerClient>,
    close: RemoteCloseHandle,
    publication: Arc<RemotePublicationGate>,
}

impl ManagedRemoteClient {
    pub(crate) fn server_id(&self) -> &str {
        &self.client.hello.server_id
    }

    pub(crate) fn epoch(&self) -> &str {
        self.client.epoch()
    }

    pub(crate) fn domain_client(&self) -> Arc<SessionServerClient> {
        self.client.clone()
    }

    pub(crate) async fn close_awaited(self) {
        self.publication.suppress();
        self.client.writer.close();
        self.close.close_awaited().await;
    }

    pub(crate) fn close_request(&self) -> impl FnOnce() + Send + 'static {
        let publication = self.publication.clone();
        let client = self.client.clone();
        let close = self.close.clone();
        move || {
            publication.suppress();
            client.writer.close();
            close.begin_close();
        }
    }

    pub(crate) fn publication_permit(&self) -> RemotePublicationPermit {
        RemotePublicationPermit(self.publication.clone())
    }

    pub(crate) fn set_on_lost(&self, lost: LostHook) -> bool {
        *lock(&self.publication.lost) = Some(lost);
        self.publication.is_lost()
    }
}

pub(crate) struct RemotePublicationPermit(Arc<RemotePublicationGate>);

impl RemotePublicationPermit {
    pub(crate) fn prepare(self) -> Option<PreparedRemotePublication> {
        self.0.prepare()
    }

    #[cfg(test)]
    pub(crate) fn publish(self) {
        if let Some(prepared) = self.prepare() {
            prepared.deliver_if(|| true, || true);
        }
    }
}

pub(crate) struct PreparedRemotePublication {
    gate: Arc<RemotePublicationGate>,
}

impl PreparedRemotePublication {
    pub(crate) fn commit_and_take(
        self,
        current: impl FnOnce() -> bool,
        commit: impl FnOnce() -> bool,
    ) -> Option<CommittedRemotePublication> {
        if !current() {
            self.gate.suppress();
            return None;
        }
        let events = {
            let mut state = lock(&self.gate.state);
            match std::mem::replace(&mut *state, RemotePublicationState::Suppressed) {
                RemotePublicationState::Prepared {
                    events,
                    lost: false,
                } if commit() => {
                    *state = RemotePublicationState::Published;
                    events
                }
                _ => return None,
            }
        };
        Some(CommittedRemotePublication {
            gate: self.gate,
            events,
        })
    }

    #[cfg(test)]
    pub(crate) fn deliver_if(
        self,
        current: impl FnOnce() -> bool,
        commit: impl FnOnce() -> bool,
    ) -> bool {
        let Some(committed) = self.commit_and_take(current, commit) else {
            return false;
        };
        committed.emit();
        true
    }
}

pub(crate) struct CommittedRemotePublication {
    gate: Arc<RemotePublicationGate>,
    events: Vec<NamespacedTerminalEvent>,
}

impl CommittedRemotePublication {
    pub(crate) fn emit(self) {
        for event in self.events {
            let _ = self.gate.emit_committed(event);
        }
    }
}

enum RemotePublicationState {
    Pending {
        events: Vec<NamespacedTerminalEvent>,
        lost: bool,
    },
    Prepared {
        events: Vec<NamespacedTerminalEvent>,
        lost: bool,
    },
    Published,
    Suppressed,
}

struct RemotePublicationGate {
    private_lease: Option<PrivateAuthorityLease>,
    state: Mutex<RemotePublicationState>,
    sink: Arc<dyn NamespacedTerminalEventSink>,
    lost: Mutex<Option<LostHook>>,
}

impl RemotePublicationGate {
    #[cfg(test)]
    fn new(sink: Arc<dyn NamespacedTerminalEventSink>, lost: Option<LostHook>) -> Arc<Self> {
        Self::new_private(sink, lost, None)
    }

    fn new_private(
        sink: Arc<dyn NamespacedTerminalEventSink>,
        lost: Option<LostHook>,
        private_lease: Option<PrivateAuthorityLease>,
    ) -> Arc<Self> {
        Arc::new(Self {
            private_lease,
            state: Mutex::new(RemotePublicationState::Pending {
                events: Vec::new(),
                lost: false,
            }),
            sink,
            lost: Mutex::new(lost),
        })
    }

    fn emit_committed(&self, event: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        match &self.private_lease {
            Some(lease) => self.sink.emit_private(event, lease),
            None => self.sink.emit(event),
        }
    }

    fn prepare(self: &Arc<Self>) -> Option<PreparedRemotePublication> {
        {
            let mut state = lock(&self.state);
            match std::mem::replace(&mut *state, RemotePublicationState::Suppressed) {
                RemotePublicationState::Pending {
                    events,
                    lost: false,
                } => {
                    *state = RemotePublicationState::Prepared {
                        events,
                        lost: false,
                    };
                }
                RemotePublicationState::Pending { lost: true, .. } => {
                    *state = RemotePublicationState::Suppressed;
                    return None;
                }
                RemotePublicationState::Published => return None,
                RemotePublicationState::Prepared { events, lost } => {
                    *state = RemotePublicationState::Prepared { events, lost };
                    return None;
                }
                RemotePublicationState::Suppressed => {
                    *state = RemotePublicationState::Suppressed;
                    return None;
                }
            }
        };
        Some(PreparedRemotePublication { gate: self.clone() })
    }

    #[cfg(test)]
    fn publish(self: &Arc<Self>) {
        if let Some(prepared) = self.prepare() {
            prepared.deliver_if(|| true, || true);
        }
    }

    #[cfg(test)]
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        NamespacedTerminalEventSink::emit(
            self,
            NamespacedTerminalEvent {
                namespace: EventNamespace {
                    server_id: "test-server".into(),
                    epoch: "test-epoch".into(),
                    generation: 1,
                },
                event,
            },
        )
    }

    fn is_lost(&self) -> bool {
        matches!(
            &*lock(&self.state),
            RemotePublicationState::Pending { lost: true, .. }
                | RemotePublicationState::Prepared { lost: true, .. }
        )
    }

    fn suppress(&self) {
        *lock(&self.state) = RemotePublicationState::Suppressed;
    }

    fn lost(&self) {
        let publish = {
            let mut state = lock(&self.state);
            match &mut *state {
                RemotePublicationState::Pending { lost, .. } => {
                    *lost = true;
                    false
                }
                RemotePublicationState::Prepared { lost, .. } => {
                    *lost = true;
                    false
                }
                RemotePublicationState::Published => true,
                RemotePublicationState::Suppressed => false,
            }
        };
        if publish {
            self.emit_lost();
        }
    }

    fn emit_lost(&self) {
        let lost = {
            let configured = lock(&self.lost);
            configured.as_ref().and_then(|hook| lock(hook).clone())
        };
        if let Some(lost) = lost {
            lost();
        }
    }
}

impl NamespacedTerminalEventSink for RemotePublicationGate {
    fn emit(&self, event: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        let event = {
            let mut state = lock(&self.state);
            match &mut *state {
                RemotePublicationState::Pending { events, .. } => {
                    events.push(event);
                    return Ok(());
                }
                RemotePublicationState::Prepared { events, .. } => {
                    events.push(event);
                    return Ok(());
                }
                RemotePublicationState::Published => event,
                RemotePublicationState::Suppressed => return Ok(()),
            }
        };
        self.emit_committed(event)
    }
}

/// Ends a spawn's in-flight mark in the ledger however the spawn ends.
struct Spawning<'a> {
    ledger: &'a Ledger,
    session: &'a str,
}

impl Drop for Spawning<'_> {
    fn drop(&mut self) {
        lock(&self.ledger.spawning).remove(self.session);
    }
}

impl TerminalDomain for SessionServerClient {
    fn subscribe(
        &self,
        session: &PtySessionId,
        viewport: &ViewportId,
    ) -> Result<SubscribeResult, PortUnavailable> {
        self.ask(
            Request::Subscribe {
                session: session.clone(),
                viewport: viewport.clone(),
            },
            REPLY_TIMEOUT,
            reply!(Reply::Subscribed { result } => *result),
        )
    }

    fn unsubscribe(&self, stream: StreamId) -> Result<bool, PortUnavailable> {
        self.flag(Request::Unsubscribe { stream }, REPLY_TIMEOUT)
    }

    /// Resize without waiting: applied in order, never holding up the caller.
    fn resize(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), PortUnavailable> {
        self.post(Request::Resize {
            viewer: viewer.clone(),
            session: session.clone(),
            cols,
            rows,
        })
    }

    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), PortUnavailable> {
        Ok(self.write_input_typed(session, bytes)?)
    }

    fn submit_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<SubmitOutcome, PortUnavailable> {
        let submitted = self.flag(
            Request::SubmitLine {
                session: session.clone(),
                line: line.to_string(),
                quiet_ms,
                confirm_ms,
                timeout_ms,
                max_nudges,
            },
            REPLY_TIMEOUT + Duration::from_millis(timeout_ms),
        )?;
        Ok(SubmitOutcome { submitted })
    }

    fn submit_shell_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        timeout_ms: u64,
    ) -> Result<SubmitOutcome, PortUnavailable> {
        self.submit_shell_line_typed(session, line, quiet_ms, timeout_ms)
            .map_err(Into::into)
    }

    fn is_idle(&self, session: &PtySessionId) -> Result<bool, PortUnavailable> {
        self.flag(
            Request::IsIdle {
                session: session.clone(),
            },
            REPLY_TIMEOUT,
        )
    }

    /// Kill without waiting: the teardown's side effects arrive as events.
    fn kill(&self, session: &PtySessionId) -> Result<(), PortUnavailable> {
        self.post(Request::Kill {
            session: session.clone(),
        })
    }

    fn handoff_warm_member(
        &self,
        member: &PtySessionId,
        session: &PtySessionId,
        target_cwd: &str,
        cols: u16,
        rows: u16,
        meta: SessionMeta,
    ) -> Result<(), PortUnavailable> {
        // Hosted here now: its events are live, from the very first.
        lock(&self.ledger.withdrawn).remove(session.as_str());
        self.done(
            Request::HandoffWarmMember {
                member: member.clone(),
                session: session.clone(),
                target_cwd: target_cwd.to_string(),
                cols,
                rows,
                meta,
            },
            REPLY_TIMEOUT,
        )?;
        let mut hosted = lock(&self.ledger.hosted);
        hosted.remove(member.as_str());
        self.ledger.record(&mut hosted, session.as_str(), false);
        Ok(())
    }

    /// The exit-time complement kill, run by the host: the non-persisting
    /// sessions this app opened there go; persisting ones keep running in the
    /// sidecar. Every app on the Mac shares the one sidecar, so another app's
    /// tabs are never touched; the last app to quit also takes every unhanded
    /// warm-pool member. A sidecar older than the scoped reap takes every
    /// non-persisting session, as it always did.
    fn reap_non_persisting(&self) -> Result<(), PortUnavailable> {
        if !self.hello.supports(&Capability::ScopedReapV1) {
            return self.done(Request::ReapNonPersisting, EXIT_REAP_TIMEOUT);
        }
        self.done(
            Request::ReapOwnedNonPersisting {
                sessions: self.ledger.opened_sessions(),
            },
            EXIT_REAP_TIMEOUT,
        )
    }

    fn has_session(&self, session: &PtySessionId) -> Result<bool, PortUnavailable> {
        self.flag(
            Request::HasSession {
                session: session.clone(),
            },
            READ_TIMEOUT,
        )
    }

    fn alive_session_ids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        self.ask(
            Request::AliveSessionIds {
                sessions: sessions.to_vec(),
            },
            READ_TIMEOUT,
            reply!(Reply::Sessions { sessions } => sessions),
        )
    }

    fn redeliver_exits(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        if !self.hello.supports(&Capability::ExitRedeliveryV1) {
            return Err(PortUnavailable(
                "this terminal server cannot redeliver exits".into(),
            ));
        }
        self.ask(
            Request::RedeliverExits {
                sessions: sessions.to_vec(),
            },
            READ_TIMEOUT,
            reply!(Reply::Sessions { sessions } => sessions),
        )
    }

    fn live_session_ids(&self) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        self.ask(
            Request::LiveSessionIds,
            READ_TIMEOUT,
            reply!(Reply::Sessions { sessions } => sessions),
        )
    }

    fn live_child_pids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<Vec<Option<u32>>, PortUnavailable> {
        self.ask(
            Request::LiveChildPids {
                sessions: sessions.to_vec(),
            },
            READ_TIMEOUT,
            reply!(Reply::Pids { pids } => pids),
        )
    }

    fn session_output(
        &self,
        session: &PtySessionId,
        identity: &codesense_pty_core::activity::Identity,
        turn_generation: u64,
    ) -> Result<Option<codesense_pty_core::agent_cli::output::SessionRecord>, PortUnavailable> {
        if !self.hello.supports(&Capability::SessionOutputV1) {
            return Ok(None);
        }
        self.ask(
            Request::SessionOutput {
                session: session.clone(),
                identity: identity.clone(),
                turn_generation,
            },
            READ_TIMEOUT,
            reply!(Reply::SessionOutput{record}=>record),
        )
    }

    fn activity_snapshot(
        &self,
    ) -> Result<Vec<codesense_pty_core::activity::Snapshot>, PortUnavailable> {
        if !self.hello.supports(&Capability::ActivityV1) {
            return Err(PortUnavailable(
                "Agent activity requires a terminal server update. Existing terminals are unchanged; finish and close them before updating the server, then launch the agent again.".into(),
            ));
        }
        self.ask(
            Request::ActivitySnapshot,
            READ_TIMEOUT,
            reply!(Reply::ActivitySnapshot { snapshots } => snapshots),
        )
    }

    fn activity_prepare_shell(
        &self,
        session: &PtySessionId,
        provider: codesense_pty_core::agent_cli::AgentCli,
    ) -> Result<codesense_pty_core::activity::ShellLaunch, PortUnavailable> {
        if !self.hello.supports(&Capability::ActivityV1) {
            return Err(PortUnavailable(
                "Agent activity requires a terminal server update. Existing terminals are unchanged; finish and close them before updating the server, then launch the agent again.".into(),
            ));
        }
        self.ask(
            Request::PrepareShellActivity {
                session: session.clone(),
                provider,
            },
            READ_TIMEOUT,
            reply!(Reply::ShellActivity { launch } => *launch),
        )
    }

    fn activity_mutate(
        &self,
        mutation: codesense_pty_core::activity::Mutation,
    ) -> Result<codesense_pty_core::activity::Snapshot, PortUnavailable> {
        if !self.hello.supports(&Capability::ActivityV1) {
            return Err(PortUnavailable(
                "Agent activity requires a terminal server update. Existing terminals are unchanged; finish and close them before updating the server, then launch the agent again.".into(),
            ));
        }
        self.ask(
            Request::MutateActivity { mutation },
            READ_TIMEOUT,
            reply!(Reply::ActivityUpdated { snapshot } => *snapshot),
        )
    }

    fn inbox_wake_holds(
        &self,
        generation: u64,
        holds: Vec<codesense_pty_core::inbox_host::WakeHold>,
    ) -> Result<(), PortUnavailable> {
        if !self.hello.supports(&Capability::InboxWakeHoldsV1) {
            return Err(PortUnavailable(
                "spend holds require a terminal server update".into(),
            ));
        }
        match self.call(Request::InboxWakeHolds { generation, holds }, REPLY_TIMEOUT)? {
            Reply::Done => Ok(()),
            other => Err(PortUnavailable(format!(
                "unexpected wake hold reply {other:?}"
            ))),
        }
    }

    fn inbox_mail(
        &self,
        session: &PtySessionId,
        notice: &str,
        token: &str,
    ) -> Result<(), PortUnavailable> {
        self.require_inbox_wait()?;
        match self.call(
            Request::InboxMail {
                session: session.clone(),
                notice: notice.to_string(),
                token: token.to_string(),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::Done => Ok(()),
            other => Err(PortUnavailable(format!(
                "unexpected inbox mail reply {other:?}"
            ))),
        }
    }

    fn inbox_waiters(
        &self,
    ) -> Result<Vec<codesense_pty_core::domain::InboxWaiterState>, PortUnavailable> {
        self.require_inbox_wait()?;
        match self.call(Request::InboxWaiters, REPLY_TIMEOUT)? {
            Reply::InboxWaiters { waiters } => Ok(waiters),
            other => Err(PortUnavailable(format!(
                "unexpected inbox waiters reply {other:?}"
            ))),
        }
    }

    fn agents_alive(
        &self,
        queries: &[(PtySessionId, String)],
    ) -> Result<Vec<bool>, PortUnavailable> {
        if !self.hello.supports(&Capability::AgentProcessV1) {
            return Err(PortUnavailable(
                "owner process telemetry unavailable; host upgrade required".into(),
            ));
        }
        self.ask(
            Request::AgentsAlive {
                queries: queries.to_vec(),
            },
            READ_TIMEOUT,
            reply!(Reply::AgentsAlive { alive } => alive),
        )
    }

    fn worktree_usage(
        &self,
        source: &str,
    ) -> Result<codesense_pty_core::layout::WorktreeUsage, PortUnavailable> {
        // Architecture: an old daemon's empty layout cannot prove that no launch is starting.
        if !self.hello.supports(&Capability::WorktreeSafetyV1) {
            return Err(PortUnavailable(codesense_pty_core::layout::source_refusal(
                codesense_pty_core::layout::OLD_SERVER,
            )));
        }
        self.ask(
            Request::WorktreeUsage {
                source: source.to_string(),
            },
            READ_TIMEOUT,
            reply!(Reply::WorktreeUsage { usage } => usage),
        )
    }

    fn session_len_and_dead(&self) -> Result<Option<(usize, usize)>, PortUnavailable> {
        self.ask(
            Request::SessionLenAndDead,
            READ_TIMEOUT,
            reply!(Reply::Counts { counts } => counts),
        )
    }

    fn live_session_home_dirs(&self) -> Result<Option<Vec<PathBuf>>, PortUnavailable> {
        self.ask(
            Request::LiveSessionHomeDirs,
            READ_TIMEOUT,
            reply!(Reply::Dirs { dirs } => dirs),
        )
    }

    fn session_cwd(&self, session: &PtySessionId) -> Result<Option<PathBuf>, PortUnavailable> {
        self.ask(
            Request::SessionCwd {
                session: session.clone(),
            },
            READ_TIMEOUT,
            reply!(Reply::Cwd { cwd } => cwd),
        )
    }

    fn read_terminal_text(
        &self,
        session: &PtySessionId,
        lines: u32,
        mode: TextMode,
    ) -> Result<TerminalText, PortUnavailable> {
        self.ask(
            Request::ReadText {
                session: session.clone(),
                lines,
                mode,
            },
            REPLY_TIMEOUT,
            reply!(Reply::Text { text } => text),
        )
    }

    fn debug_dump(&self, session: &PtySessionId) -> Result<DebugDump, PortUnavailable> {
        self.ask(
            Request::DebugDump {
                session: session.clone(),
            },
            REPLY_TIMEOUT,
            reply!(Reply::Dump { dump } => *dump),
        )
    }

    fn layout_snapshot(
        &self,
        viewer: &ViewerId,
        client_boot_id: &ClientBootId,
    ) -> Result<LayoutSnapshot, LayoutError> {
        self.layout(
            Request::LayoutSnapshot {
                viewer: viewer.clone(),
                client_boot_id: client_boot_id.clone(),
            },
            reply!(Reply::Layout { snapshot } => *snapshot),
        )
    }

    /// The host reserves the tab, `spawn` hosts its session here (the caller
    /// resolved this host for it), then the host places it: the same two
    /// halves as in-process, across two requests.
    fn open_tab(
        &self,
        request: OpenTab,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError> {
        let session = request.session.clone();
        self.layout_done(Request::ReserveTab { request })?;
        // A panicking spawn still finishes the reservation before unwinding.
        let spawned = std::panic::catch_unwind(AssertUnwindSafe(spawn));
        #[cfg(test)]
        if self
            .rm4f_skip_interrupted_reservation_abort
            .load(Ordering::Acquire)
        {
            if let Ok(Err(error)) = &spawned {
                return Err(LayoutError::Spawn(error.clone()));
            }
        }
        let finished = self.layout(
            Request::FinishTab {
                session,
                spawned: spawned.as_ref().map_or_else(
                    |_| Err("open_tab: the spawn panicked".to_string()),
                    Clone::clone,
                ),
            },
            reply!(Reply::Tab { entry } => *entry),
        );
        match spawned {
            Ok(_) => finished,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// The sidecar answers the first half once its reap is complete, so the
    /// ended session is gone there before `spawn` runs. A sidecar older than the
    /// respawn verb is named as the terminal server needing a restart.
    fn respawn_tab(
        &self,
        session: &PtySessionId,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError> {
        if self.hello.proto_minor < RESPAWN_PROTO_MINOR {
            return Err(LayoutError::Unreachable(
                RESPAWN_NEEDS_UPDATED_SERVER.to_string(),
            ));
        }
        let revived = self.layout(
            Request::BeginRespawn {
                session: session.clone(),
            },
            |reply| match reply {
                Reply::Done => Ok(None),
                Reply::Tab { entry } => Ok(Some(*entry)),
                other => Err(other),
            },
        )?;
        if let Some(entry) = revived {
            return Ok(entry);
        }
        // A panicking spawn still finishes the respawn before unwinding.
        let spawned = std::panic::catch_unwind(AssertUnwindSafe(spawn));
        let finished = self.layout(
            Request::FinishRespawn {
                session: session.clone(),
                spawned: spawned.as_ref().map_or_else(
                    |_| Err("respawn_tab: the spawn panicked".to_string()),
                    Clone::clone,
                ),
            },
            reply!(Reply::Tab { entry } => *entry),
        );
        match spawned {
            Ok(_) => finished,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn close_tab(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        mode: CloseMode,
    ) -> Result<CloseOutcome, LayoutError> {
        let outcome = self.layout(
            Request::CloseTab {
                viewer: viewer.clone(),
                session: session.clone(),
                mode,
            },
            reply!(Reply::Closed { outcome } => outcome),
        )?;
        if outcome != CloseOutcome::Backgrounded
            && self.hello.supports(&Capability::WritableAttachment)
        {
            self.release_writable(session)
                .map_err(|e| LayoutError::Unreachable(e.to_string()))?;
        }
        Ok(outcome)
    }

    fn place_tab(
        &self,
        session: &PtySessionId,
        workspace: &WorkspaceKey,
        placement: Placement,
        index: Option<usize>,
        descriptor: Option<WorkspaceDescriptor>,
    ) -> Result<(), LayoutError> {
        self.layout_done(Request::PlaceTab {
            session: session.clone(),
            workspace: workspace.clone(),
            placement,
            index,
            descriptor,
        })
    }

    fn rename_tab(&self, session: &PtySessionId, name: String) -> Result<(), LayoutError> {
        self.layout_done(Request::RenameTab {
            session: session.clone(),
            name,
        })
    }

    fn move_tab(&self, session: &PtySessionId, to_index: usize) -> Result<(), LayoutError> {
        self.layout_done(Request::MoveTab {
            session: session.clone(),
            to_index,
        })
    }

    fn set_active(
        &self,
        viewer: &ViewerId,
        workspace: &WorkspaceKey,
        session: Option<&PtySessionId>,
    ) -> Result<(), LayoutError> {
        self.layout_done(Request::SetActive {
            viewer: viewer.clone(),
            workspace: workspace.clone(),
            session: session.cloned(),
        })
    }

    fn set_grid(&self, workspace: &WorkspaceKey, grid: GridState) -> Result<(), LayoutError> {
        self.layout_done(Request::SetGrid {
            workspace: workspace.clone(),
            grid,
        })
    }

    fn set_view_hint(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        hint: ViewHint,
    ) -> Result<(), LayoutError> {
        self.layout_done(Request::SetViewHint {
            viewer: viewer.clone(),
            session: session.clone(),
            hint,
        })
    }

    fn set_tags(&self, session: &PtySessionId, patch: TagPatch) -> Result<(), LayoutError> {
        self.layout_done(Request::SetTags {
            session: session.clone(),
            patch,
        })
    }

    fn set_descriptor(
        &self,
        workspace: &WorkspaceKey,
        descriptor: WorkspaceDescriptor,
    ) -> Result<(), LayoutError> {
        self.layout_done(Request::SetDescriptor {
            workspace: workspace.clone(),
            descriptor,
        })
    }

    fn close_workspace(&self, workspace: &WorkspaceKey) -> Result<(), LayoutError> {
        self.layout_done(Request::CloseWorkspace {
            workspace: workspace.clone(),
        })
    }
}

/// SW-XS-9: chunks of one bundle upload in flight on one connection. Each is
/// its own call (own request id); every reply lands on one channel and names
/// the chunk it acknowledges, so acks may come back in any order.
pub(crate) struct BundleChunkPipe<'a> {
    client: &'a SessionServerClient,
    request: codesense_pty_core::remote_workspace::PrepareWorkspace,
    tx: mpsc::Sender<Result<Reply, String>>,
    rx: mpsc::Receiver<Result<Reply, String>>,
    /// Request id → chunk index.
    in_flight: HashMap<u64, u32>,
}

/// How long one chunk may wait for its ack.
const CHUNK_ACK_TIMEOUT: Duration = Duration::from_secs(30);

impl codesense_pty_core::remote_workspace::pipeline::ChunkLink for BundleChunkPipe<'_> {
    fn submit(&mut self, index: u32, bytes: Vec<u8>) -> Result<(), String> {
        use sha2::{Digest, Sha256};
        let chunk = codesense_pty_core::remote_workspace::StagedBundleChunk {
            request: self.request.clone(),
            index,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
        };
        let rid = self.client.rid();
        let frame = Frame::control(&Message::Call {
            rid,
            request: Request::UploadBundleChunk { chunk },
        })?;
        lock(&self.client.pending).insert(rid, self.tx.clone());
        self.in_flight.insert(rid, index);
        // The writer queue is bounded; a full queue drains as frames go out.
        let started = std::time::Instant::now();
        loop {
            if !self.client.is_alive() {
                return Err("the session server connection is closed".into());
            }
            match self.client.writer.try_send(&frame) {
                Ok(()) => return Ok(()),
                Err(WriteEnqueueError::Backpressure(_))
                    if started.elapsed() < CHUNK_ACK_TIMEOUT =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(WriteEnqueueError::Backpressure(pressure)) => {
                    return Err(format!("session server write backpressure: {pressure:?}"))
                }
                Err(WriteEnqueueError::Closed) => {
                    self.client.alive.store(false, Ordering::SeqCst);
                    return Err("the session server connection is closed".into());
                }
                Err(WriteEnqueueError::InvalidFrame(message)) => {
                    return Err(format!("send to the session server: {message}"))
                }
            }
        }
    }

    fn next_ack(&mut self) -> Result<u32, String> {
        let started = std::time::Instant::now();
        loop {
            if !self.client.is_alive() {
                return Err("the session server connection closed before answering".into());
            }
            let left = CHUNK_ACK_TIMEOUT.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err("the session server did not answer in time".into());
            }
            match self.rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(Ok(Reply::BundleChunkStaged { index })) => {
                    let rid = self
                        .in_flight
                        .iter()
                        .find(|(_, sent)| **sent == index)
                        .map(|(rid, _)| *rid)
                        .ok_or("the machine acknowledged a chunk that was not sent")?;
                    self.in_flight.remove(&rid);
                    self.client
                        .private_lease()
                        .ok_or("private authority unavailable")?
                        .validate()?;
                    return Ok(index);
                }
                Ok(Ok(_)) => return Err("unexpected bundle chunk reply".into()),
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the session server connection closed before answering".into())
                }
            }
        }
    }
}

impl Drop for BundleChunkPipe<'_> {
    fn drop(&mut self) {
        let mut pending = lock(&self.client.pending);
        for rid in self.in_flight.keys() {
            pending.remove(rid);
        }
    }
}

/// A connection's shared state, as its reader thread sees it.
struct Connection<'a> {
    pending: &'a Pending,
    alive: &'a AtomicBool,
    rev: &'a AtomicU64,
    ledger: &'a Ledger,
    client: &'a Weak<SessionServerClient>,
    namespace: &'a EventNamespace,
    is_remote: bool,
}

enum PumpEventSink {
    Legacy(Arc<dyn TerminalEventSink>),
    Namespaced(Arc<dyn NamespacedTerminalEventSink>),
}

impl PumpEventSink {
    fn emit(
        &self,
        namespace: &EventNamespace,
        event: TerminalDomainEvent,
    ) -> Result<(), PortUnavailable> {
        match self {
            Self::Legacy(sink) => sink.emit(event),
            Self::Namespaced(sink) => sink.emit(NamespacedTerminalEvent {
                namespace: namespace.clone(),
                event,
            }),
        }
    }
}

/// Deliver replies to their callers and events to `sink`, in arrival order,
/// until the connection ends; then every caller still waiting learns its reply
/// is lost. A removed session's input channel is closed on the client, and
/// each layout change advances the connection's `rev`.
fn pump(
    mut reader: BufReader<Box<dyn TransportReader>>,
    connection: Connection<'_>,
    sink: &PumpEventSink,
    lost: Option<LostHook>,
) {
    let Connection {
        pending,
        alive,
        rev,
        ledger,
        client,
        namespace,
        is_remote,
    } = connection;
    let mut ingress = Ingress::default();
    let failure = loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break None,
            Err(error) => break Some(error),
        };
        if matches!(
            frame.kind,
            codesense_host_proto::Kind::EXECUTOR_DATA | codesense_host_proto::Kind::EXECUTOR_CLOSE
        ) {
            if let Some(client) = client.upgrade() {
                client.executor_streams.receive(frame);
            }
            continue;
        }
        if matches!(
            frame.kind,
            codesense_host_proto::Kind::MCP_OPEN
                | codesense_host_proto::Kind::MCP_DATA
                | codesense_host_proto::Kind::MCP_CLOSE
        ) {
            if let Some(client) = client.upgrade() {
                client.reverse_streams.receive(&client, frame);
            }
            continue;
        }
        match ingress.accept(&frame) {
            Ok(Some(Inbound::Event(event))) => {
                // A layout carries a server lifetime on the wire. Reject a
                // mismatched epoch before it can mutate the revision or reach
                // any app effect. Remote worktree events are intentionally
                // excluded: local content reconciliation has no remote source
                // boundary in RM-4e.
                if matches!(
                    &event,
                    TerminalDomainEvent::LayoutChanged { epoch, .. } if epoch != &namespace.epoch
                ) || (is_remote && matches!(&event, TerminalDomainEvent::WorktreeLive { .. }))
                {
                    continue;
                }
                if let TerminalDomainEvent::MachineStats { sample } = &event {
                    if let Some(client) = client.upgrade() {
                        *lock(&client.machine_stats) = Some((Instant::now(), sample.clone()));
                    }
                }
                if let TerminalDomainEvent::SessionRemoved { session, .. } = &event {
                    lock(&ledger.hosted).remove(session.as_str());
                    let open = client
                        .upgrade()
                        .and_then(|c| c.input_channel(session).map(|channel| (c, channel)));
                    if let Some((client, channel)) = open {
                        // Off the reader: the close waits for any write in
                        // flight on the channel, which must not stall replies.
                        // Only the channel open now: a respawn opens its own.
                        let session = session.clone();
                        off_the_reader("pty-input-close", move || {
                            client.close_input(&session, Some(channel))
                        });
                    }
                }
                if let TerminalDomainEvent::LayoutChanged { rev: changed, .. } = &event {
                    rev.fetch_max(*changed, Ordering::SeqCst);
                }
                let stale =
                    event_session(&event).is_some_and(|id| lock(&ledger.withdrawn).contains(id));
                if !stale && ledger.deliver_once(&namespace.epoch, &event) {
                    let _ = sink.emit(namespace, event);
                }
            }
            Ok(Some(Inbound::Reply { rid, result })) => {
                #[cfg(test)]
                if let Some(client) = client.upgrade() {
                    client.observation.update(|s| {
                        let error = match &result {
                            Err(error) => Some(error.clone()),
                            Ok(Reply::LayoutRefused { error }) => Some(error.to_string()),
                            Ok(Reply::Cancelled) => Some("spawn cancelled".into()),
                            Ok(_) => None,
                        };
                        s.replies.push(Rm4fReply {
                            rid,
                            transport_success: result.is_ok(),
                            success: error.is_none(),
                            error,
                            reply: result.as_ref().ok().cloned(),
                        })
                    });
                }
                match lock(pending).remove(&rid) {
                    Some(caller) => {
                        let _ = caller.send(result);
                    }
                    // Its caller stopped waiting: a viewport attached for it
                    // is detached again, rather than left attached until the
                    // connection closes.
                    None => {
                        if let (Ok(Reply::Subscribed { result }), Some(client)) =
                            (result, client.upgrade())
                        {
                            off_the_reader("pty-orphan-detach", move || {
                                if let Err(error) = crate::terminal::session_server::SessionServerClient::unsubscribe(&client, result.stream_id) {
                                    log::warn!(target: "pty", "detach of an abandoned viewport failed: {error}");
                                }
                            });
                        }
                    }
                }
            }
            Ok(Some(Inbound::ManifestPart { rid, part })) => {
                if let Some(client) = client.upgrade() {
                    if let Some(parts) = lock(&client.manifest_parts).get(&rid) {
                        let _ = parts.send(part);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => break Some(error),
        }
    };
    if let Some(client) = client.upgrade() {
        client.reverse_streams.retire();
        client.executor_streams.retire();
    }
    if let Some(error) = failure {
        log::warn!(target: "pty", "session server stream ended: {error}");
    }
    // Loss notification is the EOF linearization point. Remote publication
    // gates serialize this callback with adoption before liveness changes,
    // so an EOF cannot create a dead Connected window or leak buffered events.
    if let Some(lost) = lost.and_then(|hook| lock(&hook).clone()) {
        lost();
    }
    alive.store(false, Ordering::SeqCst);
    // Dropping the senders tells each waiting caller its reply is lost.
    lock(pending).clear();
}

/// Run `work` on a thread of its own, named `name`, so the connection's reader
/// never waits on a write.
fn off_the_reader(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = std::thread::Builder::new().name(name.into()).spawn(work) {
        log::warn!(target: "pty", "{name} thread failed: {error}");
    }
}

/// Cancel a spawn of `session` over a new, event-less connection to the
/// sidecar of the instance rooted at `root`; `true` once the sidecar confirms.
fn cancel_spawn_over_fresh_connection(root: &Path, session: &str) -> bool {
    let confirmed = handshake(root)
        .map_err(|e| e.to_string())
        .and_then(|mut admitted| {
            // The sidecar answers once a spawn in flight is torn down.
            admitted
                .transport
                .reader
                .get_mut()
                .set_read_timeout(Some(REPLY_TIMEOUT))
                .map_err(|e| e.to_string())?;
            let request = Request::CancelSpawn {
                session: PtySessionId::new(session),
            };
            call_once(
                &mut admitted.transport.reader,
                &mut admitted.transport.writer,
                1,
                request,
            )?
            .map(|_| ())
        });
    if let Err(error) = &confirmed {
        log::warn!(target: "pty", "could not cancel {session} in the session server: {error}");
    }
    confirmed.is_ok()
}

/// When the last launch and reconnect attempts failed.
#[derive(Default)]
struct Attempts {
    last_launch_failure: Option<Instant>,
    last_reconnect_failure: Option<Instant>,
    /// The sidecar was found incompatible, and when: reported again without
    /// a new handshake within [`RECONNECT_BACKOFF`].
    incompatible: Option<(Instant, Skew)>,
}

/// How a new connection to the sidecar relates to the connection before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continuity {
    /// The same sidecar lifetime carried on while the app was away: its
    /// sessions and layout are intact, but the viewports the lost connection
    /// attached were detached. `layout_moved` when its layout changed past the
    /// newest revision the connection before saw.
    Resumed { layout_moved: bool },
    /// Another sidecar lifetime: the one before is gone, with `ended`, every
    /// session this app hosted in it (each with whether it was a warm-pool
    /// member), taken when the new lifetime was adopted.
    Restarted { ended: Vec<(PtySessionId, bool)> },
}

/// The warm-pool flag the ledger keeps for each session it hosts.
type Hosted = Vec<(PtySessionId, bool)>;

/// Whether this instance's sidecar may hold sessions, as a read that must not
/// guess sees it.
pub enum Presence {
    /// Connected now: ask it.
    Connected(Arc<SessionServerClient>),
    /// No sidecar runs for the instance: it holds nothing.
    Absent,
    /// One runs, or may be starting, but cannot be asked now: what it holds
    /// is unknown.
    Unknown(String),
}

/// Explicit confirmation of the local host and terminals an update will end.
/// A changed host or session set requires another user review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalActivityUpgrade {
    pub server_id: String,
    pub epoch: String,
    pub pid: u32,
    pub session_ids: Vec<String>,
}

pub(crate) fn require_spend_hold_capability(hello: &Hello) -> Result<(), String> {
    if hello.supports(&Capability::InboxWakeHoldsV1) {
        return Ok(());
    }
    Err("Spend cap enforcement needs a terminal-server update. Finish and close its terminals, then use Restart terminal server… before launching this capped agent.".into())
}

fn read_activity_upgrade(root: &Path) -> Result<Option<LocalActivityUpgrade>, ConnectError> {
    read_server_identity(root, |hello| {
        !hello.supports(&Capability::ActivityV2) || !hello.supports(&Capability::InboxWakeHoldsV1)
    })
}

/// SW-AGR-19: the running server's identity and sessions — what a confirmed
/// stop binds to — when `wanted` says this server is one to replace.
fn read_server_identity(
    root: &Path,
    wanted: impl FnOnce(&Hello) -> bool,
) -> Result<Option<LocalActivityUpgrade>, ConnectError> {
    let mut admitted = handshake(root)?;
    if !wanted(&admitted.hello) {
        return Ok(None);
    }
    admitted
        .transport
        .reader
        .get_mut()
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|error| ConnectError::Refused(error.to_string()))?;
    let reply = call_once(
        &mut admitted.transport.reader,
        &mut admitted.transport.writer,
        1,
        Request::LiveSessionIds,
    )
    .map_err(ConnectError::Refused)?
    .map_err(ConnectError::Refused)?;
    let Reply::Sessions { sessions } = reply else {
        return Err(ConnectError::Refused(
            "The terminal server could not list its sessions".into(),
        ));
    };
    let mut session_ids: Vec<_> = sessions
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect();
    session_ids.sort();
    Ok(Some(LocalActivityUpgrade {
        server_id: admitted.hello.server_id,
        epoch: admitted.hello.epoch,
        pid: admitted.hello.pid,
        session_ids,
    }))
}

/// The app's connection to its sidecar. Reading the live client never waits
/// on a connect or launch in progress; connecting, reconnecting and launching
/// are serialized among themselves, so there is never more than one live
/// connection pushing events.
#[derive(Default)]
pub struct SessionServerState {
    /// The root of the instance whose sidecar this is; `None` for an app
    /// with no instance of its own (a test's), which never probes one.
    root: Option<PathBuf>,
    client: Mutex<Option<Arc<SessionServerClient>>>,
    pub(crate) admission: Arc<LocalRuntimeAdmission>,
    /// How the newest connection continued from the one before, until taken.
    change: Mutex<Option<Continuity>>,
    attempts: Mutex<Attempts>,
    ledger: Arc<Ledger>,
    /// The sink every connection's events go to; set once sidecar mode is
    /// resolved for this app.
    sink: Mutex<Option<Arc<dyn TerminalEventSink>>>,
    /// Told whenever a connection ends — whichever connection, whenever the
    /// hook was set.
    on_lost: Arc<Mutex<Option<OnLost>>>,
    /// A background reconnect is running.
    reconnecting: AtomicBool,
    /// This app process started the sidecar it is connected to (nothing was
    /// listening), so its layout holds nothing from before this process.
    launched_sidecar: AtomicBool,
    /// SV-7: the sidecar (by server id and epoch) a person or an agent
    /// stopped on purpose, until the app connects to another one: that
    /// server's end is announced quietly, and the warm pool does not start a
    /// server again to refill meanwhile. A terminal opened (by a person, an
    /// agent or a routine) still starts one on demand, by design.
    user_stopped: Mutex<Option<(String, String)>>,
}

impl SessionServerState {
    #[cfg(test)]
    pub(crate) fn close_test_connection(&self) {
        let client = lock(&self.client).take();
        if let Some(client) = client {
            client.sever();
        }
        lock(&self.admission.sink_factory).take();
        lock(&self.sink).take();
    }

    /// SV-7: the sidecar `server_id` at `epoch` is being stopped on purpose:
    /// until the app connects to another one, its end is no surprise to
    /// announce, and the warm pool does not start a server to refill.
    pub fn mark_user_stopped(&self, server_id: &str, epoch: &str) {
        *lock(&self.user_stopped) = Some((server_id.to_string(), epoch.to_string()));
    }

    /// SV-7: whether a sidecar was stopped on purpose and the app has not
    /// connected to another since.
    pub fn user_stopped(&self) -> bool {
        lock(&self.user_stopped).is_some()
    }

    /// SV-7: whether the sidecar the app last connected to is the one
    /// stopped on purpose (its end goes unannounced).
    pub fn lost_server_was_stopped(&self) -> bool {
        let stopped = lock(&self.user_stopped).clone();
        let last = lock(&self.client)
            .as_ref()
            .map(|client| (client.server_id().to_string(), client.epoch().to_string()));
        stopped.is_some() && stopped == last
    }

    /// SV-7: the stop is over (it failed, or a person started the server).
    pub fn clear_user_stopped(&self) {
        lock(&self.user_stopped).take();
    }

    /// The connection to the sidecar of the instance rooted at `root`.
    pub fn at(root: Option<PathBuf>) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    pub(crate) async fn construct_remote_client(
        &self,
        admitted: AdmittedRemoteTransport,
        sink: Arc<dyn NamespacedTerminalEventSink>,
        ledger: RemoteSessionLedger,
    ) -> Result<ManagedRemoteClient, ConnectError> {
        SessionServerClient::from_remote_admitted(admitted, sink, ledger.0, None).await
    }

    /// The root of the instance whose sidecar this is.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// A bare local session id cannot retire another runtime's events. Actual
    /// retirement is connection-scoped through the admission owner.
    pub fn withdraw(&self, _session: &str) {}

    /// Tell `lost` whenever a connection made from now on ends.
    pub fn on_lost(&self, lost: OnLost) {
        *lock(&self.on_lost) = Some(lost);
    }

    /// Connect to the sidecar of the instance rooted at `root`.
    fn connect(
        &self,
        root: &Path,
        sink: Arc<dyn TerminalEventSink>,
    ) -> Result<Arc<SessionServerClient>, ConnectError> {
        SessionServerClient::from_admitted_transport_inner(
            root,
            handshake(root)?,
            PumpEventSink::Legacy(sink),
            self.ledger.clone(),
            Some(self.on_lost.clone()),
            Some(self.admission.clone()),
        )
    }

    /// Whether this instance's sidecar may hold sessions, answered without
    /// waiting: the live connection when there is
    /// one; else whether a sidecar runs at all. A connect or launch in progress
    /// is never waited on — the sidecar it reaches may hold sessions — and a
    /// sidecar that runs unconnected (hung, incompatible, or not reached yet)
    /// is unknown.
    pub fn presence(&self) -> Presence {
        if let Some(client) = self.connected() {
            return Presence::Connected(client);
        }
        let Some(root) = self.root() else {
            return Presence::Absent;
        };
        // Held by every connect and launch, so the probe below never races
        // this app's own launch of the sidecar.
        let _attempts = match self.attempts.try_lock() {
            Ok(attempts) => attempts,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Presence::Unknown("the session server is being connected".to_string())
            }
        };
        if let Some(client) = self.connected() {
            return Presence::Connected(client);
        }
        match host_running(root) {
            Ok(false) => Presence::Absent,
            Ok(true) => {
                Presence::Unknown("the session server runs but is not connected".to_string())
            }
            Err(error) => Presence::Unknown(error),
        }
    }

    /// The sidecar this app hosted sessions in is gone (no sidecar runs any
    /// more): forget and return every session hosted there, each with whether
    /// it was a warm-pool member — their leaving was never pushed to the app.
    pub fn take_lost(&self) -> Hosted {
        self.ledger.take_all()
    }

    /// Mark a background reconnect as running; `false` when one already is.
    pub fn begin_reconnect(&self) -> bool {
        !self.reconnecting.swap(true, Ordering::SeqCst)
    }

    /// The background reconnect finished.
    pub fn end_reconnect(&self) {
        self.reconnecting.store(false, Ordering::SeqCst);
    }

    /// Whether this app process launched the sidecar it connects to, rather
    /// than finding one already running from an earlier app process.
    pub fn launched_sidecar(&self) -> bool {
        self.launched_sidecar.load(Ordering::SeqCst)
    }

    /// The live client, if connected now. Never connects.
    pub fn connected(&self) -> Option<Arc<SessionServerClient>> {
        let client = lock(&self.client).clone()?;
        self.admission.is_installed(&client).then_some(client)
    }

    /// The live client, connecting to — or, when nothing listens, launching —
    /// the sidecar of the instance rooted at `root`. `launch` starts the
    /// sidecar; it is not retried within [`LAUNCH_BACKOFF`] of a failure.
    pub fn connect_or_launch(
        &self,
        root: &Path,
        sink: Arc<dyn TerminalEventSink>,
        launch: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<Arc<SessionServerClient>, ConnectError> {
        lock(&self.sink).get_or_insert_with(|| sink.clone());
        if let Some(client) = self.connected() {
            return Ok(client);
        }
        let mut attempts = lock(&self.attempts);
        if let Some(client) = self.connected() {
            return Ok(client);
        }
        if let Some((at, skew)) = &attempts.incompatible {
            if at.elapsed() < RECONNECT_BACKOFF {
                return Err(ConnectError::Incompatible(skew.clone()));
            }
        }
        let client = match self.connect(root, sink.clone()) {
            Ok(client) => Ok(client),
            Err(ConnectError::Unreachable(message)) => {
                if attempts
                    .last_launch_failure
                    .is_some_and(|at| at.elapsed() < LAUNCH_BACKOFF)
                {
                    return Err(ConnectError::Unreachable(format!(
                        "{message}; a recent launch failed"
                    )));
                }
                let started = launch(root)
                    .map_err(ConnectError::Unreachable)
                    .and_then(|()| wait_for_listener(|| self.connect(root, sink.clone())));
                attempts.last_launch_failure =
                    matches!(started, Err(ConnectError::Unreachable(_))).then(Instant::now);
                if started.is_ok() {
                    self.launched_sidecar.store(true, Ordering::SeqCst);
                }
                started
            }
            Err(error) => Err(error),
        };
        let client = match client {
            Err(ConnectError::Incompatible(skew)) => {
                attempts.incompatible = Some((Instant::now(), skew.clone()));
                return Err(ConnectError::Incompatible(skew));
            }
            connected => {
                // Whatever serves the socket now, it is not the incompatible
                // sidecar any more.
                attempts.incompatible = None;
                connected?
            }
        };
        log::info!(
            target: "pty",
            "session server pid {} hosts this instance's PTYs",
            client.pid()
        );
        self.adopt(client.clone())?;
        Ok(client)
    }

    /// Make `client` the live connection, noting how it continues from the
    /// one before (a restart outranks a resume not yet taken).
    fn adopt(&self, client: Arc<SessionServerClient>) -> Result<(), ConnectError> {
        self.admission
            .install(&client, || self.install_client(client.clone()))
    }

    fn install_client(&self, client: Arc<SessionServerClient>) {
        // SV-7: connected to another server than the one stopped on purpose.
        {
            let mut stopped = lock(&self.user_stopped);
            if stopped.as_ref().is_some_and(|(server_id, epoch)| {
                server_id != client.server_id() || epoch != client.epoch()
            }) {
                stopped.take();
            }
        }
        let mut live = lock(&self.client);
        if let Some(before) = live.as_ref() {
            let continuity = if before.epoch() == client.epoch() {
                Continuity::Resumed {
                    layout_moved: client.hello.rev != before.rev(),
                }
            } else {
                // Taken now, before anything can be hosted in the new lifetime.
                Continuity::Restarted {
                    ended: self.ledger.take_all(),
                }
            };
            let mut change = lock(&self.change);
            *change = Some(match (change.take(), continuity) {
                (
                    Some(Continuity::Restarted { mut ended }),
                    Continuity::Restarted { ended: more },
                ) => {
                    ended.extend(more);
                    Continuity::Restarted { ended }
                }
                (Some(restarted @ Continuity::Restarted { .. }), Continuity::Resumed { .. })
                | (_, restarted @ Continuity::Restarted { .. }) => restarted,
                (
                    Some(Continuity::Resumed {
                        layout_moved: before,
                    }),
                    Continuity::Resumed { layout_moved },
                ) => Continuity::Resumed {
                    layout_moved: before || layout_moved,
                },
                (None, resumed) => resumed,
            });
        }
        *live = Some(client);
    }

    /// This instance's sidecar as last found incompatible, if the last
    /// connection attempt found it so.
    pub fn incompatible(&self) -> Option<Skew> {
        lock(&self.attempts)
            .incompatible
            .as_ref()
            .map(|(_, skew)| skew.clone())
    }

    /// The sessions this app hosted in the sidecar that `ended` says are
    /// gone, each with whether it was a warm-pool member, forgotten as taken:
    /// their leaving was never pushed to the app. `ended` runs outside the
    /// ledger's lock (it may ask the sidecar).
    pub fn take_ended(&self, ended: impl Fn(&PtySessionId) -> bool) -> Hosted {
        self.ledger.take_ended(ended)
    }

    /// The sessions this app hosts in the sidecar that have started there
    /// and whose exit it has not had: running, or ended with the exit lost.
    pub fn unexited_sessions(&self) -> Vec<PtySessionId> {
        self.ledger.unexited_sessions()
    }

    /// SV-7: every session this app opened in the sidecar that has not left
    /// it, warm-pool members and spawns awaiting their reply included.
    pub fn opened_sessions(&self) -> Vec<PtySessionId> {
        self.ledger.opened_sessions()
    }

    /// The sink the sidecar's events go to, once sidecar mode is resolved.
    pub fn sink(&self) -> Option<Arc<dyn TerminalEventSink>> {
        lock(&self.sink).clone()
    }

    /// How the newest connection continued from the one before, once: `None`
    /// for the app's first connection, or once taken.
    pub fn take_change(&self) -> Option<Continuity> {
        lock(&self.change).take()
    }

    pub fn activity_upgrade_status(&self) -> Result<Option<LocalActivityUpgrade>, String> {
        let Some(root) = self.root() else {
            return Ok(None);
        };
        let _attempts = lock(&self.attempts);
        read_activity_upgrade(root).map_err(|error| error.to_string())
    }

    /// SW-AGR-19: the running server's identity and sessions, for a This Mac
    /// update's confirmation (`None` with no instance root).
    pub fn local_server_identity(&self) -> Result<Option<LocalActivityUpgrade>, String> {
        let Some(root) = self.root() else {
            return Ok(None);
        };
        let _attempts = lock(&self.attempts);
        read_server_identity(root, |_| true).map_err(|error| error.to_string())
    }

    /// SV-7: stop This Mac's terminal server, after the person confirmed
    /// ending exactly `expected`'s sessions — the confirmed, epoch-checked stop
    /// of [`Self::replace_local_server`], with nothing launched after it.
    /// `terminate` ends the process, which ends its sessions. Whether the old
    /// server stopped (or was found already gone); a failure says whether it
    /// did, and a server that still runs is admitted again.
    pub fn stop_local_server(
        &self,
        root: &Path,
        expected: &LocalActivityUpgrade,
        sink: Arc<dyn TerminalEventSink>,
        terminate: impl FnOnce(u32) -> Result<(), String>,
    ) -> Result<bool, RestartFailure> {
        let failure = |message: String, old_stopped| RestartFailure {
            old_stopped,
            error: ConnectError::Refused(message),
        };
        // Err((why, old stopped, reattach the old server)).
        let stopped: Result<bool, (String, bool, bool)> = {
            let mut attempts = lock(&self.attempts);
            let stopped = match read_server_identity(root, |_| true) {
                Ok(Some(current)) => {
                    if current != *expected || current.pid <= 1 {
                        Err((
                            "The terminal server or its sessions changed. Review the update again."
                                .to_string(),
                            false,
                            false,
                        ))
                    } else {
                        self.admission.revoke_sidecar();
                        match terminate(current.pid) {
                            // It still runs: let the app hold it again.
                            Err(error) => Err((error, false, true)),
                            Ok(()) => {
                                let released = wait_until_released(root).map_err(|error| {
                                    (
                                        error.to_string(),
                                        // An unknown lock state never launches.
                                        !matches!(host_running(root), Ok(false)),
                                        UnixStream::connect(socket_path(root)).is_ok(),
                                    )
                                });
                                let settled = after_stop(released);
                                if matches!(settled, Ok(true)) {
                                    if let Some(client) = lock(&self.client).as_ref() {
                                        client.alive.store(false, Ordering::SeqCst);
                                    }
                                }
                                settled
                            }
                        }
                    }
                }
                // Every answering server is reported (`|_| true`): nothing to
                // replace here.
                Ok(None) => Err((
                    "The terminal server could not be reviewed.".into(),
                    false,
                    false,
                )),
                // Retrying after the confirmed server stopped may launch, but
                // never terminates an unknown one.
                Err(ConnectError::Unreachable(_)) if matches!(host_running(root), Ok(false)) => {
                    Ok(true)
                }
                Err(error) => Err((error.to_string(), false, false)),
            };
            *attempts = Attempts::default();
            stopped
        };
        match stopped {
            Ok(old_stopped) => Ok(old_stopped),
            Err((why, old_stopped, reattach)) => {
                if reattach {
                    // Admit the server that is still running again; never
                    // launch one here.
                    let _ = self.connect_or_launch(root, sink, |_| {
                        Err("not relaunched: the old terminal server still runs".into())
                    });
                }
                Err(failure(why, old_stopped))
            }
        }
    }

    /// SW-AGR-19: replace This Mac's terminal server with this build's, after
    /// the person confirmed ending exactly `expected`'s sessions: the same
    /// confirmed, epoch-checked stop as the activity update, for any server.
    pub fn replace_local_server(
        &self,
        root: &Path,
        expected: &LocalActivityUpgrade,
        sink: Arc<dyn TerminalEventSink>,
        terminate: impl FnOnce(u32) -> Result<(), String>,
        launch: impl Fn(&Path) -> Result<(), String>,
    ) -> Result<(Arc<SessionServerClient>, bool), RestartFailure> {
        let old_stopped = self.stop_local_server(root, expected, sink.clone(), terminate)?;
        // The new server: launched once, and once more if it did not come up.
        let client = match self.connect_or_launch(root, sink.clone(), &launch) {
            Ok(client) => client,
            Err(_) if old_stopped => {
                *lock(&self.attempts) = Attempts::default();
                self.connect_or_launch(root, sink, &launch)
                    .map_err(|error| RestartFailure { old_stopped, error })?
            }
            Err(error) => return Err(RestartFailure { old_stopped, error }),
        };
        Ok((client, old_stopped))
    }

    /// Only an explicit user confirmation may stop a compatible older owner.
    /// This is not live migration or an atomic idle drain: a legacy host has
    /// no admission freeze. The confirmation covers all terminals it owns.
    pub fn update_activity_server(
        &self,
        root: &Path,
        expected: &LocalActivityUpgrade,
        sink: Arc<dyn TerminalEventSink>,
        terminate: impl FnOnce(u32) -> Result<(), String>,
        launch: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(Arc<SessionServerClient>, bool), RestartFailure> {
        let failure = |message: String, old_stopped| RestartFailure {
            old_stopped,
            error: ConnectError::Refused(message),
        };
        let old_stopped = {
            let mut attempts = lock(&self.attempts);
            let stopped = match read_activity_upgrade(root) {
                Ok(Some(current)) => {
                    if current != *expected || current.pid <= 1 {
                        return Err(failure(
                            "The terminal server or its sessions changed. Review the update again."
                                .into(),
                            false,
                        ));
                    }
                    self.admission.revoke_sidecar();
                    terminate(current.pid).map_err(|error| failure(error, false))?;
                    wait_until_unreachable(root)
                        .map_err(|error| failure(error.to_string(), true))?;
                    if let Some(client) = lock(&self.client).as_ref() {
                        client.alive.store(false, Ordering::SeqCst);
                    }
                    true
                }
                // Another completed update already installed a capable host.
                Ok(None) => false,
                // Retrying after the confirmed old owner stopped may launch,
                // but must never terminate an unknown replacement.
                Err(ConnectError::Unreachable(_)) if matches!(host_running(root), Ok(false)) => {
                    true
                }
                Err(error) => return Err(failure(error.to_string(), false)),
            };
            *attempts = Attempts::default();
            stopped
        };
        let client = self
            .connect_or_launch(root, sink, launch)
            .map_err(|error| RestartFailure { old_stopped, error })?;
        if !client.supports_activity() {
            return Err(failure(
                "The replacement terminal server does not support agent activity.".into(),
                old_stopped,
            ));
        }
        client
            .activity_snapshot()
            .map_err(|error| failure(error.to_string(), old_stopped))?;
        Ok((client, old_stopped))
    }

    /// End this instance's running sidecar when it is incompatible (whatever
    /// protocol it speaks) or hung (it runs but will not complete the
    /// handshake), and launch this build's in its place: `terminate` ends the
    /// process its hello names — or, for a hung one, the pid it recorded —
    /// which ends its sessions. A compatible sidecar (one an earlier restart
    /// already started) is never ended: it is only connected to. The attempts' backoffs are cleared, so the relaunch is
    /// immediate. Returns the new connection and
    /// how many sessions the ended sidecar said it ran (`None` when it did not
    /// say, or nothing ran); a failure says whether the old sidecar was
    /// already gone.
    pub fn restart(
        &self,
        root: &Path,
        sink: Arc<dyn TerminalEventSink>,
        terminate: impl FnOnce(u32) -> Result<(), String>,
        launch: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(Arc<SessionServerClient>, Option<u32>), RestartFailure> {
        let still_running = |error: ConnectError| RestartFailure {
            old_stopped: false,
            error,
        };
        let ended = {
            let mut attempts = lock(&self.attempts);
            let ended = match sidecar_to_end(root) {
                Ok(Some((pid, ended))) => {
                    self.admission.revoke_sidecar();
                    terminate(pid)
                        .map_err(ConnectError::Refused)
                        .map_err(still_running)?;
                    wait_until_unreachable(root).map_err(still_running)?;
                    // A connection to the ended sidecar is over, whether or
                    // not its reader has seen the end yet.
                    if let Some(over) = lock(&self.client).as_ref() {
                        over.alive.store(false, Ordering::SeqCst);
                    }
                    ended
                }
                // Nothing runs to end, or this build's already runs.
                Ok(None) | Err(ConnectError::Unreachable(_)) => None,
                Err(error) => return Err(still_running(error)),
            };
            *attempts = Attempts::default();
            ended
        };
        let client = self
            .connect_or_launch(root, sink, launch)
            .map_err(|error| RestartFailure {
                old_stopped: true,
                error,
            })?;
        Ok((client, ended))
    }

    /// The live client, or — with none, or after the connection was lost — a
    /// new connection to the sidecar of `root` if one listens, so its
    /// sessions stay reachable. `None` before the app's sink is set, or within
    /// [`RECONNECT_BACKOFF`] of a failed reconnect. Never launches.
    pub fn reconnected(&self, root: &Path) -> Option<Arc<SessionServerClient>> {
        let sink = lock(&self.sink).clone()?;
        if let Some(client) = self.connected() {
            return Some(client);
        }
        let mut attempts = lock(&self.attempts);
        if let Some(client) = self.connected() {
            return Some(client);
        }
        if attempts
            .last_reconnect_failure
            .is_some_and(|at| at.elapsed() < RECONNECT_BACKOFF)
        {
            return None;
        }
        match self.connect(root, sink) {
            Ok(client) => {
                attempts.last_reconnect_failure = None;
                attempts.incompatible = None;
                self.adopt(client.clone()).ok()?;
                Some(client)
            }
            Err(error) => {
                attempts.last_reconnect_failure = Some(Instant::now());
                log::warn!(target: "pty", "session server unreachable: {error}");
                // As a launch-or-connect records it, for the restart prompt.
                if let ConnectError::Incompatible(skew) = error {
                    attempts.incompatible = Some((Instant::now(), skew));
                }
                None
            }
        }
    }
}

/// Connect to a just-launched sidecar once it listens.
fn wait_for_listener(
    connect: impl Fn() -> Result<Arc<SessionServerClient>, ConnectError>,
) -> Result<Arc<SessionServerClient>, ConnectError> {
    let deadline = Instant::now() + STARTUP_WAIT;
    loop {
        match connect() {
            Err(ConnectError::Unreachable(message)) if Instant::now() >= deadline => {
                return Err(ConnectError::Unreachable(format!(
                    "{SIDECAR_BIN} did not start listening: {message}"
                )))
            }
            Err(ConnectError::Unreachable(_)) => std::thread::sleep(Duration::from_millis(25)),
            connected => return connected,
        }
    }
}

/// The sidecar's opt-in lifetime-tie variable (its `exit_with::EXIT_WITH_PID_ENV`;
/// a test pins the two together). The app strips it from every launch.
const SIDECAR_EXIT_WITH_PID_ENV: &str = "CODESENSE_SESSION_SERVER_EXIT_WITH_PID";

/// The sidecar serving the instance rooted at `root` that must be ended for
/// this build's to serve it, if one does: its pid and how many sessions its
/// census counts. An incompatible one names its pid in its hello, whatever
/// protocol it speaks; one that runs but will not complete the handshake
/// (hung) is found by the pid it recorded under the instance's lock, its count
/// unknown. `None` when a compatible one serves it.
fn sidecar_to_end(root: &Path) -> Result<Option<(u32, Option<u32>)>, ConnectError> {
    match handshake(root) {
        Ok(_) => Ok(None),
        Err(ConnectError::Incompatible(skew)) => skew
            .host_pid
            .map(|pid| Some((pid, skew.live_sessions)))
            .ok_or_else(|| {
                ConnectError::Refused("the terminal server did not name its process".into())
            }),
        Err(ConnectError::Refused(why)) => match host_running(root) {
            Ok(true) => host_pid(root)
                .map(|pid| Some((pid, None)))
                .ok_or(ConnectError::Refused(why)),
            _ => Err(ConnectError::Refused(why)),
        },
        Err(error) => Err(error),
    }
}

/// SW-AGR-19: after the confirmed server was signalled, what the wait for it
/// to let go comes to — `Err((why, lock still held, socket still answers))`
/// when it timed out. Released (even just after the wait): this build's is
/// launched. Not released in time: its sessions are ending (old stopped), and
/// the app holds it again only while it still answers.
fn after_stop(released: Result<(), (String, bool, bool)>) -> Result<bool, (String, bool, bool)> {
    match released {
        Ok(()) | Err((_, false, false)) => Ok(true),
        Err((why, _, answers)) => Err((why, true, answers)),
    }
}

/// SW-AGR-19: the stopped server's socket is gone and its singleton lock
/// released (so this build's server can take it).
fn wait_until_released(root: &Path) -> Result<(), ConnectError> {
    wait_until_unreachable(root)?;
    let deadline = Instant::now() + STARTUP_WAIT;
    while matches!(host_running(root), Ok(true)) {
        if Instant::now() >= deadline {
            return Err(ConnectError::Refused(
                "the terminal server still holds its lock".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

/// Wait for the instance's socket to stop answering once its sidecar ends.
fn wait_until_unreachable(root: &Path) -> Result<(), ConnectError> {
    let deadline = Instant::now() + STARTUP_WAIT;
    while UnixStream::connect(socket_path(root)).is_ok() {
        if Instant::now() >= deadline {
            return Err(ConnectError::Refused(
                "the terminal server did not stop".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

/// Whether an executable named `name` is a terminal server: bundled as the
/// bare name, built with the target triple appended.
fn is_sidecar_binary(name: &str) -> bool {
    name == SIDECAR_BIN
        || name
            .strip_prefix(SIDECAR_BIN)
            .is_some_and(|triple| triple.starts_with('-'))
}

/// Whether a process whose kernel path is `exe` and whose argv is `cmd` runs a
/// terminal server.
///
/// A process started as the activity helper never is, whatever its kernel path
/// (the helper is a hard link to the server binary). Otherwise the server's
/// file name counts, as it always has, and alone decides when there is no
/// argv to read. The kernel may also name a running
/// server by an `activity-helper` link to its binary; that counts by
/// [`strays::server_exe_of`]'s rule only when the argv is a server's: a server
/// binary's absolute path, then the server's own options, with `--root`'s
/// absolute value read before any `--`.
fn runs_sidecar(exe: Option<&Path>, cmd: &[std::ffi::OsString]) -> bool {
    let Some(exe) = exe else {
        return false;
    };
    let named = |path: &Path, name: &dyn Fn(&str) -> bool| {
        path.file_name()
            .is_some_and(|file| name(&file.to_string_lossy()))
    };
    // No argv to read (an exiting process, or one whose arguments cannot be
    // read): the file name decides, as it always has.
    let Some(started_from) = cmd.first().map(Path::new) else {
        return named(exe, &is_sidecar_binary);
    };
    if named(started_from, &|file| file == strays::HELPER_NAME) {
        return false;
    }
    if named(exe, &is_sidecar_binary) {
        return true;
    }
    let options: Vec<&std::ffi::OsString> =
        cmd[1..].iter().take_while(|arg| *arg != "--").collect();
    let server_argv = started_from.is_absolute()
        && named(started_from, &is_sidecar_binary)
        && options
            .first()
            .is_some_and(|first| strays::SERVER_ARGS.iter().any(|option| *first == option));
    if !server_argv {
        return false;
    }
    let root = options
        .iter()
        .position(|arg| *arg == "--root")
        .and_then(|at| options.get(at + 1))
        .map(|root| Path::new(root))
        .filter(|root| root.is_absolute())
        .map(|root| root.to_string_lossy().into_owned());
    strays::server_exe_of(exe, started_from, root.as_deref()).is_some()
}

/// The kernel's executable path and the argv of process `pid`, as `sysinfo`
/// reads them; `None` when there is no such process.
fn process_identity(pid: u32) -> Option<(Option<PathBuf>, Vec<std::ffi::OsString>)> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let id = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[id]),
        true,
        ProcessRefreshKind::new()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    system
        .process(id)
        .map(|process| (process.exe().map(Path::to_path_buf), process.cmd().to_vec()))
}

/// How long an ended sidecar gets to exit on `SIGTERM` before `SIGKILL`.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// End the sidecar process `pid` — only a terminal server binary, never
/// another process that happens to hold the pid — and wait for it to exit.
/// Its sessions end with it.
pub fn terminate_sidecar(pid: u32) -> Result<(), String> {
    let is_sidecar =
        process_identity(pid).is_some_and(|(exe, cmd)| runs_sidecar(exe.as_deref(), &cmd));
    if !is_sidecar {
        return Err(format!("process {pid} is not a {SIDECAR_BIN}"));
    }
    let target = pid as libc::pid_t;
    let exited = |within: Duration| {
        let deadline = Instant::now() + within;
        loop {
            if unsafe { libc::kill(target, 0) } != 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    unsafe { libc::kill(target, libc::SIGTERM) };
    if exited(TERMINATE_GRACE) {
        return Ok(());
    }
    unsafe { libc::kill(target, libc::SIGKILL) };
    if exited(TERMINATE_GRACE) {
        Ok(())
    } else {
        Err(format!("{SIDECAR_BIN} {pid} did not exit"))
    }
}

/// Launch the sidecar `binary` for the instance rooted at `root`, its spawns
/// resolving against this process's environment and the app's shipped
/// `resources`, its emulator worker the app's `emulator` (else the one beside
/// `binary`). The launched process binds the instance's socket, then forks
/// the server into its own session, parented by launchd, and exits — so the
/// server survives the app. Its stderr goes to `<root>/logs/pty-server.log`.
pub fn launch_detached(
    binary: &Path,
    root: &Path,
    resources: Option<&Path>,
    emulator: Option<&Path>,
) -> Result<(), String> {
    launch_detached_with_env(binary, root, resources, emulator, &[])
}

/// [`launch_detached`] with extra environment for the sidecar — tests tie the
/// sidecar's lifetime to their own process this way.
fn launch_detached_with_env(
    binary: &Path,
    root: &Path,
    resources: Option<&Path>,
    emulator: Option<&Path>,
    env: &[(&str, String)],
) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let log = root.join("logs");
    let stderr = std::fs::create_dir_all(&log)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log.join("pty-server.log"))
        })
        .map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(binary);
    codesense_pty_core::nofile::inherit_original_limit(&mut command);
    command.env("CODESENSE_LOCAL_WAKE_HOLDS", "1");
    // `--root` last: the instance's sidecars are found by their args' suffix.
    if let Some(resources) = resources {
        command.arg("--resources").arg(resources);
    }
    if let Some(emulator) = emulator {
        command.arg("--emulator").arg(emulator);
    }
    let status = command
        .arg("--root")
        .arg(root)
        // Never inherit a lifetime tie from the app's own environment: a product
        // sidecar must outlive the app.
        .env_remove(SIDECAR_EXIT_WITH_PID_ENV)
        .envs(env.iter().map(|(key, value)| (*key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .status()
        .map_err(|e| format!("launch {binary:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{binary:?} exited with {status}"))
    }
}

#[cfg(test)]
pub(crate) mod tests;
