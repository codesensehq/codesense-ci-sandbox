// SPDX-License-Identifier: MIT
//! The delivery core inside the app: the terminal servers it reaches, the
//! window it tells, the (re)connects that trigger a catch-up, and the pushed
//! outcomes `pty_host` hands it.

use std::sync::Arc;

use codesense_pty_core::session_wake::WakeOutcome;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::events::{
    SessionTimersChanged, SyncHealth, WakeDeliveredEvent, WakeEventSink, WakeOutcomeEvent,
    SESSION_TIMERS_CHANGED_EVENT, WAKE_DELIVERED_EVENT, WAKE_OUTCOME_EVENT, WAKE_SYNC_HEALTH_EVENT,
};
use super::host::{HostLookup, HostResolver};
use super::store::RecordDir;
use super::DeliveryService;
use crate::commands::host_connection_manager::{
    ConnectedBackendError, HostConnectionManager, HostConnectionState,
};
use crate::commands::machines::{
    RuntimeAdmissionRow, RuntimeAdmissionState, RuntimeMemberSlot, LOCAL_MACHINE_ID,
};
use crate::terminal::session_server::{EventNamespace, SessionServerState};

/// The app's delivery core, managed as tauri state.
pub(crate) type DeliveryState = Arc<DeliveryService>;

struct AppSink<R: Runtime>(AppHandle<R>);

impl<R: Runtime> WakeEventSink for AppSink<R> {
    fn delivered(&self, event: &WakeDeliveredEvent) {
        if let Err(error) = self.0.emit(WAKE_DELIVERED_EVENT, event) {
            log::warn!(target: "wake", "emit {WAKE_DELIVERED_EVENT}: {error}");
        }
    }
    fn outcome(&self, event: &WakeOutcomeEvent) {
        if let Err(error) = self.0.emit(WAKE_OUTCOME_EVENT, event) {
            log::warn!(target: "wake", "emit {WAKE_OUTCOME_EVENT}: {error}");
        }
    }
    fn sync_health(&self, health: &SyncHealth) {
        if let Err(error) = self.0.emit(WAKE_SYNC_HEALTH_EVENT, health) {
            log::warn!(target: "wake", "emit {WAKE_SYNC_HEALTH_EVENT}: {error}");
        }
    }
    fn timers_changed(&self, event: &SessionTimersChanged) {
        if let Err(error) = self.0.emit(SESSION_TIMERS_CHANGED_EVENT, event) {
            log::warn!(target: "wake", "emit {SESSION_TIMERS_CHANGED_EVENT}: {error}");
        }
    }
    /// SW-AGR-13: to the human inbox, through the routines' wake environment
    /// (the routine's team, else the team in view).
    fn notice(&self, event: &super::events::WakeNoticeEvent) {
        let env = crate::commands::paths::codesense_home_dir()
            .and_then(|home| crate::routines::wake::env_for(&home));
        match env {
            Some(env) => crate::routines::wake::WakeNotify::post(
                env.as_ref(),
                &crate::routines::wake::delivery_notice(event),
            ),
            None => {
                log::warn!(target: "wake", "wake notice not posted (no inbox yet): {}", event.text)
            }
        }
    }
}

/// How long a failed catch-up waits before each retry (while the machine
/// stays reachable). Then it waits for the next (re)connect.
const SYNC_BACKOFF: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(4),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
];

/// This Mac's session server, or a connected machine's.
struct AppResolver<R: Runtime>(AppHandle<R>);

impl<R: Runtime> HostResolver for AppResolver<R> {
    fn resolve(&self, machine: &str) -> HostLookup {
        if machine == LOCAL_MACHINE_ID {
            return match self
                .0
                .try_state::<SessionServerState>()
                .and_then(|state| state.connected())
            {
                Some(client) => HostLookup::Ready(client),
                None => HostLookup::Unavailable {
                    machine: "This Mac".into(),
                    detail: "its terminal server is not running".into(),
                },
            };
        }
        let Some(manager) = self.0.try_state::<HostConnectionManager>() else {
            return HostLookup::Unknown;
        };
        match manager.connected_backend(machine) {
            Ok(snapshot) => HostLookup::Ready(snapshot.client()),
            Err(ConnectedBackendError::UnknownMachine) => HostLookup::Unknown,
            Err(ConnectedBackendError::Incompatible { .. })
            | Err(ConnectedBackendError::UnsupportedLegacy) => HostLookup::Unavailable {
                machine: machine.to_string(),
                detail: "its terminal server is an incompatible version".into(),
            },
            Err(ConnectedBackendError::Unreachable { last_error }) => {
                HostLookup::Offline { detail: last_error }
            }
            Err(
                ConnectedBackendError::Disconnected
                | ConnectedBackendError::Connecting
                | ConnectedBackendError::Handshaking
                | ConnectedBackendError::IdentityMismatch
                | ConnectedBackendError::ManagerShuttingDown,
            ) => HostLookup::Offline {
                detail: "not connected".into(),
            },
        }
    }
}

/// Start the delivery core: manage it, and catch up with every terminal
/// server whenever it (re)connects. Call once both the local session server
/// state and the connection manager are managed.
pub(crate) fn install<R: Runtime>(app: &AppHandle<R>) {
    if app.try_state::<DeliveryState>().is_some() {
        return;
    }
    let home = crate::commands::paths::codesense_home_dir();
    let routine_host = home
        .as_deref()
        .and_then(|home| crate::routines::host::local_host_id(home).ok())
        .unwrap_or_default();
    let service = Arc::new(DeliveryService::new(
        Arc::new(AppResolver(app.clone())),
        Arc::new(AppSink(app.clone())),
        home.as_deref()
            .map(RecordDir::under_home)
            .unwrap_or_else(|| RecordDir::at(None)),
        routine_host,
    ));
    app.manage::<DeliveryState>(service.clone());
    // The run row of a routine's wake follows its delivery (R7): only for
    // wakes this desktop asked for, never for a re-read. An `unknown` posts
    // once per run to the human inbox (SW-AGR-2).
    service.subscribe(Arc::new(record_wake_run));
    if let Some(local) = app.try_state::<SessionServerState>() {
        let observed = service.clone();
        local
            .admission
            .subscribe(Arc::new(move |row: RuntimeAdmissionRow| {
                if row.member.slot == RuntimeMemberSlot::Sidecar
                    && matches!(row.state, RuntimeAdmissionState::Live { .. })
                {
                    sync_in_background(observed.clone(), LOCAL_MACHINE_ID.to_string());
                }
            }));
    }
    if let Some(manager) = app.try_state::<HostConnectionManager>() {
        let observed = service.clone();
        manager.subscribe(Arc::new(move |event| {
            if matches!(event.state, HostConnectionState::Connected { .. }) {
                sync_in_background(observed.clone(), event.machine_id.clone());
            }
        }));
    }
    // A local session server that connected before this subscription.
    sync_in_background(service, LOCAL_MACHINE_ID.to_string());
}

/// Catch up with `machine` off the caller's thread (observers run inside the
/// connection machinery and must not wait on a round trip).
fn sync_in_background(service: Arc<DeliveryService>, machine: String) {
    let spawned = std::thread::Builder::new()
        .name("wake-sync".into())
        .spawn(move || {
            if let Err(error) = service.sync_with_retry(&machine, &SYNC_BACKOFF, std::thread::sleep)
            {
                log::warn!(target: "wake", "wake sync with {machine} gave up until it reconnects: {error}");
            }
        });
    if let Err(error) = spawned {
        log::warn!(target: "wake", "wake sync thread: {error}");
    }
}

/// A `TerminalDomainEvent::Wake` from this Mac's session server.
pub(crate) fn on_local_wake<R: Runtime>(app: &AppHandle<R>, outcome: WakeOutcome) {
    if let Some(service) = app.try_state::<DeliveryState>() {
        service.on_pushed(LOCAL_MACHINE_ID, outcome);
    }
}

/// A `TerminalDomainEvent::Wake` from the machine whose connection carries
/// `namespace`. One whose connection is no longer admitted is dropped: the
/// next connect's catch-up reads it from the log.
pub(crate) fn on_remote_wake<R: Runtime>(
    app: &AppHandle<R>,
    namespace: &EventNamespace,
    outcome: WakeOutcome,
) {
    let Some(service) = app.try_state::<DeliveryState>() else {
        return;
    };
    let machine = app
        .try_state::<HostConnectionManager>()
        .and_then(|manager| manager.target_for_namespace(namespace))
        .map(|target| target.member.machine_id);
    if let Some(machine) = machine {
        service.on_pushed(&machine, outcome);
    }
}

/// The posture of a session from what its launch recorded (the agent
/// revision it launched, never the agent's current definition, and whether a
/// raw launch skipped permission checks). Fails closed: see
/// [`super::authority::posture_from_launch`].
pub(crate) fn session_posture<R: Runtime>(
    app: &AppHandle<R>,
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    session: &str,
) -> super::authority::SessionPosture {
    let launch = app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>();
    let stamp = launch
        .as_ref()
        .and_then(|launch| launch.agent_stamp_for_session(session));
    let raw_bypass = launch
        .as_ref()
        .and_then(|launch| launch.remote_shell_for_session(session))
        .is_some_and(|record| !record.permission_mode.is_default());
    super::authority::posture_from_launch(stamp.as_deref(), raw_bypass, |key, revision| {
        crate::agents::service::get_pinned(home, principal, key, revision)
            .ok()
            .map(|pinned| pinned.agent)
    })
}

/// Record a routed wake outcome on its routine run (SW-AGR-2: an `unknown`
/// posts once to the human inbox; a wake whose agent quit is resolved again).
fn record_wake_run(outcome: &super::RoutedOutcome) {
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        return;
    };
    if let Some(store) = crate::routines::finish::app_store() {
        match crate::routines::wake::env_for(&home) {
            Some(env) => crate::routines::wake::record_routed(&store, outcome, env.as_ref()),
            None => crate::routines::wake::record_routed(
                &store,
                outcome,
                &crate::routines::wake::NoNotify,
            ),
        }
    }
}
