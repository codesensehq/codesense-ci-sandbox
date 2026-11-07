// SPDX-License-Identifier: MIT
//! Shared real SSH admission for independent manager and backend consumers.
//!
//! Ignored SSH helpers must run under C2's validated manifest/topology,
//! `fresh_slots` and `capture`, with exact discovery and serial fixture ownership.
//! A failed cleanup deadline is not proof of joined owners: C2 capture must reap
//! the invocation group before fresh_slots restores anything, preserving slots
//! if ownership remains unresolved. Raw standalone Cargo execution is unsupported.
use super::*;
use crate::commands::machines::rm4f_support::{Endpoint, OwnedMachines};
use crate::commands::pty_host::{StreamHost, StreamIds};
use crate::terminal::session_server::{NamespacedTerminalEvent, NamespacedTerminalEventSink};
use codesense_pty_core::domain::{PortUnavailable, TerminalDomainEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationPhase {
    BeforeHandshake,
    BeforePublication,
    AfterConstruction,
}

struct ValidationBarrier {
    phase: ValidationPhase,
    entered: Notify,
    release: tokio::sync::Semaphore,
}

struct ValidationHold(Arc<ValidationBarrier>);
impl Drop for ValidationHold {
    fn drop(&mut self) {
        self.0.release.add_permits(1);
    }
}

#[derive(Default)]
pub(crate) struct RecordingPresentation {
    pub streams: Arc<StreamIds>,
    pub events: Mutex<Vec<NamespacedTerminalEvent>>,
    pub output: Mutex<Vec<(u64, String)>>,
    pub trace: Mutex<Vec<OracleTrace>>,
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OracleTrace {
    Terminal {
        event: NamespacedTerminalEvent,
    },
    Publication {
        machine_id: String,
        namespace: Option<EventNamespace>,
    },
}

impl NamespacedTerminalEventSink for RecordingPresentation {
    fn emit(&self, event: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        lock(&self.trace).push(OracleTrace::Terminal {
            event: event.clone(),
        });
        if let TerminalDomainEvent::StreamOutput { stream, data, seq } = &event.event {
            self.streams.deliver_output(
                StreamHost::remote_namespace(&event.namespace),
                *stream,
                data.clone(),
                *seq,
                &mut |local, data, _| {
                    lock(&self.output).push((local, data.into()));
                    true
                },
            );
        }
        lock(&self.events).push(event);
        Ok(())
    }
}

struct OwnedDriver {
    machines: Arc<OwnedMachines>,
    presentation: Arc<RecordingPresentation>,
    state: SessionServerState,
    validation: Mutex<Option<Arc<ValidationBarrier>>>,
    rejected: Mutex<Vec<(ValidationPhase, PinnedTransportGenerationError)>>,
    opening_cancelled: Notify,
    opening_deadline: Notify,
    admitted_proofs: Mutex<BTreeMap<String, Vec<AdmittedProof>>>,
}

/// A read-only snapshot of the greeting on the actual admitted transport.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AdmittedProof {
    pub machine_id: String,
    pub server_id: String,
    pub epoch: String,
    pub pid: u32,
}

impl OwnedDriver {
    async fn validate(
        &self,
        phase: ValidationPhase,
        id: String,
        generation: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        let barrier = {
            let mut held = lock(&self.validation);
            if held.as_ref().is_some_and(|barrier| barrier.phase == phase) {
                held.take()
            } else {
                None
            }
        };
        if let Some(barrier) = barrier {
            barrier.entered.notify_one();
            barrier
                .release
                .acquire()
                .await
                .expect("validation release")
                .forget();
        }
        let machines = self.machines.clone();
        let result = tokio::task::spawn_blocking(move || machines.validate(&id, &generation))
            .await
            .map_err(|e| PinnedTransportGenerationError::Missing(e.to_string()))?;
        if let Err(error) = &result {
            lock(&self.rejected).push((phase, error.clone()));
        }
        result
    }
}

#[async_trait::async_trait]
impl RemoteAttemptDriver for OwnedDriver {
    async fn legacy_proof(
        &self,
        _app: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<Option<machines::LegacyHostProof>, String> {
        let machines = self.machines.clone();
        crate::commands::await_blocking("owned legacy proof", move || machines.legacy_proof(&id))
            .await
    }

    async fn open(
        &self,
        _app: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        self.machines
            .begin_open(id)
            .finish()
            .await
            .map(RemoteTransportOwner::from_pinned)
    }

    async fn open_before(
        &self,
        _app: Option<tauri::AppHandle>,
        id: String,
        mut deadline: Deadline<'_>,
        token: CancellationToken,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        let mut attempt = self.machines.begin_open(id);
        tokio::select! {
            result = attempt.wait() => result.map(RemoteTransportOwner::from_pinned),
            _ = &mut deadline => {
                self.opening_deadline.notify_one();
                attempt.cancel_and_reap().await.map_err(machines::PinnedSshOpenError::Other)?;
                Err(machines::PinnedSshOpenError::Other("owned SSH opening deadline exceeded".into()))
            }
            _ = token.cancelled() => {
                self.opening_cancelled.notify_one();
                attempt.cancel_and_reap().await.map_err(machines::PinnedSshOpenError::Other)?;
                Err(machines::PinnedSshOpenError::Other("owned SSH opening cancelled".into()))
            }
        }
    }

    async fn revalidate_before_handshake(
        &self,
        pending: &PendingRemoteTransport,
        _app: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        pending
            .validate_generation_for_test(|generation| {
                self.validate(ValidationPhase::BeforeHandshake, id, generation)
            })
            .await
    }

    async fn revalidate_before_publication(
        &self,
        admitted: &AdmittedRemoteTransport,
        _app: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        self.validate(
            ValidationPhase::BeforePublication,
            id,
            admitted.publication_generation(),
        )
        .await
    }

    async fn revalidate_after_construction(
        &self,
        generation: String,
        _app: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        self.validate(ValidationPhase::AfterConstruction, id, generation)
            .await
    }

    async fn construct_client(
        &self,
        admitted: AdmittedRemoteTransport,
        _app: Option<tauri::AppHandle>,
        ledger: RemoteSessionLedger,
    ) -> Result<ManagedRemoteClient, String> {
        let Some(index) = self
            .machines
            .server_ids
            .iter()
            .position(|server_id| server_id == admitted.verified_server_id())
        else {
            admitted.close_awaited().await;
            return Err("admitted ServerId has no saved machine owner".into());
        };
        let proof = AdmittedProof {
            machine_id: self.machines.ids[index].clone(),
            server_id: admitted.verified_server_id().to_owned(),
            epoch: admitted.verified_epoch().to_owned(),
            pid: admitted.verified_pid(),
        };
        lock(&self.admitted_proofs)
            .entry(proof.machine_id.clone())
            .or_default()
            .push(proof);
        self.state
            .construct_remote_client(admitted, self.presentation.clone(), ledger)
            .await
            .map_err(|e| e.to_string())
    }
}

pub(crate) struct OwnedManagerHarness {
    pub manager: Arc<HostConnectionManager>,
    pub machines: Arc<OwnedMachines>,
    pub presentation: Arc<RecordingPresentation>,
    history: Arc<Mutex<Vec<ManagerEvent>>>,
    driver: Arc<OwnedDriver>,
}

const CLEANUP_DEADLINE: Duration = Duration::from_secs(20);

/// The task owns both application state and its SSH-owner observations even if
/// the caller's bounded observation fails. Only C2 capture may cancel the process.
struct OwnedDrain {
    task: tokio::task::JoinHandle<()>,
    manager: Arc<HostConnectionManager>,
    machines: Arc<OwnedMachines>,
}

impl OwnedDrain {
    async fn observe(&mut self, deadline: Duration) -> Result<(), String> {
        match tokio::time::timeout(deadline, &mut self.task).await {
            Ok(joined) => joined.map_err(|error| format!("owned drain failed: {error}")),
            Err(_) => Err(format!(
                "owned drain observation exceeded {deadline:?}; NOT joined: phase={:?}, active_operations={}, {}; the mandatory outer capture retains process-group cleanup authority",
                self.manager.shutdown_phase(),
                self.manager.active_operations.load(Ordering::SeqCst),
                self.machines.owner_diagnostics(),
            )),
        }
    }
}

impl OwnedManagerHarness {
    /// Drain on normal completion and assertion unwind, with a bounded observation.
    /// This wrapper must be awaited. Arbitrarily dropping/aborting it is unsupported;
    /// outer C2 capture owns process cancellation and the final cleanup backstop.
    pub(crate) async fn with_cleanup<F: std::future::Future>(&self, work: F) -> F::Output {
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(work).catch_unwind().await;
        self.drain().await;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    pub(crate) async fn supplied() -> Result<Self, String> {
        let endpoints = Endpoint::supplied_pair();
        let machines = crate::commands::await_blocking("enroll owned A/B", move || {
            OwnedMachines::enroll(endpoints)
        })
        .await?;
        Self::from_machines(machines).await
    }

    async fn from_machines(machines: Arc<OwnedMachines>) -> Result<Self, String> {
        let presentation = Arc::new(RecordingPresentation::default());
        let driver = Arc::new(OwnedDriver {
            machines: machines.clone(),
            presentation: presentation.clone(),
            state: SessionServerState::default(),
            validation: Mutex::new(None),
            rejected: Mutex::new(Vec::new()),
            opening_cancelled: Notify::new(),
            opening_deadline: Notify::new(),
            admitted_proofs: Mutex::new(BTreeMap::new()),
        });
        let manager = Arc::new(HostConnectionManager::with_attempt_driver(driver.clone()));
        let history = Arc::new(Mutex::new(Vec::new()));
        let recording = history.clone();
        let trace = presentation.clone();
        let observed = Arc::downgrade(&manager);
        manager.subscribe(Arc::new(move |event| {
            if matches!(event.state, HostConnectionState::Connected { .. }) {
                let namespace = observed.upgrade().and_then(|manager| {
                    manager
                        .connected_backend(&event.machine_id)
                        .ok()
                        .map(|backend| backend.client().namespace().clone())
                });
                lock(&trace.trace).push(OracleTrace::Publication {
                    machine_id: event.machine_id.clone(),
                    namespace,
                });
            }
            lock(&recording).push(event);
        }));
        *lock(&manager.retirement_for_test) = Some(Arc::new(TestRetirementPresentation {
            streams: presentation.streams.clone(),
            sink: presentation.clone(),
            before_permit: None,
        }));
        let weak = Arc::downgrade(&manager);
        *lock(&manager.loss_recovery_for_test) =
            Some(Arc::new(move |id, incarnation, generation, expected| {
                if let Some(manager) = weak.upgrade() {
                    tauri::async_runtime::spawn(async move {
                        manager
                            .recover_exact_lost_client(None, id, incarnation, generation, expected)
                            .await;
                    });
                }
            }));
        for id in &machines.ids {
            manager
                .observe_mutation(
                    manager.reserve_observation(),
                    id.clone(),
                    Some(machines.record(id)?),
                )
                .await;
        }
        Ok(Self {
            manager,
            machines,
            presentation,
            history,
            driver,
        })
    }

    pub(crate) async fn connect(&self, index: usize) -> Result<(), String> {
        self.manager
            .connect_remote_inner(
                None,
                self.machines.ids[index].clone(),
                ConnectOrigin::ExplicitForeground,
            )
            .await
            .map(|_| ())
    }

    pub(crate) fn admitted_proofs(&self, machine_id: &str) -> Vec<AdmittedProof> {
        lock(&self.driver.admitted_proofs)
            .get(machine_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) async fn drain(&self) {
        self.begin_drain()
            .observe(CLEANUP_DEADLINE)
            .await
            .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));
    }

    fn begin_drain(&self) -> OwnedDrain {
        self.manager.begin_shutdown();
        let manager = self.manager.clone();
        let machines = self.machines.clone();
        let task = tokio::spawn(async move {
            manager.drain_shutdown().await;
            assert_eq!(manager.shutdown_phase(), ShutdownPhase::Drained);
            machines.assert_reaped();
        });
        OwnedDrain {
            task,
            manager: self.manager.clone(),
            machines: self.machines.clone(),
        }
    }

    fn hold_validation(&self, phase: ValidationPhase) -> ValidationHold {
        let barrier = Arc::new(ValidationBarrier {
            phase,
            entered: Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        assert!(lock(&self.driver.validation)
            .replace(barrier.clone())
            .is_none());
        ValidationHold(barrier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::machines::rm4f_support::SavedDrift;
    use futures_util::FutureExt;

    #[test]
    fn recording_presentation_maps_same_wire_stream_to_distinct_locals() {
        use codesense_pty_core::domain::{PtySessionId, SubscribeResult, ViewportId};
        use codesense_pty_core::term_state::StreamId;
        let presentation = RecordingPresentation::default();
        let namespaces = ["server-a", "server-b"].map(|server| {
            crate::terminal::session_server::mint_event_namespace(server, "same-epoch").unwrap()
        });
        let subscription = SubscribeResult {
            snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
            stream_id: StreamId(7),
            story_id: None,
            workspace_id: None,
            epic_id: None,
        };
        let locals = namespaces.each_ref().map(|namespace| {
            presentation.streams.attach(
                StreamHost::remote_namespace(namespace),
                &PtySessionId::new("same-session"),
                &ViewportId::new("same-viewport"),
                &subscription,
            )
        });
        assert_ne!(locals[0], locals[1]);
        for (namespace, data) in namespaces.iter().zip(["A", "B"]) {
            presentation
                .emit(NamespacedTerminalEvent {
                    namespace: namespace.clone(),
                    event: TerminalDomainEvent::StreamOutput {
                        stream: StreamId(7),
                        data: data.into(),
                        seq: 1,
                    },
                })
                .unwrap();
        }
        assert_eq!(
            *lock(&presentation.output),
            [(locals[0], "A".into()), (locals[1], "B".into())]
        );
        let events = lock(&presentation.events);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].namespace, namespaces[0]);
        assert_eq!(events[1].namespace, namespaces[1]);
    }

    async fn bounded<T>(work: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(20), work)
            .await
            .expect("owned fixture barrier deadline")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires C2-owned A/B fresh slots"]
    async fn rm4f_c2a_generation_rejection() {
        let harness = OwnedManagerHarness::supplied().await.unwrap();
        harness.with_cleanup(async {
            for phase in [ValidationPhase::BeforeHandshake, ValidationPhase::BeforePublication, ValidationPhase::AfterConstruction] {
                for kind in [SavedDrift::Binding, SavedDrift::Pin] {
                    lock(&harness.history).clear();
                    let hold = harness.hold_validation(phase);
                    let manager = harness.manager.clone();
                    let id = harness.machines.ids[0].clone();
                    let attempt_id = id.clone();
                    let attempt = tokio::spawn(async move { manager.connect_remote_inner(None, attempt_id, ConnectOrigin::ExplicitForeground).await });
                    bounded(hold.0.entered.notified()).await;
                    let machines = harness.machines.clone();
                    let drift_id = id.clone();
                    let restore = crate::commands::await_blocking("drift temporary saved authority", move || machines.drift(&drift_id, kind)).await.unwrap();
                    drop(hold);
                    let rejection = bounded(attempt).await.unwrap();
                    assert!(rejection.is_err(), "stale {phase:?}/{kind:?} candidate published");
                    assert!(harness.manager.connected_backend(&id).is_err());
                    assert!(lock(&harness.history).iter().all(|event| {
                        !matches!(event.state, HostConnectionState::Connected { .. })
                    }), "rejected attempt published Connected in its observer history");
                    assert_eq!(
                        lock(&harness.driver.rejected).last(),
                        Some(&(phase, PinnedTransportGenerationError::Stale)),
                        "drift must reject the actual stale generation, not an unrelated missing resource"
                    );
                    harness.machines.assert_reaped();
                    crate::commands::await_blocking("restore temporary saved authority", move || restore.restore()).await.unwrap();
                    harness.connect(0).await.unwrap();
                    assert_eq!(lock(&harness.history).iter().filter(|event| {
                        matches!(event.state, HostConnectionState::Connected { .. })
                    }).count(), 1, "restored admission must positively exercise the same observer");
                    assert_eq!(harness.manager.connected_backend(&id).unwrap().server_id(), harness.machines.server_ids[0]);
                    harness.manager.disconnect(&id).await.unwrap();
                    harness.machines.assert_reaped();
                    eprintln!("RM4F C2a: {phase:?}/{kind:?} actual Stale generation rejection, no publication, restored reconnect, reaped");
                }
            }
            assert_eq!(lock(&harness.driver.rejected).len(), 6);
        }).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires C2-owned A/B fresh slots"]
    async fn rm4f_c2a_owned_cleanup() {
        let harness = OwnedManagerHarness::supplied().await.unwrap();
        harness.with_cleanup(async {
            for index in 0..2 { harness.connect(index).await.unwrap(); }
            for id in &harness.machines.ids { harness.manager.disconnect(id).await.unwrap(); }
            harness.machines.assert_reaped();
            let hold = harness.machines.hold_next_open();
            let manager = harness.manager.clone();
            let id = harness.machines.ids[0].clone();
            let attempt = tokio::spawn(async move { manager.connect_remote_inner(None, id, ConnectOrigin::ExplicitForeground).await });
            bounded(hold.0.entered()).await;
            assert!(harness.manager.begin_shutdown());
            bounded(harness.driver.opening_cancelled.notified()).await;
            assert!(!attempt.is_finished(), "cancel returned before owned opening worker joined");
            let drain = harness.manager.drain_shutdown();
            tokio::pin!(drain);
            assert!(futures_util::poll!(&mut drain).is_pending(), "drain returned before owned opening work joined");
            drop(hold);
            assert!(bounded(attempt).await.unwrap().is_err());
            bounded(drain).await;
            for id in &harness.machines.ids { assert!(harness.manager.connected_backend(id).is_err()); }
            harness.machines.assert_reaped();
            eprintln!("RM4F C2a: normal A/B close and cancelled opening reaped; drain awaited worker; no stale publication");
        }).await;

        // A work assertion must unwind through a bounded owned drain before
        // it reaches the outer assertion catcher. Reuse saved state, not slots.
        let panicking = OwnedManagerHarness::from_machines(harness.machines.clone())
            .await
            .unwrap();
        let panic = std::panic::AssertUnwindSafe(panicking.with_cleanup(async {
            panicking.connect(0).await.unwrap();
            panic!("deliberate C2a cleanup unwind");
        }))
        .catch_unwind()
        .await;
        let panic = panic.expect_err("deliberate assertion did not propagate");
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"deliberate C2a cleanup unwind")
        );
        assert_eq!(panicking.manager.shutdown_phase(), ShutdownPhase::Drained);
        panicking.machines.assert_reaped();
        eprintln!("RM4F C2a: deliberate panic propagated only after owned drain/reap");

        let deadline = OwnedManagerHarness::from_machines(harness.machines.clone())
            .await
            .unwrap();
        deadline.with_cleanup(async {
            let hold = deadline.machines.hold_next_open();
            let manager = deadline.manager.clone();
            let id = deadline.machines.ids[0].clone();
            let attempt = tokio::spawn(async move {
                manager.connect_remote_inner(None, id, ConnectOrigin::ExplicitForeground).await
            });
            bounded(hold.0.entered()).await;
            bounded(deadline.driver.opening_deadline.notified()).await;
            assert!(!attempt.is_finished(), "opening deadline abandoned its blocking worker");
            let mut drain = deadline.begin_drain();
            let diagnostic = drain.observe(Duration::from_millis(25)).await.unwrap_err();
            assert!(diagnostic.contains("NOT joined"));
            assert!(diagnostic.contains("active_operations=1"));
            assert!(!drain.task.is_finished(), "cleanup timeout lost its owned task");
            eprintln!("RM4F C2a: controlled cleanup deadline: {diagnostic}");
            drop(hold);
            let error = bounded(attempt).await.unwrap().unwrap_err();
            assert!(error.contains("opening deadline exceeded"), "{error}");
            drain.observe(CLEANUP_DEADLINE).await.unwrap();
            assert!(lock(&deadline.history).iter().all(|event| {
                !matches!(event.state, HostConnectionState::Connected { .. })
            }));
            deadline.machines.assert_reaped();
            eprintln!("RM4F C2a: held-opening deadline kept owner; retained cleanup task joined/reaped after release");
        }).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires C2-owned A/B fresh slots"]
    async fn rm4f_c2a_pinned_ab_admission() {
        let harness = OwnedManagerHarness::supplied().await.unwrap();
        let outcome = harness
            .with_cleanup(async {
                for index in 0..2 {
                    harness.connect(index).await?;
                    let snapshot = harness
                        .manager
                        .connected_backend(&harness.machines.ids[index])
                        .map_err(|e| format!("{e:?}"))?;
                    assert_eq!(snapshot.server_id(), harness.machines.server_ids[index]);
                    assert_eq!(snapshot.machine_id(), harness.machines.ids[index]);
                    assert!(harness.machines.record(snapshot.machine_id())?.set_up);
                    eprintln!(
                        "RM4F C2a: admitted {} with pinned ServerId {}",
                        snapshot.machine_id(),
                        snapshot.server_id()
                    );
                }
                assert!(Arc::ptr_eq(
                    &harness.presentation.streams,
                    &lock(&harness.manager.retirement_for_test)
                        .as_ref()
                        .unwrap()
                        .streams
                ));
                Ok::<_, String>(())
            })
            .await;
        outcome.unwrap();
    }
}
