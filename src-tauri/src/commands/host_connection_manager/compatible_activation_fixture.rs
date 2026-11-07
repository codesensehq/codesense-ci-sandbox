// SPDX-License-Identifier: MIT
//! Owned SSH qualification adapter. Only environment resolution and presentation
//! are injected; preparation, token consumption, replacement and admission use
//! the command's production manager owners and the pinned SSH implementations.
use super::*;
use codesense_session_server::testing::RecordingSink;

type Validate = Arc<dyn Fn(&str, &str) -> Result<(), PinnedTransportGenerationError> + Send + Sync>;

struct PinnedFixtureDriver {
    open: OpenTransportOverride,
    proof: LegacyProofOverride,
    validate: Validate,
    sink: Arc<RecordingSink>,
}

#[async_trait::async_trait]
impl RemoteAttemptDriver for PinnedFixtureDriver {
    async fn legacy_proof(
        &self,
        _: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<Option<machines::LegacyHostProof>, String> {
        (self.proof)(id)
    }
    async fn open(
        &self,
        _: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<RemoteTransportOwner, machines::PinnedSshOpenError> {
        (self.open)(id).map(RemoteTransportOwner::from_pinned)
    }
    async fn revalidate_before_handshake(
        &self,
        pending: &PendingRemoteTransport,
        _: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        pending
            .validate_generation_for_test(|generation| {
                std::future::ready((self.validate)(&id, &generation))
            })
            .await
    }
    async fn revalidate_before_publication(
        &self,
        admitted: &AdmittedRemoteTransport,
        _: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        (self.validate)(&id, &admitted.publication_generation())
    }
    async fn revalidate_after_construction(
        &self,
        generation: String,
        _: Option<tauri::AppHandle>,
        id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        (self.validate)(&id, &generation)
    }
    async fn construct_client(
        &self,
        admitted: AdmittedRemoteTransport,
        _: Option<tauri::AppHandle>,
        ledger: RemoteSessionLedger,
    ) -> Result<ManagedRemoteClient, String> {
        SessionServerState::default()
            .construct_remote_client(admitted, self.sink.clone(), ledger)
            .await
            .map_err(|error| error.to_string())
    }
}

pub(crate) async fn adopt(
    record: MachineManagerRecord,
    client: ManagedRemoteClient,
    open: OpenTransportOverride,
    proof: LegacyProofOverride,
    validate: Validate,
) -> Arc<HostConnectionManager> {
    let sink = Arc::new(RecordingSink::default());
    let manager = Arc::new(HostConnectionManager::with_attempt_driver(Arc::new(
        PinnedFixtureDriver {
            open,
            proof,
            validate,
            sink: sink.clone(),
        },
    )));
    *lock(&manager.retirement_for_test) = Some(Arc::new(TestRetirementPresentation {
        streams: Arc::default(),
        sink,
        before_permit: None,
    }));
    let id = record.machine_id.clone();
    let observation = manager.reserve_observation();
    manager
        .observe_mutation(observation, id.clone(), Some(record))
        .await;
    let (incarnation, generation, _, _) = manager
        .begin_attempt(&id, ConnectOrigin::ExplicitForeground)
        .unwrap();
    // Mirror the production dispatcher: synchronous capture precedes queued
    // recovery. The old fixture's missing loss hook hid expected retirement EOF.
    let weak = Arc::downgrade(&manager);
    let recovery: LossRecovery = Arc::new(move |id, incarnation, generation, expected| {
        if let Some(manager) = weak.upgrade() {
            manager.capture_exact_loss(&id, incarnation, generation, &expected);
            tauri::async_runtime::spawn(async move {
                manager
                    .recover_exact_lost_client(None, id, incarnation, generation, expected)
                    .await;
            });
        }
    });
    *lock(&manager.loss_recovery_for_test) = Some(recovery.clone());
    assert!(!install_remote_loss_dispatcher(
        &client,
        remote_loss_dispatcher(
            id.clone(),
            incarnation,
            generation,
            &client.domain_client(),
            recovery,
        )
    ));
    let ledger = client.domain_client().remote_ledger_for_test();
    assert!(manager
        .adopt_current(
            &id,
            AdoptionFreshness {
                incarnation,
                generation,
                validated_observation: observation.0,
                retirement_committed: false
            },
            client,
            ledger
        )
        .is_ok());
    manager
}

pub(crate) async fn prepare(manager: &HostConnectionManager, id: &str) -> ReplacementPrompt {
    manager.prepare_replacement(None, id).await.unwrap()
}

pub(crate) async fn restart(
    manager: &HostConnectionManager,
    id: &str,
    token: String,
    after_publication: impl FnOnce(&Arc<SessionServerClient>),
    dispatch: impl FnOnce(
        String,
        machines::LegacyHostProof,
        String,
        String,
    ) -> Result<(), machines::ReplacementDispatchFailure>,
) -> ReplacementResult {
    let admission_token = token.clone();
    let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let recovery = lock(&manager.loss_recovery_for_test).clone().unwrap();
        let core = lock(&manager.core);
        let gate = core.machines.get(id).unwrap();
        let client = gate.connected.as_ref().unwrap();
        let callback = remote_loss_dispatcher(
            id.into(),
            gate.incarnation,
            gate.connected_generation.unwrap(),
            &client.domain_client(),
            recovery,
        );
        let observed = captures.clone();
        assert!(!install_remote_loss_dispatcher(
            client,
            Arc::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                callback();
            })
        ));
    }
    let result = restart_machine_terminal_server_core(
        manager,
        id.into(),
        token,
        |id, proof, token, epoch| std::future::ready(dispatch(id, proof, token, epoch)),
        |id, origin| manager.connect_remote_inner(None, id, origin),
        |id| {
            if let Ok(backend) = manager.connected_backend(&id) {
                after_publication(&backend.client());
            }
            std::future::ready(manager.replacement_admission(&id, &admission_token))
        },
        |id| async move { manager.disconnect(&id).await },
    )
    .await
    .unwrap();
    assert!(
        captures.load(Ordering::SeqCst) > 0,
        "cooperative retirement must exercise the installed synchronous loss callback"
    );
    result
}
