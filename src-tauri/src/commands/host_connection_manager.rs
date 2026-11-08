// SPDX-License-Identifier: MIT
//! Per-machine connection intent and publication authority.
//!
//! This module deliberately does not admit remote protocol identity.  The
//! remote attempt path stays fail closed until the greeting-first admission
//! step is installed; it nevertheless owns ordering, cancellation and normal
//! transport teardown now.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use codesense_host_proto::Census;
use codesense_pty_core::domain::{ClientBootId, TerminalDomain, ViewerId};
use codesense_pty_core::layout::LayoutSnapshot;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::Manager;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::host_connection::{
    AdmittedRemoteTransport, PendingRemoteTransport, RemoteAdmissionError, RemoteTransportOwner,
};
use super::machines::PinnedTransportGenerationError;
use super::machines::{self, LOCAL_MACHINE_ID};
use super::machines::{
    FrozenRuntimeLayout, RuntimeAdmissionBaseline, RuntimeAdmissionRow, RuntimeAdmissionState,
    RuntimeCounter, RuntimeHistoryReason, RuntimeMemberAddress, RuntimeMemberTarget,
};
use crate::terminal::session_server::{
    EventNamespace, ManagedRemoteClient, OnLost, RemoteSessionLedger, SessionServerClient,
    SessionServerState,
};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ObservationSeq(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ActorIncarnation(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AttemptGeneration(u64);

type LossRecovery = Arc<
    dyn Fn(String, ActorIncarnation, AttemptGeneration, Arc<SessionServerClient>) + Send + Sync,
>;

fn remote_loss_dispatcher(
    machine_id: String,
    incarnation: ActorIncarnation,
    generation: AttemptGeneration,
    expected: &Arc<SessionServerClient>,
    recover: LossRecovery,
) -> OnLost {
    let expected = Arc::downgrade(expected);
    Arc::new(move || {
        if let Some(expected) = expected.upgrade() {
            recover(machine_id.clone(), incarnation, generation, expected);
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineManagerRecord {
    pub machine_id: String,
    pub scope: Option<super::machines::registry::Scope>,
    pub revision: Option<super::machines::registry::RegistryRevision>,
    pub fingerprint: String,
    pub set_up: bool,
    pub auto_connect: bool,
    /// The machine held open tabs when last seen: it reconnects in the
    /// background like an auto-connect machine, so its sessions come back.
    /// The registry sets it only where a background connect can't prompt.
    pub open_tabs: bool,
}

impl MachineManagerRecord {
    pub(crate) fn local() -> Self {
        Self {
            machine_id: LOCAL_MACHINE_ID.into(),
            scope: None,
            revision: None,
            fingerprint: "builtin:local".into(),
            set_up: true,
            auto_connect: false,
            open_tabs: false,
        }
    }
}

/// Persists a remote machine's "had open tabs" bit when it flips.
pub(crate) type OpenTabsWriter = Arc<dyn Fn(String, bool) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum HostConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
    Connected {
        server_id: String,
    },
    Incompatible {
        n_sessions: u32,
        proof: Option<machines::LegacyHostProof>,
    },
    UnsupportedLegacy {
        count: LegacySessionCount,
        proof: machines::LegacyHostProof,
    },
    ReplacementReserved {
        count: Option<u32>,
    },
    Replacing {
        count: Option<u32>,
    },
    Readmitting {
        count: Option<u32>,
    },
    ReplacementFailed {
        old_stopped: bool,
        count: Option<u32>,
        repair: String,
    },
    Unreachable {
        last_error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LegacySessionCount {
    Unknown,
}

/// Opening runs several sequential SSH sessions before the relay starts (a
/// private account: host-key scan, helper staging, account check, server
/// lookup). Each costs a handful of round trips to authenticate, about 2 s on
/// a 300 ms intercontinental link, so the budget is sized for that path with
/// headroom rather than for localhost.
pub(crate) const CONNECT_DEADLINE: Duration = Duration::from_secs(20);
/// The hello arrives only after the relay's own SSH session authenticates and
/// the remote side re-verifies its account: a few seconds on a slow link.
pub(crate) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);
/// Extra time a connect gets when its deadline finds it re-sending the
/// private helper after an app update: that upload is bounded on its own
/// (30 s) and a slow link must not fail midway through it.
pub(crate) const UPLOAD_GRACE: Duration = Duration::from_secs(60);
const HEARTBEAT_CADENCE: Duration = Duration::from_secs(5);
const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(5);
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 30_000;
const REPLACEMENT_RESERVATION_TTL: Duration = Duration::from_secs(120);

fn validate_replacement_admission(
    expected_server_id: &str,
    old_epoch: Option<&str>,
    actual_server_id: &str,
    actual_epoch: &str,
    census: &Census,
) -> Result<(), &'static str> {
    if actual_server_id != expected_server_id {
        return Err("replacement candidate changed server identity");
    }
    if old_epoch.is_some_and(|old| old == actual_epoch) {
        return Err("replacement candidate reused the retired epoch");
    }
    if census.live_sessions != 0 {
        return Err("replacement candidate reported a non-empty census");
    }
    Ok(())
}

fn retry_backoff(machine_id: &str, attempt: u32) -> Duration {
    let exponent = attempt.min(16);
    let base = BACKOFF_BASE_MS
        .saturating_mul(1_u64 << exponent)
        .min(BACKOFF_CAP_MS);
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in machine_id
        .as_bytes()
        .iter()
        .copied()
        .chain(attempt.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let jitter_bps = (hash % 2001) as i64 - 1000;
    let millis = (((base as i128) * (10_000_i128 + i128::from(jitter_bps)) / 10_000)
        .clamp(0, i128::from(u64::MAX)) as u64)
        .min(BACKOFF_CAP_MS);
    Duration::from_millis(millis)
}

trait ConnectionScheduler: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

type Deadline<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

struct TokioConnectionScheduler;

impl ConnectionScheduler for TokioConnectionScheduler {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectOrigin {
    #[allow(
        dead_code,
        reason = "Retained enrollment-origin contract; currently no constructing caller"
    )]
    EnrollmentForeground,
    ExplicitForeground,
    Retry,
    Restart,
    Auto,
    /// A manager-owned retry after connection demand was established.
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    Running,
    Draining,
    Drained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectedBackendError {
    ManagerShuttingDown,
    UnknownMachine,
    Disconnected,
    Connecting,
    Handshaking,
    Incompatible {
        snapshot: Box<LastKnownHostSnapshot>,
    },
    UnsupportedLegacy,
    Unreachable {
        last_error: String,
    },
    IdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CensusSnapshot {
    pub(crate) live_sessions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastKnownHostSnapshot {
    pub(crate) layout: Option<LayoutSnapshot>,
    pub(crate) census: CensusSnapshot,
    pub(crate) server_id: String,
    pub(crate) epoch: String,
    pub(crate) rev: u64,
    pub(crate) shutdown_for_replacement: bool,
    /// SW-AGR-19: the frozen host's process, for the confirmed reinstall
    /// fallback (0 when unknown).
    pub(crate) host_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagerProjection {
    Present(HostConnectionState),
    ConfirmedRemoved,
    NotYetObserved,
}

#[derive(Clone)]
pub(crate) struct ConnectedBackendSnapshot {
    machine_id: String,
    incarnation: ActorIncarnation,
    server_id: String,
    epoch: String,
    client: Arc<SessionServerClient>,
}

impl ConnectedBackendSnapshot {
    pub(crate) fn machine_id(&self) -> &str {
        &self.machine_id
    }

    pub(crate) fn incarnation(&self) -> ActorIncarnation {
        self.incarnation
    }

    pub(crate) fn server_id(&self) -> &str {
        &self.server_id
    }

    pub(crate) fn client(&self) -> Arc<SessionServerClient> {
        self.client.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerEvent {
    pub order: u64,
    pub machine_id: String,
    pub incarnation: ActorIncarnation,
    pub generation: Option<AttemptGeneration>,
    pub retry_attempt: u32,
    pub consecutive_heartbeat_misses: u8,
    pub state: HostConnectionState,
    pub admission: Option<RuntimeAdmissionRow>,
    pub scope: Option<super::machines::registry::Scope>,
}

type Observer = Arc<dyn Fn(ManagerEvent) + Send + Sync>;

struct RegistryWatchObservation {
    hinted_ids: std::collections::BTreeSet<String>,
    broad: bool,
    event_revisions: Vec<super::machines::registry::FileRevision>,
}

fn adapt_registry_event(event: &notify::Event) -> Option<RegistryWatchObservation> {
    let relevant = event.paths.iter().any(|path| {
        path.components()
            .any(|part| part.as_os_str() == "machines.d")
            || path
                .file_name()
                .is_some_and(|name| name == "machine-bindings.json")
    });
    if !relevant {
        return None;
    }
    let hinted_ids: std::collections::BTreeSet<_> = event
        .paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let id = name.strip_suffix(".yaml")?;
            super::machines::registry::is_machine_id(id).then(|| id.to_string())
        })
        .collect();
    let broad = hinted_ids.is_empty();
    let event_revisions = event
        .paths
        .iter()
        .filter_map(|path| {
            super::machines::registry::Registry::file_revision(path)
                .ok()
                .flatten()
        })
        .collect();
    Some(RegistryWatchObservation {
        hinted_ids,
        broad,
        event_revisions,
    })
}

struct ActiveAttempt {
    generation: AttemptGeneration,
    token: CancellationToken,
}

#[derive(Clone, PartialEq, Eq)]
struct PendingSelfWrite {
    seq: ObservationSeq,
    revision: Option<super::machines::registry::RegistryRevision>,
}

#[derive(Default)]
struct SubmittedIntents {
    disconnect: AtomicU64,
    retry: AtomicU64,
    removal: AtomicU64,
}

struct IntentGate {
    private_lease: Option<super::auth::private_authority::PrivateAuthorityLease>,
    present: bool,
    /// The registry layer of the latest record, so every connection event can
    /// tell the UI which machine list to re-read.
    scope: Option<super::machines::registry::Scope>,
    incarnation: ActorIncarnation,
    fingerprint: String,
    disconnect_epoch: u64,
    retry_epoch: u64,
    foreground_demand: bool,
    auto_connect: bool,
    auto_suppressed: bool,
    auto_pending: bool,
    pending_self_write: Option<PendingSelfWrite>,
    active: Option<ActiveAttempt>,
    connected: Option<ManagedRemoteClient>,
    quarantine: Option<tokio::sync::watch::Receiver<bool>>,
    serial: Arc<tokio::sync::Mutex<()>>,
    submitted: Arc<SubmittedIntents>,
    handled_disconnect: u64,
    handled_retry: u64,
    handled_removal: u64,
    state: HostConnectionState,
    retry_attempt: u32,
    retry_ticket: u64,
    consecutive_heartbeat_misses: u8,
    connected_generation: Option<AttemptGeneration>,
    remote_ledger: RemoteSessionLedger,
    retirement_reserved: Option<AttemptGeneration>,
    /// Last deliberately retired connection; only an exact same lifetime may
    /// use it to resume retained viewport streams.
    resume_identity: Option<crate::terminal::session_server::EventNamespace>,
    last_layout: Option<LayoutSnapshot>,
    incompatible_snapshot: Option<LastKnownHostSnapshot>,
    frozen_generation: Option<AttemptGeneration>,
    replacement: Option<ReplacementReservation>,
    /// The record's persisted "had open tabs" bit.
    open_tabs: bool,
    /// Workspaces the live host layout shows holding at least one tab.
    tabbed_workspaces: BTreeSet<String>,
}

#[derive(Clone)]
struct ReplacementReservation {
    token: String,
    incarnation: ActorIncarnation,
    generation: AttemptGeneration,
    fingerprint: String,
    proof: machines::LegacyHostProof,
    count: Option<u32>,
    old_epoch: Option<String>,
    frozen_state: HostConnectionState,
    connected_client: Option<Arc<SessionServerClient>>,
    progress: Arc<Mutex<ReplacementProgress>>,
    expires_at: std::time::Instant,
}

#[derive(Default)]
struct ReplacementProgress {
    consumed: bool,
    attempt: Option<AttemptGeneration>,
    census: Option<u32>,
    admission: Option<ConnectedBackendSnapshot>,
}

/// SW-AGR-19: how a machine's terminal server is updated (see
/// [`HostConnectionManager::update_plan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdatePlan {
    Replace,
    Reinstall { pid: Option<u32> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementPrompt {
    pub server_token: String,
    pub count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementFailed {
    pub old_stopped: bool,
    pub count: Option<u32>,
    pub repair: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ReplacementResult {
    Replaced {
        count: Option<u32>,
        /// SW-AGR-19 PR-4: the tabs reopened on the new server.
        #[serde(skip_serializing_if = "Option::is_none")]
        restored: Option<machines::restore::RestoreSummary>,
    },
    Failed(ReplacementFailed),
}

struct ManagerCore {
    last_observation: BTreeMap<String, ObservationSeq>,
    shutdown: ShutdownPhase,
    shutdown_submitted: Arc<AtomicBool>,
    machines: BTreeMap<String, IntentGate>,
    admissions: BTreeMap<String, RuntimeAdmissionRow>,
    events: VecDeque<ManagerEvent>,
    observers: Vec<Observer>,
    dispatching: bool,
}

#[cfg(test)]
pub(crate) mod compatible_activation_fixture;
#[cfg(test)]
mod rm4f_fault_tests;
#[cfg(test)]
pub(crate) mod rm4f_support;

#[cfg(test)]
struct TestRetirementPresentation {
    streams: Arc<super::pty_host::StreamIds>,
    sink: Arc<dyn crate::terminal::session_server::NamespacedTerminalEventSink>,
    before_permit: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Synchronous intent gate plus async attempt owner. All callbacks are
/// dispatched after the core mutex is released.
pub struct HostConnectionManager {
    observation_seq: AtomicU64,
    incarnation_seq: AtomicU64,
    event_seq: Arc<AtomicU64>,
    active_operations: AtomicU64,
    shutdown_submitted: Arc<AtomicBool>,
    operations_changed: Notify,
    publication_barrier: Mutex<()>,
    registry_watcher: Mutex<Option<(std::path::PathBuf, RecommendedWatcher)>>,
    attempt_driver: Arc<dyn RemoteAttemptDriver>,
    scheduler: Arc<dyn ConnectionScheduler>,
    open_tabs_writer: Mutex<Option<OpenTabsWriter>>,
    #[cfg(test)]
    before_publication_delivery: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_publication_precheck: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    loss_recovery_for_test: Mutex<Option<LossRecovery>>,
    #[cfg(test)]
    retirement_for_test: Mutex<Option<Arc<TestRetirementPresentation>>>,
    #[cfg(test)]
    rm4f_reuse_old_epoch_after_daemon_exit: Mutex<Option<String>>,
    core: Arc<Mutex<ManagerCore>>,
}

struct OperationGuard<'a>(&'a HostConnectionManager);

fn install_remote_loss_dispatcher(client: &ManagedRemoteClient, hook: OnLost) -> bool {
    client.set_on_lost(Arc::new(Mutex::new(Some(hook))))
}

/// Complete a changed-lifetime handoff without exposing the replacement
/// before the retired lifetime's user-visible teardown has finished.
///
/// `reserve` and `adopt` are the only operations allowed to touch the manager
/// gate. `retire` runs between them, after the reservation lock is released.
/// A failed reservation returns the still-inert candidate to its caller for
/// awaited quarantine.
fn commit_changed_epoch<C>(
    candidate: C,
    reserve: impl FnOnce() -> bool,
    retire: impl FnOnce() -> bool,
    adopt: impl FnOnce(C) -> Result<(), C>,
) -> Result<(), C> {
    if !reserve() {
        return Err(candidate);
    }
    if !retire() {
        return Err(candidate);
    }
    adopt(candidate)
}

#[async_trait::async_trait]
trait RemoteAttemptDriver: Send + Sync {
    async fn legacy_proof(
        &self,
        _app: Option<tauri::AppHandle>,
        _machine_id: String,
    ) -> Result<Option<machines::LegacyHostProof>, String> {
        Ok(None)
    }
    fn install_loss_dispatcher(
        &self,
        client: &ManagedRemoteClient,
        dispatcher: Option<OnLost>,
    ) -> Result<bool, String> {
        #[cfg(test)]
        let dispatcher = dispatcher.unwrap_or_else(|| Arc::new(|| {}));
        #[cfg(not(test))]
        let dispatcher = dispatcher.ok_or("remote attempt supplied no loss dispatcher")?;
        Ok(install_remote_loss_dispatcher(client, dispatcher))
    }

    async fn open(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError>;

    async fn open_before(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
        mut deadline: Deadline<'_>,
        token: CancellationToken,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        let open = self.open(app, machine_id);
        tokio::pin!(open);
        tokio::select! {
            result = &mut open => result,
            _ = &mut deadline => Err(machines::PinnedSshOpenError::Other(format!("connection deadline exceeded after {}ms", CONNECT_DEADLINE.as_millis()))),
            _ = token.cancelled() => Err(machines::PinnedSshOpenError::Other("connection attempt was superseded".into())),
        }
    }

    async fn revalidate_before_handshake(
        &self,
        pending: &PendingRemoteTransport,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError>;

    async fn admit(
        &self,
        pending: PendingRemoteTransport,
        token: CancellationToken,
    ) -> Result<AdmittedRemoteTransport, RemoteAdmissionError> {
        pending.admit(HANDSHAKE_DEADLINE, token).await
    }

    async fn revalidate_before_publication(
        &self,
        admitted: &AdmittedRemoteTransport,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        let app = app.ok_or_else(|| {
            PinnedTransportGenerationError::Missing(
                "production remote attempt has no application handle".into(),
            )
        })?;
        admitted
            .revalidate_before_publication(app, machine_id)
            .await
    }

    async fn revalidate_after_construction(
        &self,
        _generation: String,
        _app: Option<tauri::AppHandle>,
        _machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        Ok(())
    }

    async fn construct_client(
        &self,
        admitted: AdmittedRemoteTransport,
        app: Option<tauri::AppHandle>,
        ledger: RemoteSessionLedger,
    ) -> Result<ManagedRemoteClient, String> {
        let app = match app {
            Some(app) => app,
            None => {
                admitted.close_awaited().await;
                return Err("production remote attempt has no application handle".into());
            }
        };
        let sink = Arc::new(super::pty_host::AppTerminalSink::new(
            app.clone(),
            super::pty_host::StreamHost::Sidecar,
        ));
        app.state::<SessionServerState>()
            .construct_remote_client(admitted, sink, ledger)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
type OpenTransportOverride = Arc<
    dyn Fn(String) -> Result<machines::PinnedSshTransport, machines::PinnedSshOpenError>
        + Send
        + Sync,
>;
#[cfg(test)]
type LegacyProofOverride =
    Arc<dyn Fn(String) -> Result<Option<machines::LegacyHostProof>, String> + Send + Sync>;

type StartedAttempt = (
    ActorIncarnation,
    AttemptGeneration,
    CancellationToken,
    Arc<tokio::sync::Mutex<()>>,
);

struct AdoptionFreshness {
    incarnation: ActorIncarnation,
    generation: AttemptGeneration,
    validated_observation: u64,
    retirement_committed: bool,
}

#[derive(Default)]
struct ProductionRemoteAttemptDriver {
    #[cfg(test)]
    open_override: Option<OpenTransportOverride>,
    #[cfg(test)]
    proof_override: Option<LegacyProofOverride>,
}

#[async_trait::async_trait]
impl RemoteAttemptDriver for ProductionRemoteAttemptDriver {
    async fn legacy_proof(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<Option<machines::LegacyHostProof>, String> {
        #[cfg(test)]
        if let Some(proof) = &self.proof_override {
            return proof(machine_id);
        }
        machines::legacy_host_proof(
            app.ok_or("production remote attempt has no application handle")?,
            machine_id,
        )
        .await
    }
    async fn open(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        #[cfg(test)]
        if let Some(open) = &self.open_override {
            return open(machine_id).map(RemoteTransportOwner::from_pinned);
        }
        machines::open_pinned_ssh_transport(
            app.ok_or_else(|| {
                machines::PinnedSshOpenError::Other(
                    "production remote attempt has no application handle".into(),
                )
            })?,
            machine_id,
        )
        .await
        .map(RemoteTransportOwner::from_pinned)
    }

    async fn revalidate_after_construction(
        &self,
        generation: String,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        let app = app.ok_or_else(|| {
            PinnedTransportGenerationError::Missing(
                "production remote attempt has no application handle".into(),
            )
        })?;
        machines::revalidate_pinned_transport_generation(app, machine_id, generation).await
    }

    async fn open_before(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
        mut deadline: Deadline<'_>,
        token: CancellationToken,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        #[cfg(test)]
        if self.open_override.is_some() {
            return self.open(app, machine_id).await;
        }
        let app = app.ok_or_else(|| {
            machines::PinnedSshOpenError::Other(
                "production remote attempt has no application handle".into(),
            )
        })?;
        let mut attempt = machines::begin_pinned_ssh_transport(app, machine_id.clone())
            .await
            .map_err(machines::PinnedSshOpenError::Other)?;
        tokio::select! {
            result = attempt.wait() => result.map(RemoteTransportOwner::from_pinned),
            _ = &mut deadline => {
                if attempt.step() == codesense_session_server::private_account::UPLOAD_STEP {
                    tokio::select! {
                        result = attempt.wait() => return result.map(RemoteTransportOwner::from_pinned),
                        _ = tokio::time::sleep(UPLOAD_GRACE) => {}
                        _ = token.cancelled() => {
                            attempt.cancel_and_reap().await.map_err(machines::PinnedSshOpenError::Other)?;
                            return Err(machines::PinnedSshOpenError::Other("connection attempt was superseded".into()));
                        }
                    }
                }
                let step = attempt.step();
                log::warn!(target: "machines", "connect to {machine_id} ran out of time; steps: {:?}", attempt.timings());
                attempt.cancel_and_reap().await.map_err(machines::PinnedSshOpenError::Other)?;
                Err(machines::PinnedSshOpenError::Other(format!("connection deadline exceeded after {}ms while {step}", CONNECT_DEADLINE.as_millis())))
            }
            _ = token.cancelled() => {
                attempt.cancel_and_reap().await.map_err(machines::PinnedSshOpenError::Other)?;
                Err(machines::PinnedSshOpenError::Other("connection attempt was superseded".into()))
            }
        }
    }

    async fn revalidate_before_handshake(
        &self,
        pending: &PendingRemoteTransport,
        app: Option<tauri::AppHandle>,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        let app = app.ok_or_else(|| {
            PinnedTransportGenerationError::Missing(
                "production remote attempt has no application handle".into(),
            )
        })?;
        pending.revalidate_before_handshake(app, machine_id).await
    }
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.0.active_operations.fetch_sub(1, Ordering::SeqCst);
        self.0.operations_changed.notify_waiters();
    }
}

impl Default for HostConnectionManager {
    fn default() -> Self {
        let shutdown_submitted = Arc::new(AtomicBool::new(false));
        let manager = Self {
            observation_seq: AtomicU64::new(0),
            incarnation_seq: AtomicU64::new(0),
            event_seq: Arc::new(AtomicU64::new(0)),
            active_operations: AtomicU64::new(0),
            shutdown_submitted: shutdown_submitted.clone(),
            operations_changed: Notify::new(),
            publication_barrier: Mutex::new(()),
            registry_watcher: Mutex::new(None),
            attempt_driver: Arc::new(ProductionRemoteAttemptDriver::default()),
            scheduler: Arc::new(TokioConnectionScheduler),
            open_tabs_writer: Mutex::new(None),
            #[cfg(test)]
            before_publication_delivery: Mutex::new(None),
            #[cfg(test)]
            after_publication_precheck: Mutex::new(None),
            #[cfg(test)]
            loss_recovery_for_test: Mutex::new(None),
            #[cfg(test)]
            retirement_for_test: Mutex::new(None),
            #[cfg(test)]
            rm4f_reuse_old_epoch_after_daemon_exit: Mutex::new(None),
            core: Arc::new(Mutex::new(ManagerCore {
                last_observation: BTreeMap::new(),
                shutdown: ShutdownPhase::Running,
                shutdown_submitted,
                machines: BTreeMap::new(),
                admissions: BTreeMap::new(),
                events: VecDeque::new(),
                observers: Vec::new(),
                dispatching: false,
            })),
        };
        let seq = manager.reserve_observation();
        manager.apply_initial_observation(seq, vec![MachineManagerRecord::local()]);
        manager
    }
}

impl HostConnectionManager {
    pub(crate) fn machine_observation_authority(&self, machine_id: &str) -> Option<ObservationSeq> {
        lock(&self.core).last_observation.get(machine_id).copied()
    }

    pub(crate) fn projection_snapshot(
        &self,
        machine_ids: impl IntoIterator<Item = String>,
    ) -> BTreeMap<String, ManagerProjection> {
        let core = lock(&self.core);
        machine_ids
            .into_iter()
            .map(|machine_id| {
                let projection = match core.machines.get(&machine_id) {
                    Some(gate) if gate.present => ManagerProjection::Present(gate.state.clone()),
                    Some(_) => ManagerProjection::ConfirmedRemoved,
                    None if core.last_observation.contains_key(&machine_id) => {
                        ManagerProjection::ConfirmedRemoved
                    }
                    None => ManagerProjection::NotYetObserved,
                };
                (machine_id, projection)
            })
            .collect()
    }

    /// A registered machine's connection state, as the manager projects it.
    #[cfg(test)]
    pub(crate) fn set_state_for_test(&self, machine_id: &str, state: HostConnectionState) {
        let mut core = lock(&self.core);
        if let Some(gate) = core.machines.get_mut(machine_id) {
            gate.state = state;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_failure_for_test(&self, machine_id: &str, error: &str) {
        if let Some(gate) = lock(&self.core).machines.get_mut(machine_id) {
            gate.state = HostConnectionState::Unreachable {
                last_error: error.into(),
            };
        }
    }

    fn retire_connected(gate: &mut IntentGate) {
        gate.connected_generation = None;
        gate.resume_identity = None;
        gate.consecutive_heartbeat_misses = 0;
        gate.retry_attempt = 0;
        gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        if let Some(client) = gate.connected.take() {
            let (closed, observation) = tokio::sync::watch::channel(false);
            gate.quarantine = Some(observation);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    client.close_awaited().await;
                    let _ = closed.send(true);
                });
            }
        }
    }

    async fn await_quarantine(&self, machine_id: &str) {
        let mut quarantine = {
            let core = lock(&self.core);
            core.machines
                .get(machine_id)
                .and_then(|gate| gate.quarantine.clone())
        };
        if let Some(observation) = &mut quarantine {
            while !*observation.borrow() {
                if observation.changed().await.is_err() {
                    log::error!(target: "pty", "remote close quarantine ended without true Closed");
                    std::future::pending::<()>().await;
                }
            }
            let mut core = lock(&self.core);
            if let Some(gate) = core.machines.get_mut(machine_id) {
                if gate
                    .quarantine
                    .as_ref()
                    .is_some_and(|current| *current.borrow())
                {
                    gate.quarantine = None;
                }
            }
        }
    }
    /// A manager whose every SSH open is refused, so a test can drive the
    /// connect path without touching a real machine.
    #[cfg(test)]
    pub(crate) fn refusing_ssh() -> Self {
        Self::with_attempt_driver(Arc::new(tests::ImmediateFailDriver))
    }
    #[cfg(test)]
    fn with_attempt_driver(driver: Arc<dyn RemoteAttemptDriver>) -> Self {
        Self {
            attempt_driver: driver,
            ..Self::default()
        }
    }
    #[cfg(test)]
    fn with_attempt_driver_and_scheduler(
        driver: Arc<dyn RemoteAttemptDriver>,
        scheduler: Arc<dyn ConnectionScheduler>,
    ) -> Self {
        Self {
            attempt_driver: driver,
            scheduler,
            ..Self::default()
        }
    }
    pub fn reserve_observation(&self) -> ObservationSeq {
        ObservationSeq(self.observation_seq.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Whether `machine_id` is a live member that connect and retry can act on.
    pub(crate) fn is_member(&self, machine_id: &str) -> bool {
        lock(&self.core)
            .machines
            .get(machine_id)
            .is_some_and(|gate| gate.present)
    }

    /// Watch the registry under `root`. The root is per profile, so a profile
    /// change replaces the watch rather than keeping the old one.
    pub(crate) fn watch_registry(
        &self,
        app: tauri::AppHandle,
        root: &std::path::Path,
    ) -> Result<(), String> {
        if lock(&self.registry_watcher)
            .as_ref()
            .is_some_and(|(watched, _)| watched == root)
        {
            return Ok(());
        }
        let callback_app = app.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: Result<notify::Event, notify::Error>| {
                let Ok(event) = event else {
                    return;
                };
                let Some(observation) = adapt_registry_event(&event) else {
                    return;
                };
                let manager = callback_app.state::<HostConnectionManager>();
                let seq = manager.reserve_observation();
                let observation_app = callback_app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = machines::observe_external_registry(
                        observation_app,
                        seq,
                        observation.hinted_ids,
                        observation.broad,
                        observation.event_revisions,
                    )
                    .await
                    {
                        log::warn!(target: "machines", "refresh connection registry: {error}");
                    }
                });
            },
            Config::default(),
        )
        .map_err(|error| format!("create machine registry watcher: {error}"))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| format!("watch machine registry {root:?}: {error}"))?;
        *lock(&self.registry_watcher) = Some((root.to_path_buf(), watcher));
        Ok(())
    }

    pub(crate) async fn observe_mutation(
        &self,
        seq: ObservationSeq,
        machine_id: String,
        record: Option<MachineManagerRecord>,
    ) {
        let submitted = {
            let core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                None
            } else {
                core.machines.get(&machine_id).and_then(|gate| {
                    record
                        .as_ref()
                        .is_none_or(|record| record.fingerprint != gate.fingerprint)
                        .then(|| gate.submitted.clone())
                })
            }
        };
        if let Some(submitted) = &submitted {
            submitted.removal.fetch_add(1, Ordering::SeqCst);
        }
        let publication = lock(&self.publication_barrier);
        let serial = {
            let mut core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                return;
            }
            let serial = core
                .machines
                .get(&machine_id)
                .map(|gate| gate.serial.clone());
            if let Some(gate) = core.machines.get_mut(&machine_id) {
                if let Some(active) = gate.active.take() {
                    active.token.cancel();
                }
            }
            if submitted.is_some() {
                self.enqueue_locked(&mut core, &machine_id);
            }
            serial
        };
        drop(publication);
        self.dispatch();
        let _quarantine = match serial {
            Some(serial) => Some(serial.lock_owned().await),
            None => None,
        };
        let retired = {
            let mut core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                return;
            }
            core.last_observation.insert(machine_id.clone(), seq);
            if let Some(gate) = core.machines.get_mut(&machine_id) {
                gate.handled_removal = gate.submitted.removal.load(Ordering::SeqCst);
            }
            let mut retired = false;
            match record {
                Some(record) => match core.machines.get_mut(&machine_id) {
                    Some(gate) if gate.present => {
                        let changed = gate.fingerprint != record.fingerprint;
                        let enables_auto = !gate.auto_connect
                            && (record.auto_connect || record.open_tabs)
                            && record.set_up;
                        if changed {
                            Self::retire_connected(gate);
                            gate.last_layout = None;
                            gate.incompatible_snapshot = None;
                            retired = true;
                            gate.state = HostConnectionState::Disconnected;
                        }
                        gate.scope = record.scope.clone();
                        gate.fingerprint = record.fingerprint;
                        gate.auto_connect =
                            (record.auto_connect || record.open_tabs) && record.set_up;
                        gate.open_tabs = record.open_tabs;
                        gate.pending_self_write = Some(PendingSelfWrite {
                            seq,
                            revision: record.revision,
                        });
                        // Tabs opening on a live connection is no reason to
                        // connect again; only a newly enabled auto-connect is.
                        gate.auto_pending |= enables_auto && !record.open_tabs;
                        if enables_auto {
                            gate.auto_suppressed = false;
                        }
                    }
                    Some(gate) => {
                        Self::retire_connected(gate);
                        gate.last_layout = None;
                        gate.incompatible_snapshot = None;
                        retired = true;
                        gate.present = true;
                        gate.incarnation = self.next_incarnation();
                        gate.scope = record.scope.clone();
                        gate.fingerprint = record.fingerprint;
                        gate.auto_connect =
                            (record.auto_connect || record.open_tabs) && record.set_up;
                        gate.open_tabs = record.open_tabs;
                        gate.auto_suppressed = false;
                        gate.auto_pending = gate.auto_connect;
                        gate.pending_self_write = Some(PendingSelfWrite {
                            seq,
                            revision: record.revision,
                        });
                        gate.state = HostConnectionState::Disconnected;
                    }
                    None => {
                        core.machines.insert(machine_id.clone(), {
                            let revision = record.revision.clone();
                            let mut gate = Self::new_gate(self.next_incarnation(), record);
                            gate.pending_self_write = Some(PendingSelfWrite { seq, revision });
                            gate
                        });
                    }
                },
                None => {
                    if let Some(gate) = core.machines.get_mut(&machine_id) {
                        Self::retire_connected(gate);
                        gate.last_layout = None;
                        gate.incompatible_snapshot = None;
                        retired = true;
                        gate.present = false;
                        gate.foreground_demand = false;
                        gate.auto_pending = false;
                        gate.pending_self_write = None;
                        if let Some(active) = gate.active.take() {
                            active.token.cancel();
                        }
                        gate.state = HostConnectionState::Disconnected;
                    }
                }
            }
            self.enqueue_locked(&mut core, &machine_id);
            retired
        };
        self.dispatch();
        if retired {
            self.await_quarantine(&machine_id).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn observe_external(
        &self,
        seq: ObservationSeq,
        records: Vec<MachineManagerRecord>,
        hinted_ids: std::collections::BTreeSet<String>,
        broad: bool,
        event_revisions: Vec<super::machines::registry::FileRevision>,
    ) {
        self.observe_external_keeping(
            seq,
            records,
            hinted_ids,
            broad,
            event_revisions,
            &std::collections::BTreeSet::new(),
        )
        .await;
    }

    /// [`Self::observe_external`], leaving the machines in `unreadable` (a
    /// declaration file present but unreadable) as they are: an unreadable
    /// file is not a removal.
    pub(crate) async fn observe_external_keeping(
        &self,
        seq: ObservationSeq,
        records: Vec<MachineManagerRecord>,
        hinted_ids: std::collections::BTreeSet<String>,
        broad: bool,
        event_revisions: Vec<super::machines::registry::FileRevision>,
        unreadable: &std::collections::BTreeSet<String>,
    ) {
        let incoming: BTreeMap<_, _> = records
            .into_iter()
            .map(|record| (record.machine_id.clone(), record))
            .collect();
        let affected = {
            let core = lock(&self.core);
            let mut affected = hinted_ids;
            for (id, record) in &incoming {
                if id != LOCAL_MACHINE_ID
                    && core.machines.get(id).map(|gate| gate.fingerprint.as_str())
                        != Some(record.fingerprint.as_str())
                {
                    affected.insert(id.clone());
                }
            }
            if broad {
                for (id, gate) in &core.machines {
                    if id != LOCAL_MACHINE_ID && gate.present && !incoming.contains_key(id) {
                        affected.insert(id.clone());
                    }
                }
            }
            affected.retain(|id| incoming.contains_key(id) || !unreadable.contains(id));
            affected
        };
        for id in affected {
            self.observe_external_id(
                seq,
                id.clone(),
                incoming.get(&id).cloned(),
                &event_revisions,
            )
            .await;
        }
    }

    fn next_incarnation(&self) -> ActorIncarnation {
        ActorIncarnation(self.incarnation_seq.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn apply_initial_observation(
        &self,
        seq: ObservationSeq,
        mut records: Vec<MachineManagerRecord>,
    ) {
        if !records
            .iter()
            .any(|record| record.machine_id == LOCAL_MACHINE_ID)
        {
            records.push(MachineManagerRecord::local());
        }
        let incoming: BTreeMap<_, _> = records
            .into_iter()
            .map(|record| (record.machine_id.clone(), record))
            .collect();
        let mut core = lock(&self.core);
        for id in incoming.keys() {
            core.last_observation.insert(id.clone(), seq);
        }

        for (id, gate) in &mut core.machines {
            if !incoming.contains_key(id) && gate.present {
                gate.last_layout = None;
                gate.incompatible_snapshot = None;
                gate.present = false;
                gate.foreground_demand = false;
                if let Some(active) = gate.active.take() {
                    active.token.cancel();
                }
                gate.state = HostConnectionState::Disconnected;
            }
        }
        for (id, record) in incoming {
            match core.machines.get_mut(&id) {
                Some(gate) if gate.present => {
                    if gate.fingerprint != record.fingerprint {
                        gate.last_layout = None;
                        gate.incompatible_snapshot = None;
                    }
                    gate.scope = record.scope.clone();
                    gate.fingerprint = record.fingerprint;
                    gate.auto_connect = (record.auto_connect || record.open_tabs) && record.set_up;
                    gate.open_tabs = record.open_tabs;
                    gate.auto_pending = gate.auto_connect;
                    gate.pending_self_write = None;
                }
                Some(gate) => {
                    Self::retire_connected(gate);
                    gate.present = true;
                    gate.incarnation = self.next_incarnation();
                    gate.scope = record.scope.clone();
                    gate.fingerprint = record.fingerprint;
                    gate.auto_connect = (record.auto_connect || record.open_tabs) && record.set_up;
                    gate.open_tabs = record.open_tabs;
                    gate.auto_suppressed = false;
                    gate.auto_pending = gate.auto_connect;
                    gate.pending_self_write = None;
                    gate.state = HostConnectionState::Disconnected;
                }
                None => {
                    core.machines
                        .insert(id, Self::new_gate(self.next_incarnation(), record));
                }
            }
        }
    }

    async fn observe_external_id(
        &self,
        seq: ObservationSeq,
        machine_id: String,
        record: Option<MachineManagerRecord>,
        event_revisions: &[super::machines::registry::FileRevision],
    ) {
        if machine_id == LOCAL_MACHINE_ID {
            return;
        }
        let causal_self_echo = {
            let core = lock(&self.core);
            core.machines.get(&machine_id).is_some_and(|gate| {
                let Some(expected) = gate.pending_self_write.as_ref() else {
                    return false;
                };
                let same_snapshot = record.as_ref().and_then(|record| record.revision.as_ref())
                    == expected.revision.as_ref();
                same_snapshot
                    && event_revisions.iter().any(|observed| {
                        expected.revision.as_ref().is_some_and(|revision| {
                            revision.declaration.as_ref() == Some(observed)
                                || revision.bindings.as_ref() == Some(observed)
                        })
                    })
            })
        };
        if causal_self_echo {
            let mut core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_none_or(|last| seq > *last)
            {
                core.last_observation.insert(machine_id.clone(), seq);
                if let Some(gate) = core.machines.get_mut(&machine_id) {
                    gate.pending_self_write = None;
                }
            }
            return;
        }
        let submitted = {
            let core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                None
            } else {
                core.machines
                    .get(&machine_id)
                    .map(|gate| gate.submitted.clone())
            }
        };
        if let Some(submitted) = &submitted {
            submitted.removal.fetch_add(1, Ordering::SeqCst);
        }
        let publication = lock(&self.publication_barrier);
        let serial = {
            let mut core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                return;
            }
            let serial = core
                .machines
                .get(&machine_id)
                .map(|gate| gate.serial.clone());
            if let Some(gate) = core.machines.get_mut(&machine_id) {
                if let Some(active) = gate.active.take() {
                    active.token.cancel();
                }
            }
            if submitted.is_some() {
                self.enqueue_locked(&mut core, &machine_id);
            }
            serial
        };
        drop(publication);
        self.dispatch();
        let _quarantine = match serial {
            Some(serial) => Some(serial.lock_owned().await),
            None => None,
        };
        let retired = {
            let mut core = lock(&self.core);
            if core
                .last_observation
                .get(&machine_id)
                .is_some_and(|last| seq <= *last)
            {
                return;
            }
            core.last_observation.insert(machine_id.clone(), seq);
            if let Some(gate) = core.machines.get_mut(&machine_id) {
                gate.handled_removal = gate.submitted.removal.load(Ordering::SeqCst);
            }
            let mut retired = false;
            match record {
                Some(record) => match core.machines.get_mut(&machine_id) {
                    Some(gate) => {
                        Self::retire_connected(gate);
                        gate.last_layout = None;
                        gate.incompatible_snapshot = None;
                        retired = true;
                        gate.present = true;
                        gate.incarnation = self.next_incarnation();
                        gate.scope = record.scope.clone();
                        gate.fingerprint = record.fingerprint;
                        gate.auto_connect =
                            (record.auto_connect || record.open_tabs) && record.set_up;
                        gate.open_tabs = record.open_tabs;
                        gate.auto_pending = gate.auto_connect;
                        gate.pending_self_write = None;
                        gate.state = HostConnectionState::Disconnected;
                    }
                    None => {
                        core.machines.insert(
                            machine_id.clone(),
                            Self::new_gate(self.next_incarnation(), record),
                        );
                    }
                },
                None => {
                    if let Some(gate) = core.machines.get_mut(&machine_id) {
                        Self::retire_connected(gate);
                        gate.last_layout = None;
                        gate.incompatible_snapshot = None;
                        retired = true;
                        gate.present = false;
                        gate.foreground_demand = false;
                        gate.auto_pending = false;
                        gate.pending_self_write = None;
                        gate.state = HostConnectionState::Disconnected;
                    }
                }
            }
            self.enqueue_locked(&mut core, &machine_id);
            retired
        };
        self.dispatch();
        if retired {
            self.await_quarantine(&machine_id).await;
        }
    }

    fn new_gate(incarnation: ActorIncarnation, record: MachineManagerRecord) -> IntentGate {
        IntentGate {
            present: true,
            scope: record.scope,
            incarnation,
            fingerprint: record.fingerprint,
            disconnect_epoch: 0,
            retry_epoch: 0,
            foreground_demand: false,
            auto_connect: (record.auto_connect || record.open_tabs) && record.set_up,
            auto_suppressed: false,
            auto_pending: (record.auto_connect || record.open_tabs) && record.set_up,
            pending_self_write: None,
            active: None,
            connected: None,
            quarantine: None,
            serial: Arc::new(tokio::sync::Mutex::new(())),
            submitted: Arc::new(SubmittedIntents::default()),
            handled_disconnect: 0,
            handled_retry: 0,
            handled_removal: 0,
            state: HostConnectionState::Disconnected,
            retry_attempt: 0,
            retry_ticket: 0,
            consecutive_heartbeat_misses: 0,
            connected_generation: None,
            remote_ledger: RemoteSessionLedger::default(),
            retirement_reserved: None,
            resume_identity: None,
            last_layout: None,
            private_lease: None,
            incompatible_snapshot: None,
            frozen_generation: None,
            replacement: None,
            open_tabs: record.open_tabs,
            tabbed_workspaces: BTreeSet::new(),
        }
    }

    /// Where a flipped "had open tabs" bit is persisted.
    pub(crate) fn set_open_tabs_writer(&self, writer: OpenTabsWriter) {
        *lock(&self.open_tabs_writer) = Some(writer);
    }

    /// One workspace of a connected remote machine's host layout changed:
    /// `tabs` is how many it now holds.
    pub(crate) fn note_remote_workspace_tabs(
        &self,
        machine_id: &str,
        workspace: &str,
        tabs: usize,
    ) {
        self.note_remote_tabs(machine_id, |set| {
            if tabs == 0 {
                set.remove(workspace);
            } else {
                set.insert(workspace.to_string());
            }
        });
    }

    /// A connected remote machine's whole host layout, as a snapshot read it.
    pub(crate) fn note_remote_layout(&self, machine_id: &str, layout: &LayoutSnapshot) {
        let tabbed: BTreeSet<String> = layout
            .workspaces
            .iter()
            .filter(|(_, ws)| !ws.tabs.is_empty())
            .map(|(key, _)| key.as_str().to_string())
            .collect();
        self.note_remote_tabs(machine_id, |set| *set = tabbed);
    }

    fn note_remote_tabs(&self, machine_id: &str, update: impl FnOnce(&mut BTreeSet<String>)) {
        let flipped = {
            let mut core = lock(&self.core);
            let Some(gate) = core
                .machines
                .get_mut(machine_id)
                .filter(|gate| gate.present && machine_id != LOCAL_MACHINE_ID)
            else {
                return;
            };
            update(&mut gate.tabbed_workspaces);
            let has = !gate.tabbed_workspaces.is_empty();
            (has != gate.open_tabs).then(|| {
                gate.open_tabs = has;
                has
            })
        };
        let writer = lock(&self.open_tabs_writer).clone();
        if let (Some(has), Some(writer)) = (flipped, writer) {
            writer(machine_id.to_string(), has);
        }
    }

    pub fn subscribe(&self, observer: Observer) {
        lock(&self.core).observers.push(observer);
    }

    fn dispatch(&self) {
        Self::dispatch_core(&self.core);
    }

    fn dispatch_core(core_owner: &Arc<Mutex<ManagerCore>>) {
        {
            let mut core = lock(core_owner);
            if core.dispatching {
                return;
            }
            core.dispatching = true;
        }
        loop {
            let (event, observers) = {
                let mut core = lock(core_owner);
                let Some(event) = core.events.pop_front() else {
                    core.dispatching = false;
                    return;
                };
                (event, core.observers.clone())
            };
            for observer in observers {
                observer(event.clone());
            }
        }
    }

    fn enqueue_locked(&self, core: &mut ManagerCore, machine_id: &str) {
        Self::enqueue_core(core, &self.event_seq, machine_id);
    }

    fn usable_gate(gate: &IntentGate) -> bool {
        gate.present
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && matches!(gate.state, HostConnectionState::Connected { .. })
            && gate
                .connected
                .as_ref()
                .is_some_and(|client| client.domain_client().is_alive())
    }

    fn admission_state(core: &ManagerCore, machine_id: &str) -> Option<RuntimeAdmissionState> {
        if machine_id == LOCAL_MACHINE_ID {
            return None;
        }
        let gate = core.machines.get(machine_id)?;
        if !gate.present {
            return Some(RuntimeAdmissionState::Removed);
        }
        if Self::private_epoch(core, machine_id).is_some()
            && gate
                .private_lease
                .as_ref()
                .is_none_or(|lease| lease.validate().is_err())
        {
            return Some(RuntimeAdmissionState::Unavailable);
        }
        if core.shutdown == ShutdownPhase::Running
            && !core.shutdown_submitted.load(Ordering::SeqCst)
            && Self::usable_gate(gate)
        {
            let client = gate.connected.as_ref()?.domain_client();
            return Some(RuntimeAdmissionState::Live {
                namespace: client.namespace().into(),
            });
        }
        let incompatible = gate
            .incompatible_snapshot
            .as_ref()
            .and_then(|frozen| frozen.layout.as_ref());
        let snapshot = incompatible.or(gate.last_layout.as_ref());
        Some(match snapshot {
            Some(snapshot) => {
                let previous = core
                    .admissions
                    .get(machine_id)
                    .and_then(|row| match &row.state {
                        RuntimeAdmissionState::Live { namespace } => Some(namespace.clone()),
                        RuntimeAdmissionState::Historical { historical } => {
                            historical.last_namespace.clone()
                        }
                        _ => None,
                    })
                    .filter(|namespace| {
                        namespace.server_id == snapshot.server_id.as_str()
                            && namespace.epoch == snapshot.epoch
                    });
                RuntimeAdmissionState::Historical {
                    historical: Box::new(FrozenRuntimeLayout {
                        last_namespace: previous,
                        server_id: snapshot.server_id.as_str().into(),
                        epoch: snapshot.epoch.clone(),
                        snapshot: snapshot.clone(),
                        reason: if incompatible.is_some() {
                            RuntimeHistoryReason::Incompatible
                        } else {
                            RuntimeHistoryReason::Unreachable
                        },
                    }),
                }
            }
            None => RuntimeAdmissionState::Unavailable,
        })
    }

    fn private_epoch(core: &ManagerCore, machine_id: &str) -> Option<String> {
        core.machines
            .get(machine_id)
            .and_then(|gate| gate.private_lease.as_ref())
            .map(|lease| lease.epoch())
            .or_else(|| {
                core.admissions
                    .get(machine_id)?
                    .private_authority_epoch
                    .clone()
            })
    }

    fn enqueue_core(core: &mut ManagerCore, sequence: &AtomicU64, machine_id: &str) {
        let order = match sequence.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        }) {
            Ok(previous) => previous + 1,
            Err(_) => {
                core.shutdown = ShutdownPhase::Draining;
                return;
            }
        };
        let admission = Self::admission_state(core, machine_id).and_then(|state| {
            if core.admissions.get(machine_id).is_some_and(|previous| {
                previous.state == state
                    && previous.private_authority_epoch == Self::private_epoch(core, machine_id)
            }) {
                // The connection event may advance without an admission change.
                // Do not replay a tombstone (or live row) as a fresh feed delta.
                return None;
            }
            let row = RuntimeAdmissionRow {
                private_authority_epoch: Self::private_epoch(core, machine_id),
                member: RuntimeMemberAddress::remote(machine_id),
                revision: RuntimeCounter::new(order),
                state,
            };
            core.admissions.insert(machine_id.into(), row.clone());
            Some(row)
        });
        let Some(gate) = core.machines.get(machine_id) else {
            return;
        };
        core.events.push_back(ManagerEvent {
            order,
            machine_id: machine_id.into(),
            incarnation: gate.incarnation,
            generation: gate
                .active
                .as_ref()
                .map(|attempt| attempt.generation)
                .or(gate.connected_generation),
            retry_attempt: gate.retry_attempt,
            consecutive_heartbeat_misses: gate.consecutive_heartbeat_misses,
            state: gate.state.clone(),
            admission,
            scope: gate.scope.clone(),
        });
    }

    pub(crate) fn admission_baseline(&self) -> RuntimeAdmissionBaseline {
        self.admission_baseline_with_private_leases().0
    }

    pub(crate) fn admission_baseline_with_private_leases(
        &self,
    ) -> (
        RuntimeAdmissionBaseline,
        BTreeMap<String, super::auth::private_authority::PrivateAuthorityLease>,
    ) {
        let (rows, leases) = {
            let mut core = lock(&self.core);
            let changed: Vec<_> = core
                .machines
                .keys()
                .filter(|id| {
                    Self::admission_state(&core, id).is_some_and(|state| {
                        core.admissions.get(*id).is_none_or(|row| {
                            row.state != state
                                || row.private_authority_epoch != Self::private_epoch(&core, id)
                        })
                    })
                })
                .cloned()
                .collect();
            for id in changed {
                self.enqueue_locked(&mut core, &id);
            }
            let leases = core
                .machines
                .iter()
                .filter_map(|(id, gate)| Some((id.clone(), gate.private_lease.clone()?)))
                .collect();
            (core.admissions.values().cloned().collect(), leases)
        };
        self.dispatch();
        (RuntimeAdmissionBaseline { rows }, leases)
    }

    pub(crate) fn target_for_namespace(
        &self,
        namespace: &EventNamespace,
    ) -> Option<RuntimeMemberTarget> {
        let core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running || self.shutdown_submitted.load(Ordering::SeqCst)
        {
            return None;
        }
        core.admissions.values().find_map(|row| match &row.state {
            RuntimeAdmissionState::Live { namespace: current }
                if current == &namespace.into()
                    && core
                        .machines
                        .get(&row.member.machine_id)
                        .is_some_and(Self::usable_gate) =>
            {
                Some(RuntimeMemberTarget {
                    member: row.member.clone(),
                    admission_revision: row.revision,
                    namespace: current.clone(),
                })
            }
            _ => None,
        })
    }

    pub(crate) fn capture_admitted_backend(
        &self,
        target: &RuntimeMemberTarget,
    ) -> Result<ConnectedBackendSnapshot, ConnectedBackendError> {
        let backend = self.connected_backend(&target.member.machine_id)?;
        let core = lock(&self.core);
        if !core
            .admissions
            .get(&target.member.machine_id)
            .is_some_and(|row| row.matches(target))
        {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        drop(core);
        self.revalidate_backend(&backend)?;
        Ok(backend)
    }

    fn begin_attempt(
        &self,
        machine_id: &str,
        origin: ConnectOrigin,
    ) -> Result<StartedAttempt, String> {
        let submitted = {
            let core = lock(&self.core);
            core.machines
                .get(machine_id)
                .map(|gate| gate.submitted.clone())
                .ok_or_else(|| format!("machine {machine_id:?} is not registered"))?
        };
        if origin == ConnectOrigin::Retry {
            submitted.retry.fetch_add(1, Ordering::SeqCst);
        }
        let _publication = lock(&self.publication_barrier);
        if self.shutdown_submitted.load(Ordering::SeqCst) {
            return Err("application is shutting down".into());
        }
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running {
            return Err("application is shutting down".into());
        }
        let gate = core
            .machines
            .get_mut(machine_id)
            .filter(|gate| gate.present)
            .ok_or_else(|| format!("machine {machine_id:?} is not registered"))?;
        if matches!(
            gate.state,
            HostConnectionState::Incompatible { .. }
                | HostConnectionState::UnsupportedLegacy { .. }
        ) && origin != ConnectOrigin::Restart
        {
            return Err("terminal server replacement is required before reconnecting".into());
        }
        if origin == ConnectOrigin::Retry
            && (submitted.disconnect.load(Ordering::SeqCst) > gate.handled_disconnect
                || submitted.removal.load(Ordering::SeqCst) > gate.handled_removal)
        {
            return Err("disconnect has priority over retry".into());
        }
        if origin == ConnectOrigin::Retry {
            gate.handled_retry = submitted.retry.load(Ordering::SeqCst);
            gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        }
        if origin == ConnectOrigin::Auto && !gate.auto_connect {
            return Err("background connection is not enabled".into());
        }
        if origin == ConnectOrigin::Recovery
            && !(gate.foreground_demand || (gate.auto_connect && !gate.auto_suppressed))
        {
            return Err("connection demand was withdrawn".into());
        }
        if origin == ConnectOrigin::Auto {
            gate.auto_pending = false;
        }
        if matches!(
            origin,
            ConnectOrigin::EnrollmentForeground
                | ConnectOrigin::ExplicitForeground
                | ConnectOrigin::Retry
                | ConnectOrigin::Restart
        ) {
            gate.foreground_demand = true;
            gate.auto_suppressed = false;
        }
        if let Some(active) = gate.active.take() {
            active.token.cancel();
        }
        let generation = AttemptGeneration(gate.retry_epoch + gate.disconnect_epoch + 1);
        gate.retry_epoch = gate.retry_epoch.saturating_add(1);
        let token = CancellationToken::new();
        if origin == ConnectOrigin::Restart {
            if let Some(reservation) = &gate.replacement {
                lock(&reservation.progress).attempt = Some(generation);
            }
        } else {
            gate.replacement = None;
        }
        gate.active = Some(ActiveAttempt {
            generation,
            token: token.clone(),
        });
        gate.retirement_reserved = None;
        gate.state = HostConnectionState::Connecting;
        let incarnation = gate.incarnation;
        let serial = gate.serial.clone();
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
        Ok((incarnation, generation, token, serial))
    }

    fn complete_fail_closed(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        error: String,
    ) -> bool {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let no_higher_priority_submission = !self.shutdown_submitted.load(Ordering::SeqCst)
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry;
        let current = running
            && no_higher_priority_submission
            && gate.present
            && gate.incarnation == incarnation
            && gate.active.as_ref().is_some_and(|attempt| {
                attempt.generation == generation && !attempt.token.is_cancelled()
            });
        if !current {
            return false;
        }
        gate.state = HostConnectionState::Unreachable { last_error: error };
        self.enqueue_locked(&mut core, machine_id);
        if let Some(gate) = core.machines.get_mut(machine_id) {
            gate.active = None;
            gate.connected_generation = None;
        }
        drop(core);
        self.dispatch();
        true
    }

    fn retry_plan(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
    ) -> Option<(Duration, u32, u64)> {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let gate = core.machines.get_mut(machine_id)?;
        let demanded = gate.foreground_demand || (gate.auto_connect && !gate.auto_suppressed);
        if !running
            || !gate.present
            || gate.incarnation != incarnation
            || !demanded
            || gate.active.is_some()
            || gate.connected.is_some()
            || matches!(
                gate.state,
                HostConnectionState::Incompatible { .. }
                    | HostConnectionState::UnsupportedLegacy { .. }
            )
        {
            return None;
        }
        let attempt = gate.retry_attempt;
        gate.retry_attempt = gate.retry_attempt.saturating_add(1);
        gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        Some((
            retry_backoff(machine_id, attempt),
            attempt,
            gate.retry_ticket,
        ))
    }

    fn complete_incompatible(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        host: super::host_connection::IncompatibleRemoteHost,
        proof: Option<machines::LegacyHostProof>,
    ) -> bool {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let current = running
            && !self.shutdown_submitted.load(Ordering::SeqCst)
            && gate.present
            && gate.incarnation == incarnation
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
            && gate.active.as_ref().is_some_and(|attempt| {
                attempt.generation == generation && !attempt.token.is_cancelled()
            });
        if !current {
            return false;
        }
        let n_sessions = host.census.live_sessions;
        let replacement_supported = host
            .hello
            .capabilities
            .contains(&codesense_host_proto::Capability::ShutdownForReplacement);
        gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
            layout: gate.last_layout.clone(),
            census: CensusSnapshot {
                live_sessions: n_sessions,
            },
            server_id: host.hello.server_id,
            epoch: host.hello.epoch,
            rev: host.hello.rev,
            shutdown_for_replacement: replacement_supported,
            host_pid: host.hello.pid,
        });
        gate.state = HostConnectionState::Incompatible {
            n_sessions,
            proof: replacement_supported.then_some(proof).flatten(),
        };
        gate.frozen_generation = Some(generation);
        gate.active = None;
        gate.connected = None;
        gate.connected_generation = None;
        gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
        true
    }

    fn complete_unsupported_legacy(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        proof: machines::LegacyHostProof,
    ) -> bool {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let current = running
            && !self.shutdown_submitted.load(Ordering::SeqCst)
            && gate.present
            && gate.incarnation == incarnation
            && gate.active.as_ref().is_some_and(|attempt| {
                attempt.generation == generation && !attempt.token.is_cancelled()
            })
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry;
        if !current {
            return false;
        }
        gate.state = HostConnectionState::UnsupportedLegacy {
            count: LegacySessionCount::Unknown,
            proof,
        };
        gate.frozen_generation = Some(generation);
        gate.active = None;
        gate.connected = None;
        gate.connected_generation = None;
        gate.incompatible_snapshot = None;
        gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
        true
    }

    fn fail_current(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        error: String,
    ) -> bool {
        let completed = self.complete_fail_closed(machine_id, incarnation, generation, error);
        if completed {
            if let Some(app) = app {
                self.schedule_retry(app, machine_id.to_string(), incarnation);
            }
        }
        completed
    }

    fn schedule_retry(
        &self,
        app: tauri::AppHandle,
        machine_id: String,
        incarnation: ActorIncarnation,
    ) {
        let Some((delay, _attempt, ticket)) = self.retry_plan(&machine_id, incarnation) else {
            return;
        };
        let scheduler = self.scheduler.clone();
        tauri::async_runtime::spawn(async move {
            scheduler.sleep(delay).await;
            let manager = app.state::<HostConnectionManager>();
            let still_current = {
                let core = lock(&manager.core);
                core.shutdown == ShutdownPhase::Running
                    && core.machines.get(&machine_id).is_some_and(|gate| {
                        gate.present
                            && gate.incarnation == incarnation
                            && gate.active.is_none()
                            && gate.connected.is_none()
                            && gate.retry_ticket == ticket
                            && (gate.foreground_demand
                                || (gate.auto_connect && !gate.auto_suppressed))
                    })
            };
            if still_current {
                let _ = manager
                    .connect_remote(app.clone(), machine_id, ConnectOrigin::Recovery)
                    .await;
            }
        });
    }

    fn spawn_heartbeat(
        &self,
        app: tauri::AppHandle,
        machine_id: String,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
    ) {
        let scheduler = self.scheduler.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                scheduler.sleep(HEARTBEAT_CADENCE).await;
                let manager = app.state::<HostConnectionManager>();
                let client = {
                    let core = lock(&manager.core);
                    let Some(gate) = core.machines.get(&machine_id) else {
                        return;
                    };
                    if core.shutdown != ShutdownPhase::Running
                        || !gate.present
                        || gate.incarnation != incarnation
                        || gate.connected_generation != Some(generation)
                    {
                        return;
                    }
                    let Some(client) = gate.connected.as_ref() else {
                        return;
                    };
                    client.domain_client()
                };

                // LiveSessionIds is a read-only, argument-free protocol request.  It
                // goes through SessionServerClient's ordinary correlation ledger,
                // so health traffic cannot collide with a real session identifier.
                let healthy = Self::probe_heartbeat(client, scheduler.as_ref()).await;
                if manager
                    .record_heartbeat_result(&machine_id, incarnation, generation, healthy)
                    .await
                {
                    manager.schedule_retry(app.clone(), machine_id.clone(), incarnation);
                    return;
                }
            }
        });
    }

    async fn probe_heartbeat(
        client: Arc<SessionServerClient>,
        scheduler: &dyn ConnectionScheduler,
    ) -> bool {
        let request = tokio::task::spawn_blocking(move || client.heartbeat(HEARTBEAT_DEADLINE));
        Self::within_heartbeat_deadline(
            async move { request.await.is_ok_and(|reply| reply.is_ok()) },
            scheduler,
        )
        .await
    }

    async fn within_heartbeat_deadline(
        response: impl Future<Output = bool> + Send,
        scheduler: &dyn ConnectionScheduler,
    ) -> bool {
        let deadline = scheduler.sleep(HEARTBEAT_DEADLINE);
        tokio::pin!(deadline);
        tokio::pin!(response);
        tokio::select! {
            healthy = &mut response => healthy,
            _ = &mut deadline => false,
        }
    }

    /// Returns true when the second miss retired the exact connected generation.
    async fn record_heartbeat_result(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        healthy: bool,
    ) -> bool {
        let serial = {
            let mut core = lock(&self.core);
            let Some(gate) = core.machines.get_mut(machine_id) else {
                return false;
            };
            if matches!(
                gate.state,
                HostConnectionState::Replacing { .. } | HostConnectionState::Readmitting { .. }
            ) || gate.incarnation != incarnation
                || gate.connected_generation != Some(generation)
                || gate.connected.is_none()
            {
                return false;
            }
            if healthy {
                gate.consecutive_heartbeat_misses = 0;
                return false;
            }
            gate.consecutive_heartbeat_misses = gate.consecutive_heartbeat_misses.saturating_add(1);
            if gate.consecutive_heartbeat_misses < 2 {
                return false;
            }
            gate.serial.clone()
        };

        let _serial = serial.lock_owned().await;
        let client = {
            let mut core = lock(&self.core);
            let Some(gate) = core.machines.get_mut(machine_id) else {
                return false;
            };
            if matches!(
                gate.state,
                HostConnectionState::Replacing { .. } | HostConnectionState::Readmitting { .. }
            ) || gate.incarnation != incarnation
                || gate.connected_generation != Some(generation)
                || gate.consecutive_heartbeat_misses < 2
            {
                return false;
            }
            let connected = gate.connected.take();
            if let Some(client) = &connected {
                gate.resume_identity = Some(client.domain_client().namespace().clone());
            }
            connected
        };
        let Some(client) = client else {
            return false;
        };
        client.close_awaited().await;
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let no_higher_priority_submission = !self.shutdown_submitted.load(Ordering::SeqCst)
            && gate.active.is_none()
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry;
        if !running
            || !no_higher_priority_submission
            || !gate.present
            || gate.incarnation != incarnation
            || gate.connected_generation != Some(generation)
        {
            return false;
        }
        gate.state = HostConnectionState::Unreachable {
            last_error: "heartbeat response deadline missed twice".into(),
        };
        self.enqueue_locked(&mut core, machine_id);
        if let Some(gate) = core.machines.get_mut(machine_id) {
            gate.connected_generation = None;
        }
        drop(core);
        self.dispatch();
        true
    }

    fn mark_handshaking(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
    ) -> bool {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let current = running
            && gate.present
            && gate.incarnation == incarnation
            && gate.active.as_ref().is_some_and(|attempt| {
                attempt.generation == generation && !attempt.token.is_cancelled()
            });
        if !current {
            return false;
        }
        gate.state = HostConnectionState::Handshaking;
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
        true
    }

    fn adopt_current(
        &self,
        machine_id: &str,
        freshness: AdoptionFreshness,
        client: ManagedRemoteClient,
        ledger: RemoteSessionLedger,
    ) -> Result<(), ManagedRemoteClient> {
        let AdoptionFreshness {
            incarnation,
            generation,
            validated_observation,
            retirement_committed,
        } = freshness;
        let server_id = client.server_id().to_string();
        let alive = client.domain_client();
        let private_lease = alive.private_lease();
        let Some(publication) = client.publication_permit().prepare() else {
            return Err(client);
        };
        let _publication = lock(&self.publication_barrier);
        #[cfg(test)]
        if let Some(hook) = lock(&self.before_publication_delivery).clone() {
            hook();
        }
        let mut candidate = Some(client);
        let current = || {
            let core = lock(&self.core);
            let advisory = core.shutdown == ShutdownPhase::Running
                && core.machines.get(machine_id).is_some_and(|gate| {
                    gate.present
                        && gate.incarnation == incarnation
                        && gate.active.as_ref().is_some_and(|attempt| {
                            attempt.generation == generation && !attempt.token.is_cancelled()
                        })
                        && alive.is_alive()
                        && !self.shutdown_submitted.load(Ordering::SeqCst)
                        && gate.submitted.disconnect.load(Ordering::SeqCst)
                            <= gate.handled_disconnect
                        && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
                        && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
                        && (retirement_committed
                            || core.last_observation.get(machine_id).map(|seq| seq.0)
                                == Some(validated_observation))
                });
            drop(core);
            #[cfg(test)]
            if let Some(hook) = lock(&self.after_publication_precheck).clone() {
                hook();
            }
            advisory
        };
        let committed = publication.commit_and_take(current, || {
            let mut core = lock(&self.core);
            let running = core.shutdown == ShutdownPhase::Running;
            let same_machine_authority = core.last_observation.get(machine_id).map(|seq| seq.0)
                == Some(validated_observation);
            let Some(gate) = core.machines.get_mut(machine_id) else {
                return false;
            };
            let exact = running
                && gate.present
                && gate.incarnation == incarnation
                && gate.active.as_ref().is_some_and(|attempt| {
                    attempt.generation == generation && !attempt.token.is_cancelled()
                })
                && alive.is_alive()
                && !self.shutdown_submitted.load(Ordering::SeqCst)
                && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
                && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
                && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
                && (retirement_committed || same_machine_authority);
            if !exact {
                return false;
            }
            let previous_private_epoch = Self::private_epoch(&core, machine_id);
            let committed = alive
                .commit_private(|| {
                    let gate = core
                        .machines
                        .get_mut(machine_id)
                        .expect("validated machine remains under core lock");
                    gate.active = None;
                    gate.connected = candidate.take();
                    if previous_private_epoch != private_lease.as_ref().map(|lease| lease.epoch()) {
                        gate.last_layout = None;
                    }
                    gate.private_lease = private_lease;
                    gate.connected_generation = Some(generation);
                    gate.remote_ledger = ledger;
                    gate.retirement_reserved = None;
                    gate.incompatible_snapshot = None;
                    gate.frozen_generation = None;
                    if let Some(reservation) = &gate.replacement {
                        let mut progress = lock(&reservation.progress);
                        if progress.attempt == Some(generation) && progress.census.is_some() {
                            progress.admission = Some(ConnectedBackendSnapshot {
                                machine_id: machine_id.to_string(),
                                incarnation,
                                server_id: server_id.clone(),
                                epoch: alive.epoch().to_string(),
                                client: alive.clone(),
                            });
                        }
                    }
                    gate.resume_identity = None;
                    gate.retry_attempt = 0;
                    gate.retry_ticket = gate.retry_ticket.saturating_add(1);
                    gate.consecutive_heartbeat_misses = 0;
                    gate.state = HostConnectionState::Connected { server_id };
                    true
                })
                .unwrap_or(false);
            if committed {
                self.enqueue_locked(&mut core, machine_id);
            }
            committed
        });
        drop(_publication);
        let Some(committed) = committed else {
            return Err(candidate.expect("rejected publication retains candidate"));
        };
        committed.emit();
        self.dispatch();
        Ok(())
    }

    /// Reserve the changed-epoch transition while the replacement client is
    /// still publication-gated. No callbacks run under this lock.
    fn reserve_epoch_retirement(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        validated_observation: u64,
    ) -> bool {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let same_machine_authority =
            core.last_observation.get(machine_id).map(|seq| seq.0) == Some(validated_observation);
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return false;
        };
        let no_higher_priority_submission = !self.shutdown_submitted.load(Ordering::SeqCst)
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry;
        let current = running
            && no_higher_priority_submission
            && same_machine_authority
            && gate.present
            && gate.incarnation == incarnation
            && gate.active.as_ref().is_some_and(|attempt| {
                attempt.generation == generation && !attempt.token.is_cancelled()
            });
        if current {
            gate.retirement_reserved = Some(generation);
        }
        current
    }

    /// Atomically consume the one-shot right to begin irreversible retirement
    /// effects. Priority submissions linearize through their SeqCst counters;
    /// once this returns true, retirement won, and it can run callbacks after
    /// releasing the manager gate.
    fn take_epoch_retirement_permit(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
    ) -> bool {
        let mut core = lock(&self.core);
        let current = core.shutdown == ShutdownPhase::Running
            && !self.shutdown_submitted.load(Ordering::SeqCst)
            && core.machines.get(machine_id).is_some_and(|gate| {
                gate.present
                    && gate.incarnation == incarnation
                    && gate.retirement_reserved == Some(generation)
                    && gate.active.as_ref().is_some_and(|attempt| {
                        attempt.generation == generation && !attempt.token.is_cancelled()
                    })
                    && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
                    && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
                    && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
            });
        if current {
            if let Some(gate) = core.machines.get_mut(machine_id) {
                gate.retirement_reserved = None;
            }
        }
        current
    }

    fn capture_exact_loss(
        &self,
        machine_id: &str,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        expected: &Arc<SessionServerClient>,
    ) {
        let publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return;
        };
        if gate.incarnation != incarnation
            || gate.connected_generation != Some(generation)
            || !gate
                .connected
                .as_ref()
                .is_some_and(|client| Arc::ptr_eq(&client.domain_client(), expected))
        {
            return;
        }
        // Cooperative replacement owns the retiring connection's expected EOF.
        // Keep its reservation intact until dispatch completes and Check B can
        // validate the exact lifetime. Ordinary Connected loss still recovers.
        if matches!(
            gate.state,
            HostConnectionState::Replacing { .. } | HostConnectionState::Readmitting { .. }
        ) {
            return;
        }
        gate.state = HostConnectionState::Unreachable {
            last_error: "remote terminal stream ended".into(),
        };
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        drop(publication);
        self.dispatch();
    }

    async fn recover_exact_lost_client(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
        incarnation: ActorIncarnation,
        generation: AttemptGeneration,
        expected: Arc<SessionServerClient>,
    ) {
        let connected = {
            let mut core = lock(&self.core);
            let running = core.shutdown == ShutdownPhase::Running;
            let Some(gate) = core.machines.get_mut(&machine_id) else {
                return;
            };
            let exact = !matches!(
                gate.state,
                HostConnectionState::Replacing { .. } | HostConnectionState::Readmitting { .. }
            ) && running
                && !self.shutdown_submitted.load(Ordering::SeqCst)
                && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
                && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
                && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
                && gate.present
                && gate.incarnation == incarnation
                && gate.connected_generation == Some(generation)
                && gate
                    .connected
                    .as_ref()
                    .is_some_and(|client| Arc::ptr_eq(&client.domain_client(), &expected));
            if !exact {
                return;
            }
            let connected = gate.connected.take();
            if let Some(client) = &connected {
                gate.resume_identity = Some(client.domain_client().namespace().clone());
            }
            gate.connected_generation = None;
            gate.state = HostConnectionState::Unreachable {
                last_error: "remote terminal stream ended".into(),
            };
            self.enqueue_locked(&mut core, &machine_id);
            connected
        };
        self.dispatch();
        if let Some(client) = connected {
            client.close_awaited().await;
        }
        if let Some(app) = app {
            self.schedule_retry(app, machine_id, incarnation);
        }
    }

    pub async fn connect_remote(
        &self,
        app: tauri::AppHandle,
        machine_id: String,
        origin: ConnectOrigin,
    ) -> Result<(), String> {
        self.connect_remote_inner(Some(app), machine_id, origin)
            .await
    }

    async fn connect_remote_inner(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: String,
        origin: ConnectOrigin,
    ) -> Result<(), String> {
        if machine_id == LOCAL_MACHINE_ID {
            return Err("This Mac is resolved by the local connector".into());
        }
        let replacement_identity = (origin == ConnectOrigin::Restart)
            .then(|| {
                let core = lock(&self.core);
                core.machines.get(&machine_id).and_then(|gate| {
                    let server_id = match &gate.state {
                        HostConnectionState::Incompatible {
                            proof: Some(proof), ..
                        }
                        | HostConnectionState::UnsupportedLegacy { proof, .. } => {
                            proof.expected_server_id().to_string()
                        }
                        HostConnectionState::Readmitting { .. } => {
                            gate.replacement.as_ref().map(|reservation| {
                                reservation.proof.expected_server_id().to_string()
                            })?
                        }
                        _ => return None,
                    };
                    Some((
                        server_id,
                        gate.replacement
                            .as_ref()
                            .and_then(|r| r.old_epoch.clone())
                            .or_else(|| {
                                gate.incompatible_snapshot
                                    .as_ref()
                                    .map(|snapshot| snapshot.epoch.clone())
                            }),
                    ))
                })
            })
            .flatten();
        self.active_operations.fetch_add(1, Ordering::SeqCst);
        let _operation = OperationGuard(self);
        let (incarnation, generation, token, serial) = self.begin_attempt(&machine_id, origin)?;
        let _serial = serial.lock().await;
        self.await_quarantine(&machine_id).await;
        let prior = {
            let mut core = lock(&self.core);
            core.machines
                .get_mut(&machine_id)
                .filter(|gate| gate.incarnation == incarnation)
                .and_then(|gate| gate.connected.take())
        };
        let saved_identity = {
            let core = lock(&self.core);
            core.machines
                .get(&machine_id)
                .filter(|gate| gate.incarnation == incarnation)
                .and_then(|gate| gate.resume_identity.clone())
        };
        let prior_identity = prior
            .as_ref()
            .map(|client| client.domain_client().namespace().clone())
            .or(saved_identity);
        if let Some(client) = prior {
            client.close_awaited().await;
        }
        if token.is_cancelled() {
            return Err("connection attempt was superseded".into());
        }
        let deadline = self.scheduler.sleep(CONNECT_DEADLINE);
        let opened = self
            .attempt_driver
            .open_before(app.clone(), machine_id.clone(), deadline, token.clone())
            .await;
        let owner = match opened {
            Ok(owner) => owner,
            Err(error) => {
                let detail = error.to_string();
                if !matches!(
                    error,
                    machines::PinnedSshOpenError::LegacyProtocolCandidate(_)
                ) {
                    self.fail_current(
                        app.clone(),
                        &machine_id,
                        incarnation,
                        generation,
                        detail.clone(),
                    );
                    return Err(detail);
                }
                let legacy = self
                    .attempt_driver
                    .legacy_proof(app.clone(), machine_id.clone())
                    .await;
                match legacy {
                    Ok(Some(proof)) => {
                        let detail =
                            "unsupported legacy terminal protocol 1 (live session count unknown)"
                                .to_string();
                        self.complete_unsupported_legacy(
                            &machine_id,
                            incarnation,
                            generation,
                            proof,
                        );
                        return Err(detail);
                    }
                    Ok(None) => {
                        self.complete_fail_closed(
                            &machine_id,
                            incarnation,
                            generation,
                            detail.clone(),
                        );
                        return Err(detail);
                    }
                    Err(proof_error) => {
                        self.fail_current(
                            app.clone(),
                            &machine_id,
                            incarnation,
                            generation,
                            proof_error.clone(),
                        );
                        return Err(proof_error);
                    }
                }
            }
        };
        if token.is_cancelled() {
            owner.close_awaited().await;
            return Err("connection attempt was superseded".into());
        }
        let pending = owner.into_pending();
        let validated_observation = self
            .machine_observation_authority(&machine_id)
            .map(|seq| seq.0)
            .ok_or_else(|| format!("machine {machine_id:?} has not been observed"))?;
        if let Err(error) = self
            .attempt_driver
            .revalidate_before_handshake(&pending, app.clone(), machine_id.clone())
            .await
        {
            let detail = error.to_string();
            pending.close_awaited().await;
            self.fail_current(
                app.clone(),
                &machine_id,
                incarnation,
                generation,
                detail.clone(),
            );
            return Err(detail);
        }
        if !self.mark_handshaking(&machine_id, incarnation, generation) {
            pending.close_awaited().await;
            return Err("connection attempt was superseded".into());
        }
        let admitted = match self.attempt_driver.admit(pending, token.clone()).await {
            Ok(admitted) => admitted,
            Err(RemoteAdmissionError::Incompatible(host)) => {
                let detail = RemoteAdmissionError::Incompatible(host.clone()).to_string();
                let proof = match self
                    .attempt_driver
                    .legacy_proof(app.clone(), machine_id.clone())
                    .await
                {
                    Ok(proof) => proof,
                    Err(error) => {
                        log::warn!(target: "machines", "frozen daemon has no replacement proof: {error}");
                        None
                    }
                };
                self.complete_incompatible(&machine_id, incarnation, generation, *host, proof);
                return Err(detail);
            }
            Err(error) => {
                let detail = error.to_string();
                self.fail_current(
                    app.clone(),
                    &machine_id,
                    incarnation,
                    generation,
                    detail.clone(),
                );
                return Err(detail);
            }
        };
        let publication_generation = admitted.publication_generation();
        if origin == ConnectOrigin::Restart {
            let denied = replacement_identity
                .as_ref()
                .ok_or("replacement lost its frozen server identity before publication")
                .and_then(|(expected, old_epoch)| {
                    validate_replacement_admission(
                        expected,
                        old_epoch.as_deref(),
                        admitted.verified_server_id(),
                        admitted.verified_epoch(),
                        admitted.verified_census(),
                    )
                });
            if let Err(detail) = denied {
                admitted.close_awaited().await;
                self.fail_current(
                    app.clone(),
                    &machine_id,
                    incarnation,
                    generation,
                    detail.into(),
                );
                return Err(detail.into());
            }
        }
        if origin == ConnectOrigin::Restart {
            let core = lock(&self.core);
            if let Some(reservation) = core
                .machines
                .get(&machine_id)
                .and_then(|gate| gate.replacement.as_ref())
            {
                let mut progress = lock(&reservation.progress);
                if progress.attempt == Some(generation) {
                    progress.census = Some(admitted.verified_census().live_sessions);
                }
            }
        }
        if let Err(error) = self
            .attempt_driver
            .revalidate_before_publication(&admitted, app.clone(), machine_id.clone())
            .await
        {
            let detail = error.to_string();
            admitted.close_awaited().await;
            self.fail_current(
                app.clone(),
                &machine_id,
                incarnation,
                generation,
                detail.clone(),
            );
            return Err(detail);
        }
        if token.is_cancelled() {
            admitted.close_awaited().await;
            return Err("connection attempt was superseded".into());
        }
        let candidate_identity = (
            admitted.verified_server_id().to_string(),
            admitted.verified_epoch().to_string(),
        );
        #[cfg(test)]
        let candidate_identity =
            self.rm4f_candidate_identity(&machine_id, prior_identity.as_ref(), candidate_identity);
        let same_lifetime = prior_identity.as_ref().is_some_and(|identity| {
            identity.server_id == candidate_identity.0 && identity.epoch == candidate_identity.1
        });
        let (candidate_ledger, retired_ledger) = {
            let core = lock(&self.core);
            let ledger = core
                .machines
                .get(&machine_id)
                .filter(|gate| gate.incarnation == incarnation)
                .map(|gate| gate.remote_ledger.clone())
                .unwrap_or_default();
            if same_lifetime {
                (ledger, None)
            } else {
                (
                    RemoteSessionLedger::default(),
                    prior_identity.as_ref().map(|_| ledger),
                )
            }
        };
        let client = match self
            .attempt_driver
            .construct_client(admitted, app.clone(), candidate_ledger.clone())
            .await
        {
            Ok(client) => client,
            Err(detail) => {
                self.fail_current(
                    app.clone(),
                    &machine_id,
                    incarnation,
                    generation,
                    detail.clone(),
                );
                return Err(detail);
            }
        };
        let production_recovery = app.clone().map(|app_handle| {
            Arc::new(
                move |machine_id: String, incarnation, generation, expected| {
                    app_handle
                        .state::<HostConnectionManager>()
                        .capture_exact_loss(&machine_id, incarnation, generation, &expected);
                    let app = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let recovery_app = app.clone();
                        let manager = app.state::<HostConnectionManager>();
                        manager
                            .recover_exact_lost_client(
                                Some(recovery_app),
                                machine_id,
                                incarnation,
                                generation,
                                expected,
                            )
                            .await;
                    });
                },
            ) as LossRecovery
        });
        #[cfg(test)]
        let recovery = production_recovery.or_else(|| lock(&self.loss_recovery_for_test).clone());
        #[cfg(not(test))]
        let recovery = production_recovery;
        let dispatcher = recovery.map(|recover| {
            remote_loss_dispatcher(
                machine_id.clone(),
                incarnation,
                generation,
                &client.domain_client(),
                recover,
            )
        });
        let lost_before_install = match self
            .attempt_driver
            .install_loss_dispatcher(&client, dispatcher)
        {
            Ok(lost) => lost,
            Err(detail) => {
                client.close_awaited().await;
                self.fail_current(
                    app.clone(),
                    &machine_id,
                    incarnation,
                    generation,
                    detail.clone(),
                );
                return Err(detail);
            }
        };
        if lost_before_install {
            client.close_awaited().await;
            let detail = "remote terminal stream ended before publication";
            self.fail_current(
                app.clone(),
                &machine_id,
                incarnation,
                generation,
                detail.into(),
            );
            return Err(detail.into());
        }
        let resumed_identity = (client.server_id().to_string(), client.epoch().to_string());
        #[cfg(test)]
        let resumed_identity =
            self.rm4f_candidate_identity(&machine_id, prior_identity.as_ref(), resumed_identity);
        let resumed_client = client.domain_client();
        let changed_epoch = prior_identity.as_ref().is_some_and(|identity| {
            identity.server_id == resumed_identity.0 && identity.epoch != resumed_identity.1
        });
        if let Err(error) = self
            .attempt_driver
            .revalidate_after_construction(publication_generation, app.clone(), machine_id.clone())
            .await
        {
            let detail = error.to_string();
            client.close_awaited().await;
            self.fail_current(
                app.clone(),
                &machine_id,
                incarnation,
                generation,
                detail.clone(),
            );
            return Err(detail);
        }
        // The registry generation check above and the manager commit below
        // share this retained observation authority. Any registry mutation or
        // watcher event reserves an observation first; a reservation that
        // linearizes before adoption makes the candidate fail closed.
        let adopted = if changed_epoch {
            let has_presentation = app.is_some();
            #[cfg(test)]
            let has_presentation = has_presentation || lock(&self.retirement_for_test).is_some();
            if !has_presentation {
                client.close_awaited().await;
                return Err("changed remote lifetime has no application handle".into());
            }
            commit_changed_epoch(
                client,
                || {
                    self.reserve_epoch_retirement(
                        &machine_id,
                        incarnation,
                        generation,
                        validated_observation,
                    )
                },
                || {
                    if let (Some(retired), Some(previous)) =
                        (retired_ledger.clone(), prior_identity.as_ref())
                    {
                        return self.retire_previous_lifetime(
                            app.as_ref(),
                            previous,
                            retired,
                            || {
                                self.take_epoch_retirement_permit(
                                    &machine_id,
                                    incarnation,
                                    generation,
                                )
                            },
                        );
                    }
                    false
                },
                |client| {
                    self.adopt_current(
                        &machine_id,
                        AdoptionFreshness {
                            incarnation,
                            generation,
                            validated_observation,
                            retirement_committed: true,
                        },
                        client,
                        candidate_ledger,
                    )
                },
            )
        } else {
            self.adopt_current(
                &machine_id,
                AdoptionFreshness {
                    incarnation,
                    generation,
                    validated_observation,
                    retirement_committed: false,
                },
                client,
                candidate_ledger,
            )
        };
        match adopted {
            Ok(()) => {
                if let Some(app) = app {
                    if prior_identity.as_ref().is_some_and(|identity| {
                        identity.server_id == resumed_identity.0
                            && identity.epoch == resumed_identity.1
                    }) {
                        if let Ok(snapshot) = self.connected_backend(&machine_id) {
                            super::pty_backend::resume_remote_streams(
                                &app,
                                &resumed_client,
                                prior_identity.as_ref().expect("same lifetime checked"),
                                || self.revalidate_backend(&snapshot).is_ok(),
                            );
                        }
                    }
                    self.spawn_heartbeat(app, machine_id, incarnation, generation);
                }
                Ok(())
            }
            Err(client) => {
                client.close_awaited().await;
                let detail = "connection candidate was lost or superseded before publication";
                self.fail_current(app, &machine_id, incarnation, generation, detail.into());
                Err(detail.into())
            }
        }
    }

    /// Mutate only reconstructed lifetime decisions for this manager/machine;
    /// admitted greeting bytes and the constructed client's namespace stay real.
    #[cfg(test)]
    fn rm4f_candidate_identity(
        &self,
        machine_id: &str,
        previous: Option<&EventNamespace>,
        mut candidate: (String, String),
    ) -> (String, String) {
        if lock(&self.rm4f_reuse_old_epoch_after_daemon_exit).as_deref() == Some(machine_id) {
            if let Some(previous) = previous {
                candidate.1.clone_from(&previous.epoch);
            }
        }
        candidate
    }

    fn retire_previous_lifetime(
        &self,
        app: Option<&tauri::AppHandle>,
        previous: &EventNamespace,
        ledger: RemoteSessionLedger,
        take_permit: impl FnOnce() -> bool,
    ) -> bool {
        if let Some(app) = app {
            return super::pty_backend::retire_remote_lifetime(
                app,
                &super::pty_host::AppTerminalSink::new(
                    app.clone(),
                    super::pty_host::StreamHost::remote_namespace(previous),
                ),
                previous,
                ledger,
                take_permit,
            );
        }
        #[cfg(test)]
        if let Some(presentation) = lock(&self.retirement_for_test).clone() {
            if let Some(before_permit) = &presentation.before_permit {
                before_permit();
            }
            return super::pty_backend::retire_remote_lifetime_core(
                &presentation.streams,
                presentation.sink.as_ref(),
                previous,
                ledger,
                take_permit,
            );
        }
        false
    }

    pub async fn disconnect(&self, machine_id: &str) -> Result<(), String> {
        let submitted = {
            let core = lock(&self.core);
            core.machines
                .get(machine_id)
                .map(|gate| gate.submitted.clone())
                .ok_or("no machine")?
        };
        let submitted_epoch = submitted.disconnect.fetch_add(1, Ordering::SeqCst) + 1;
        let publication = lock(&self.publication_barrier);
        let (serial, disconnect_epoch) = {
            let mut core = lock(&self.core);
            let gate = core.machines.get_mut(machine_id).ok_or("no machine")?;
            gate.disconnect_epoch = gate.disconnect_epoch.saturating_add(1);
            gate.foreground_demand = false;
            gate.auto_suppressed = true;
            gate.auto_pending = false;
            if let Some(active) = gate.active.take() {
                active.token.cancel();
            }
            let serial = gate.serial.clone();
            self.enqueue_locked(&mut core, machine_id);
            (serial, submitted_epoch)
        };
        drop(publication);
        self.dispatch();
        let _quarantine = serial.lock_owned().await;
        self.await_quarantine(machine_id).await;
        let connected = {
            let mut core = lock(&self.core);
            core.machines
                .get_mut(machine_id)
                .and_then(|gate| gate.connected.take())
        };
        if let Some(client) = connected {
            client.close_awaited().await;
        }
        let mut core = lock(&self.core);
        let gate = core.machines.get_mut(machine_id).ok_or("no machine")?;
        gate.handled_disconnect = disconnect_epoch;
        gate.connected_generation = None;
        gate.resume_identity = None;
        gate.consecutive_heartbeat_misses = 0;
        gate.retry_attempt = 0;
        gate.retry_ticket = gate.retry_ticket.saturating_add(1);
        gate.state = HostConnectionState::Disconnected;
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
        Ok(())
    }

    pub fn begin_shutdown(&self) -> bool {
        self.shutdown_submitted.store(true, Ordering::SeqCst);
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running {
            return false;
        }
        core.shutdown = ShutdownPhase::Draining;
        let mut close = Vec::new();
        for gate in core.machines.values_mut() {
            if let Some(active) = gate.active.take() {
                active.token.cancel();
            }
            if let Some(client) = &gate.connected {
                close.push(client.close_request());
            }
        }
        let ids: Vec<_> = core.machines.keys().cloned().collect();
        for id in ids {
            self.enqueue_locked(&mut core, &id);
        }
        drop(core);
        drop(_publication);
        self.dispatch();
        for request in close {
            request();
        }
        true
    }

    pub fn mark_drained(&self) {
        let mut core = lock(&self.core);
        if core.shutdown == ShutdownPhase::Draining {
            core.shutdown = ShutdownPhase::Drained;
        }
    }

    /// Await every manager-owned attempt. This owned drain is never timed
    /// out internally; callers may time out their observation, but that must
    /// not cancel the drain or authorize process exit.
    pub async fn drain_shutdown(&self) {
        self.drain_connections().await;
        self.mark_drained();
    }

    /// [`drain_shutdown`](Self::drain_shutdown) without the final `Drained` mark: the
    /// caller has more to finish before exit may be allowed, then calls
    /// [`mark_drained`](Self::mark_drained).
    pub async fn drain_connections(&self) {
        loop {
            let notified = self.operations_changed.notified();
            if self.active_operations.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
        let connected: Vec<_> = {
            let mut core = lock(&self.core);
            core.machines
                .values_mut()
                .filter_map(|gate| gate.connected.take())
                .collect()
        };
        for client in connected {
            client.close_awaited().await;
        }
        let quarantined: Vec<_> = {
            let core = lock(&self.core);
            core.machines
                .values()
                .filter_map(|gate| gate.quarantine.clone())
                .collect()
        };
        for mut observation in quarantined {
            while !*observation.borrow() {
                if observation.changed().await.is_err() {
                    log::error!(target: "pty", "shutdown quarantine ended without true Closed");
                    std::future::pending::<()>().await;
                }
            }
        }
    }

    pub fn shutdown_phase(&self) -> ShutdownPhase {
        lock(&self.core).shutdown
    }

    #[cfg(test)]
    pub(crate) fn set_active_operations_for_test(&self, count: u64) {
        self.active_operations.store(count, Ordering::SeqCst);
        self.operations_changed.notify_waiters();
    }

    #[cfg(test)]
    pub fn state(&self, machine_id: &str) -> Option<HostConnectionState> {
        lock(&self.core)
            .machines
            .get(machine_id)
            .filter(|gate| gate.present)
            .map(|gate| gate.state.clone())
    }

    /// SW-AGR-19: how this machine's terminal server is updated. A server
    /// that can take part in the proof-bound safe replacement gets it; any
    /// other (older than `ShutdownForReplacement`, a legacy install, a frozen
    /// one without proof) gets the confirmed reinstall: stop it by its pid
    /// over SSH, install this app's build, reconnect.
    pub(crate) fn update_plan(&self, machine_id: &str) -> Result<UpdatePlan, String> {
        if let Ok(backend) = self.connected_backend(machine_id) {
            return Ok(
                if backend.client.private_lease().is_some() && backend.client.supports_replacement()
                {
                    UpdatePlan::Replace
                } else {
                    UpdatePlan::Reinstall {
                        pid: Some(backend.client.server_pid()).filter(|pid| *pid != 0),
                    }
                },
            );
        }
        let core = lock(&self.core);
        let gate = core
            .machines
            .get(machine_id)
            .filter(|gate| gate.present)
            .ok_or("machine is not registered")?;
        let snapshot = gate.incompatible_snapshot.as_ref();
        match &gate.state {
            HostConnectionState::Incompatible { proof: Some(_), .. }
                if snapshot.is_some_and(|s| s.shutdown_for_replacement) =>
            {
                Ok(UpdatePlan::Replace)
            }
            HostConnectionState::Incompatible { proof, .. } => Ok(UpdatePlan::Reinstall {
                pid: snapshot
                    .map(|s| s.host_pid)
                    .filter(|pid| *pid != 0)
                    .or_else(|| proof.as_ref().map(|proof| proof.expected_pid())),
            }),
            HostConnectionState::UnsupportedLegacy { proof, .. } => Ok(UpdatePlan::Reinstall {
                pid: Some(proof.expected_pid()).filter(|pid| *pid != 0),
            }),
            // The old server is gone: nothing of its to aim at.
            HostConnectionState::ReplacementFailed {
                old_stopped: true, ..
            } => Ok(UpdatePlan::Reinstall { pid: None }),
            HostConnectionState::ReplacementFailed { .. } => Ok(UpdatePlan::Reinstall {
                pid: snapshot.map(|s| s.host_pid).filter(|pid| *pid != 0),
            }),
            _ => Err("connect the machine first: its terminal server is not running".into()),
        }
    }

    /// SW-AGR-19: a reinstall that stopped the old server but could not
    /// bring up the new one leaves the machine offering the update again (a
    /// retry installs and connects; nothing is left to stop). Only while it is
    /// not connected or connecting.
    pub(crate) fn record_update_failure(
        &self,
        machine_id: &str,
        old_stopped: bool,
        repair: String,
    ) {
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        if let Some(gate) = core
            .machines
            .get_mut(machine_id)
            .filter(|gate| gate.present)
        {
            // A frozen or legacy server already offers the update (with its
            // count and proof): never overwrite that.
            let frozen = matches!(
                gate.state,
                HostConnectionState::Incompatible { .. }
                    | HostConnectionState::UnsupportedLegacy { .. }
            );
            if gate.connected.is_none() && gate.active.is_none() && !frozen {
                gate.state = HostConnectionState::ReplacementFailed {
                    old_stopped,
                    count: None,
                    repair,
                };
            }
        }
    }

    async fn prepare_replacement(
        &self,
        app: Option<tauri::AppHandle>,
        machine_id: &str,
    ) -> Result<ReplacementPrompt, String> {
        let backend = match self.connected_backend(machine_id) {
            Ok(backend) => backend,
            Err(_) => return self.reserve_replacement(machine_id),
        };
        if backend.client.private_lease().is_none() || !backend.client.supports_replacement() {
            return Err("terminal server does not support safe private replacement".into());
        }
        let (generation, fingerprint) = {
            let core = lock(&self.core);
            let gate = core
                .machines
                .get(machine_id)
                .ok_or("machine is not registered")?;
            (
                gate.connected_generation
                    .ok_or("machine is not connected")?,
                gate.fingerprint.clone(),
            )
        };
        let proof = self
            .attempt_driver
            .legacy_proof(app, machine_id.to_string())
            .await?
            .ok_or("connected server has no replacement proof")?;
        if proof.expected_server_id() != backend.server_id
            || proof.expected_pid() != backend.client.server_pid()
        {
            return Err("replacement proof changed server identity".into());
        }
        self.revalidate_backend(&backend)
            .map_err(|_| "connected server changed during preparation")?;
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running || self.shutdown_submitted.load(Ordering::SeqCst)
        {
            return Err("application is shutting down".into());
        }
        let gate = core
            .machines
            .get_mut(machine_id)
            .ok_or("machine is not registered")?;
        if !Self::usable_gate(gate)
            || gate.incarnation != backend.incarnation
            || gate.connected_generation != Some(generation)
            || gate.fingerprint != fingerprint
            || gate
                .private_lease
                .as_ref()
                .is_none_or(|lease| lease.validate().is_err())
            || gate
                .connected
                .as_ref()
                .is_none_or(|client| !Arc::ptr_eq(&client.domain_client(), &backend.client))
        {
            return Err("connected server changed during preparation".into());
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        gate.replacement = Some(ReplacementReservation {
            token: token.clone(),
            incarnation: gate.incarnation,
            generation,
            fingerprint,
            proof,
            count: None,
            old_epoch: Some(backend.epoch),
            frozen_state: gate.state.clone(),
            connected_client: Some(backend.client),
            progress: Arc::default(),
            expires_at: std::time::Instant::now() + REPLACEMENT_RESERVATION_TTL,
        });
        Ok(ReplacementPrompt {
            server_token: token,
            count: None,
        })
    }

    pub fn reserve_replacement(&self, machine_id: &str) -> Result<ReplacementPrompt, String> {
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running || self.shutdown_submitted.load(Ordering::SeqCst)
        {
            return Err("application is shutting down".into());
        }
        let gate = core
            .machines
            .get_mut(machine_id)
            .filter(|gate| gate.present)
            .ok_or("machine is not registered")?;
        if gate
            .replacement
            .as_ref()
            .is_some_and(|r| std::time::Instant::now() >= r.expires_at)
        {
            if let Some(expired) = gate.replacement.take() {
                if expired.connected_client.is_none()
                    && gate.state
                        == (HostConnectionState::ReplacementReserved {
                            count: expired.count,
                        })
                {
                    gate.state = expired.frozen_state;
                }
            }
        }
        let generation = gate
            .frozen_generation
            .ok_or("terminal server is not frozen")?;
        if matches!(
            gate.state,
            HostConnectionState::Incompatible { .. }
                | HostConnectionState::UnsupportedLegacy { .. }
        ) && !gate
            .incompatible_snapshot
            .as_ref()
            .is_some_and(|s| s.shutdown_for_replacement)
        {
            return Err("terminal server does not support safe replacement".into());
        }
        let (proof, count, old_epoch) = match &gate.state {
            HostConnectionState::Incompatible { n_sessions, proof } => (
                proof
                    .clone()
                    .ok_or("frozen server has no replacement proof")?,
                Some(*n_sessions),
                gate.incompatible_snapshot.as_ref().map(|s| s.epoch.clone()),
            ),
            HostConnectionState::UnsupportedLegacy { .. } => {
                return Err("terminal server does not support safe replacement".into())
            }
            _ => return Err("terminal server is not frozen".into()),
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let frozen_state = gate.state.clone();
        gate.replacement = Some(ReplacementReservation {
            token: token.clone(),
            incarnation: gate.incarnation,
            generation,
            fingerprint: gate.fingerprint.clone(),
            proof,
            count,
            old_epoch,
            frozen_state,
            connected_client: None,
            progress: Arc::default(),
            expires_at: std::time::Instant::now() + REPLACEMENT_RESERVATION_TTL,
        });
        gate.state = HostConnectionState::ReplacementReserved { count };
        drop(core);
        let expiry_core = self.core.clone();
        let expiry_event_seq = self.event_seq.clone();
        let expiry_scheduler = self.scheduler.clone();
        let expiry_machine = machine_id.to_string();
        let expiry_token = token.clone();
        tauri::async_runtime::spawn(async move {
            expiry_scheduler.sleep(REPLACEMENT_RESERVATION_TTL).await;
            let mut core = lock(&expiry_core);
            let Some(gate) = core.machines.get_mut(&expiry_machine) else {
                return;
            };
            let same = gate.replacement.as_ref().is_some_and(|reservation| {
                reservation.token == expiry_token
                    && reservation.generation == generation
                    && gate.state
                        == HostConnectionState::ReplacementReserved {
                            count: reservation.count,
                        }
            });
            if same {
                if let Some(expired) = gate.replacement.take() {
                    gate.state = expired.frozen_state;
                }
                HostConnectionManager::enqueue_core(&mut core, &expiry_event_seq, &expiry_machine);
                drop(core);
                HostConnectionManager::dispatch_core(&expiry_core);
            }
        });
        Ok(ReplacementPrompt {
            server_token: token,
            count,
        })
    }

    fn consume_replacement(
        &self,
        machine_id: &str,
        server_token: &str,
    ) -> Result<ReplacementReservation, String> {
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        let gate = core
            .machines
            .get_mut(machine_id)
            .filter(|gate| gate.present)
            .ok_or("machine is not registered")?;
        if !matches!(
            gate.state,
            HostConnectionState::Connected { .. } | HostConnectionState::ReplacementReserved { .. }
        ) || gate
            .replacement
            .as_ref()
            .is_some_and(|reservation| lock(&reservation.progress).consumed)
        {
            return Err("replacement token is missing or already consumed".into());
        }
        let reservation = gate
            .replacement
            .take()
            .ok_or("replacement token is missing or already consumed")?;
        if std::time::Instant::now() >= reservation.expires_at {
            if reservation.connected_client.is_none()
                && gate.state
                    == (HostConnectionState::ReplacementReserved {
                        count: reservation.count,
                    })
            {
                gate.state = reservation.frozen_state;
            }
            return Err("replacement token expired".into());
        }
        let compatible = reservation.connected_client.is_some();
        let exact_frozen = if let Some(expected) = &reservation.connected_client {
            Self::usable_gate(gate)
                && gate
                    .private_lease
                    .as_ref()
                    .is_some_and(|lease| lease.validate().is_ok())
                && gate
                    .connected
                    .as_ref()
                    .is_some_and(|client| Arc::ptr_eq(&client.domain_client(), expected))
        } else {
            gate.state
                == HostConnectionState::ReplacementReserved {
                    count: reservation.count,
                }
        };
        let unchanged = !self.shutdown_submitted.load(Ordering::SeqCst)
            && reservation.token == server_token
            && gate.incarnation == reservation.incarnation
            && (if compatible {
                gate.connected_generation
            } else {
                gate.frozen_generation
            }) == Some(reservation.generation)
            && gate.fingerprint == reservation.fingerprint
            && exact_frozen
            && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
            && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
            && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry;
        if unchanged {
            lock(&reservation.progress).consumed = true;
            if compatible {
                gate.frozen_generation = Some(reservation.generation);
            }
            gate.state = HostConnectionState::Replacing {
                count: reservation.count,
            };
            gate.replacement = Some(reservation.clone());
            self.enqueue_locked(&mut core, machine_id);
            drop(core);
            drop(_publication);
            self.dispatch();
            Ok(reservation)
        } else {
            if !compatible && exact_frozen {
                gate.state = reservation.frozen_state;
            }
            Err("replacement token is stale or forged".into())
        }
    }

    fn replacement_still_current(
        &self,
        machine_id: &str,
        reservation: &ReplacementReservation,
    ) -> bool {
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        let current = core.shutdown == ShutdownPhase::Running
            && !self.shutdown_submitted.load(Ordering::SeqCst)
            && core.machines.get(machine_id).is_some_and(|gate| {
                let exact_frozen = gate.state
                    == HostConnectionState::Replacing {
                        count: reservation.count,
                    };
                gate.present
                    && gate.incarnation == reservation.incarnation
                    && gate.fingerprint == reservation.fingerprint
                    && gate.frozen_generation == Some(reservation.generation)
                    && exact_frozen
                    && gate.submitted.disconnect.load(Ordering::SeqCst) <= gate.handled_disconnect
                    && gate.submitted.removal.load(Ordering::SeqCst) <= gate.handled_removal
                    && gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
            });
        if current {
            if let Some(gate) = core.machines.get_mut(machine_id) {
                gate.replacement = Some(reservation.clone());
                if reservation.connected_client.is_some() {
                    gate.connected_generation = None;
                }
                gate.state = HostConnectionState::Readmitting {
                    count: reservation.count,
                };
            }
        }
        current
    }

    fn replacement_admission(
        &self,
        machine_id: &str,
        token: &str,
    ) -> Result<ReplacementAdmission, ConnectedBackendError> {
        let (backend, census) = {
            let core = lock(&self.core);
            let reservation = core
                .machines
                .get(machine_id)
                .and_then(|gate| gate.replacement.as_ref())
                .filter(|reservation| reservation.token == token)
                .ok_or(ConnectedBackendError::IdentityMismatch)?;
            let progress = lock(&reservation.progress);
            (
                progress
                    .admission
                    .clone()
                    .ok_or(ConnectedBackendError::IdentityMismatch)?,
                progress
                    .census
                    .ok_or(ConnectedBackendError::IdentityMismatch)?,
            )
        };
        self.revalidate_backend(&backend)?;
        Ok(ReplacementAdmission {
            server_id: backend.server_id,
            epoch: backend.epoch,
            census,
        })
    }

    fn finish_replacement_failure(
        &self,
        machine_id: &str,
        reservation: &ReplacementReservation,
        old_stopped: bool,
        repair: String,
    ) {
        let _publication = lock(&self.publication_barrier);
        let mut core = lock(&self.core);
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return;
        };
        let progress = lock(&reservation.progress);
        let same_reservation = gate
            .replacement
            .as_ref()
            .is_some_and(|current| current.token == reservation.token);
        let same_attempt = progress.attempt.is_none_or(|attempt| {
            gate.active
                .as_ref()
                .is_some_and(|active| active.generation == attempt)
                || gate.connected_generation == Some(attempt)
                || (matches!(gate.state, HostConnectionState::Unreachable { .. })
                    && same_reservation)
        });
        let same_client = progress.admission.as_ref().is_none_or(|admission| {
            gate.connected
                .as_ref()
                .is_some_and(|client| Arc::ptr_eq(&client.domain_client(), &admission.client))
                && gate.connected_generation == progress.attempt
        });
        if gate.incarnation != reservation.incarnation
            || gate.fingerprint != reservation.fingerprint
            || !same_reservation
            || !same_attempt
            || !same_client
            || gate.submitted.disconnect.load(Ordering::SeqCst) > gate.handled_disconnect
            || gate.submitted.removal.load(Ordering::SeqCst) > gate.handled_removal
            || gate.submitted.retry.load(Ordering::SeqCst) > gate.handled_retry
        {
            return;
        }
        drop(progress);
        if !old_stopped && reservation.connected_client.is_some() {
            if gate.state
                != (HostConnectionState::Replacing {
                    count: reservation.count,
                })
                || gate.connected_generation != Some(reservation.generation)
                || gate.submitted.disconnect.load(Ordering::SeqCst) > gate.handled_disconnect
                || gate.submitted.removal.load(Ordering::SeqCst) > gate.handled_removal
                || gate.submitted.retry.load(Ordering::SeqCst) > gate.handled_retry
            {
                return;
            }
            gate.connected_generation = Some(reservation.generation);
            gate.frozen_generation = None;
        }
        gate.state = if old_stopped {
            HostConnectionState::ReplacementFailed {
                old_stopped,
                count: reservation.count,
                repair,
            }
        } else {
            reservation.frozen_state.clone()
        };
        self.enqueue_locked(&mut core, machine_id);
        drop(core);
        self.dispatch();
    }

    pub(crate) fn connected_backend(
        &self,
        machine_id: &str,
    ) -> Result<ConnectedBackendSnapshot, ConnectedBackendError> {
        let core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running || self.shutdown_submitted.load(Ordering::SeqCst)
        {
            return Err(ConnectedBackendError::ManagerShuttingDown);
        }
        let gate = core
            .machines
            .get(machine_id)
            .filter(|gate| gate.present)
            .ok_or(ConnectedBackendError::UnknownMachine)?;
        if Self::private_epoch(&core, machine_id).is_some()
            && gate
                .private_lease
                .as_ref()
                .is_none_or(|lease| lease.validate().is_err())
        {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        let state_server_id = match &gate.state {
            HostConnectionState::Disconnected => return Err(ConnectedBackendError::Disconnected),
            HostConnectionState::Connecting => return Err(ConnectedBackendError::Connecting),
            HostConnectionState::Handshaking => return Err(ConnectedBackendError::Handshaking),
            HostConnectionState::Incompatible { .. } => {
                return Err(ConnectedBackendError::Incompatible {
                    snapshot: Box::new(
                        gate.incompatible_snapshot
                            .clone()
                            .ok_or(ConnectedBackendError::IdentityMismatch)?,
                    ),
                })
            }
            HostConnectionState::UnsupportedLegacy { .. } => {
                return Err(ConnectedBackendError::UnsupportedLegacy)
            }
            HostConnectionState::ReplacementReserved { .. }
            | HostConnectionState::Replacing { .. }
            | HostConnectionState::Readmitting { .. } => {
                return Err(ConnectedBackendError::Connecting)
            }
            HostConnectionState::ReplacementFailed { repair, .. } => {
                return Err(ConnectedBackendError::Unreachable {
                    last_error: repair.clone(),
                })
            }
            HostConnectionState::Unreachable { last_error } => {
                return Err(ConnectedBackendError::Unreachable {
                    last_error: last_error.clone(),
                })
            }
            HostConnectionState::Connected { server_id } => server_id,
        };
        let connected = gate
            .connected
            .as_ref()
            .ok_or(ConnectedBackendError::IdentityMismatch)?;
        if !Self::usable_gate(gate) || connected.server_id() != state_server_id {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        Ok(ConnectedBackendSnapshot {
            machine_id: machine_id.to_string(),
            incarnation: gate.incarnation,
            server_id: state_server_id.clone(),
            epoch: connected.epoch().to_string(),
            client: connected.domain_client(),
        })
    }

    pub(crate) fn revalidate_backend(
        &self,
        expected: &ConnectedBackendSnapshot,
    ) -> Result<(), ConnectedBackendError> {
        let current = self.connected_backend(&expected.machine_id)?;
        if current.incarnation != expected.incarnation
            || current.server_id != expected.server_id
            || current.epoch != expected.epoch
            || !Arc::ptr_eq(&current.client, &expected.client)
        {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        Ok(())
    }

    pub(crate) fn cache_verified_layout(
        &self,
        expected: &ConnectedBackendSnapshot,
        layout: LayoutSnapshot,
    ) -> Result<(), ConnectedBackendError> {
        if layout.server_id.as_str() != expected.server_id || layout.epoch != expected.epoch {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running || self.shutdown_submitted.load(Ordering::SeqCst)
        {
            return Err(ConnectedBackendError::ManagerShuttingDown);
        }
        let gate = core
            .machines
            .get_mut(&expected.machine_id)
            .ok_or(ConnectedBackendError::UnknownMachine)?;
        let state_server_id = match &gate.state {
            HostConnectionState::Connected { server_id } => server_id,
            _ => return Err(ConnectedBackendError::IdentityMismatch),
        };
        let connected = gate
            .connected
            .as_ref()
            .ok_or(ConnectedBackendError::IdentityMismatch)?;
        if !Self::usable_gate(gate)
            || gate.incarnation != expected.incarnation
            || state_server_id != &expected.server_id
            || connected.server_id() != expected.server_id
            || connected.epoch() != expected.epoch
            || !Arc::ptr_eq(&connected.domain_client(), &expected.client)
        {
            return Err(ConnectedBackendError::IdentityMismatch);
        }
        expected
            .client
            .commit_private(|| {
                gate.last_layout = Some(layout);
            })
            .map_err(|_| ConnectedBackendError::IdentityMismatch)
    }

    pub(crate) fn verified_remote_layouts(
        &self,
        viewer: &ViewerId,
        boot: &ClientBootId,
    ) -> Vec<LayoutSnapshot> {
        let (connected_ids, frozen) = self.legacy_layout_candidates();
        self.read_legacy_layouts(connected_ids, frozen, |client| {
            client.layout_snapshot(viewer, boot)
        })
    }

    fn legacy_layout_candidates(&self) -> (Vec<String>, Vec<LayoutSnapshot>) {
        {
            let core = lock(&self.core);
            let connected = core
                .machines
                .iter()
                .filter(|(id, gate)| {
                    gate.present
                        && Self::private_epoch(&core, id).is_none()
                        && matches!(gate.state, HostConnectionState::Connected { .. })
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let frozen = core
                .machines
                .iter()
                .filter(|(id, gate)| {
                    gate.present
                        && Self::private_epoch(&core, id).is_none()
                        && matches!(gate.state, HostConnectionState::Incompatible { .. })
                })
                .filter_map(|(_, gate)| gate.incompatible_snapshot.as_ref()?.layout.clone())
                .collect::<Vec<_>>();
            (connected, frozen)
        }
    }

    fn read_legacy_layouts(
        &self,
        connected_ids: Vec<String>,
        frozen: Vec<LayoutSnapshot>,
        mut read: impl FnMut(
            &Arc<SessionServerClient>,
        ) -> Result<LayoutSnapshot, codesense_pty_core::layout::LayoutError>,
    ) -> Vec<LayoutSnapshot> {
        let mut layouts = frozen;
        for machine_id in connected_ids {
            let Ok(snapshot) = self.connected_backend(&machine_id) else {
                continue;
            };
            // Candidate IDs can outlive a legacy-to-private replacement. Only
            // the exact returned client determines whether this read is legacy.
            if snapshot.client.private_lease().is_some() {
                continue;
            }
            let Ok(layout) = read(&snapshot.client) else {
                continue;
            };
            if self
                .cache_verified_layout(&snapshot, layout.clone())
                .is_ok()
            {
                layouts.push(layout);
            }
        }
        layouts
    }

    /// Lift the background-reconnect suppression a disconnect set, for a
    /// disconnect that was not the person's (a file unreadable past its
    /// grace, now repaired). The next candidates read offers it again.
    pub(crate) fn resume_auto_connect(&self, machine_id: &str) {
        let mut core = lock(&self.core);
        if let Some(gate) = core.machines.get_mut(machine_id) {
            if gate.auto_suppressed {
                gate.auto_suppressed = false;
                gate.auto_pending = gate.auto_connect;
            }
        }
    }

    pub(crate) fn auto_connect_candidates(&self) -> Vec<String> {
        let mut core = lock(&self.core);
        if core.shutdown != ShutdownPhase::Running {
            return Vec::new();
        }
        core.machines
            .iter_mut()
            .filter_map(|(id, gate)| {
                let eligible = id.as_str() != LOCAL_MACHINE_ID
                    && gate.present
                    && gate.auto_connect
                    && !gate.auto_suppressed
                    && gate.active.is_none()
                    && gate.auto_pending;
                eligible.then(|| {
                    gate.auto_pending = false;
                    id.clone()
                })
            })
            .collect()
    }

    pub(crate) fn settle_self_write(
        &self,
        machine_id: &str,
        seq: ObservationSeq,
        revision: &Option<super::machines::registry::RegistryRevision>,
    ) -> Vec<String> {
        let mut core = lock(&self.core);
        let running = core.shutdown == ShutdownPhase::Running;
        let Some(gate) = core.machines.get_mut(machine_id) else {
            return Vec::new();
        };
        if gate.pending_self_write.as_ref()
            != Some(&PendingSelfWrite {
                seq,
                revision: revision.clone(),
            })
        {
            return Vec::new();
        }
        gate.pending_self_write = None;
        if running
            && machine_id != LOCAL_MACHINE_ID
            && gate.present
            && gate.auto_connect
            && !gate.auto_suppressed
            && gate.active.is_none()
            && gate.auto_pending
        {
            gate.auto_pending = false;
            vec![machine_id.to_string()]
        } else {
            Vec::new()
        }
    }
}

#[tauri::command]
pub async fn connect_machine(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<machines::MachineSummary, String> {
    let manager = app.state::<HostConnectionManager>();
    let ensured = machine_id.clone();
    connection_command(
        &manager,
        Some(app.clone()),
        machines::ensure_manager_member(&app, &ensured),
        machine_id.clone(),
        ConnectOrigin::ExplicitForeground,
        || machines::manager_machine_summary(&app, machine_id),
    )
    .await
}

#[tauri::command]
pub async fn disconnect_machine(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<machines::MachineSummary, String> {
    let manager = app.state::<HostConnectionManager>();
    manager.disconnect(&machine_id).await?;
    machines::manager_machine_summary(&app, machine_id).await
}

#[tauri::command]
pub async fn retry_machine(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<machines::MachineSummary, String> {
    let manager = app.state::<HostConnectionManager>();
    let ensured = machine_id.clone();
    connection_command(
        &manager,
        Some(app.clone()),
        machines::ensure_manager_member(&app, &ensured),
        machine_id.clone(),
        ConnectOrigin::Retry,
        || machines::manager_machine_summary(&app, machine_id),
    )
    .await
}

#[tauri::command]
pub async fn prepare_machine_terminal_server_replacement(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<ReplacementPrompt, String> {
    // SW-AGR-19 PR-8: one update at a time — never beside an Update all run.
    machines::update_all::refuse_while_running()?;
    prepare_replacement_now(app, machine_id).await
}

/// The replacement's preparation itself (the Update all run's own step).
pub(crate) async fn prepare_replacement_now(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<ReplacementPrompt, String> {
    app.state::<HostConnectionManager>()
        .prepare_replacement(Some(app.clone()), &machine_id)
        .await
}

#[tauri::command]
pub async fn restart_machine_terminal_server(
    app: tauri::AppHandle,
    machine_id: String,
    server_token: String,
) -> Result<ReplacementResult, String> {
    machines::update_all::refuse_while_running()?;
    restart_now(app, machine_id, server_token).await
}

/// The prepared restart itself (the Update all run's own step).
pub(crate) async fn restart_now(
    app: tauri::AppHandle,
    machine_id: String,
    server_token: String,
) -> Result<ReplacementResult, String> {
    // SW-AGR-19 PR-4: record the tabs once the token is consumed and while
    // the old server can still answer for them; reopen them once the new one
    // is admitted.
    let home = machines::codesense_home_of(&app).await;
    let recorded: Arc<Mutex<Option<machines::restore::Recorded>>> = Arc::default();
    let record_slot = recorded.clone();
    let record_home = home.clone();
    let record_app = app.clone();
    let restore_app = app.clone();
    let restore_id = machine_id.clone();
    let manager = app.state::<HostConnectionManager>();
    let disconnect_manager = manager.clone();
    let dispatch_app = app.clone();
    let connect_app = app.clone();
    let admission_manager = &*manager;
    let admission_token = server_token.clone();
    let result = restart_machine_terminal_server_core(
        &manager,
        machine_id,
        server_token,
        |machine_id, proof, token, old_epoch| async move {
            if let Some(home) = &record_home {
                *lock(&record_slot) =
                    machines::restore::capture(&record_app, home, &machine_id).await;
            }
            machines::replace_pinned_terminal_server(
                dispatch_app,
                machine_id,
                proof,
                token,
                old_epoch,
            )
            .await
            .map(|_| ())
        },
        |machine_id, origin| manager.connect_remote(connect_app, machine_id, origin),
        |machine_id| {
            std::future::ready(
                admission_manager.replacement_admission(&machine_id, &admission_token),
            )
        },
        |machine_id| async move { disconnect_manager.disconnect(&machine_id).await },
    )
    .await;
    let Some(home) = home else {
        return result;
    };
    let label = machines::label_of(&restore_app, &restore_id).await;
    let recorded = lock(&recorded).take();
    use machines::restore::{finish, UpdateEnd};
    match result {
        Ok(ReplacementResult::Replaced { count, .. }) => {
            let restored = finish(
                &restore_app,
                &home,
                &restore_id,
                &label,
                recorded,
                UpdateEnd::Replaced,
            )
            .await;
            Ok(ReplacementResult::Replaced { count, restored })
        }
        Ok(ReplacementResult::Failed(failed)) => {
            let end = if failed.old_stopped {
                UpdateEnd::OldStopped
            } else {
                UpdateEnd::OldRunning
            };
            finish(&restore_app, &home, &restore_id, &label, recorded, end).await;
            Ok(ReplacementResult::Failed(failed))
        }
        Err(error) => {
            // Refused before anything stopped: this attempt's own record (if
            // it got that far) describes tabs that still run.
            finish(
                &restore_app,
                &home,
                &restore_id,
                &label,
                recorded,
                UpdateEnd::OldRunning,
            )
            .await;
            Err(error)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplacementAdmission {
    server_id: String,
    epoch: String,
    census: u32,
}

async fn restart_machine_terminal_server_core<
    Dispatch,
    DispatchFuture,
    Connect,
    ConnectFuture,
    Admit,
    AdmitFuture,
    Disconnect,
    DisconnectFuture,
>(
    manager: &HostConnectionManager,
    machine_id: String,
    server_token: String,
    dispatch: Dispatch,
    connect: Connect,
    admit: Admit,
    disconnect: Disconnect,
) -> Result<ReplacementResult, String>
where
    Dispatch: FnOnce(String, machines::LegacyHostProof, String, String) -> DispatchFuture,
    DispatchFuture: Future<Output = Result<(), machines::ReplacementDispatchFailure>>,
    Connect: FnOnce(String, ConnectOrigin) -> ConnectFuture,
    ConnectFuture: Future<Output = Result<(), String>>,
    Admit: FnOnce(String) -> AdmitFuture,
    AdmitFuture: Future<Output = Result<ReplacementAdmission, ConnectedBackendError>>,
    Disconnect: FnOnce(String) -> DisconnectFuture,
    DisconnectFuture: Future<Output = Result<(), String>>,
{
    // Check A: token consumption and every desktop authority check linearize
    // before env resolution, credential access, install writes, or SSH spawn.
    let reservation = manager.consume_replacement(&machine_id, &server_token)?;
    let count = reservation.count;
    let expected_server_id = reservation.proof.expected_server_id().to_string();
    let Some(old_epoch) = reservation.old_epoch.clone() else {
        let repair = "frozen server epoch is unavailable".to_string();
        manager.finish_replacement_failure(&machine_id, &reservation, false, repair.clone());
        return Ok(ReplacementResult::Failed(ReplacementFailed {
            old_stopped: false,
            count,
            repair,
        }));
    };
    let dispatched = dispatch(
        machine_id.clone(),
        reservation.proof.clone(),
        server_token.clone(),
        old_epoch,
    )
    .await;
    if let Err(failure) = dispatched {
        manager.finish_replacement_failure(
            &machine_id,
            &reservation,
            failure.old_stopped,
            failure.message.clone(),
        );
        return Ok(ReplacementResult::Failed(ReplacementFailed {
            old_stopped: failure.old_stopped,
            count,
            repair: failure.message,
        }));
    }
    // Check B: the same lifetime and registry fingerprint must still own this
    // result after the exactly-reaped child completes.
    if !manager.replacement_still_current(&machine_id, &reservation) {
        manager.finish_replacement_failure(
            &machine_id,
            &reservation,
            true,
            "machine authority changed after the old server stopped".into(),
        );
        return Ok(ReplacementResult::Failed(ReplacementFailed {
            old_stopped: true,
            count,
            repair: "machine authority changed after the old server stopped".into(),
        }));
    }
    if let Err(repair) = connect(machine_id.clone(), ConnectOrigin::Restart).await {
        manager.finish_replacement_failure(&machine_id, &reservation, true, repair.clone());
        return Ok(ReplacementResult::Failed(ReplacementFailed {
            old_stopped: true,
            count,
            repair,
        }));
    }
    let admitted = match admit(machine_id.clone()).await {
        Ok(admitted) => admitted,
        Err(error) => {
            let repair = format!("replacement readmission was not published: {error:?}");
            manager.finish_replacement_failure(&machine_id, &reservation, true, repair.clone());
            return Ok(ReplacementResult::Failed(ReplacementFailed {
                old_stopped: true,
                count,
                repair,
            }));
        }
    };
    let fresh_epoch = reservation
        .old_epoch
        .as_ref()
        .is_none_or(|old| old != &admitted.epoch);
    if admitted.server_id != expected_server_id || !fresh_epoch || admitted.census != 0 {
        let disconnect_error = disconnect(machine_id.clone()).await.err();
        let repair = disconnect_error.map_or_else(
            || "replacement failed final server identity or epoch publication permit".into(),
            |error| {
                format!(
                    "replacement failed final server identity or epoch publication permit; cleanup failed: {error}"
                )
            },
        );
        manager.finish_replacement_failure(&machine_id, &reservation, true, repair.clone());
        return Ok(ReplacementResult::Failed(ReplacementFailed {
            old_stopped: true,
            count,
            repair,
        }));
    }
    Ok(ReplacementResult::Replaced {
        count,
        restored: None,
    })
}

/// `connect_machine` and `retry_machine`: make sure the manager knows the
/// machine (`ensure_member`), then connect and summarize.
pub(crate) async fn connection_command<Summary>(
    manager: &HostConnectionManager,
    app: Option<tauri::AppHandle>,
    ensure_member: impl Future<Output = Result<(), String>>,
    machine_id: String,
    origin: ConnectOrigin,
    summarize: impl FnOnce() -> Summary,
) -> Result<machines::MachineSummary, String>
where
    Summary: Future<Output = Result<machines::MachineSummary, String>>,
{
    ensure_member.await?;
    connection_command_core(manager, app, machine_id, origin, summarize).await
}

async fn connection_command_core<Summary>(
    manager: &HostConnectionManager,
    app: Option<tauri::AppHandle>,
    machine_id: String,
    origin: ConnectOrigin,
    summarize: impl FnOnce() -> Summary,
) -> Result<machines::MachineSummary, String>
where
    Summary: Future<Output = Result<machines::MachineSummary, String>>,
{
    let operation = manager.connect_remote_inner(app, machine_id, origin).await;
    let summary = summarize().await;
    match summary {
        Ok(summary)
            if operation.is_ok()
                || matches!(
                    summary.status,
                    machines::MachineStatus::Connected { .. }
                        | machines::MachineStatus::ConnectionFailed { .. }
                        | machines::MachineStatus::Disconnected
                ) =>
        {
            Ok(summary)
        }
        Ok(_) => Err(operation.expect_err("guarded by operation failure")),
        Err(summary_error) => Err(operation.err().unwrap_or(summary_error)),
    }
}

#[cfg(test)]
pub(crate) async fn test_adopt_real_private_client(
    machine_id: &str,
    client: ManagedRemoteClient,
) -> Arc<HostConnectionManager> {
    let manager = Arc::new(HostConnectionManager::default());
    let seq = manager.reserve_observation();
    manager
        .observe_mutation(
            seq,
            machine_id.into(),
            Some(MachineManagerRecord {
                machine_id: machine_id.into(),
                scope: None,
                revision: None,
                fingerprint: "private-real-ssh".into(),
                set_up: true,
                auto_connect: false,
                open_tabs: false,
            }),
        )
        .await;
    let (incarnation, generation, _, _) = manager
        .begin_attempt(machine_id, ConnectOrigin::ExplicitForeground)
        .unwrap();
    let ledger = client.domain_client().remote_ledger_for_test();
    assert!(manager
        .adopt_current(
            machine_id,
            AdoptionFreshness {
                incarnation,
                generation,
                validated_observation: seq.0,
                retirement_committed: false
            },
            client,
            ledger
        )
        .is_ok());
    manager
}

#[cfg(test)]
pub(crate) async fn test_production_legacy_path(
    machine_id: String,
    open: Arc<
        dyn Fn(String) -> Result<machines::PinnedSshTransport, machines::PinnedSshOpenError>
            + Send
            + Sync,
    >,
    proof: Arc<dyn Fn(String) -> Result<Option<machines::LegacyHostProof>, String> + Send + Sync>,
) -> HostConnectionState {
    let manager =
        HostConnectionManager::with_attempt_driver(Arc::new(ProductionRemoteAttemptDriver {
            open_override: Some(open),
            proof_override: Some(proof),
        }));
    manager
        .observe_mutation(
            manager.reserve_observation(),
            machine_id.clone(),
            Some(MachineManagerRecord {
                machine_id: machine_id.clone(),
                scope: None,
                fingerprint: "production-fixture".into(),
                auto_connect: false,
                open_tabs: false,
                set_up: true,
                revision: None,
            }),
        )
        .await;
    let _ = manager
        .connect_remote_inner(None, machine_id.clone(), ConnectOrigin::ExplicitForeground)
        .await;
    manager.state(&machine_id).expect("fixture machine state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicI32;
    use std::time::{Duration, Instant};

    fn command_summary(status: machines::MachineStatus) -> machines::MachineSummary {
        machines::MachineSummary {
            machine_id: "box-123456".into(),
            kind: "ssh",
            label: "Box".into(),
            scope: None,
            declaration: None,
            binding: None,
            can_publish: false,
            remote_mcp_control: false,
            status,
            attention: 0,
            capabilities: machines::MachineCapabilities {
                agent_callbacks: false,
            },
            machine_stats: None,
            machine_stats_supported: None,
            terminal_server: None,
        }
    }

    pub(super) struct ImmediateFailDriver;

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for ImmediateFailDriver {
        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            Err(machines::PinnedSshOpenError::Other("ssh refused".into()))
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            unreachable!("open fails first")
        }
    }

    #[tokio::test]
    async fn production_connect_and_retry_core_preserves_errors_across_summary_races() {
        for (origin, transitional) in [
            (
                ConnectOrigin::ExplicitForeground,
                machines::MachineStatus::Connecting,
            ),
            (ConnectOrigin::Retry, machines::MachineStatus::Handshaking),
        ] {
            let manager = HostConnectionManager::with_attempt_driver(Arc::new(ImmediateFailDriver));
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-123456".into(),
                    Some(remote("box-123456", "command-race", false)),
                )
                .await;
            let result =
                connection_command_core(&manager, None, "box-123456".into(), origin, || async {
                    let mut core = lock(&manager.core);
                    core.machines.get_mut("box-123456").unwrap().state = match transitional {
                        machines::MachineStatus::Connecting => HostConnectionState::Connecting,
                        _ => HostConnectionState::Handshaking,
                    };
                    Ok(command_summary(transitional))
                })
                .await;
            assert_eq!(result, Err("ssh refused".into()));
        }
    }

    #[tokio::test]
    async fn rm4c_framed_older_major_freezes_identity_census_and_blocks_retry() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "skew", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(manager.complete_incompatible(
            "box-123456",
            incarnation,
            generation,
            super::super::host_connection::IncompatibleRemoteHost {
                hello: codesense_host_proto::FrozenHostHello {
                    proto_major: codesense_host_proto::PROTO_MAJOR - 2,
                    proto_minor: 0,
                    agent_version: "old-agent".into(),
                    server_id: "verified-server".into(),
                    server_name: "old-host".into(),
                    epoch: "old-epoch".into(),
                    rev: 17,
                    instance_id: "instance".into(),
                    capabilities: vec![codesense_host_proto::Capability::Terminals],
                    viewer_id: None,
                    pid: 42,
                    emulator: None,
                },
                census: codesense_host_proto::Census { live_sessions: 6 },
            },
            Some(machines::LegacyHostProof::test_proof("verified-server")),
        ));
        let Err(ConnectedBackendError::Incompatible { snapshot }) =
            manager.connected_backend("box-123456")
        else {
            panic!("older host exposed a live backend")
        };
        assert_eq!(snapshot.layout, None);
        assert_eq!(snapshot.census.live_sessions, 6);
        assert_eq!(snapshot.server_id, "verified-server");
        // SW-AGR-19: the reinstall fallback aims at the pid it greeted with.
        assert_eq!(snapshot.host_pid, 42);
        assert!(manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .is_err());
        assert!(manager.retry_plan("box-123456", incarnation).is_none());
        assert_eq!(
            manager.reserve_replacement("box-123456").unwrap_err(),
            "terminal server does not support safe replacement"
        );
    }

    /// SW-AGR-19: a server that can take part in the safe replacement gets
    /// it; any other frozen or failed one gets the confirmed reinstall, aimed
    /// at the pid it greeted with; a machine with no server is refused.
    #[tokio::test]
    async fn agr19_update_plan_picks_replace_or_reinstall() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        let id = "box-plan-123456";
        manager
            .observe_mutation(
                manager.reserve_observation(),
                id.into(),
                Some(remote(id, id, false)),
            )
            .await;
        let set = |state: HostConnectionState, shutdown: bool, pid: u32| {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(id).unwrap();
            gate.state = state;
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 2 },
                server_id: "server-plan".into(),
                epoch: "old".into(),
                rev: 1,
                shutdown_for_replacement: shutdown,
                host_pid: pid,
            });
        };
        assert!(
            manager.update_plan(id).is_err(),
            "not connected, not frozen"
        );
        let proved = || HostConnectionState::Incompatible {
            n_sessions: 2,
            proof: Some(machines::LegacyHostProof::test_proof("server-plan")),
        };
        set(proved(), true, 77);
        assert_eq!(manager.update_plan(id), Ok(UpdatePlan::Replace));
        set(proved(), false, 77);
        assert_eq!(
            manager.update_plan(id),
            Ok(UpdatePlan::Reinstall { pid: Some(77) })
        );
        set(
            HostConnectionState::Incompatible {
                n_sessions: 2,
                proof: None,
            },
            false,
            0,
        );
        assert_eq!(
            manager.update_plan(id),
            Ok(UpdatePlan::Reinstall { pid: None })
        );
        // No pid on the snapshot: the proof names the process.
        set(proved(), false, 0);
        assert_eq!(
            manager.update_plan(id),
            Ok(UpdatePlan::Reinstall { pid: Some(42) })
        );
        set(
            HostConnectionState::UnsupportedLegacy {
                count: LegacySessionCount::Unknown,
                proof: machines::LegacyHostProof::test_proof("server-plan"),
            },
            false,
            0,
        );
        assert_eq!(
            manager.update_plan(id),
            Ok(UpdatePlan::Reinstall { pid: Some(42) })
        );
        // A failure record never overwrites a frozen or legacy server's own
        // state (it already offers the update, with its count and proof).
        manager.record_update_failure(id, false, "unreachable".into());
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::UnsupportedLegacy { .. })
        ));
        set(proved(), false, 9);
        manager.record_update_failure(id, true, "x".into());
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::Incompatible { .. })
        ));
        assert!(manager.update_plan("box-unknown-123456").is_err());
    }

    /// SW-AGR-19: a connected private server that supports the safe
    /// replacement gets it; one without the capability, or without a private
    /// lease, gets the reinstall aimed at the pid it greeted with. A failed
    /// reinstall leaves the machine offering the update again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agr19_update_plan_for_a_connected_server() {
        for (supported, lease, expected) in [
            (true, true, UpdatePlan::Replace),
            (false, true, UpdatePlan::Reinstall { pid: Some(7) }),
            (true, false, UpdatePlan::Reinstall { pid: Some(42) }),
        ] {
            let driver = Arc::new(ConnectedRealOwnerDriver::with_replacement_support(
                supported,
            ));
            driver.release_close_gates();
            let _revoke = lease.then(|| {
                let (lease, revoke) = super::super::auth::private_authority::test_lease(1);
                *lock(&driver.private_lease) = Some(lease);
                revoke
            });
            let manager = HostConnectionManager::with_attempt_driver(driver);
            let id = "box-123456";
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, "box", false)),
                )
                .await;
            manager
                .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            assert_eq!(
                manager.update_plan(id),
                Ok(expected),
                "supported {supported}, lease {lease}"
            );
            // Connected: a failure record never overwrites a live connection.
            manager.record_update_failure(id, true, "nope".into());
            assert!(manager.connected_backend(id).is_ok());
            // The reinstall lets go of the server, then reconnects the old one
            // when nothing stopped: an explicit connect after the disconnect
            // admits it again (a Restart-origin connect would refuse it).
            manager.disconnect(id).await.unwrap();
            manager
                .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            assert!(manager.connected_backend(id).is_ok());
            manager.disconnect(id).await.unwrap();
            manager.record_update_failure(id, true, "install failed".into());
            assert_eq!(
                manager.state(id),
                Some(HostConnectionState::ReplacementFailed {
                    old_stopped: true,
                    count: None,
                    repair: "install failed".into(),
                })
            );
            assert!(matches!(
                manager.update_plan(id),
                Ok(UpdatePlan::Reinstall { .. })
            ));
        }
    }

    #[tokio::test]
    async fn rm4d_tokens_are_one_use_stale_forged_and_machine_isolated() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        for id in ["box-a-123456", "box-b-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(id).unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 3,
                proof: Some(machines::LegacyHostProof::test_proof("server-a")),
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 3 },
                server_id: "server-a".into(),
                epoch: "old-epoch".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
            gate.frozen_generation = Some(AttemptGeneration(7));
        }

        let forged = manager.reserve_replacement("box-a-123456").unwrap();
        assert_eq!(forged.count, Some(3));
        assert!(manager
            .consume_replacement("box-a-123456", "forged")
            .is_err());
        assert!(manager
            .consume_replacement("box-a-123456", &forged.server_token)
            .is_err());
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Incompatible { .. })
        ));

        let expired = manager.reserve_replacement("box-a-123456").unwrap();
        lock(&manager.core)
            .machines
            .get_mut("box-a-123456")
            .unwrap()
            .replacement
            .as_mut()
            .unwrap()
            .expires_at = std::time::Instant::now();
        let expired_error = manager
            .consume_replacement("box-a-123456", &expired.server_token)
            .err()
            .expect("expired reservation must be refused");
        assert_eq!(expired_error, "replacement token expired");
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Incompatible { .. })
        ));

        let valid = manager.reserve_replacement("box-a-123456").unwrap();
        let reservation = manager
            .consume_replacement("box-a-123456", &valid.server_token)
            .unwrap();
        assert!(manager
            .consume_replacement("box-a-123456", &valid.server_token)
            .is_err());
        assert!(manager.replacement_still_current("box-a-123456", &reservation));
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Readmitting { count: Some(3) })
        ));
        manager.finish_replacement_failure(
            "box-a-123456",
            &reservation,
            true,
            "candidate rejected".into(),
        );
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::ReplacementFailed {
                old_stopped: true,
                count: Some(3),
                ..
            })
        ));

        let stale = manager.reserve_replacement("box-b-123456").unwrap();
        lock(&manager.core)
            .machines
            .get_mut("box-b-123456")
            .unwrap()
            .fingerprint = "changed".into();
        assert!(manager
            .consume_replacement("box-b-123456", &stale.server_token)
            .is_err());
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::ReplacementFailed {
                old_stopped: true,
                count: Some(3),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn rm4d_reservation_expires_automatically_and_cannot_cancel_consumed_token() {
        let scheduler = Arc::new(ManualScheduler::default());
        let manager = HostConnectionManager::with_attempt_driver_and_scheduler(
            Arc::new(NeverOpenDriver),
            scheduler.clone(),
        );
        for id in ["box-expire-123456", "box-consume-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(id).unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 2,
                proof: Some(machines::LegacyHostProof::test_proof("server-expiry")),
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: Some(rm4c_layout("server-expiry", "old-expiry")),
                census: CensusSnapshot { live_sessions: 2 },
                server_id: "server-expiry".into(),
                epoch: "old-expiry".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
            gate.frozen_generation = Some(AttemptGeneration(12));
        }

        let historical = manager
            .admission_baseline()
            .rows
            .into_iter()
            .find(|row| row.member.machine_id == "box-expire-123456")
            .unwrap();
        assert!(matches!(
            historical.state,
            RuntimeAdmissionState::Historical { .. }
        ));
        let rows = Arc::new(Mutex::new(Vec::new()));
        let observed = rows.clone();
        manager.subscribe(Arc::new(move |event| {
            if let Some(row) = event.admission.clone() {
                lock(&observed).push(row);
            }
        }));
        manager.reserve_replacement("box-expire-123456").unwrap();
        let consumed = manager.reserve_replacement("box-consume-123456").unwrap();
        manager
            .consume_replacement("box-consume-123456", &consumed.server_token)
            .unwrap();
        while scheduler.pending() < 2 {
            tokio::task::yield_now().await;
        }
        scheduler.fire(REPLACEMENT_RESERVATION_TTL);
        scheduler.fire(REPLACEMENT_RESERVATION_TTL);
        for _ in 0..100 {
            if matches!(
                manager.state("box-expire-123456"),
                Some(HostConnectionState::Incompatible { .. })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            manager.state("box-expire-123456"),
            Some(HostConnectionState::Incompatible { .. })
        ));
        assert!(matches!(
            manager.state("box-consume-123456"),
            Some(HostConnectionState::Replacing { count: Some(2) })
        ));
        let restored = manager
            .admission_baseline()
            .rows
            .into_iter()
            .find(|row| row.member == historical.member)
            .unwrap();
        assert_eq!(
            restored.state, historical.state,
            "expiry restores captured historical namespace/snapshot, never Live"
        );
        assert!(restored.revision >= historical.revision);
        assert!(lock(&rows)
            .iter()
            .filter(|row| row.member == historical.member)
            .all(|row| !matches!(row.state, RuntimeAdmissionState::Live { .. })));
    }

    #[tokio::test]
    async fn rm4d_restart_command_core_orders_dispatch_check_b_and_restart_publication() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-core-123456".into(),
                Some(remote("box-core-123456", "core", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut("box-core-123456").unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 4,
                proof: Some(machines::LegacyHostProof::test_proof("server-core")),
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 4 },
                server_id: "server-core".into(),
                epoch: "epoch-old".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
            gate.frozen_generation = Some(AttemptGeneration(9));
        }
        let prompt = manager.reserve_replacement("box-core-123456").unwrap();
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let dispatch_events = events.clone();
        let connect_events = events.clone();
        let admit_events = events.clone();
        let manager_ref = &manager;
        let expected_token = prompt.server_token.clone();
        let result = restart_machine_terminal_server_core(
            &manager,
            "box-core-123456".into(),
            prompt.server_token.clone(),
            move |machine, proof, token, old_epoch| {
                assert_eq!(machine, "box-core-123456");
                assert_eq!(proof.expected_server_id(), "server-core");
                assert_eq!(token, expected_token);
                assert_eq!(old_epoch, "epoch-old");
                lock(&dispatch_events).push("dispatch".into());
                std::future::ready(Ok(()))
            },
            move |machine, origin| {
                assert_eq!(machine, "box-core-123456");
                assert_eq!(origin, ConnectOrigin::Restart);
                assert!(matches!(
                    manager_ref.state("box-core-123456"),
                    Some(HostConnectionState::Readmitting { count: Some(4) })
                ));
                lock(&connect_events).push("connect-restart".into());
                std::future::ready(Ok(()))
            },
            move |machine| {
                assert_eq!(machine, "box-core-123456");
                lock(&admit_events).push("publish-empty".into());
                std::future::ready(Ok(ReplacementAdmission {
                    server_id: "server-core".into(),
                    epoch: "epoch-new".into(),
                    census: 0,
                }))
            },
            |_| std::future::ready(Ok(())),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            ReplacementResult::Replaced {
                count: Some(4),
                restored: None
            }
        );
        assert_eq!(
            *lock(&events),
            ["dispatch", "connect-restart", "publish-empty"]
        );
        assert!(manager
            .consume_replacement("box-core-123456", &prompt.server_token)
            .is_err());

        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-fail-123456".into(),
                Some(remote("box-fail-123456", "fail", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut("box-fail-123456").unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 1,
                proof: Some(machines::LegacyHostProof::test_proof("server-fail")),
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "server-fail".into(),
                epoch: "epoch-fail".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
            gate.frozen_generation = Some(AttemptGeneration(10));
        }
        let failed_prompt = manager.reserve_replacement("box-fail-123456").unwrap();
        let failed = restart_machine_terminal_server_core(
            &manager,
            "box-fail-123456".into(),
            failed_prompt.server_token,
            |_, _, _, _| {
                std::future::ready(Err(machines::ReplacementDispatchFailure {
                    old_stopped: true,
                    message: "injected destructive ambiguity".into(),
                }))
            },
            |_, _| std::future::ready(Err("connect must not run".into())),
            |_| std::future::ready(Err(ConnectedBackendError::Disconnected)),
            |_| std::future::ready(Ok(())),
        )
        .await
        .unwrap();
        assert!(matches!(
            failed,
            ReplacementResult::Failed(ReplacementFailed {
                old_stopped: true,
                ..
            })
        ));
        assert!(matches!(
            manager.state("box-fail-123456"),
            Some(HostConnectionState::ReplacementFailed {
                old_stopped: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn rm4d_owner_generation_swap_after_probe() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "before", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut("box-123456").unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 1,
                proof: Some(machines::LegacyHostProof::test_proof("server-a")),
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "server-a".into(),
                epoch: "old-epoch".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
            gate.frozen_generation = Some(AttemptGeneration(7));
        }
        let prompt = manager.reserve_replacement("box-123456").unwrap();
        let reservation = manager
            .consume_replacement("box-123456", &prompt.server_token)
            .unwrap();
        lock(&manager.core)
            .machines
            .get_mut("box-123456")
            .unwrap()
            .fingerprint = "owner-swapped-after-probe".into();
        assert!(!manager.replacement_still_current("box-123456", &reservation));
        assert!(!matches!(
            manager.state("box-123456"),
            Some(HostConnectionState::Readmitting { .. } | HostConnectionState::Connected { .. })
        ));
    }

    #[test]
    fn rm4d_non_empty_candidate_is_denied_before_publication() {
        assert_eq!(
            validate_replacement_admission(
                "server-a",
                Some("old-epoch"),
                "server-a",
                "new-epoch",
                &Census { live_sessions: 1 },
            ),
            Err("replacement candidate reported a non-empty census")
        );
    }

    #[test]
    fn rm4d_expected_server_new_epoch_empty_census_passes_publication_gate() {
        assert_eq!(
            validate_replacement_admission(
                "server-a",
                Some("old-epoch"),
                "server-a",
                "new-epoch",
                &Census { live_sessions: 0 },
            ),
            Ok(())
        );
    }

    #[test]
    fn rm4d_identity_or_epoch_denial_never_receives_a_publication_permit() {
        for (server_id, epoch) in [("server-b", "new-epoch"), ("server-a", "old-epoch")] {
            assert!(validate_replacement_admission(
                "server-a",
                Some("old-epoch"),
                server_id,
                epoch,
                &Census { live_sessions: 0 },
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn rm4c_verified_v1_proof_publishes_unknown_count_without_backend_or_retry() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "legacy-123456".into(),
                Some(remote("legacy-123456", "legacy", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("legacy-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(manager.complete_unsupported_legacy(
            "legacy-123456",
            incarnation,
            generation,
            machines::LegacyHostProof::test_proof("server-a"),
        ));
        assert!(matches!(
            manager.state("legacy-123456"),
            Some(HostConnectionState::UnsupportedLegacy {
                count: LegacySessionCount::Unknown,
                ..
            })
        ));
        assert!(matches!(
            manager.connected_backend("legacy-123456"),
            Err(ConnectedBackendError::UnsupportedLegacy)
        ));
        assert!(manager
            .begin_attempt("legacy-123456", ConnectOrigin::Retry)
            .is_err());
        assert!(manager.retry_plan("legacy-123456", incarnation).is_none());
    }

    #[tokio::test]
    async fn rm4c_v1_production_path_publishes_proof_without_terminal_transport() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(VerifiedLegacyDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "legacy-path-123456".into(),
                Some(remote("legacy-path-123456", "legacy", false)),
            )
            .await;
        let error = manager
            .connect_remote_inner(
                None,
                "legacy-path-123456".into(),
                ConnectOrigin::ExplicitForeground,
            )
            .await
            .unwrap_err();
        assert!(error.contains("protocol 1"));
        assert!(matches!(
            manager.state("legacy-path-123456"),
            Some(HostConnectionState::UnsupportedLegacy {
                count: LegacySessionCount::Unknown,
                ..
            })
        ));
    }

    #[test]
    fn changed_epoch_commit_orders_old_teardown_before_replacement_exposure() {
        let observed = Mutex::new(Vec::new());
        let candidate = "publication-gated";
        let result = commit_changed_epoch(
            candidate,
            || {
                lock(&observed).push("reserved");
                true
            },
            || {
                lock(&observed).extend([
                    "old-exit",
                    "old-session-removed",
                    "layout-resync",
                    "server-restarted",
                ]);
                true
            },
            |candidate| {
                assert_eq!(candidate, "publication-gated");
                let mut observed = lock(&observed);
                // Adoption releases the real RemotePublicationGate before it
                // dispatches Connected, so model that exact visible order.
                observed.push("immediate-new-epoch-event");
                observed.push("connected");
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            *lock(&observed),
            [
                "reserved",
                "old-exit",
                "old-session-removed",
                "layout-resync",
                "server-restarted",
                "immediate-new-epoch-event",
                "connected",
            ]
        );

        lock(&observed).clear();
        let rejected = commit_changed_epoch(
            candidate,
            || false,
            || {
                lock(&observed).push("must-not-retire");
                true
            },
            |_| {
                lock(&observed).push("must-not-publish");
                Ok(())
            },
        );
        assert_eq!(rejected, Err(candidate));
        assert!(lock(&observed).is_empty());
    }

    #[test]
    fn priority_submitted_after_retirement_effects_does_not_strand_replacement() {
        let retirement_committed = AtomicBool::new(false);
        let late_priority = AtomicBool::new(false);
        let replacement_adopted = AtomicBool::new(false);
        let result = commit_changed_epoch(
            "replacement",
            || true,
            || {
                retirement_committed.store(true, Ordering::SeqCst);
                // This intent linearizes after the one-shot permit and old
                // lifetime effects. It must close the newly adopted lifetime
                // through normal serialization, not strand an empty gate.
                late_priority.store(true, Ordering::SeqCst);
                true
            },
            |candidate| {
                assert_eq!(candidate, "replacement");
                assert!(retirement_committed.load(Ordering::SeqCst));
                assert!(late_priority.load(Ordering::SeqCst));
                replacement_adopted.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert!(replacement_adopted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn every_priority_submission_after_reservation_denies_retirement_permit() {
        for winner in ["disconnect", "removal", "retry", "shutdown"] {
            let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-123456".into(),
                    Some(remote("box-123456", winner, false)),
                )
                .await;
            let (incarnation, generation, _, _) = manager
                .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
                .unwrap();
            assert!(manager.reserve_epoch_retirement(
                "box-123456",
                incarnation,
                generation,
                manager.observation_seq.load(Ordering::SeqCst),
            ));
            match winner {
                "disconnect" => manager.disconnect("box-123456").await.unwrap(),
                "removal" => {
                    manager
                        .observe_mutation(manager.reserve_observation(), "box-123456".into(), None)
                        .await;
                }
                "retry" => {
                    manager
                        .begin_attempt("box-123456", ConnectOrigin::Retry)
                        .unwrap();
                }
                "shutdown" => assert!(manager.begin_shutdown()),
                _ => unreachable!(),
            }
            let app = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app");
            app.manage(super::super::pty_host::StreamIds::default());
            let streams = app.state::<super::super::pty_host::StreamIds>();
            let old_namespace =
                crate::terminal::session_server::EventNamespace::for_test("server-a", "old", 81);
            let old_host = super::super::pty_host::StreamHost::remote_namespace(&old_namespace);
            streams.attach(
                old_host.clone(),
                &codesense_pty_core::domain::PtySessionId::new("old-session"),
                &codesense_pty_core::domain::ViewportId::new("viewport"),
                &codesense_pty_core::domain::SubscribeResult {
                    snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
                    stream_id: codesense_pty_core::term_state::StreamId(1),
                    story_id: None,
                    workspace_id: None,
                    epic_id: None,
                },
            );
            let sink = ConnectedSink::default();
            let replacement_publications = AtomicU64::new(0);
            let result = commit_changed_epoch(
                (),
                || true,
                || {
                    super::super::pty_backend::retire_remote_lifetime(
                        app.handle(),
                        &sink,
                        &old_namespace,
                        RemoteSessionLedger::default(),
                        || {
                            manager.take_epoch_retirement_permit(
                                "box-123456",
                                incarnation,
                                generation,
                            )
                        },
                    )
                },
                |_| {
                    replacement_publications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            );
            assert_eq!(result, Err(()), "{winner}");
            assert_eq!(streams.attached_on(old_host).len(), 1, "{winner}");
            assert!(lock(&sink.events).is_empty(), "{winner}");
            assert_eq!(
                replacement_publications.load(Ordering::SeqCst),
                0,
                "{winner}"
            );
        }
    }

    #[tokio::test]
    async fn retirement_permit_is_one_shot() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "permit", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(manager.reserve_epoch_retirement(
            "box-123456",
            incarnation,
            generation,
            manager
                .machine_observation_authority("box-123456")
                .unwrap()
                .0,
        ));
        assert!(manager.take_epoch_retirement_permit("box-123456", incarnation, generation));
        assert!(!manager.take_epoch_retirement_permit("box-123456", incarnation, generation));
    }

    #[tokio::test]
    async fn only_target_machine_observation_invalidates_epoch_commit() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "generation", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        let validated = manager
            .machine_observation_authority("box-123456")
            .unwrap()
            .0;
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-other-123456".into(),
                Some(remote("box-other-123456", "unrelated", false)),
            )
            .await;
        assert!(manager.reserve_epoch_retirement("box-123456", incarnation, generation, validated,));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "changed-generation", false)),
            )
            .await;
        assert!(!manager.reserve_epoch_retirement(
            "box-123456",
            incarnation,
            generation,
            validated,
        ));
    }

    #[derive(Default)]
    struct ManualScheduler {
        sleepers: Mutex<VecDeque<(Duration, tokio::sync::oneshot::Sender<()>)>>,
    }

    impl ManualScheduler {
        fn fire(&self, expected: Duration) {
            let (actual, wake) = lock(&self.sleepers)
                .pop_front()
                .expect("a virtual-time sleeper");
            assert_eq!(actual, expected);
            let _ = wake.send(());
        }

        fn pending(&self) -> usize {
            lock(&self.sleepers).len()
        }
    }

    impl ConnectionScheduler for ManualScheduler {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let (wake, sleep) = tokio::sync::oneshot::channel();
            lock(&self.sleepers).push_back((duration, wake));
            Box::pin(async move {
                let _ = sleep.await;
            })
        }
    }

    struct NeverOpenDriver;
    struct VerifiedLegacyDriver;

    async fn assert_process_reaped(pid: libc::pid_t) {
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } == -1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for NeverOpenDriver {
        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            std::future::pending().await
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            unreachable!("the open phase never completes")
        }
    }

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for VerifiedLegacyDriver {
        async fn legacy_proof(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<Option<machines::LegacyHostProof>, String> {
            Ok(Some(machines::LegacyHostProof::test_proof("server-a")))
        }

        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            Err(machines::PinnedSshOpenError::LegacyProtocolCandidate(
                "framed server is absent".into(),
            ))
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            unreachable!("legacy proof never constructs a terminal transport")
        }
    }

    use codesense_host_proto::{Census, Hello};
    use codesense_pty_core::domain::{
        PortUnavailable, SubscribeResult, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
        WorkspaceKey,
    };
    use codesense_pty_core::protocol::{Egress, Message, Reply};

    fn remote(id: &str, fingerprint: &str, auto_connect: bool) -> MachineManagerRecord {
        MachineManagerRecord {
            machine_id: id.into(),
            scope: Some(super::super::machines::registry::Scope::Personal),
            revision: None,
            fingerprint: fingerprint.into(),
            set_up: true,
            auto_connect,
            open_tabs: false,
        }
    }

    fn revision(
        path: &str,
        contents: &[u8],
        modified_nanos: u128,
    ) -> super::super::machines::registry::RegistryRevision {
        super::super::machines::registry::RegistryRevision {
            declaration: Some(super::super::machines::registry::FileRevision {
                path: path.into(),
                device: 1,
                inode: modified_nanos as u64,
                modified_nanos,
                len: contents.len() as u64,
                contents: contents.to_vec(),
            }),
            bindings: None,
        }
    }

    #[test]
    fn backend_snapshot_is_one_lock_typed_and_requires_matching_admitted_identity() {
        let manager = HostConnectionManager::default();
        assert!(matches!(
            manager.connected_backend("missing"),
            Err(ConnectedBackendError::UnknownMachine)
        ));
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::Disconnected)
        ));
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(LOCAL_MACHINE_ID).unwrap();
            gate.state = HostConnectionState::Connecting;
        }
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::Connecting)
        ));
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(LOCAL_MACHINE_ID).unwrap();
            gate.state = HostConnectionState::Handshaking;
        }
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::Handshaking)
        ));
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(LOCAL_MACHINE_ID).unwrap();
            gate.state = HostConnectionState::Unreachable {
                last_error: "auth refused".into(),
            };
        }
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::Unreachable { last_error })
                if last_error == "auth refused"
        ));
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(LOCAL_MACHINE_ID).unwrap();
            gate.state = HostConnectionState::Connected {
                server_id: "unbacked".into(),
            };
        }
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::IdentityMismatch)
        ));
        assert!(manager.begin_shutdown());
        assert!(matches!(
            manager.connected_backend(LOCAL_MACHINE_ID),
            Err(ConnectedBackendError::ManagerShuttingDown)
        ));
    }

    struct BlockingRealOwnerDriver {
        entered: tokio::sync::Semaphore,
        release: tokio::sync::Semaphore,
        pid: AtomicI32,
        failure: PinnedTransportGenerationError,
    }

    struct CheckTwoRealOwnerDriver {
        _root: tempfile::TempDir,
        program: std::path::PathBuf,
        entered: tokio::sync::Semaphore,
        release: tokio::sync::Semaphore,
        pid: AtomicI32,
        failure: PinnedTransportGenerationError,
        seen_generation: Mutex<Option<String>>,
    }

    #[derive(Default)]
    struct ConnectedSink {
        events: Mutex<Vec<TerminalDomainEvent>>,
        envelopes: Mutex<Vec<crate::terminal::session_server::NamespacedTerminalEvent>>,
        fail_stream: AtomicBool,
    }

    impl TerminalEventSink for ConnectedSink {
        fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            let stream = matches!(event, TerminalDomainEvent::StreamOutput { .. });
            lock(&self.events).push(event);
            if stream && self.fail_stream.load(Ordering::SeqCst) {
                return Err(PortUnavailable("injected pane stream failure".into()));
            }
            Ok(())
        }
    }

    impl crate::terminal::session_server::NamespacedTerminalEventSink for ConnectedSink {
        fn emit(
            &self,
            envelope: crate::terminal::session_server::NamespacedTerminalEvent,
        ) -> Result<(), PortUnavailable> {
            lock(&self.envelopes).push(envelope.clone());
            TerminalEventSink::emit(self, envelope.event)
        }
    }

    struct ConnectedRealOwnerDriver {
        _root: tempfile::TempDir,
        program: std::path::PathBuf,
        pids: Mutex<Vec<libc::pid_t>>,
        sink: Arc<ConnectedSink>,
        close_gate: Arc<super::super::host_connection::TestCloseGate>,
        loss_dispatcher: Mutex<Option<OnLost>>,
        eof_before_install_trigger: Option<std::path::PathBuf>,
        greetings: Mutex<BTreeMap<String, String>>,
        close_gates: Mutex<Vec<Arc<super::super::host_connection::TestCloseGate>>>,
        attempt_roots: Mutex<Vec<tempfile::TempDir>>,
        closing: AtomicBool,
        private_lease: Mutex<Option<super::super::auth::private_authority::PrivateAuthorityLease>>,
        replacement_proof_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    }

    impl ConnectedRealOwnerDriver {
        fn new() -> Self {
            Self::with_replacement_support(false)
        }

        fn with_replacement_support(supported: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let greeting = root.path().join("greeting.bin");
            let hello = Hello {
                proto_major: codesense_host_proto::PROTO_MAJOR,
                proto_minor: codesense_host_proto::PROTO_MINOR,
                agent_version: "test".into(),
                server_id: "test-server".into(),
                server_name: "test".into(),
                epoch: "epoch".into(),
                rev: 1,
                instance_id: "/remote/root".into(),
                capabilities: if supported {
                    vec![
                        codesense_host_proto::Capability::Terminals,
                        codesense_host_proto::Capability::ShutdownForReplacement,
                    ]
                } else {
                    vec![codesense_host_proto::Capability::Terminals]
                },
                viewer_id: None,
                pid: if supported { 42 } else { 7 },
                emulator: Some(codesense_host_proto::EmulatorHello::current()),
                build: None,
                client_id: None,
            };
            let mut wire = hello.frame().unwrap().encode();
            wire.extend(Census { live_sessions: 0 }.frame().encode());
            std::fs::write(&greeting, wire).unwrap();
            let client_bytes = codesense_pty_core::protocol::client_hello(
                hello.instance_id.clone(),
                env!("CARGO_PKG_VERSION"),
                &crate::commands::pty_layout::app_viewer(),
            )
            .frame()
            .unwrap()
            .encode()
            .len()
                + codesense_host_proto::Frame::control(&Message::Call {
                    rid: 1,
                    request: codesense_pty_core::protocol::Request::Listen,
                })
                .unwrap()
                .encode()
                .len();
            let reply = root.path().join("listen-reply.bin");
            std::fs::write(
                &reply,
                codesense_host_proto::Frame::control(&Message::Reply {
                    rid: 1,
                    result: Ok(Reply::Done),
                })
                .unwrap()
                .encode(),
            )
            .unwrap();
            let program = root.path().join("connected-host");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ntrap '' TERM HUP INT\ncat '{}'\ndd bs=1 count={} of=/dev/null 2>/dev/null\ncat '{}'\ncat >/dev/null\n",
                    greeting.display(), client_bytes,
                    reply.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _root: root,
                program,
                pids: Mutex::new(Vec::new()),
                sink: Arc::default(),
                close_gate: super::super::host_connection::TestCloseGate::new(),
                loss_dispatcher: Mutex::new(None),
                eof_before_install_trigger: None,
                greetings: Mutex::default(),
                close_gates: Mutex::default(),
                attempt_roots: Mutex::default(),
                closing: AtomicBool::new(false),
                private_lease: Mutex::new(None),
                replacement_proof_hook: Mutex::new(None),
            }
        }

        fn ending_before_install() -> Self {
            let mut driver = Self::new();
            let trigger = driver._root.path().join("allow-eof");
            let script = std::fs::read_to_string(&driver.program).unwrap();
            std::fs::write(
                &driver.program,
                script.replace(
                    "cat >/dev/null",
                    &format!(
                        "while test ! -e '{}'; do sleep 0.01; done\nexit 0",
                        trigger.display()
                    ),
                ),
            )
            .unwrap();
            driver.eof_before_install_trigger = Some(trigger);
            driver
        }

        fn with_failed_pane_and_healthy_control() -> Self {
            let root = tempfile::tempdir().unwrap();
            let hello = Hello {
                proto_major: codesense_host_proto::PROTO_MAJOR,
                proto_minor: codesense_host_proto::PROTO_MINOR,
                agent_version: "test".into(),
                server_id: "test-server".into(),
                server_name: "test".into(),
                epoch: "epoch".into(),
                rev: 1,
                instance_id: "/remote/root".into(),
                capabilities: vec![codesense_host_proto::Capability::Terminals],
                viewer_id: None,
                pid: 7,
                emulator: Some(codesense_host_proto::EmulatorHello::current()),
                build: None,
                client_id: None,
            };
            let greeting = root.path().join("greeting.bin");
            let mut wire = hello.frame().unwrap().encode();
            wire.extend(Census { live_sessions: 0 }.frame().encode());
            std::fs::write(&greeting, wire).unwrap();
            let listen = root.path().join("listen.bin");
            std::fs::write(
                &listen,
                codesense_host_proto::Frame::control(&Message::Reply {
                    rid: 1,
                    result: Ok(Reply::Done),
                })
                .unwrap()
                .encode(),
            )
            .unwrap();
            let pane = root.path().join("pane.bin");
            let mut pane_wire = codesense_host_proto::Frame::control(&Message::Reply {
                rid: 2,
                result: Ok(Reply::Subscribed {
                    result: Box::new(SubscribeResult {
                        snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
                        stream_id: codesense_pty_core::term_state::StreamId(41),
                        story_id: None,
                        workspace_id: None,
                        epic_id: None,
                    }),
                }),
            })
            .unwrap()
            .encode();
            pane_wire.extend(
                Egress::default()
                    .frames(TerminalDomainEvent::StreamOutput {
                        stream: codesense_pty_core::term_state::StreamId(41),
                        data: "pane ended".into(),
                        seq: 1,
                    })
                    .unwrap()
                    .into_iter()
                    .flat_map(|frame| frame.encode()),
            );
            std::fs::write(&pane, pane_wire).unwrap();
            let heartbeat = root.path().join("heartbeat.bin");
            std::fs::write(
                &heartbeat,
                codesense_host_proto::Frame::control(&Message::Reply {
                    rid: 3,
                    result: Ok(Reply::Sessions {
                        sessions: Default::default(),
                    }),
                })
                .unwrap()
                .encode(),
            )
            .unwrap();
            let program = root.path().join("connected-host");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ntrap '' TERM HUP INT\ncat '{}'\nsleep 0.1\ncat '{}'\nsleep 0.2\ncat '{}'\nsleep 0.2\ncat '{}'\ncat >/dev/null\n",
                    greeting.display(), listen.display(), pane.display(), heartbeat.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let sink = Arc::new(ConnectedSink::default());
            sink.fail_stream.store(true, Ordering::SeqCst);
            Self {
                _root: root,
                program,
                pids: Mutex::new(Vec::new()),
                sink,
                close_gate: super::super::host_connection::TestCloseGate::new(),
                loss_dispatcher: Mutex::new(None),
                eof_before_install_trigger: None,
                greetings: Mutex::default(),
                close_gates: Mutex::default(),
                attempt_roots: Mutex::default(),
                closing: AtomicBool::new(false),
                private_lease: Mutex::new(None),
                replacement_proof_hook: Mutex::new(None),
            }
        }

        fn latest_pid(&self) -> libc::pid_t {
            *lock(&self.pids).last().expect("opened remote process")
        }

        fn greet(&self, machine: &str, epoch: &str) {
            lock(&self.greetings).insert(machine.into(), epoch.into());
        }

        fn prepare_greeting(&self, machine: &str) -> std::path::PathBuf {
            let Some(epoch) = lock(&self.greetings).get(machine).cloned() else {
                return self.program.clone();
            };
            let root = tempfile::tempdir_in(self._root.path()).unwrap();
            let greeting = root.path().join("greeting.bin");
            let reply_path = root.path().join("listen-reply.bin");
            let hello = Hello {
                proto_major: codesense_host_proto::PROTO_MAJOR,
                proto_minor: codesense_host_proto::PROTO_MINOR,
                agent_version: "test".into(),
                server_id: "test-server".into(),
                server_name: machine.into(),
                epoch: epoch.clone(),
                rev: 1,
                instance_id: "/remote/root".into(),
                capabilities: vec![codesense_host_proto::Capability::Terminals],
                viewer_id: None,
                pid: 7,
                emulator: Some(codesense_host_proto::EmulatorHello::current()),
                build: None,
                client_id: None,
            };
            let mut wire = hello.frame().unwrap().encode();
            wire.extend(Census { live_sessions: 0 }.frame().encode());
            std::fs::write(&greeting, wire).unwrap();
            // The reader sees a real event before Listen completes. Construction
            // therefore returns with this event waiting behind publication.
            let mut reply = Vec::new();
            for frame in Egress::default()
                .frames(TerminalDomainEvent::LayoutChanged {
                    epoch,
                    rev: 2,
                    workspace: WorkspaceKey::new(machine),
                    layout: None,
                })
                .unwrap()
            {
                reply.extend(frame.encode());
            }
            reply.extend(
                codesense_host_proto::Frame::control(&Message::Reply {
                    rid: 1,
                    result: Ok(Reply::Done),
                })
                .unwrap()
                .encode(),
            );
            std::fs::write(&reply_path, reply).unwrap();
            let program = root.path().join("connected-host");
            let script = std::fs::read_to_string(&self.program)
                .unwrap()
                .replace(
                    self._root.path().join("greeting.bin").to_str().unwrap(),
                    greeting.to_str().unwrap(),
                )
                .replace(
                    self._root.path().join("listen-reply.bin").to_str().unwrap(),
                    reply_path.to_str().unwrap(),
                );
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            lock(&self.attempt_roots).push(root);
            program
        }

        fn release_close_gates(&self) {
            self.closing.store(true, Ordering::SeqCst);
            self.close_gate.release();
            for gate in lock(&self.close_gates).iter() {
                gate.release();
            }
        }
    }

    impl Drop for ConnectedRealOwnerDriver {
        fn drop(&mut self) {
            self.release_close_gates();
        }
    }

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for ConnectedRealOwnerDriver {
        async fn legacy_proof(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<Option<machines::LegacyHostProof>, String> {
            if let Some(hook) = lock(&self.replacement_proof_hook).as_ref() {
                hook();
            }
            Ok(Some(machines::LegacyHostProof::test_proof("test-server")))
        }

        fn install_loss_dispatcher(
            &self,
            client: &ManagedRemoteClient,
            dispatcher: Option<OnLost>,
        ) -> Result<bool, String> {
            if let Some(trigger) = &self.eof_before_install_trigger {
                std::fs::write(trigger, []).map_err(|error| error.to_string())?;
                let deadline = Instant::now() + Duration::from_secs(10);
                while client.domain_client().is_alive() {
                    if Instant::now() >= deadline {
                        panic!("fake host reader did not observe EOF within 10 seconds before dispatcher installation");
                    }
                    std::thread::yield_now();
                }
            }
            let dispatcher = dispatcher
                .or_else(|| lock(&self.loss_dispatcher).clone())
                .unwrap_or_else(|| Arc::new(|| {}));
            Ok(install_remote_loss_dispatcher(client, dispatcher))
        }

        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            let program = self.prepare_greeting(&machine_id);
            let (mut transport, pid) = super::super::machines::test_pinned_transport(&program)?;
            transport.private_lease = lock(&self.private_lease).clone();
            lock(&self.pids).push(pid);
            let gate = if lock(&self.greetings).is_empty() {
                self.close_gate.clone()
            } else {
                super::super::host_connection::TestCloseGate::new()
            };
            lock(&self.close_gates).push(gate.clone());
            if self.closing.load(Ordering::SeqCst) {
                gate.release();
            }
            Ok(RemoteTransportOwner::from_pinned_with_close_gate(
                transport, gate,
            ))
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            Ok(())
        }

        async fn revalidate_before_publication(
            &self,
            _admitted: &AdmittedRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            Ok(())
        }

        async fn construct_client(
            &self,
            admitted: AdmittedRemoteTransport,
            _app: Option<tauri::AppHandle>,
            ledger: RemoteSessionLedger,
        ) -> Result<ManagedRemoteClient, String> {
            SessionServerState::default()
                .construct_remote_client(admitted, self.sink.clone(), ledger)
                .await
                .map_err(|error| error.to_string())
        }
    }

    impl CheckTwoRealOwnerDriver {
        fn new(failure: PinnedTransportGenerationError) -> Self {
            let root = tempfile::tempdir().unwrap();
            let greeting = root.path().join("greeting.bin");
            let hello = Hello {
                proto_major: codesense_host_proto::PROTO_MAJOR,
                proto_minor: codesense_host_proto::PROTO_MINOR,
                agent_version: "test".into(),
                server_id: "test-server".into(),
                server_name: "test".into(),
                epoch: "epoch".into(),
                rev: 1,
                instance_id: "/remote/root".into(),
                capabilities: vec![codesense_host_proto::Capability::Terminals],
                viewer_id: None,
                pid: 7,
                emulator: Some(codesense_host_proto::EmulatorHello::current()),
                build: None,
                client_id: None,
            };
            let client_bytes = codesense_pty_core::protocol::client_hello(
                hello.instance_id.clone(),
                env!("CARGO_PKG_VERSION"),
                &crate::commands::pty_layout::app_viewer(),
            )
            .frame()
            .unwrap()
            .encode()
            .len()
                + codesense_host_proto::Frame::control(&Message::Call {
                    rid: 1,
                    request: codesense_pty_core::protocol::Request::Listen,
                })
                .unwrap()
                .encode()
                .len();
            let mut wire = hello.frame().unwrap().encode();
            wire.extend(Census { live_sessions: 0 }.frame().encode());
            std::fs::write(&greeting, wire).unwrap();
            let tail = root.path().join("listen-and-push.bin");
            let mut tail_wire = codesense_host_proto::Frame::control(&Message::Reply {
                rid: 1,
                result: Ok(Reply::Done),
            })
            .unwrap()
            .encode();
            for frame in Egress::default()
                .frames(TerminalDomainEvent::LayoutChanged {
                    epoch: "immediate".into(),
                    rev: 2,
                    workspace: WorkspaceKey::new("remote"),
                    layout: None,
                })
                .unwrap()
            {
                tail_wire.extend(frame.encode());
            }
            std::fs::write(&tail, tail_wire).unwrap();
            let program = root.path().join("fake-ssh");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ncat '{}'\ndd bs=1 count={} of=/dev/null 2>/dev/null\ncat '{}'\ncat >/dev/null\n",
                    greeting.display(),
                    client_bytes,
                    tail.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _root: root,
                program,
                entered: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
                pid: AtomicI32::new(0),
                failure,
                seen_generation: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for CheckTwoRealOwnerDriver {
        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            let (transport, pid) = super::super::machines::test_pinned_transport(&self.program)?;
            self.pid.store(pid, Ordering::SeqCst);
            Ok(RemoteTransportOwner::from_pinned(transport))
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            Ok(())
        }

        async fn revalidate_before_publication(
            &self,
            _admitted: &AdmittedRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            Ok(())
        }

        async fn revalidate_after_construction(
            &self,
            generation: String,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            *lock(&self.seen_generation) = Some(generation);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Err(self.failure.clone())
        }

        async fn construct_client(
            &self,
            admitted: AdmittedRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _ledger: RemoteSessionLedger,
        ) -> Result<ManagedRemoteClient, String> {
            crate::terminal::session_server::SessionServerClient::from_remote_admitted(
                admitted,
                Arc::new(ConnectedSink::default()),
                Arc::default(),
                None,
            )
            .await
            .map_err(|error| error.to_string())
        }
    }

    #[async_trait::async_trait]
    impl RemoteAttemptDriver for BlockingRealOwnerDriver {
        async fn open(
            &self,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
            let (transport, pid) =
                super::super::machines::test_pinned_transport(std::path::Path::new("/bin/cat"))?;
            self.pid.store(pid, Ordering::SeqCst);
            Ok(RemoteTransportOwner::from_pinned(transport))
        }

        async fn revalidate_before_handshake(
            &self,
            _pending: &PendingRemoteTransport,
            _app: Option<tauri::AppHandle>,
            _machine_id: String,
        ) -> Result<(), PinnedTransportGenerationError> {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Err(self.failure.clone())
        }
    }

    #[tokio::test]
    async fn reverse_observation_completion_is_per_machine() {
        let manager = HostConnectionManager::default();
        let t1 = manager.reserve_observation();
        let t2 = manager.reserve_observation();
        let t3 = manager.reserve_observation();
        manager
            .observe_external(
                t3,
                vec![remote("box-123456", "watch", false)],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        manager
            .observe_mutation(
                t2,
                "other-123456".into(),
                Some(remote("other-123456", "other", false)),
            )
            .await;
        manager
            .observe_external(
                t1,
                vec![remote("box-123456", "scan", false)],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        assert_eq!(
            lock(&manager.core).machines["box-123456"].fingerprint,
            "watch"
        );
        assert!(lock(&manager.core).machines.contains_key(LOCAL_MACHINE_ID));
    }

    #[tokio::test]
    async fn ambiguous_same_fingerprint_event_barriers_only_affected_machine() {
        let manager = HostConnectionManager::default();
        let first = manager.reserve_observation();
        manager
            .observe_mutation(
                first,
                "box-123456".into(),
                Some(remote("box-123456", "same", false)),
            )
            .await;
        let second = manager.reserve_observation();
        manager
            .observe_mutation(
                second,
                "other-123456".into(),
                Some(remote("other-123456", "same", false)),
            )
            .await;
        let (before_a, before_b, before_local) = {
            let core = lock(&manager.core);
            (
                core.machines["box-123456"].incarnation,
                core.machines["other-123456"].incarnation,
                core.machines[LOCAL_MACHINE_ID].incarnation,
            )
        };
        let watch = manager.reserve_observation();
        manager
            .observe_external(
                watch,
                vec![
                    remote("box-123456", "same", false),
                    remote("other-123456", "same", false),
                ],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        let core = lock(&manager.core);
        assert_ne!(before_a, core.machines["box-123456"].incarnation);
        assert_eq!(before_b, core.machines["other-123456"].incarnation);
        assert_eq!(before_local, core.machines[LOCAL_MACHINE_ID].incarnation);
    }

    #[tokio::test]
    async fn two_observers_see_commits_in_order_under_reentrancy() {
        let manager = Arc::new(HostConnectionManager::default());
        let seq = manager.reserve_observation();
        manager
            .observe_mutation(
                seq,
                "box-123456".into(),
                Some(remote("box-123456", "a", false)),
            )
            .await;
        // Initial admission was committed before these subscribers existed.
        // Anchor to that real sequence without resetting production history.
        let first_order = manager.event_seq.load(Ordering::SeqCst) + 1;
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let weak = Arc::downgrade(&manager);
        let first = deliveries.clone();
        manager.subscribe(Arc::new(move |event| {
            lock(&first).push(("a", event.order));
            if event.order == first_order {
                let manager = weak.upgrade().unwrap();
                let mut core = lock(&manager.core);
                manager.enqueue_locked(&mut core, "box-123456");
                drop(core);
                manager.dispatch();
            }
        }));
        let second = deliveries.clone();
        manager.subscribe(Arc::new(move |event| {
            lock(&second).push(("b", event.order))
        }));
        manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert_eq!(
            &*lock(&deliveries),
            &[
                ("a", first_order),
                ("b", first_order),
                ("a", first_order + 1),
                ("b", first_order + 1)
            ]
        );
    }

    #[tokio::test]
    async fn shutdown_is_single_drainer_and_timeout_cannot_mark_drained() {
        let manager = Arc::new(HostConnectionManager::default());
        assert!(manager.begin_shutdown());
        assert!(!manager.begin_shutdown());
        assert_eq!(manager.shutdown_phase(), ShutdownPhase::Draining);
        manager.active_operations.store(1, Ordering::SeqCst);
        let draining = manager.clone();
        let mut drain = tokio::spawn(async move { draining.drain_shutdown().await });
        assert!(tokio::time::timeout(Duration::ZERO, &mut drain)
            .await
            .is_err());
        assert_eq!(manager.shutdown_phase(), ShutdownPhase::Draining);
        manager.active_operations.store(0, Ordering::SeqCst);
        manager.operations_changed.notify_waiters();
        drain.await.unwrap();
        assert_eq!(manager.shutdown_phase(), ShutdownPhase::Drained);
    }

    #[tokio::test]
    async fn replacement_waits_for_old_generation_owner_to_finish() {
        let manager = HostConnectionManager::default();
        let seq = manager.reserve_observation();
        manager
            .observe_mutation(
                seq,
                "box-123456".into(),
                Some(remote("box-123456", "a", false)),
            )
            .await;
        let (_, first_generation, first_token, serial) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        let first_owner = serial.lock().await;
        let (_, second_generation, _, second_serial) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(first_token.is_cancelled());
        assert_ne!(first_generation, second_generation);
        assert!(second_serial.try_lock().is_err());
        drop(first_owner);
        assert!(second_serial.try_lock().is_ok());
    }

    #[tokio::test]
    async fn disconnect_waits_for_active_owner_before_event() {
        let manager = HostConnectionManager::default();
        let seq = manager.reserve_observation();
        manager
            .observe_mutation(
                seq,
                "box-123456".into(),
                Some(remote("box-123456", "a", true)),
            )
            .await;
        let (_, _, _, serial) = manager
            .begin_attempt("box-123456", ConnectOrigin::Auto)
            .unwrap();
        let owner = serial.lock().await;
        let disconnect = manager.disconnect("box-123456");
        tokio::pin!(disconnect);
        assert!(tokio::time::timeout(Duration::ZERO, &mut disconnect)
            .await
            .is_err());
        assert!(manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .is_err());
        drop(owner);
        disconnect.await.unwrap();
        assert!(manager.auto_connect_candidates().is_empty());
    }

    #[tokio::test]
    async fn submitted_priority_barriers_reject_all_six_stale_completion_races() {
        async fn seeded() -> HostConnectionManager {
            let manager = HostConnectionManager::default();
            let seq = manager.reserve_observation();
            manager
                .observe_mutation(
                    seq,
                    "box-123456".into(),
                    Some(remote("box-123456", "a", false)),
                )
                .await;
            manager
        }

        // shutdown > completion
        let manager = seeded().await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(manager.begin_shutdown());
        assert!(!manager.complete_fail_closed(
            "box-123456",
            incarnation,
            generation,
            "late".into()
        ));

        // removal > completion, even before the remover acquires the gate.
        let manager = seeded().await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        lock(&manager.core).machines["box-123456"]
            .submitted
            .removal
            .fetch_add(1, Ordering::SeqCst);
        assert!(!manager.complete_fail_closed(
            "box-123456",
            incarnation,
            generation,
            "late".into()
        ));

        // Disconnect > Retry and Disconnect > completion.
        let manager = seeded().await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        lock(&manager.core).machines["box-123456"]
            .submitted
            .disconnect
            .fetch_add(1, Ordering::SeqCst);
        assert!(manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .is_err());
        assert!(!manager.complete_fail_closed(
            "box-123456",
            incarnation,
            generation,
            "late".into()
        ));

        // Retry > the prior completion; the new generation > the old one.
        let manager = seeded().await;
        let (incarnation, old_generation, old_token, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        let (_, new_generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .unwrap();
        assert!(old_token.is_cancelled());
        assert_ne!(old_generation, new_generation);
        assert!(!manager.complete_fail_closed(
            "box-123456",
            incarnation,
            old_generation,
            "late".into()
        ));
        assert!(manager.complete_fail_closed(
            "box-123456",
            incarnation,
            new_generation,
            "current".into()
        ));
    }

    #[tokio::test]
    async fn dropped_echo_then_same_byte_remove_readd_barriers_only_affected_id() {
        let manager = HostConnectionManager::default();
        let mutation = manager.reserve_observation();
        manager
            .observe_mutation(
                mutation,
                "box-123456".into(),
                Some(remote("box-123456", "same", true)),
            )
            .await;
        let other = manager.reserve_observation();
        manager
            .observe_mutation(
                other,
                "other-123456".into(),
                Some(remote("other-123456", "same", false)),
            )
            .await;
        let (old_a, old_b, old_local) = {
            let core = lock(&manager.core);
            (
                core.machines["box-123456"].incarnation,
                core.machines["other-123456"].incarnation,
                core.machines[LOCAL_MACHINE_ID].incarnation,
            )
        };

        // The original self-write echo was dropped. A later ambiguous event
        // with identical bytes is therefore treated as remove/readd, never as
        // a receipt that can preserve A's actor.
        let external = manager.reserve_observation();
        manager
            .observe_external(
                external,
                vec![
                    remote("box-123456", "same", true),
                    remote("other-123456", "same", false),
                ],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        let core = lock(&manager.core);
        assert_ne!(old_a, core.machines["box-123456"].incarnation);
        assert_eq!(old_b, core.machines["other-123456"].incarnation);
        assert_eq!(old_local, core.machines[LOCAL_MACHINE_ID].incarnation);
    }

    #[tokio::test]
    async fn scope_change_is_an_identity_barrier() {
        let manager = HostConnectionManager::default();
        let first = manager.reserve_observation();
        manager
            .observe_mutation(
                first,
                "box-123456".into(),
                Some(remote("box-123456", "personal", false)),
            )
            .await;
        let old = lock(&manager.core).machines["box-123456"].incarnation;
        let mut shared = remote("box-123456", "team", false);
        shared.scope = Some(super::super::machines::registry::Scope::Team {
            org_slug: "acme/context".into(),
            team: "platform".into(),
        });
        let second = manager.reserve_observation();
        manager
            .observe_external(
                second,
                vec![shared],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        assert_ne!(old, lock(&manager.core).machines["box-123456"].incarnation);
    }

    #[tokio::test]
    async fn broad_directory_event_diffs_both_directions_and_removes_missing_remote() {
        let manager = HostConnectionManager::default();
        let initial = manager.reserve_observation();
        manager
            .observe_mutation(
                initial,
                "box-123456".into(),
                Some(remote("box-123456", "old", false)),
            )
            .await;
        let batch = manager.reserve_observation();
        manager
            .observe_external(
                batch,
                vec![remote("other-123456", "new", false)],
                BTreeSet::new(),
                true,
                Vec::new(),
            )
            .await;
        assert!(manager.state("box-123456").is_none());
        assert_eq!(
            manager.state("other-123456"),
            Some(HostConnectionState::Disconnected)
        );
        assert_eq!(
            manager.state(LOCAL_MACHINE_ID),
            Some(HostConnectionState::Disconnected)
        );
    }

    #[tokio::test]
    async fn a_machine_with_open_tabs_at_quit_reconnects_at_launch() {
        let manager = HostConnectionManager::default();
        let mut tabbed = remote("team-box-123456", "saved", false);
        tabbed.open_tabs = true;
        let idle = remote("idle-box-123456", "saved", false);
        for record in [tabbed, idle] {
            let seq = manager.reserve_observation();
            manager
                .observe_mutation(seq, record.machine_id.clone(), Some(record))
                .await;
        }
        assert_eq!(manager.auto_connect_candidates(), ["team-box-123456"]);
        assert!(
            manager.auto_connect_candidates().is_empty(),
            "asked once per launch"
        );
        let gate = &lock(&manager.core).machines["team-box-123456"];
        assert!(gate.auto_connect, "a dropped connection is retried quietly");
    }

    #[tokio::test]
    async fn tabs_opening_and_closing_keep_the_launch_bit_without_reconnecting() {
        let manager = HostConnectionManager::default();
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = written.clone();
        manager.set_open_tabs_writer(Arc::new(move |id, open| lock(&sink).push((id, open))));
        let seq = manager.reserve_observation();
        manager
            .observe_mutation(
                seq,
                "box-123456".into(),
                Some(remote("box-123456", "saved", false)),
            )
            .await;
        manager.note_remote_workspace_tabs("box-123456", "team|ws-1", 2);
        manager.note_remote_workspace_tabs("box-123456", "team|ws-2", 1);
        manager.note_remote_workspace_tabs("box-123456", "team|ws-1", 0);
        assert_eq!(*lock(&written), [("box-123456".to_string(), true)]);
        manager.note_remote_workspace_tabs("box-123456", "team|ws-2", 0);
        assert_eq!(
            lock(&written).last(),
            Some(&("box-123456".to_string(), false))
        );

        // The registry echoes the kept bit: it is no reason to connect again.
        let mut echoed = remote("box-123456", "saved", false);
        echoed.open_tabs = true;
        let seq = manager.reserve_observation();
        manager
            .observe_mutation(seq, "box-123456".into(), Some(echoed))
            .await;
        assert!(manager.auto_connect_candidates().is_empty());
        manager.note_remote_workspace_tabs("unknown-123456", "team|ws-1", 1);
        assert_eq!(lock(&written).len(), 2);
    }

    #[tokio::test]
    async fn causal_self_echo_settles_exactly_one_auto_attempt_without_incarnation_thrash() {
        let manager = HostConnectionManager::default();
        let mut saved = remote("box-123456", "saved", true);
        let saved_revision = revision("/registry/box-123456.yaml", b"saved", 11);
        saved.revision = Some(saved_revision.clone());
        let mutation = manager.reserve_observation();
        manager
            .observe_mutation(mutation, "box-123456".into(), Some(saved.clone()))
            .await;
        let incarnation = lock(&manager.core).machines["box-123456"].incarnation;
        let echo = manager.reserve_observation();
        manager
            .observe_external(
                echo,
                vec![saved],
                BTreeSet::from(["box-123456".into()]),
                false,
                vec![saved_revision.declaration.unwrap()],
            )
            .await;
        assert_eq!(
            incarnation,
            lock(&manager.core).machines["box-123456"].incarnation
        );
        assert_eq!(manager.auto_connect_candidates(), ["box-123456"]);
        assert!(manager.auto_connect_candidates().is_empty());
    }

    #[tokio::test]
    async fn dropped_echo_bounded_settle_schedules_once_then_late_ambiguous_event_barriers() {
        let manager = HostConnectionManager::default();
        let mut saved = remote("box-123456", "saved", true);
        saved.revision = Some(revision("/registry/box-123456.yaml", b"saved", 11));
        let saved_revision = saved.revision.clone();
        let mutation = manager.reserve_observation();
        manager
            .observe_mutation(mutation, "box-123456".into(), Some(saved.clone()))
            .await;
        let first_incarnation = lock(&manager.core).machines["box-123456"].incarnation;
        assert_eq!(
            manager.settle_self_write("box-123456", mutation, &saved_revision),
            ["box-123456"]
        );
        assert!(manager
            .settle_self_write("box-123456", mutation, &saved_revision)
            .is_empty());

        let external = manager.reserve_observation();
        manager
            .observe_external(
                external,
                vec![saved],
                BTreeSet::from(["box-123456".into()]),
                false,
                Vec::new(),
            )
            .await;
        assert_ne!(
            first_incarnation,
            lock(&manager.core).machines["box-123456"].incarnation
        );
        assert_eq!(manager.auto_connect_candidates(), ["box-123456"]);
    }

    #[tokio::test]
    async fn stale_settle_timers_cannot_consume_newer_receipts_or_auto_intent() {
        let manager = HostConnectionManager::default();
        let mut first = remote("box-123456", "m1", true);
        first.revision = Some(revision("/registry/box-123456.yaml", b"m1", 1));
        let first_revision = first.revision.clone();
        let m1 = manager.reserve_observation();
        manager
            .observe_mutation(m1, "box-123456".into(), Some(first))
            .await;

        let mut second = remote("box-123456", "m2", true);
        let second_revision = revision("/registry/box-123456.yaml", b"m2", 2);
        second.revision = Some(second_revision.clone());
        let m2 = manager.reserve_observation();
        manager
            .observe_mutation(m2, "box-123456".into(), Some(second.clone()))
            .await;
        assert!(manager
            .settle_self_write("box-123456", m1, &first_revision)
            .is_empty());
        let echo = manager.reserve_observation();
        manager
            .observe_external(
                echo,
                vec![second],
                BTreeSet::from(["box-123456".into()]),
                false,
                vec![second_revision.declaration.clone().unwrap()],
            )
            .await;
        assert_eq!(manager.auto_connect_candidates(), ["box-123456"]);
        assert!(manager
            .settle_self_write("box-123456", m2, &Some(second_revision))
            .is_empty());

        let mut disabled = remote("box-123456", "off", false);
        disabled.revision = Some(revision("/registry/box-123456.yaml", b"off", 3));
        let disabled_revision = disabled.revision.clone();
        let m3 = manager.reserve_observation();
        manager
            .observe_mutation(m3, "box-123456".into(), Some(disabled))
            .await;
        assert!(manager
            .settle_self_write("box-123456", m3, &disabled_revision)
            .is_empty());
        let mut enabled = remote("box-123456", "on", true);
        enabled.revision = Some(revision("/registry/box-123456.yaml", b"on", 4));
        let enabled_revision = enabled.revision.clone();
        let m4 = manager.reserve_observation();
        manager
            .observe_mutation(m4, "box-123456".into(), Some(enabled))
            .await;
        assert_eq!(
            manager.settle_self_write("box-123456", m4, &enabled_revision),
            ["box-123456"]
        );
        assert!(manager
            .settle_self_write("box-123456", m3, &disabled_revision)
            .is_empty());
    }

    #[test]
    fn notify_event_adapter_distinguishes_exact_machine_and_broad_scope_events() {
        let root = tempfile::tempdir().unwrap();
        let machines = root.path().join("machines.d");
        std::fs::create_dir_all(&machines).unwrap();
        let declaration = machines.join("box-123456.yaml");
        std::fs::write(&declaration, b"saved").unwrap();
        let exact = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )))
        .add_path(declaration);
        let adapted = adapt_registry_event(&exact).unwrap();
        assert_eq!(adapted.hinted_ids, BTreeSet::from(["box-123456".into()]));
        assert!(!adapted.broad);
        assert_eq!(adapted.event_revisions.len(), 1);

        let broad = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Any,
        )))
        .add_path(machines);
        let adapted = adapt_registry_event(&broad).unwrap();
        assert!(adapted.hinted_ids.is_empty());
        assert!(adapted.broad);
    }

    #[tokio::test]
    async fn disconnect_core_waits_for_injected_real_owner_reap_and_zero_publication() {
        async fn prove(failure: PinnedTransportGenerationError) {
            let expected_error = failure.to_string();
            let driver = Arc::new(BlockingRealOwnerDriver {
                entered: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
                pid: AtomicI32::new(0),
                failure,
            });
            let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
            let seq = manager.reserve_observation();
            manager
                .observe_mutation(
                    seq,
                    "box-123456".into(),
                    Some(remote("box-123456", "saved", false)),
                )
                .await;
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
            let connecting = manager.clone();
            let connect = tokio::spawn(async move {
                connecting
                    .connect_remote_inner(
                        None,
                        "box-123456".into(),
                        ConnectOrigin::ExplicitForeground,
                    )
                    .await
            });
            driver.entered.acquire().await.unwrap().forget();
            let disconnecting = manager.clone();
            let mut disconnect =
                tokio::spawn(async move { disconnecting.disconnect("box-123456").await });
            assert!(tokio::time::timeout(Duration::ZERO, &mut disconnect)
                .await
                .is_err());
            driver.release.add_permits(1);
            assert_eq!(connect.await.unwrap().unwrap_err(), expected_error);
            disconnect.await.unwrap().unwrap();

            let pid = driver.pid.load(Ordering::SeqCst);
            assert!(pid > 0);
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            let events = lock(&events);
            assert!(events
                .iter()
                .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.state == HostConnectionState::Disconnected)
                    .count(),
                1
            );
        }

        prove(PinnedTransportGenerationError::Missing(
            "binding disappeared".into(),
        ))
        .await;
        prove(PinnedTransportGenerationError::Stale).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn check_two_missing_or_stale_with_immediate_push_never_publishes_and_truly_reaps() {
        async fn prove(failure: PinnedTransportGenerationError) {
            let expected_error = failure.to_string();
            let driver = Arc::new(CheckTwoRealOwnerDriver::new(failure));
            let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
            let seq = manager.reserve_observation();
            manager
                .observe_mutation(
                    seq,
                    "box-123456".into(),
                    Some(remote("box-123456", "saved", false)),
                )
                .await;
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));

            let connecting = manager.clone();
            let connect = tokio::spawn(async move {
                connecting
                    .connect_remote_inner(
                        None,
                        "box-123456".into(),
                        ConnectOrigin::ExplicitForeground,
                    )
                    .await
            });
            driver.entered.acquire().await.unwrap().forget();
            assert_eq!(
                lock(&driver.seen_generation).as_deref(),
                Some("test-generation"),
                "post-construction validation receives the admitted transport generation"
            );
            assert!(lock(&events)
                .iter()
                .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })));
            driver.release.add_permits(1);
            assert_eq!(connect.await.unwrap().unwrap_err(), expected_error);

            let pid = driver.pid.load(Ordering::SeqCst);
            assert!(pid > 0);
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            assert!(lock(&events)
                .iter()
                .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })));
        }

        prove(PinnedTransportGenerationError::Missing(
            "removed after greeting".into(),
        ))
        .await;
        prove(PinnedTransportGenerationError::Stale).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connected_real_owner_is_reaped_before_sync_or_external_barrier_ack_and_reconnect() {
        #[derive(Clone, Copy)]
        enum Barrier {
            SyncScopeChange,
            ExternalRemoval,
        }

        async fn assert_reaped(pid: libc::pid_t) {
            for _ in 0..100 {
                if unsafe { libc::kill(pid, 0) } == -1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }

        async fn prove(barrier: Barrier) {
            let driver = Arc::new(ConnectedRealOwnerDriver::new());
            let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-123456".into(),
                    Some(remote("box-123456", "initial", false)),
                )
                .await;
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-654321".into(),
                    Some(remote("box-654321", "independent", false)),
                )
                .await;
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
            manager
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            let first_pid = driver.latest_pid();
            let snapshot = manager.connected_backend("box-123456").unwrap();
            assert_eq!(snapshot.machine_id(), "box-123456");
            assert_eq!(snapshot.server_id(), "test-server");
            assert!(manager.revalidate_backend(&snapshot).is_ok());
            let resolved =
                super::super::pty_backend::resolve_remote_backend(&manager, "box-123456")
                    .expect("production remote shell resolver");
            let target = resolved
                .admission_target()
                .expect("native Create requires admission")
                .clone();
            assert_eq!(target.member.machine_id, snapshot.machine_id());
            assert_eq!(target.namespace, snapshot.client().namespace().into());
            assert_eq!(
                manager.target_for_namespace(snapshot.client().namespace()),
                Some(target.clone())
            );
            let admitted = manager.capture_admitted_backend(&target).unwrap();
            assert!(Arc::ptr_eq(&resolved.client().unwrap(), &admitted.client()));
            assert!(Arc::ptr_eq(&resolved.client().unwrap(), &snapshot.client()));
            let captured = snapshot.client();
            let original_namespace = captured.namespace().clone();
            let original_generation = lock(&manager.core).machines["box-123456"]
                .connected_generation
                .unwrap();
            let original_live = {
                let events = lock(&events);
                let live: Vec<_> = events
                    .iter()
                    .filter(|event| event.machine_id == "box-123456")
                    .filter_map(|event| event.admission.as_ref())
                    .filter(|row| matches!(row.state, RuntimeAdmissionState::Live { .. }))
                    .collect();
                assert_eq!(live.len(), 1);
                (*live[0]).clone()
            };
            assert!(matches!(
                manager.connected_backend("box-654321"),
                Err(ConnectedBackendError::Disconnected)
            ));
            assert!(matches!(
                manager.state("box-123456"),
                Some(HostConnectionState::Connected { .. })
            ));

            let observing = manager.clone();
            let mut observation = tokio::spawn(async move {
                match barrier {
                    Barrier::SyncScopeChange => {
                        let mut changed = remote("box-123456", "scope-team", false);
                        changed.scope = Some(super::super::machines::registry::Scope::Team {
                            org_slug: "org".into(),
                            team: "terminal".into(),
                        });
                        observing
                            .observe_mutation(
                                observing.reserve_observation(),
                                "box-123456".into(),
                                Some(changed),
                            )
                            .await;
                    }
                    Barrier::ExternalRemoval => {
                        observing
                            .observe_external(
                                observing.reserve_observation(),
                                Vec::new(),
                                BTreeSet::from(["box-123456".into()]),
                                false,
                                Vec::new(),
                            )
                            .await;
                    }
                }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut observation)
                    .await
                    .is_err()
            );
            for _ in 0..100 {
                if driver.close_gate.entered() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert!(
                driver.close_gate.entered(),
                "normal close reached the real guard"
            );
            {
                let events = lock(&events);
                let target: Vec<_> = events
                    .iter()
                    .filter(|event| event.machine_id == "box-123456")
                    .collect();
                let live: Vec<_> = target
                    .iter()
                    .filter_map(|event| event.admission.as_ref())
                    .filter(|row| matches!(row.state, RuntimeAdmissionState::Live { .. }))
                    .collect();
                assert_eq!(
                    live,
                    vec![&original_live],
                    "no replacement or live republication while closing"
                );
                assert!(
                    target.iter().any(|event| {
                        event.admission.as_ref().is_some_and(|row| {
                            row.revision > original_live.revision
                                && !matches!(row.state, RuntimeAdmissionState::Live { .. })
                        })
                    }),
                    "authority must be explicitly revoked before the owner finishes closing"
                );
                for event in target
                    .iter()
                    .filter(|event| matches!(event.state, HostConnectionState::Connected { .. }))
                {
                    assert_eq!(event.incarnation, snapshot.incarnation());
                    assert_eq!(event.generation, Some(original_generation));
                }
            }
            assert!(manager.connected_backend("box-123456").is_err());
            assert!(manager.capture_admitted_backend(&target).is_err());
            {
                let core = lock(&manager.core);
                let gate = &core.machines["box-123456"];
                assert!(gate.connected.is_none());
                assert!(gate.connected_generation.is_none());
                assert!(!*gate.quarantine.as_ref().unwrap().borrow());
            }
            assert_eq!(driver.latest_pid(), first_pid);
            assert_eq!(lock(&driver.pids).as_slice(), &[first_pid]);
            assert!(Arc::ptr_eq(&snapshot.client(), &captured));
            assert_eq!(captured.namespace(), &original_namespace);
            assert!(driver.sink.events.lock().unwrap().is_empty());

            driver.close_gate.release();
            observation.await.unwrap();
            assert_reaped(first_pid).await;
            assert!(matches!(
                manager.revalidate_backend(&snapshot),
                Err(ConnectedBackendError::Disconnected | ConnectedBackendError::UnknownMachine)
            ));
            assert!(captured
                .has_session(&codesense_pty_core::domain::PtySessionId::new(
                    "never-local"
                ))
                .is_err());
            assert!(driver.sink.events.lock().unwrap().is_empty());
            match barrier {
                Barrier::SyncScopeChange => assert_eq!(
                    manager.state("box-123456"),
                    Some(HostConnectionState::Disconnected)
                ),
                Barrier::ExternalRemoval => {
                    assert_eq!(manager.state("box-123456"), None)
                }
            }

            if matches!(barrier, Barrier::ExternalRemoval) {
                manager
                    .observe_mutation(
                        manager.reserve_observation(),
                        "box-123456".into(),
                        Some(remote("box-123456", "readded", false)),
                    )
                    .await;
            }
            manager
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            assert!(manager.capture_admitted_backend(&target).is_err());
            let fresh =
                super::super::pty_backend::resolve_remote_backend(&manager, "box-123456").unwrap();
            let fresh_target = fresh.admission_target().unwrap();
            assert_ne!(fresh_target, &target);
            assert!(manager.capture_admitted_backend(fresh_target).is_ok());
            let replacement = manager.connected_backend("box-123456").unwrap();
            assert_eq!(replacement.server_id(), snapshot.server_id());
            assert!(manager.revalidate_backend(&replacement).is_ok());
            assert!(matches!(
                manager.revalidate_backend(&snapshot),
                Err(ConnectedBackendError::IdentityMismatch)
            ));
            let second_pid = driver.latest_pid();
            assert_ne!(second_pid, first_pid);
            let disconnecting = manager.clone();
            let disconnect =
                tokio::spawn(async move { disconnecting.disconnect("box-123456").await });
            disconnect.await.unwrap().unwrap();
            assert_reaped(second_pid).await;
        }

        prove(Barrier::SyncScopeChange).await;
        prove(Barrier::ExternalRemoval).await;
    }

    #[test]
    fn retry_backoff_is_exact_capped_deterministic_and_within_ten_percent() {
        for attempt in 0_u32..16 {
            let nominal = BACKOFF_BASE_MS
                .saturating_mul(1_u64 << attempt.min(16))
                .min(BACKOFF_CAP_MS);
            let first = retry_backoff("box-123456", attempt);
            assert_eq!(first, retry_backoff("box-123456", attempt));
            let millis = first.as_millis() as u64;
            assert!(millis >= nominal * 90 / 100, "attempt {attempt}: {millis}");
            assert!(millis <= nominal * 110 / 100, "attempt {attempt}: {millis}");
        }
        assert!(retry_backoff("box-123456", 40) <= Duration::from_millis(30_000));
        assert_ne!(
            retry_backoff("box-123456", 3),
            retry_backoff("box-654321", 3)
        );
    }

    #[tokio::test]
    async fn connect_deadline_uses_virtual_time_and_retains_registered_demand() {
        let scheduler = Arc::new(ManualScheduler::default());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver_and_scheduler(
            Arc::new(NeverOpenDriver),
            scheduler.clone(),
        ));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "deadline", false)),
            )
            .await;
        let connecting = manager.clone();
        let attempt = tokio::spawn(async move {
            connecting
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
        });
        while scheduler.pending() == 0 {
            tokio::task::yield_now().await;
        }
        scheduler.fire(CONNECT_DEADLINE);
        assert_eq!(
            attempt.await.unwrap().unwrap_err(),
            format!(
                "connection deadline exceeded after {}ms",
                CONNECT_DEADLINE.as_millis()
            )
        );
        let core = lock(&manager.core);
        let gate = &core.machines["box-123456"];
        assert!(gate.foreground_demand, "failure does not clear demand");
        assert!(gate.present);
        assert!(matches!(
            gate.state,
            HostConnectionState::Unreachable { .. }
        ));
    }

    #[tokio::test]
    async fn retry_plan_is_uncapped_until_disconnect_withdraws_demand() {
        let manager = HostConnectionManager::default();
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "retry", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .unwrap();
        assert!(manager.complete_fail_closed(
            "box-123456",
            incarnation,
            generation,
            "offline".into(),
        ));
        for expected in 0..128 {
            let (_, attempt, _) = manager.retry_plan("box-123456", incarnation).unwrap();
            assert_eq!(attempt, expected);
        }
        manager.disconnect("box-123456").await.unwrap();
        assert!(manager.retry_plan("box-123456", incarnation).is_none());
    }

    #[tokio::test]
    async fn retry_tickets_make_old_and_duplicate_sleepers_no_ops() {
        let manager = HostConnectionManager::default();
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "tickets", false)),
            )
            .await;
        let (incarnation, generation, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::ExplicitForeground)
            .unwrap();
        assert!(manager.complete_fail_closed(
            "box-123456",
            incarnation,
            generation,
            "first".into(),
        ));
        let (_, _, first) = manager.retry_plan("box-123456", incarnation).unwrap();
        let (_, _, second) = manager.retry_plan("box-123456", incarnation).unwrap();
        assert_ne!(
            first, second,
            "a later failure invalidates the older sleeper"
        );
        let (_, _, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .unwrap();
        let core = lock(&manager.core);
        let gate = &core.machines["box-123456"];
        assert_ne!(gate.retry_ticket, first);
        assert_ne!(gate.retry_ticket, second);
    }

    #[tokio::test]
    async fn two_heartbeat_misses_retire_exact_generation_only_after_true_close() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "heartbeat", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let pid = driver.latest_pid();
        let (incarnation, generation) = {
            let core = lock(&manager.core);
            let gate = &core.machines["box-123456"];
            (gate.incarnation, gate.connected_generation.unwrap())
        };
        assert!(
            !manager
                .record_heartbeat_result("box-123456", incarnation, generation, false)
                .await
        );
        assert!(matches!(
            manager.state("box-123456"),
            Some(HostConnectionState::Connected { .. })
        ));
        let retiring = manager.clone();
        let second = tokio::spawn(async move {
            retiring
                .record_heartbeat_result("box-123456", incarnation, generation, false)
                .await
        });
        for _ in 0..100 {
            if driver.close_gate.entered() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(driver.close_gate.entered());
        assert!(!second.is_finished());
        driver.close_gate.release();
        assert!(second.await.unwrap());
        assert_process_reaped(pid).await;
        assert!(matches!(
            manager.state("box-123456"),
            Some(HostConnectionState::Unreachable { .. })
        ));
        let resume = lock(&manager.core).machines["box-123456"]
            .resume_identity
            .clone()
            .expect("recovery retains the exact connection namespace");
        assert_eq!(
            (resume.server_id.as_str(), resume.epoch.as_str()),
            ("test-server", "epoch")
        );
        let events = lock(&events);
        let unhealthy = events.last().expect("typed heartbeat transition");
        assert_eq!(unhealthy.generation, Some(generation));
        assert_eq!(unhealthy.consecutive_heartbeat_misses, 2);
        assert_eq!(unhealthy.retry_attempt, 0);
    }

    #[tokio::test]
    async fn retry_winning_during_heartbeat_reap_suppresses_stale_unreachable() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "heartbeat-race", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let (incarnation, generation) = {
            let core = lock(&manager.core);
            let gate = &core.machines["box-123456"];
            (gate.incarnation, gate.connected_generation.unwrap())
        };
        assert!(
            !manager
                .record_heartbeat_result("box-123456", incarnation, generation, false)
                .await
        );
        let retiring = manager.clone();
        let second = tokio::spawn(async move {
            retiring
                .record_heartbeat_result("box-123456", incarnation, generation, false)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(10), async {
                while !driver.close_gate.entered() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_ok(),
            "connection close gate was not entered within 10 seconds"
        );
        let (_, replacement, _, _) = manager
            .begin_attempt("box-123456", ConnectOrigin::Retry)
            .unwrap();
        driver.close_gate.release();
        assert!(!second.await.unwrap());
        let core = lock(&manager.core);
        let gate = &core.machines["box-123456"];
        assert_eq!(
            gate.active.as_ref().map(|attempt| attempt.generation),
            Some(replacement)
        );
        assert!(!matches!(
            gate.state,
            HostConnectionState::Unreachable { .. }
        ));
    }

    /// A webview reload re-registers the frontend, which re-reads the whole
    /// registry. A machine connected without auto-connect stays connected on
    /// the same lifetime, and its connection events name its layer.
    #[tokio::test]
    async fn webview_reregister_keeps_a_connected_machine_connected() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
        let record = remote("box-123456", "saved", false);
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(record.clone()),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let before = manager.connected_backend("box-123456").unwrap();
        assert!(lock(&events)
            .iter()
            .all(|event| event.scope == Some(super::super::machines::registry::Scope::Personal)));

        manager
            .observe_external(
                manager.reserve_observation(),
                vec![record],
                BTreeSet::new(),
                true,
                Vec::new(),
            )
            .await;

        assert!(matches!(
            manager.state("box-123456"),
            Some(HostConnectionState::Connected { .. })
        ));
        assert_eq!(
            manager
                .connected_backend("box-123456")
                .unwrap()
                .incarnation(),
            before.incarnation()
        );
        assert!(manager.revalidate_backend(&before).is_ok());
        assert!(manager.auto_connect_candidates().is_empty());
    }

    #[tokio::test]
    async fn pane_stream_failure_is_isolated_from_control_heartbeat_ledger() {
        let driver = Arc::new(ConnectedRealOwnerDriver::with_failed_pane_and_healthy_control());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "pane-local", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let client = manager.connected_backend("box-123456").unwrap().client();
        client
            .subscribe(
                &codesense_pty_core::domain::PtySessionId::new("failed-pane"),
                &codesense_pty_core::domain::ViewportId::new("viewport"),
            )
            .unwrap();
        assert!(HostConnectionManager::probe_heartbeat(client, &TokioConnectionScheduler).await);
        {
            let core = lock(&manager.core);
            let gate = &core.machines["box-123456"];
            assert_eq!(gate.consecutive_heartbeat_misses, 0);
            assert!(matches!(gate.state, HostConnectionState::Connected { .. }));
        }
        assert!(lock(&driver.sink.events)
            .iter()
            .any(|event| matches!(event, TerminalDomainEvent::StreamOutput { .. })));
        driver.close_gate.release();
        manager.disconnect("box-123456").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_eof_recovers_only_the_exact_machine_generation_and_client() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        for id in ["box-a-123456", "box-b-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
            manager
                .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
        }
        let a = manager.connected_backend("box-a-123456").unwrap();
        let b = manager.connected_backend("box-b-123456").unwrap();
        let (a_generation, b_generation) = {
            let core = lock(&manager.core);
            (
                core.machines["box-a-123456"].connected_generation.unwrap(),
                core.machines["box-b-123456"].connected_generation.unwrap(),
            )
        };

        manager
            .recover_exact_lost_client(
                None,
                "box-a-123456".into(),
                a.incarnation(),
                a_generation,
                b.client(),
            )
            .await;
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Connected { .. })
        ));
        assert!(matches!(
            manager.state("box-b-123456"),
            Some(HostConnectionState::Connected { .. })
        ));

        for winner in ["disconnect", "removal", "retry", "shutdown"] {
            match winner {
                "shutdown" => manager.shutdown_submitted.store(true, Ordering::SeqCst),
                _ => {
                    let core = lock(&manager.core);
                    let submitted = &core.machines["box-a-123456"].submitted;
                    match winner {
                        "disconnect" => {
                            submitted.disconnect.fetch_add(1, Ordering::SeqCst);
                        }
                        "removal" => {
                            submitted.removal.fetch_add(1, Ordering::SeqCst);
                        }
                        "retry" => {
                            submitted.retry.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => unreachable!(),
                    }
                }
            }
            manager
                .recover_exact_lost_client(
                    None,
                    "box-a-123456".into(),
                    a.incarnation(),
                    a_generation,
                    a.client(),
                )
                .await;
            assert!(
                matches!(
                    manager.state("box-a-123456"),
                    Some(HostConnectionState::Connected { .. })
                ),
                "{winner} must beat EOF recovery"
            );
            match winner {
                "shutdown" => manager.shutdown_submitted.store(false, Ordering::SeqCst),
                _ => {
                    let core = lock(&manager.core);
                    let submitted = &core.machines["box-a-123456"].submitted;
                    match winner {
                        "disconnect" => {
                            submitted.disconnect.fetch_sub(1, Ordering::SeqCst);
                        }
                        "removal" => {
                            submitted.removal.fetch_sub(1, Ordering::SeqCst);
                        }
                        "retry" => {
                            submitted.retry.fetch_sub(1, Ordering::SeqCst);
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }

        let recovering = manager.clone();
        let expected = a.client();
        let recovery = tokio::spawn(async move {
            recovering
                .recover_exact_lost_client(
                    None,
                    "box-a-123456".into(),
                    a.incarnation(),
                    a_generation,
                    expected,
                )
                .await;
        });
        while !driver.close_gate.entered() {
            tokio::task::yield_now().await;
        }
        driver.close_gate.release();
        recovery.await.unwrap();
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Unreachable { .. })
        ));
        assert!(matches!(
            manager.state("box-b-123456"),
            Some(HostConnectionState::Connected { .. })
        ));
        assert_eq!(
            lock(&manager.core).machines["box-b-123456"].connected_generation,
            Some(b_generation)
        );
        manager.disconnect("box-b-123456").await.unwrap();
    }

    const RETIRE_A: &str = "box-a-123456";
    const RETIRE_B: &str = "box-b-123456";
    const RETIRE_WAIT: Duration = Duration::from_secs(10);

    async fn with_retirement_fixture<F, Fut>(test: F)
    where
        F: FnOnce(RetirementFixture) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use futures_util::FutureExt;
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        driver.greet(RETIRE_A, "old");
        driver.greet(RETIRE_B, "old");
        let manager = Arc::new(HostConnectionManager::with_attempt_driver_and_scheduler(
            driver.clone(),
            Arc::new(ManualScheduler::default()),
        ));
        let result = std::panic::AssertUnwindSafe(async {
            let fixture = RetirementFixture::new(driver.clone(), manager.clone()).await;
            test(fixture).await;
        })
        .catch_unwind()
        .await;
        // Keep manager and driver alive outside the unwind boundary. Their
        // actual owners perform every close/reap, even after a failed assertion.
        driver.release_close_gates();
        manager.begin_shutdown();
        let draining = manager.clone();
        let drain = tokio::spawn(async move { draining.drain_shutdown().await });
        tokio::time::timeout(RETIRE_WAIT, drain)
            .await
            .expect("retirement fixture owner drain timed out")
            .expect("retirement fixture owner drain panicked");
        let pids = lock(&driver.pids).clone();
        for pid in pids {
            assert_process_reaped(pid).await;
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    async fn retirement_connect(manager: &Arc<HostConnectionManager>, machine: &'static str) {
        let manager = manager.clone();
        // Time out the observation, not the owned admission future. On failure
        // the harness can still cancel and drain this attempt through manager APIs.
        let attempt = tokio::spawn(async move {
            manager
                .connect_remote_inner(None, machine.into(), ConnectOrigin::ExplicitForeground)
                .await
        });
        tokio::time::timeout(RETIRE_WAIT, attempt)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    struct RetirementPermitRelease(Option<std::sync::mpsc::SyncSender<()>>);

    impl RetirementPermitRelease {
        fn release(mut self) {
            self.0
                .take()
                .unwrap()
                .send(())
                .expect("retirement barrier receiver ended early");
        }
    }

    impl Drop for RetirementPermitRelease {
        fn drop(&mut self) {
            // Unwind must also release the attempt so manager shutdown can
            // await its normal rejection cleanup.
            if let Some(release) = self.0.take() {
                let _ = release.send(());
            }
        }
    }

    async fn retirement_wait(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(RETIRE_WAIT, async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retirement test barrier timed out");
    }

    struct RetirementFixture {
        driver: Arc<ConnectedRealOwnerDriver>,
        manager: Arc<HostConnectionManager>,
        streams: Arc<super::super::pty_host::StreamIds>,
        old: EventNamespace,
        other: ConnectedBackendSnapshot,
        other_generation: Option<AttemptGeneration>,
        old_ledger: RemoteSessionLedger,
        other_ledger: RemoteSessionLedger,
        other_local: u64,
        old_locals: [u64; 2],
    }

    impl RetirementFixture {
        async fn new(
            driver: Arc<ConnectedRealOwnerDriver>,
            manager: Arc<HostConnectionManager>,
        ) -> Self {
            use super::super::pty_host::{StreamHost, StreamIds};
            use codesense_pty_core::domain::{PtySessionId, ViewportId};
            use codesense_pty_core::term_state::StreamId;
            let streams = Arc::new(StreamIds::default());
            *lock(&manager.retirement_for_test) = Some(Arc::new(TestRetirementPresentation {
                streams: streams.clone(),
                sink: driver.sink.clone(),
                before_permit: None,
            }));
            for id in [RETIRE_A, RETIRE_B] {
                manager
                    .observe_mutation(
                        manager.reserve_observation(),
                        id.into(),
                        Some(remote(id, id, false)),
                    )
                    .await;
                retirement_connect(&manager, id).await;
            }
            let old_client = manager.connected_backend(RETIRE_A).unwrap();
            let old = old_client.client().namespace().clone();
            let other = manager.connected_backend(RETIRE_B).unwrap();
            let (old_ledger, other_ledger, old_generation, other_generation) = {
                let core = lock(&manager.core);
                (
                    core.machines[RETIRE_A].remote_ledger.clone(),
                    core.machines[RETIRE_B].remote_ledger.clone(),
                    core.machines[RETIRE_A].connected_generation.unwrap(),
                    core.machines[RETIRE_B].connected_generation,
                )
            };
            old_ledger.record("shared", true);
            old_ledger.record("ledger-only", false);
            other_ledger.record("shared", false);
            let attached = SubscribeResult {
                snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
                stream_id: StreamId(41),
                story_id: None,
                workspace_id: None,
                epic_id: None,
            };
            let viewport = ViewportId::new("test");
            let shared_local = streams.attach(
                StreamHost::remote_namespace(&old),
                &PtySessionId::new("shared"),
                &viewport,
                &attached,
            );
            let viewed_only = SubscribeResult {
                stream_id: StreamId(42),
                ..attached.clone()
            };
            let viewed_local = streams.attach(
                StreamHost::remote_namespace(&old),
                &PtySessionId::new("viewed-only"),
                &viewport,
                &viewed_only,
            );
            let other_local = streams.attach(
                StreamHost::remote_namespace(other.client().namespace()),
                &PtySessionId::new("shared"),
                &viewport,
                &attached,
            );
            let fixture = Self {
                driver,
                manager,
                streams,
                old,
                other,
                other_generation,
                old_ledger,
                other_ledger,
                other_local,
                old_locals: [shared_local, viewed_local],
            };
            let pid = lock(&fixture.driver.pids)[0];
            assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
            retirement_wait(|| !old_client.client().is_alive()).await;
            lock(&fixture.driver.close_gates)[0].release();
            tokio::time::timeout(
                RETIRE_WAIT,
                fixture.manager.recover_exact_lost_client(
                    None,
                    RETIRE_A.into(),
                    old_client.incarnation(),
                    old_generation,
                    old_client.client(),
                ),
            )
            .await
            .unwrap();
            assert_process_reaped(pid).await;
            assert_eq!(
                lock(&fixture.manager.core).machines[RETIRE_A]
                    .resume_identity
                    .as_ref(),
                Some(&fixture.old)
            );
            lock(&fixture.driver.sink.envelopes).clear();
            fixture
        }

        fn assert_other_unchanged(&self) {
            use super::super::pty_host::StreamHost;
            use codesense_pty_core::term_state::StreamId;
            assert!(Arc::ptr_eq(
                &self.other.client(),
                &self.manager.connected_backend(RETIRE_B).unwrap().client()
            ));
            assert_eq!(
                lock(&self.manager.core).machines[RETIRE_B].connected_generation,
                self.other_generation
            );
            let mut output = Vec::new();
            self.streams.deliver_output(
                StreamHost::remote_namespace(self.other.client().namespace()),
                StreamId(41),
                "B".into(),
                1,
                &mut |to, data, _| {
                    output.push((to, data.to_owned()));
                    true
                },
            );
            assert_eq!(output, [(self.other_local, "B".into())]);
            assert_eq!(
                self.other_ledger.take_all(),
                [(
                    codesense_pty_core::domain::PtySessionId::new("shared"),
                    false
                )]
            );
        }

        async fn finish(&self) {
            for gate in lock(&self.driver.close_gates).iter() {
                gate.release();
            }
            for id in [RETIRE_A, RETIRE_B] {
                tokio::time::timeout(RETIRE_WAIT, self.manager.disconnect(id))
                    .await
                    .unwrap()
                    .unwrap();
            }
            let pids = lock(&self.driver.pids).clone();
            for pid in pids {
                assert_process_reaped(pid).await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn headless_changed_epoch_retires_before_candidate_events_and_isolates_other_machine() {
        use super::super::pty_host::StreamHost;
        use codesense_pty_core::term_state::StreamId;
        with_retirement_fixture(|fixture| async move {
        fixture.driver.greet(RETIRE_A, "new");
        retirement_connect(&fixture.manager, RETIRE_A).await;
        let replacement = fixture.manager.connected_backend(RETIRE_A).unwrap();
        assert_eq!(replacement.client().epoch(), "new");
        assert!(fixture.old_ledger.take_all().is_empty());
        let events = lock(&fixture.driver.sink.envelopes).clone();
        assert_eq!(events.len(), 7);
        for session_id in ["shared", "ledger-only", "viewed-only"] {
            assert_eq!(events[..6].iter().filter(|e| matches!(&e.event, TerminalDomainEvent::Exit { session, status: None, pool_member, requested: false, .. } if session.as_str() == session_id && *pool_member == (session_id == "shared"))).count(), 1);
            assert_eq!(events[..6].iter().filter(|e| matches!(&e.event, TerminalDomainEvent::SessionRemoved { session, reason: codesense_pty_core::domain::TeardownReason::Expire } if session.as_str() == session_id)).count(), 1);
            let exit = events.iter().position(|e| matches!(&e.event, TerminalDomainEvent::Exit { session, .. } if session.as_str() == session_id)).unwrap();
            let removed = events.iter().position(|e| matches!(&e.event, TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == session_id)).unwrap();
            assert!(exit < removed, "Exit must precede SessionRemoved for {session_id}");
        }
        assert!(events[..6].iter().all(|e| e.namespace == fixture.old));
        assert_eq!(&events[6].namespace, replacement.client().namespace());
        assert!(
            matches!(&events[6].event, TerminalDomainEvent::LayoutChanged { epoch, .. } if epoch == "new")
        );
        assert!(fixture
            .streams
            .attached_on(StreamHost::remote_namespace(&fixture.old))
            .is_empty());
        for stream in [41, 42, 99] {
            fixture.streams.deliver_output(
                StreamHost::remote_namespace(&fixture.old),
                StreamId(stream),
                "late".into(),
                4,
                &mut |_, _, _| panic!("retired namespace emitted late output"),
            );
        }
        assert!(!super::super::pty_backend::retire_remote_lifetime_core(
            &fixture.streams,
            fixture.driver.sink.as_ref(),
            &fixture.old,
            fixture.old_ledger.clone(),
            || panic!("duplicate retirement consumed permit")
        ));
        assert_eq!(lock(&fixture.driver.sink.envelopes).len(), 7);
        fixture.assert_other_unchanged();
        fixture.finish().await;
        }).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn headless_same_epoch_preserves_streams_and_ledger_without_retirement() {
        with_retirement_fixture(|fixture| async move {
            retirement_connect(&fixture.manager, RETIRE_A).await;
            assert_eq!(
                fixture
                    .streams
                    .attached_on(super::super::pty_host::StreamHost::remote_namespace(
                        &fixture.old
                    ))
                    .len(),
                2
            );
            let retained: BTreeMap<_, _> = fixture
                .old_ledger
                .take_all()
                .into_iter()
                .map(|(session, pool)| (session.as_str().to_owned(), pool))
                .collect();
            assert_eq!(
                retained,
                BTreeMap::from([
                    ("shared".to_owned(), true),
                    ("ledger-only".to_owned(), false)
                ])
            );
            let mut output = Vec::new();
            for stream in [41, 42] {
                fixture.streams.deliver_output(
                    super::super::pty_host::StreamHost::remote_namespace(&fixture.old),
                    codesense_pty_core::term_state::StreamId(stream),
                    "retained".into(),
                    8,
                    &mut |to, data, seq| {
                        output.push((to, data.to_owned(), seq));
                        true
                    },
                );
            }
            assert_eq!(
                output,
                fixture
                    .old_locals
                    .map(|local| (local, "retained".to_owned(), 8))
            );
            let events = lock(&fixture.driver.sink.envelopes).clone();
            assert_eq!(events.len(), 1);
            assert!(matches!(
                events[0].event,
                TerminalDomainEvent::LayoutChanged { .. }
            ));
            fixture.assert_other_unchanged();
            fixture.finish().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn headless_changed_epoch_without_presentation_fails_closed_and_reaps_candidate() {
        with_retirement_fixture(|fixture| async move {
            fixture.driver.greet(RETIRE_A, "new");
            *lock(&fixture.manager.retirement_for_test) = None;
            let manager = fixture.manager.clone();
            let attempt = tokio::spawn(async move {
                manager
                    .connect_remote_inner(None, RETIRE_A.into(), ConnectOrigin::ExplicitForeground)
                    .await
            });
            retirement_wait(|| {
                lock(&fixture.driver.close_gates)
                    .get(2)
                    .is_some_and(|gate| gate.entered())
            })
            .await;
            assert!(!attempt.is_finished(), "rejection must await owned close");
            assert!(lock(&fixture.driver.sink.envelopes).is_empty());
            lock(&fixture.driver.close_gates)[2].release();
            let error = tokio::time::timeout(RETIRE_WAIT, attempt)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(error, "changed remote lifetime has no application handle");
            assert_process_reaped(fixture.driver.latest_pid()).await;
            assert_eq!(
                fixture
                    .streams
                    .attached_on(super::super::pty_host::StreamHost::remote_namespace(
                        &fixture.old
                    ))
                    .len(),
                2
            );
            assert_eq!(fixture.old_ledger.take_all().len(), 2);
            fixture.assert_other_unchanged();
            fixture.finish().await;
        })
        .await;
    }

    async fn headless_priority_before_retirement(priority: &str) {
        with_retirement_fixture(|fixture| async move {
            fixture.driver.greet(RETIRE_A, "new");
            let entered = Arc::new(AtomicBool::new(false));
            let hook_entered = entered.clone();
            let weak = Arc::downgrade(&fixture.manager);
            let (release, released) = std::sync::mpsc::sync_channel(1);
            let release = RetirementPermitRelease(Some(release));
            let released = Mutex::new(released);
            *lock(&fixture.manager.retirement_for_test) =
                Some(Arc::new(TestRetirementPresentation {
                    streams: fixture.streams.clone(),
                    sink: fixture.driver.sink.clone(),
                    before_permit: Some(Arc::new(move || {
                        let manager = weak.upgrade().unwrap();
                        // APIs submit their priority counter before this barrier. Hold
                        // application of the mutation until its submission is observed.
                        let _publication = lock(&manager.publication_barrier);
                        hook_entered.store(true, Ordering::SeqCst);
                        lock(&released)
                            .recv_timeout(RETIRE_WAIT)
                            .expect("retirement barrier was not released");
                    })),
                }));
            let manager = fixture.manager.clone();
            let attempt = tokio::spawn(async move {
                manager
                    .connect_remote_inner(None, RETIRE_A.into(), ConnectOrigin::ExplicitForeground)
                    .await
            });
            retirement_wait(|| entered.load(Ordering::SeqCst)).await;
            assert!(lock(&fixture.manager.core).machines[RETIRE_A]
                .retirement_reserved
                .is_some());
            let manager = fixture.manager.clone();
            let priority_owned = priority.to_owned();
            let mutation = tokio::spawn(async move {
                match priority_owned.as_str() {
                    "disconnect" => manager.disconnect(RETIRE_A).await.unwrap(),
                    "remove" => {
                        manager
                            .observe_mutation(manager.reserve_observation(), RETIRE_A.into(), None)
                            .await
                    }
                    "retry" => {
                        manager
                            .begin_attempt(RETIRE_A, ConnectOrigin::Retry)
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
            });
            retirement_wait(|| {
                let core = lock(&fixture.manager.core);
                let gate = &core.machines[RETIRE_A];
                match priority {
                    "disconnect" => {
                        gate.submitted.disconnect.load(Ordering::SeqCst) > gate.handled_disconnect
                    }
                    "remove" => {
                        gate.submitted.removal.load(Ordering::SeqCst) > gate.handled_removal
                    }
                    "retry" => gate.submitted.retry.load(Ordering::SeqCst) > gate.handled_retry,
                    _ => unreachable!(),
                }
            })
            .await;
            release.release();
            retirement_wait(|| {
                lock(&fixture.driver.close_gates)
                    .get(2)
                    .is_some_and(|gate| gate.entered())
            })
            .await;
            assert!(
                !attempt.is_finished(),
                "candidate close must complete before rejection returns"
            );
            assert!(lock(&fixture.driver.sink.envelopes).is_empty());
            assert_eq!(
                fixture
                    .streams
                    .attached_on(super::super::pty_host::StreamHost::remote_namespace(
                        &fixture.old
                    ))
                    .len(),
                2
            );
            assert_eq!(fixture.old_ledger.take_all().len(), 2);
            lock(&fixture.driver.close_gates)[2].release();
            assert!(tokio::time::timeout(RETIRE_WAIT, attempt)
                .await
                .unwrap()
                .unwrap()
                .is_err());
            tokio::time::timeout(RETIRE_WAIT, mutation)
                .await
                .unwrap()
                .unwrap();
            assert_process_reaped(fixture.driver.latest_pid()).await;
            assert!(fixture.manager.connected_backend(RETIRE_A).is_err());
            assert!(lock(&fixture.driver.sink.envelopes).is_empty());
            fixture.assert_other_unchanged();
            fixture.finish().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn headless_disconnect_before_retirement_permit_reaps_candidate_without_effects() {
        headless_priority_before_retirement("disconnect").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn headless_removal_before_retirement_permit_reaps_candidate_without_effects() {
        headless_priority_before_retirement("remove").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn headless_retry_before_retirement_permit_reaps_candidate_without_effects() {
        headless_priority_before_retirement("retry").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_remote_reader_eof_recovers_exact_client_without_touching_other_or_local() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let scheduler = Arc::new(ManualScheduler::default());
        let local_losses = Arc::new(AtomicU64::new(0));
        let local_state = SessionServerState::default();
        let local_counter = local_losses.clone();
        local_state.on_lost(Arc::new(move || {
            local_counter.fetch_add(1, Ordering::SeqCst);
        }));
        let manager = Arc::new(HostConnectionManager::with_attempt_driver_and_scheduler(
            driver.clone(),
            scheduler,
        ));
        let recovering = Arc::downgrade(&manager);
        *lock(&manager.loss_recovery_for_test) = Some(Arc::new(
            move |machine_id, incarnation, generation, expected| {
                let Some(manager) = recovering.upgrade() else {
                    return;
                };
                tauri::async_runtime::spawn(async move {
                    manager
                        .recover_exact_lost_client(
                            None,
                            machine_id,
                            incarnation,
                            generation,
                            expected,
                        )
                        .await;
                });
            },
        ));
        for id in ["box-a-123456", "box-b-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
            manager
                .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
        }
        let stale = manager.connected_backend("box-a-123456").unwrap();
        let stale_incarnation = stale.incarnation();
        let other = manager.connected_backend("box-b-123456").unwrap();
        let stale_generation = {
            lock(&manager.core).machines["box-a-123456"]
                .connected_generation
                .unwrap()
        };
        let first_pid = lock(&driver.pids)[0];
        assert_eq!(unsafe { libc::kill(first_pid, libc::SIGKILL) }, 0);
        while !driver.close_gate.entered() {
            tokio::task::yield_now().await;
        }
        driver.close_gate.release();
        while !matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Unreachable { .. })
        ) {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            manager.state("box-b-123456"),
            Some(HostConnectionState::Connected { .. })
        ));
        assert!(Arc::ptr_eq(
            &other.client(),
            &manager.connected_backend("box-b-123456").unwrap().client()
        ));
        let resume = lock(&manager.core).machines["box-a-123456"]
            .resume_identity
            .clone()
            .expect("lost connection namespace retained");
        assert_eq!(
            (resume.server_id.as_str(), resume.epoch.as_str()),
            ("test-server", "epoch")
        );
        manager
            .recover_exact_lost_client(
                None,
                "box-a-123456".into(),
                stale_incarnation,
                stale_generation,
                stale.client(),
            )
            .await;
        assert!(matches!(
            manager.state("box-a-123456"),
            Some(HostConnectionState::Unreachable { .. })
        ));
        assert_eq!(local_losses.load(Ordering::SeqCst), 0);
        manager.disconnect("box-b-123456").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "quarantine: https://github.com/codesensehq/codesense/issues/1"]
    async fn reader_eof_before_dispatcher_install_fails_attempt_and_remains_retryable() {
        let driver = Arc::new(ConnectedRealOwnerDriver::ending_before_install());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "pre-install-eof", false)),
            )
            .await;
        let baseline = lock(&events).len();
        let connecting = manager.clone();
        let attempt = tokio::spawn(async move {
            connecting
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(10), async {
                while !driver.close_gate.entered() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_ok(),
            "connection close gate was not entered within 10 seconds"
        );
        driver.close_gate.release();
        let error = attempt.await.unwrap().unwrap_err();
        assert!(
            error.contains("ended before publication")
                || error.contains("lost or superseded before publication"),
            "unexpected pre-adoption EOF error: {error}"
        );
        assert_process_reaped(driver.latest_pid()).await;
        assert!(matches!(
            manager.state("box-123456"),
            Some(HostConnectionState::Unreachable { .. })
        ));
        let incarnation = lock(&manager.core).machines["box-123456"].incarnation;
        assert!(
            manager.retry_plan("box-123456", incarnation).is_some(),
            "foreground demand remains eligible for the recovery retry"
        );
        assert!(lock(&events)[baseline..]
            .iter()
            .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })));
        let core = lock(&manager.core);
        let gate = &core.machines["box-123456"];
        assert!(gate.active.is_none());
        assert!(gate.connected.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn priority_winner_during_real_adopt_delivery_suppresses_connected_publication() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let hook_entered = entered.clone();
        let hook_release = release.clone();
        *lock(&manager.before_publication_delivery) = Some(Arc::new(move || {
            hook_entered.wait();
            hook_release.wait();
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "adopt-priority", false)),
            )
            .await;
        let baseline = lock(&events).len();
        let connecting = manager.clone();
        let attempt = tokio::spawn(async move {
            connecting
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
        });
        entered.wait();
        let retrying = manager.clone();
        let retry = tokio::task::spawn_blocking(move || {
            retrying.begin_attempt("box-123456", ConnectOrigin::Retry)
        });
        while {
            let core = lock(&manager.core);
            let gate = &core.machines["box-123456"];
            gate.submitted.retry.load(Ordering::SeqCst) <= gate.handled_retry
        } {
            tokio::task::yield_now().await;
        }
        release.wait();
        let (_, replacement, _, _) = retry
            .await
            .unwrap()
            .expect("retry reserves the higher-priority generation");
        while !driver.close_gate.entered() {
            tokio::task::yield_now().await;
        }
        driver.close_gate.release();
        assert!(attempt.await.unwrap().is_err());
        assert!(lock(&events)[baseline..]
            .iter()
            .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })));
        let core = lock(&manager.core);
        let gate = &core.machines["box-123456"];
        assert_eq!(
            gate.active.as_ref().map(|attempt| attempt.generation),
            Some(replacement)
        );
        assert!(gate.connected.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_priority_entry_wins_in_the_precheck_to_commit_gap() {
        for winner in ["disconnect", "removal", "retry", "shutdown"] {
            let driver = Arc::new(ConnectedRealOwnerDriver::new());
            let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
            let entered = Arc::new(std::sync::Barrier::new(2));
            let release = Arc::new(std::sync::Barrier::new(2));
            let hook_entered = entered.clone();
            let hook_release = release.clone();
            *lock(&manager.after_publication_precheck) = Some(Arc::new(move || {
                hook_entered.wait();
                hook_release.wait();
            }));
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-123456".into(),
                    Some(remote("box-123456", winner, false)),
                )
                .await;
            let baseline = lock(&events).len();
            let connecting = manager.clone();
            let attempt = tokio::spawn(async move {
                connecting
                    .connect_remote_inner(
                        None,
                        "box-123456".into(),
                        ConnectOrigin::ExplicitForeground,
                    )
                    .await
            });
            entered.wait();
            let winning = match winner {
                "disconnect" => {
                    let manager = manager.clone();
                    tokio::spawn(async move { manager.disconnect("box-123456").await.unwrap() })
                }
                "removal" => {
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        manager
                            .observe_mutation(
                                manager.reserve_observation(),
                                "box-123456".into(),
                                None,
                            )
                            .await
                    })
                }
                "retry" => {
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        tokio::task::spawn_blocking(move || {
                            manager
                                .begin_attempt("box-123456", ConnectOrigin::Retry)
                                .unwrap();
                        })
                        .await
                        .unwrap();
                    })
                }
                "shutdown" => {
                    let manager = manager.clone();
                    tokio::spawn(async move {
                        tokio::task::spawn_blocking(move || assert!(manager.begin_shutdown()))
                            .await
                            .unwrap();
                    })
                }
                _ => unreachable!(),
            };
            loop {
                let submitted = match winner {
                    "shutdown" => manager.shutdown_submitted.load(Ordering::SeqCst),
                    _ => {
                        let core = lock(&manager.core);
                        let gate = &core.machines["box-123456"];
                        match winner {
                            "disconnect" => {
                                gate.submitted.disconnect.load(Ordering::SeqCst)
                                    > gate.handled_disconnect
                            }
                            "removal" => {
                                gate.submitted.removal.load(Ordering::SeqCst) > gate.handled_removal
                            }
                            "retry" => {
                                gate.submitted.retry.load(Ordering::SeqCst) > gate.handled_retry
                            }
                            _ => unreachable!(),
                        }
                    }
                };
                if submitted {
                    break;
                }
                tokio::task::yield_now().await;
            }
            release.wait();
            while !driver.close_gate.entered() {
                tokio::task::yield_now().await;
            }
            driver.close_gate.release();
            assert!(attempt.await.unwrap().is_err(), "{winner}");
            winning.await.unwrap();
            assert!(
                lock(&events)[baseline..]
                    .iter()
                    .all(|event| !matches!(event.state, HostConnectionState::Connected { .. })),
                "{winner}"
            );
            assert!(lock(&manager.core)
                .machines
                .get("box-123456")
                .is_none_or(|gate| { gate.connected.is_none() }));
        }
    }

    #[tokio::test]
    async fn priority_barriers_during_heartbeat_reap_suppress_stale_failure() {
        #[derive(Clone, Copy)]
        enum Winner {
            Disconnect,
            Removal,
            Shutdown,
        }
        async fn prove(winner: Winner) {
            let driver = Arc::new(ConnectedRealOwnerDriver::new());
            let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            manager.subscribe(Arc::new(move |event| lock(&observed).push(event)));
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    "box-123456".into(),
                    Some(remote("box-123456", "priority-race", false)),
                )
                .await;
            manager
                .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            let (incarnation, generation) = {
                let core = lock(&manager.core);
                let gate = &core.machines["box-123456"];
                (gate.incarnation, gate.connected_generation.unwrap())
            };
            assert!(
                !manager
                    .record_heartbeat_result("box-123456", incarnation, generation, false)
                    .await
            );
            let retiring = manager.clone();
            let heartbeat = tokio::spawn(async move {
                retiring
                    .record_heartbeat_result("box-123456", incarnation, generation, false)
                    .await
            });
            while !driver.close_gate.entered() {
                tokio::task::yield_now().await;
            }
            let barrier = match winner {
                Winner::Disconnect => {
                    let manager = manager.clone();
                    Some(tokio::spawn(async move {
                        manager.disconnect("box-123456").await.unwrap()
                    }))
                }
                Winner::Removal => {
                    let manager = manager.clone();
                    Some(tokio::spawn(async move {
                        manager
                            .observe_mutation(
                                manager.reserve_observation(),
                                "box-123456".into(),
                                None,
                            )
                            .await
                    }))
                }
                Winner::Shutdown => {
                    assert!(manager.begin_shutdown());
                    None
                }
            };
            if matches!(winner, Winner::Disconnect | Winner::Removal) {
                loop {
                    let submitted = {
                        let core = lock(&manager.core);
                        let gate = &core.machines["box-123456"];
                        match winner {
                            Winner::Disconnect => {
                                gate.submitted.disconnect.load(Ordering::SeqCst)
                                    > gate.handled_disconnect
                            }
                            Winner::Removal => {
                                gate.submitted.removal.load(Ordering::SeqCst) > gate.handled_removal
                            }
                            Winner::Shutdown => unreachable!(),
                        }
                    };
                    if submitted {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
            driver.close_gate.release();
            assert!(!heartbeat.await.unwrap());
            if let Some(barrier) = barrier {
                barrier.await.unwrap();
            }
            assert!(!lock(&events)
                .iter()
                .any(|event| matches!(event.state, HostConnectionState::Unreachable { .. })));
        }
        prove(Winner::Disconnect).await;
        prove(Winner::Removal).await;
        prove(Winner::Shutdown).await;
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_deadline_accepts_after_three_seconds_and_misses_at_exactly_five() {
        let healthy = tokio::spawn(async {
            HostConnectionManager::within_heartbeat_deadline(
                async {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    true
                },
                &TokioConnectionScheduler,
            )
            .await
        });
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(
            !healthy.is_finished(),
            "READ_TIMEOUT must not end heartbeat"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(healthy.await.unwrap());

        let missed = tokio::spawn(async {
            HostConnectionManager::within_heartbeat_deadline(
                std::future::pending(),
                &TokioConnectionScheduler,
            )
            .await
        });
        tokio::time::advance(Duration::from_millis(4_999)).await;
        assert!(!missed.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(!missed.await.unwrap());
    }

    fn rm4c_layout(server_id: &str, epoch: &str) -> LayoutSnapshot {
        LayoutSnapshot {
            schema: codesense_pty_core::layout::LAYOUT_SCHEMA,
            server_id: codesense_pty_core::domain::ServerId::new(server_id),
            server_name: "remote".into(),
            epoch: epoch.into(),
            rev: 1,
            workspaces: Default::default(),
            viewer: Default::default(),
        }
    }

    #[tokio::test]
    async fn rm5aa_admission_baseline_retains_removed_tombstone() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        let initial = manager.admission_baseline().rows.remove(0);
        manager
            .observe_mutation(manager.reserve_observation(), "box-123456".into(), None)
            .await;
        let removed = manager.admission_baseline().rows.remove(0);
        assert_eq!(removed.state, RuntimeAdmissionState::Removed);
        assert!(removed.revision > initial.revision);
        assert_eq!(manager.admission_baseline().rows, vec![removed]);
    }

    #[tokio::test]
    async fn rm5aa_remove_recreate_feed_keeps_monotonic_tombstone() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        let rows = Arc::new(Mutex::new(Vec::new()));
        let received = rows.clone();
        manager.subscribe(Arc::new(move |event| {
            if let Some(row) = event.admission {
                lock(&received).push(row);
            }
        }));
        for record in [
            Some(remote("box-123456", "first", false)),
            None,
            Some(remote("box-123456", "replacement", false)),
        ] {
            manager
                .observe_mutation(manager.reserve_observation(), "box-123456".into(), record)
                .await;
        }
        let rows = lock(&rows);
        let removed = rows
            .iter()
            .position(|row| row.state == RuntimeAdmissionState::Removed)
            .unwrap();
        assert!(removed > 0 && removed + 1 < rows.len());
        assert!(rows[removed].revision > rows[removed - 1].revision);
        assert!(rows[removed + 1].revision > rows[removed].revision);
        assert_eq!(
            manager.admission_baseline().rows,
            vec![rows.last().unwrap().clone()]
        );
    }

    #[tokio::test]
    async fn rm5aa_admission_revision_ignores_telemetry_only_events() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        let before = manager.admission_baseline();
        {
            let mut core = lock(&manager.core);
            core.machines
                .get_mut("box-123456")
                .unwrap()
                .consecutive_heartbeat_misses += 1;
            manager.enqueue_locked(&mut core, "box-123456");
        }
        manager.dispatch();
        assert_eq!(manager.admission_baseline(), before);
    }

    #[tokio::test]
    async fn rm5aa_incompatible_snapshot_is_historical_not_live() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut("box-123456").unwrap();
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: Some(rm4c_layout("server", "old")),
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "server".into(),
                epoch: "old".into(),
                rev: 1,
                shutdown_for_replacement: false,
                host_pid: 0,
            });
        }
        let row = manager.admission_baseline().rows.remove(0);
        assert!(
            matches!(&row.state, RuntimeAdmissionState::Historical { historical }
            if historical.last_namespace.is_none() && historical.reason == RuntimeHistoryReason::Incompatible)
        );
        let encoded = serde_json::to_value(row).unwrap();
        assert!(encoded["namespace"].is_null());
        assert_eq!(encoded["state"], "historical");
        assert!(manager.connected_backend("box-123456").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_old_loss_cannot_revoke_replacement_binding() {
        with_retirement_fixture(|fixture| async move {
            let manager = &fixture.manager;
            retirement_connect(manager, RETIRE_A).await;
            let old = manager.connected_backend(RETIRE_A).unwrap();
            let generation = lock(&manager.core).machines[RETIRE_A]
                .connected_generation
                .unwrap();
            fixture.driver.release_close_gates();
            manager.disconnect(RETIRE_A).await.unwrap();
            retirement_connect(manager, RETIRE_A).await;
            let installed = manager.admission_baseline();
            let replacement = manager.connected_backend(RETIRE_A).unwrap();
            assert_ne!(old.client().namespace(), replacement.client().namespace());
            manager.capture_exact_loss(RETIRE_A, old.incarnation(), generation, &old.client());
            assert_eq!(
                manager.admission_baseline(),
                installed,
                "stale synchronous ingress cannot advance or revoke the replacement row"
            );
            assert!(Arc::ptr_eq(
                &manager.connected_backend(RETIRE_A).unwrap().client(),
                &replacement.client()
            ));
            manager
                .recover_exact_lost_client(
                    None,
                    RETIRE_A.into(),
                    old.incarnation(),
                    generation,
                    old.client(),
                )
                .await;
            assert_eq!(manager.admission_baseline(), installed);
            assert!(Arc::ptr_eq(
                &manager.connected_backend(RETIRE_A).unwrap().client(),
                &replacement.client()
            ));
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_submitted_shutdown_revokes_baseline_before_publication_lock() {
        with_retirement_fixture(|fixture| async move {
            let manager = &fixture.manager;
            retirement_connect(manager, RETIRE_A).await;
            let before = manager.admission_baseline();
            assert!(before
                .rows
                .iter()
                .all(|row| matches!(row.state, RuntimeAdmissionState::Live { .. })));
            let revoked = std::thread::scope(|scope| {
                let publication = lock(&manager.publication_barrier);
                let shutdown = scope.spawn(|| manager.begin_shutdown());
                let cutoff = std::time::Instant::now() + RETIRE_WAIT;
                while !manager.shutdown_submitted.load(Ordering::SeqCst) {
                    assert!(
                        std::time::Instant::now() < cutoff,
                        "shutdown was not submitted"
                    );
                    std::thread::yield_now();
                }
                assert_eq!(manager.shutdown_phase(), ShutdownPhase::Running);
                let baseline = manager.admission_baseline();
                for old in &before.rows {
                    let row = baseline
                        .rows
                        .iter()
                        .find(|row| row.member == old.member)
                        .unwrap();
                    assert!(row.revision > old.revision);
                    assert!(!matches!(row.state, RuntimeAdmissionState::Live { .. }));
                    assert!(manager.connected_backend(&old.member.machine_id).is_err());
                }
                // This is exactly the remote baseline the public adapter appends;
                // serialization cannot turn its revocation back into live authority.
                let encoded = serde_json::to_value(&baseline).unwrap();
                assert!(encoded["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["state"] != "live"));
                assert_eq!(manager.shutdown_phase(), ShutdownPhase::Running);
                assert_eq!(manager.admission_baseline(), baseline);
                drop(publication);
                assert!(shutdown.join().unwrap());
                baseline
            });
            assert_eq!(manager.shutdown_phase(), ShutdownPhase::Draining);
            assert_eq!(manager.admission_baseline(), revoked);
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_shutdown_feed_revokes_before_close() {
        with_retirement_fixture(|fixture| async move {
            let manager = &fixture.manager;
            retirement_connect(manager, RETIRE_A).await;
            let before = manager.admission_baseline();
            let rows = Arc::new(Mutex::new(Vec::new()));
            let seen = rows.clone();
            manager.subscribe(Arc::new(move |event| {
                if let Some(row) = event.admission.clone() {
                    lock(&seen).push(row);
                }
            }));
            assert!(manager.begin_shutdown());
            assert_eq!(manager.shutdown_phase(), ShutdownPhase::Draining);
            for old in before.rows {
                let rows = lock(&rows);
                let revoked = rows.iter().find(|row| row.member == old.member).unwrap();
                assert!(revoked.revision > old.revision);
                assert!(!matches!(revoked.state, RuntimeAdmissionState::Live { .. }));
                assert!(manager.connected_backend(&old.member.machine_id).is_err());
            }
            // with_retirement_fixture releases the still-held close gates only
            // after these admission assertions, then joins the real owners.
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_dead_or_pending_revoked_client_cannot_publish_or_execute() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let admitted = manager.connected_backend("box-123456").unwrap();
        let live = manager.admission_baseline().rows.remove(0);
        assert!(matches!(live.state, RuntimeAdmissionState::Live { .. }));
        lock(&manager.core).machines["box-123456"]
            .submitted
            .disconnect
            .fetch_add(1, Ordering::SeqCst);
        assert!(manager.connected_backend("box-123456").is_err());
        assert!(manager
            .cache_verified_layout(
                &admitted,
                rm4c_layout(admitted.server_id(), admitted.client().epoch())
            )
            .is_err());
        let revoked = manager.admission_baseline().rows.remove(0);
        assert!(revoked.revision > live.revision);
        assert!(!matches!(revoked.state, RuntimeAdmissionState::Live { .. }));
        driver.close_gate.release();
        manager.begin_shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_historical_member_refuses_retained_command_target() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let backend = manager.connected_backend("box-123456").unwrap();
        let live = manager.admission_baseline().rows.remove(0);
        let RuntimeAdmissionState::Live { namespace } = live.state else {
            panic!("connected member must be live");
        };
        let reference = crate::commands::machines::RuntimeSessionReference::Admitted(Box::new(
            crate::commands::machines::RuntimeSessionTarget {
                runtime: RuntimeMemberTarget {
                    member: live.member.clone(),
                    admission_revision: live.revision,
                    namespace: namespace.clone(),
                },
                session: codesense_pty_core::domain::PtySessionId::new("retained-target"),
            },
        ));
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(manager);
        let manager = app.state::<HostConnectionManager>();
        let calls = std::cell::Cell::new(0);
        let invoke = || {
            crate::commands::pty_backend::with_session_reference(
                app.handle(),
                &reference,
                true,
                |_, _| {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            )
        };
        assert!(invoke().is_ok());
        assert_eq!(calls.get(), 1);
        let client = backend.client();
        manager
            .cache_verified_layout(&backend, rm4c_layout(client.server_id(), client.epoch()))
            .unwrap();
        let generation = lock(&manager.core).machines["box-123456"]
            .connected_generation
            .unwrap();
        manager.capture_exact_loss("box-123456", backend.incarnation(), generation, &client);
        let historical = manager.admission_baseline().rows.remove(0);
        assert_eq!(historical.member, live.member);
        assert!(historical.revision > live.revision);
        assert!(
            matches!(historical.state, RuntimeAdmissionState::Historical { historical }
            if historical.last_namespace == Some(namespace))
        );
        assert!(invoke().is_err());
        assert_eq!(
            calls.get(),
            1,
            "historical member must not reach the operation"
        );
        driver.close_gate.release();
        manager.begin_shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_late_snapshot_cannot_publish_after_revocation() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let stale = manager.connected_backend("box-123456").unwrap();
        let client = stale.client();
        let generation = lock(&manager.core).machines["box-123456"]
            .connected_generation
            .unwrap();
        manager.capture_exact_loss("box-123456", stale.incarnation(), generation, &client);
        assert!(manager
            .cache_verified_layout(&stale, rm4c_layout(client.server_id(), client.epoch()))
            .is_err());
        assert!(lock(&manager.core).machines["box-123456"]
            .last_layout
            .is_none());
        assert!(!matches!(
            manager.admission_baseline().rows[0].state,
            RuntimeAdmissionState::Live { .. }
        ));
        driver.close_gate.release();
        manager.begin_shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm4c_stale_layout_cannot_publish_after_fingerprint_authority_changes() {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, "box-123456".into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let stale = manager.connected_backend("box-123456").unwrap();
        driver.close_gate.release();
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "replacement", false)),
            )
            .await;
        assert!(matches!(
            manager.cache_verified_layout(&stale, rm4c_layout("test-server", "epoch")),
            Err(ConnectedBackendError::Disconnected | ConnectedBackendError::IdentityMismatch)
        ));
        let gate = &lock(&manager.core).machines["box-123456"];
        assert!(gate.last_layout.is_none());
        assert!(gate.incompatible_snapshot.is_none());
    }

    #[tokio::test]
    async fn rm4c_removal_tombstones_cached_and_frozen_layout_authority() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        manager
            .observe_mutation(
                manager.reserve_observation(),
                "box-123456".into(),
                Some(remote("box-123456", "first", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut("box-123456").unwrap();
            gate.last_layout = Some(rm4c_layout("server", "epoch"));
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: gate.last_layout.clone(),
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "server".into(),
                epoch: "epoch".into(),
                rev: 1,
                shutdown_for_replacement: false,
                host_pid: 0,
            });
        }
        manager
            .observe_mutation(manager.reserve_observation(), "box-123456".into(), None)
            .await;
        let gate = &lock(&manager.core).machines["box-123456"];
        assert!(!gate.present);
        assert!(gate.last_layout.is_none());
        assert!(gate.incompatible_snapshot.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replacement_completion_uses_admission_census_and_stale_failure_cannot_replace_newer_attempt(
    ) {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        driver.release_close_gates();
        let manager = HostConnectionManager::with_attempt_driver(driver);
        let id = "box-123456";
        manager
            .observe_mutation(
                manager.reserve_observation(),
                id.into(),
                Some(remote(id, "box", false)),
            )
            .await;
        {
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(id).unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 1,
                proof: Some(machines::LegacyHostProof::test_proof("test-server")),
            };
            gate.frozen_generation = Some(AttemptGeneration(7));
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: None,
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "test-server".into(),
                epoch: "retired-epoch".into(),
                rev: 1,
                shutdown_for_replacement: true,
                host_pid: 0,
            });
        }
        let prompt = manager.reserve_replacement(id).unwrap();
        let reservation = manager
            .consume_replacement(id, &prompt.server_token)
            .unwrap();
        assert!(manager.replacement_still_current(id, &reservation));
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::Restart)
            .await
            .unwrap();
        let published = manager.connected_backend(id).unwrap();
        let published_generation = lock(&manager.core).machines[id]
            .connected_generation
            .unwrap();
        // A session may become visible immediately after Connected publication.
        // It belongs to the new daemon and must not invalidate the empty greeting.
        let mut layout = rm4c_layout(published.server_id(), published.client().epoch());
        let workspace = layout
            .workspaces
            .entry(codesense_pty_core::domain::WorkspaceKey::new(
                "new-workspace",
            ))
            .or_default();
        workspace.tabs.push(codesense_pty_core::layout::TabEntry {
            session: codesense_pty_core::domain::PtySessionId::new("new-session"),
            name: "New terminal".into(),
            role: None,
            placement: codesense_pty_core::layout::Placement::Tab,
            state: codesense_pty_core::layout::EntryState::Live,
            tags: Default::default(),
        });
        manager.cache_verified_layout(&published, layout).unwrap();
        assert_eq!(
            manager
                .replacement_admission(id, &prompt.server_token)
                .unwrap()
                .census,
            0
        );
        manager.revalidate_backend(&published).unwrap();
        manager.begin_attempt(id, ConnectOrigin::Retry).unwrap();
        manager.finish_replacement_failure(id, &reservation, true, "late result A".into());
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::Connecting)
        ));
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::Retry)
            .await
            .unwrap();
        let newer = manager.connected_backend(id).unwrap();
        manager.finish_replacement_failure(
            id,
            &reservation,
            true,
            "late result A after B published".into(),
        );
        manager.revalidate_backend(&newer).unwrap();
        assert!(manager
            .replacement_admission(id, &prompt.server_token)
            .is_err());
        manager.capture_exact_loss(
            id,
            published.incarnation(),
            published_generation,
            &published.client(),
        );
        manager.revalidate_backend(&newer).unwrap();
        let newer_generation = lock(&manager.core).machines[id]
            .connected_generation
            .unwrap();
        manager.capture_exact_loss(id, newer.incarnation(), newer_generation, &newer.client());
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::Unreachable { .. })
        ));
        manager
            .recover_exact_lost_client(
                None,
                id.into(),
                newer.incarnation(),
                newer_generation,
                newer.client(),
            )
            .await;
        assert!(lock(&manager.core).machines[id].connected.is_none());
        manager.disconnect(id).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compatible_replacement_preserves_pending_backend_and_consumes_once() {
        let driver = Arc::new(ConnectedRealOwnerDriver::with_replacement_support(true));
        driver.release_close_gates();
        let (lease, _revoke) = super::super::auth::private_authority::test_lease(1);
        *lock(&driver.private_lease) = Some(lease);
        let manager = HostConnectionManager::with_attempt_driver(driver);
        let id = "box-123456";
        manager
            .observe_mutation(
                manager.reserve_observation(),
                id.into(),
                Some(remote(id, "box", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let before = manager.connected_backend(id).unwrap();
        let prompt = manager.prepare_replacement(None, id).await.unwrap();
        assert_eq!(prompt.count, None);
        manager.revalidate_backend(&before).unwrap();
        {
            let mut core = lock(&manager.core);
            core.machines
                .get_mut(id)
                .unwrap()
                .replacement
                .as_mut()
                .unwrap()
                .expires_at = std::time::Instant::now();
        }
        assert!(manager
            .consume_replacement(id, &prompt.server_token)
            .is_err());
        manager.revalidate_backend(&before).unwrap();
        let prompt = manager.prepare_replacement(None, id).await.unwrap();
        assert!(manager
            .consume_replacement("another-machine", &prompt.server_token)
            .is_err());
        manager.revalidate_backend(&before).unwrap();
        let reservation = manager
            .consume_replacement(id, &prompt.server_token)
            .unwrap();
        manager.capture_exact_loss(
            id,
            before.incarnation,
            reservation.generation,
            &before.client,
        );
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::Replacing { .. })
        ));
        manager
            .recover_exact_lost_client(
                None,
                id.into(),
                before.incarnation,
                reservation.generation,
                before.client.clone(),
            )
            .await;
        assert!(matches!(
            manager.state(id),
            Some(HostConnectionState::Replacing { .. })
        ));
        assert!(manager.connected_backend(id).is_err());
        assert!(manager
            .consume_replacement(id, &prompt.server_token)
            .is_err());
        manager.finish_replacement_failure(
            id,
            &reservation,
            false,
            "proof changed before drain".into(),
        );
        manager.revalidate_backend(&before).unwrap();
        manager.disconnect(id).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compatible_replacement_rechecks_registry_after_async_proof() {
        let driver = Arc::new(ConnectedRealOwnerDriver::with_replacement_support(true));
        driver.release_close_gates();
        let (lease, _revoke) = super::super::auth::private_authority::test_lease(1);
        *lock(&driver.private_lease) = Some(lease);
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        let id = "box-123456";
        manager
            .observe_mutation(
                manager.reserve_observation(),
                id.into(),
                Some(remote(id, "box", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let before = manager.connected_backend(id).unwrap();
        let core = manager.core.clone();
        *lock(&driver.replacement_proof_hook) = Some(Box::new(move || {
            lock(&core).machines.get_mut(id).unwrap().fingerprint = "changed-during-proof".into();
        }));
        assert!(manager.prepare_replacement(None, id).await.is_err());
        assert!(lock(&manager.core).machines[id].replacement.is_none());
        manager.revalidate_backend(&before).unwrap();
        manager.disconnect(id).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compatible_replacement_refuses_unsupported_and_revoked_authority() {
        for supported in [false, true] {
            let driver = Arc::new(ConnectedRealOwnerDriver::with_replacement_support(
                supported,
            ));
            driver.release_close_gates();
            let (lease, revoke) = super::super::auth::private_authority::test_lease(1);
            *lock(&driver.private_lease) = Some(lease);
            let manager = HostConnectionManager::with_attempt_driver(driver);
            let id = "box-123456";
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, "box", false)),
                )
                .await;
            manager
                .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
                .await
                .unwrap();
            if supported {
                let prompt = manager.prepare_replacement(None, id).await.unwrap();
                revoke();
                assert!(manager
                    .consume_replacement(id, &prompt.server_token)
                    .is_err());
            } else {
                assert!(manager.prepare_replacement(None, id).await.is_err());
                assert!(manager.connected_backend(id).is_ok());
            }
            manager.disconnect(id).await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_replacement_blocks_captured_legacy_read_and_clears_only_changed_authority_cache(
    ) {
        let driver = Arc::new(ConnectedRealOwnerDriver::new());
        driver.release_close_gates();
        let manager = HostConnectionManager::with_attempt_driver(driver.clone());
        let id = "box-123456";
        manager
            .observe_mutation(
                manager.reserve_observation(),
                id.into(),
                Some(remote(id, "box", false)),
            )
            .await;
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::ExplicitForeground)
            .await
            .unwrap();
        let legacy = manager.connected_backend(id).unwrap();
        let layout = rm4c_layout(legacy.server_id(), legacy.client().epoch());
        manager
            .cache_verified_layout(&legacy, layout.clone())
            .unwrap();
        let (captured_ids, frozen) = manager.legacy_layout_candidates();
        assert_eq!(captured_ids, vec![id.to_string()]);
        // Scheduling barrier: a real private transport replaces the legacy
        // client AFTER selection but BEFORE the compatibility adapter resolves it.
        let (lease, switch) = super::super::auth::private_authority::test_switching_lease(1);
        *lock(&driver.private_lease) = Some(lease.clone());
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::Retry)
            .await
            .unwrap();
        let private = manager.connected_backend(id).unwrap();
        let resolved = super::super::pty_backend::resolve_remote_backend(&manager, id).unwrap();
        let private_target = resolved.admission_target().unwrap().clone();
        assert!(Arc::ptr_eq(&resolved.client().unwrap(), &private.client()));
        assert_eq!(
            resolved.client().unwrap().private_lease().unwrap().epoch(),
            lease.epoch()
        );
        assert!(manager.capture_admitted_backend(&private_target).is_ok());
        assert_eq!(
            private.client().private_lease().unwrap().epoch(),
            lease.epoch()
        );
        assert!(lock(&manager.core).machines[id].last_layout.is_none());
        let read_calls = std::cell::Cell::new(0);
        assert!(manager
            .read_legacy_layouts(captured_ids, frozen, |_| {
                read_calls.set(read_calls.get() + 1);
                Ok(layout.clone())
            })
            .is_empty());
        assert_eq!(
            read_calls.get(),
            0,
            "private snapshot must not even be read by legacy projection"
        );
        assert!(manager
            .cache_verified_layout(&legacy, layout.clone())
            .is_err());
        manager
            .cache_verified_layout(&private, layout.clone())
            .unwrap();
        // Normal reconnect under the SAME lease preserves its own history.
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::Retry)
            .await
            .unwrap();
        assert_eq!(
            lock(&manager.core).machines[id].last_layout,
            Some(layout.clone())
        );
        // A -> B -> A mints a new authority even when server/epoch are identical.
        let _other = switch(2);
        let fresh = switch(1);
        assert!(manager.capture_admitted_backend(&private_target).is_err());
        assert!(resolved
            .client()
            .unwrap()
            .private_lease()
            .unwrap()
            .validate()
            .is_err());
        *lock(&driver.private_lease) = Some(fresh.clone());
        manager
            .connect_remote_inner(None, id.into(), ConnectOrigin::Retry)
            .await
            .unwrap();
        assert_eq!(
            manager
                .connected_backend(id)
                .unwrap()
                .client()
                .private_lease()
                .unwrap()
                .epoch(),
            fresh.epoch()
        );
        assert!(lock(&manager.core).machines[id].last_layout.is_none());
        assert!(manager.cache_verified_layout(&private, layout).is_err());
        manager.begin_shutdown();
    }

    #[tokio::test]
    async fn private_baseline_keeps_epoch_and_legacy_projection_never_reads_private_history() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        let (lease, revoke) = super::super::auth::private_authority::test_lease(1);
        for id in ["private-123456", "legacy-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
            let mut core = lock(&manager.core);
            let gate = core.machines.get_mut(id).unwrap();
            gate.state = HostConnectionState::Incompatible {
                n_sessions: 1,
                proof: None,
            };
            gate.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: Some(rm4c_layout(id, "epoch")),
                census: CensusSnapshot { live_sessions: 1 },
                server_id: id.into(),
                epoch: "epoch".into(),
                rev: 1,
                shutdown_for_replacement: false,
                host_pid: 0,
            });
            if id.starts_with("private") {
                gate.private_lease = Some(lease.clone());
            }
        }
        let (baseline, leases) = manager.admission_baseline_with_private_leases();
        let row = baseline
            .rows
            .iter()
            .find(|row| row.member.machine_id == "private-123456")
            .unwrap();
        assert_eq!(row.private_authority_epoch.as_deref(), Some("1"));
        assert!(matches!(
            row.state,
            RuntimeAdmissionState::Historical { .. }
        ));
        assert_eq!(leases["private-123456"].epoch(), "1");
        let legacy = || {
            manager.verified_remote_layouts(
                &codesense_pty_core::domain::ViewerId::new("v"),
                &codesense_pty_core::domain::ClientBootId::new("b"),
            )
        };
        assert_eq!(legacy(), vec![rm4c_layout("legacy-123456", "epoch")]);
        revoke();
        let row = manager
            .admission_baseline()
            .rows
            .into_iter()
            .find(|row| row.member.machine_id == "private-123456")
            .unwrap();
        assert_eq!(row.private_authority_epoch.as_deref(), Some("1"));
        assert_eq!(row.state, RuntimeAdmissionState::Unavailable);
        // Even loss of the exact client/lease cannot downgrade retained history.
        lock(&manager.core)
            .machines
            .get_mut("private-123456")
            .unwrap()
            .private_lease = None;
        assert_eq!(
            manager
                .admission_baseline()
                .rows
                .into_iter()
                .find(|row| row.member.machine_id == "private-123456")
                .unwrap()
                .private_authority_epoch
                .as_deref(),
            Some("1")
        );
        assert_eq!(legacy(), vec![rm4c_layout("legacy-123456", "epoch")]);
    }

    #[tokio::test]
    async fn rm4c_layout_aggregation_isolates_one_remote_failure() {
        let manager = HostConnectionManager::with_attempt_driver(Arc::new(NeverOpenDriver));
        for id in ["frozen-123456", "broken-123456"] {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.into(),
                    Some(remote(id, id, false)),
                )
                .await;
        }
        let expected = rm4c_layout("frozen-server", "frozen-epoch");
        {
            let mut core = lock(&manager.core);
            let frozen = core.machines.get_mut("frozen-123456").unwrap();
            frozen.state = HostConnectionState::Incompatible {
                n_sessions: 1,
                proof: Some(machines::LegacyHostProof::test_proof("server-a")),
            };
            frozen.incompatible_snapshot = Some(LastKnownHostSnapshot {
                layout: Some(expected.clone()),
                census: CensusSnapshot { live_sessions: 1 },
                server_id: "frozen-server".into(),
                epoch: "frozen-epoch".into(),
                rev: 1,
                shutdown_for_replacement: false,
                host_pid: 0,
            });
            // Deliberately malformed connected publication: aggregation must
            // isolate this machine instead of hiding the verified peer.
            core.machines.get_mut("broken-123456").unwrap().state =
                HostConnectionState::Connected {
                    server_id: "broken-server".into(),
                };
        }
        assert_eq!(
            manager.verified_remote_layouts(
                &codesense_pty_core::domain::ViewerId::new("viewer"),
                &codesense_pty_core::domain::ClientBootId::new("boot"),
            ),
            vec![expected]
        );
    }
}
