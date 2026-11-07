// SPDX-License-Identifier: MIT
//! Owned application state over C2-supplied endpoints; never owns fixture resources.
use super::*;
use crate::commands::credential_store::FakeStore;
use crate::commands::host_connection_manager::MachineManagerRecord;
use std::sync::{Condvar, Mutex};

#[derive(Debug, Clone, Copy)]
pub(crate) enum SavedDrift {
    Binding,
    Pin,
}

/// Restores only temporary application state, never the C2 host slot.
pub(crate) struct SavedRestore {
    machines: Arc<OwnedMachines>,
    id: String,
    binding: Option<Binding>,
    pin: Vec<u8>,
}

impl SavedRestore {
    pub(crate) fn restore(mut self) -> Result<(), String> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> Result<(), String> {
        if let Some(binding) = self.binding.as_ref() {
            self.machines
                .env
                .registry
                .update_bindings(|all| all.insert(self.id.clone(), binding.clone()))?;
            std::fs::write(self.machines.env.paths().pin_file(&self.id), &self.pin)
                .map_err(|e| e.to_string())?;
            self.binding = None;
        }
        Ok(())
    }
}

impl Drop for SavedRestore {
    fn drop(&mut self) {
        if let Err(error) = self.restore_inner() {
            self.machines.cleanup_errors.lock().unwrap().push(error);
        }
    }
}

#[derive(Default)]
pub(crate) struct OpeningGate {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    changed: Condvar,
}

impl OpeningGate {
    fn hold(&self) {
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
    pub(crate) async fn entered(&self) {
        self.entered.notified().await;
    }
    pub(crate) fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

pub(crate) struct OpeningHold(pub Arc<OpeningGate>);
impl Drop for OpeningHold {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub key: String,
    pub identity: String,
}

impl Endpoint {
    pub(crate) fn supplied_pair() -> [Self; 2] {
        fn required(name: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| panic!("missing C2 input {name}"))
        }
        let token = required("RM4F_OWNER_TOKEN");
        assert!(token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()));
        [("A", ""), ("B", "_2")].map(|(side, suffix)| Self {
            host: required(&format!("CODESENSE_REMOTE_HOST_HOST{suffix}")),
            port: required(&format!("CODESENSE_REMOTE_HOST_PORT{suffix}"))
                .parse()
                .unwrap(),
            key: required(&format!("CODESENSE_REMOTE_HOST_KEY{suffix}")),
            identity: required(&format!("RM4F_IDENTITY_{side}")),
        })
    }
}

pub(crate) struct OwnedMachines {
    env: Env,
    store: FakeStore,
    pub ids: [String; 2],
    pub server_ids: [String; 2],
    owners: Mutex<Vec<SshOwnerObservation>>,
    next_open: Mutex<Option<Arc<OpeningGate>>>,
    cleanup_errors: Mutex<Vec<String>>,
    _root: tempfile::TempDir,
}

#[derive(Debug)]
struct SshOwnerObservation {
    pid: libc::pid_t,
    retired: bool,
}

impl OwnedMachines {
    pub(crate) fn enroll(endpoints: [Endpoint; 2]) -> Result<Arc<Self>, String> {
        if endpoints[0].host == endpoints[1].host && endpoints[0].port == endpoints[1].port {
            return Err("A/B endpoints must be independent".into());
        }
        if std::fs::read(&endpoints[0].key).map_err(|e| e.to_string())?
            == std::fs::read(&endpoints[1].key).map_err(|e| e.to_string())?
            || HostKey::parse(&endpoints[0].identity)? == HostKey::parse(&endpoints[1].identity)?
        {
            return Err("A/B keys and identities must be independent".into());
        }
        let root = tempfile::tempdir_in("/tmp").map_err(|e| e.to_string())?;
        let env = super::tests::container::env(root.path());
        let store = FakeStore::default();
        let mut ids = Vec::new();
        let mut server_ids = Vec::new();
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let key = HostKey::parse(&endpoint.identity)?;
            let mut draft = MachineDraftInput {
                mode: Some(MachineMode::Legacy),
                label: format!("rm4f-owned-{index}"),
                host: endpoint.host,
                port: endpoint.port,
                username: Some("dev".into()),
                jump_host: None,
                sign_in: MachineSignIn::KeyFile {
                    key_path: endpoint.key,
                },
                trusted_host_key_fp: None,
            };
            let report = test_connection(&env, &store, None, &draft)?;
            let observed = report
                .steps
                .iter()
                .find_map(|step| step.fingerprint.as_ref())
                .ok_or("real SSH checklist supplied no fingerprint")?;
            if observed != &key.fingerprint() {
                return Err("supplied host identity differs from SSH observation".into());
            }
            draft.trusted_host_key_fp = Some(observed.clone());
            match install_server(&env, &store, None, &draft)? {
                InstallTerminalServerResult::Installed { .. } => {}
                InstallTerminalServerResult::Failed { message, .. } => return Err(message),
            }
            let saved = save(&env, &store, None, &draft, &Scope::Personal, true)?;
            let (_, declaration) =
                existing(&env, Some(&saved.machine_id))?.ok_or("missing saved declaration")?;
            if declaration.pinned_host_key.as_deref() != Some(key.to_line().as_str()) {
                return Err("saved host pin differs from supplied identity".into());
            }
            server_ids.push(
                declaration
                    .pinned_server_id
                    .ok_or("missing saved ServerId pin")?,
            );
            ids.push(saved.machine_id);
        }
        if server_ids[0] == server_ids[1] {
            return Err("A/B ServerIds must differ".into());
        }
        Ok(Arc::new(Self {
            env,
            store,
            ids: ids.try_into().unwrap(),
            server_ids: server_ids.try_into().unwrap(),
            owners: Mutex::new(Vec::new()),
            next_open: Mutex::new(None),
            cleanup_errors: Mutex::new(Vec::new()),
            _root: root,
        }))
    }

    pub(crate) fn record(&self, id: &str) -> Result<MachineManagerRecord, String> {
        manager_record(&self.env, id)?.ok_or_else(|| "missing saved manager record".into())
    }

    pub(crate) fn begin_open(self: &Arc<Self>, id: String) -> PinnedSshOpenAttempt {
        let owned = self.clone();
        let gate = self.next_open.lock().unwrap().take();
        PinnedSshOpenAttempt::spawn(move |progress| {
            let transport = open_pinned_saved_with(&owned.env, &owned.store, &id, progress)?;
            owned.owners.lock().unwrap().push(SshOwnerObservation {
                pid: transport
                    .child_guard
                    .child
                    .as_ref()
                    .expect("open SSH child")
                    .id() as libc::pid_t,
                retired: false,
            });
            if let Some(gate) = gate {
                gate.hold();
            }
            Ok(transport)
        })
    }

    pub(crate) fn legacy_proof(&self, id: &str) -> Result<Option<LegacyHostProof>, String> {
        legacy_host_proof_saved(&self.env, &self.store, id)
    }

    pub(crate) fn validate(
        &self,
        id: &str,
        generation: &str,
    ) -> Result<(), PinnedTransportGenerationError> {
        revalidate_transport_generation(&self.env, id, generation)
    }

    pub(crate) fn assert_reaped(&self) {
        let errors = self.cleanup_errors.lock().unwrap();
        assert!(
            errors.is_empty(),
            "temporary saved state restoration failed: {errors:?}"
        );
        drop(errors);
        // Call only after the actual opening/manager owner has completed its
        // awaited close. Observe each retired child once; numeric PIDs can be
        // reused after reap. Never waitpid (which could reap a reused child),
        // and never probe a historical PID again after its absence is verified.
        let mut owners = self.owners.lock().unwrap();
        for owner in owners.iter_mut().filter(|owner| !owner.retired) {
            let pid = owner.pid;
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "SSH owner {pid} still live"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            owner.retired = true;
        }
        eprintln!(
            "RM4F C2a: {} exact SSH owners reaped",
            owners.iter().filter(|owner| owner.retired).count()
        );
    }

    pub(crate) fn owner_diagnostics(&self) -> String {
        format!("SSH owner observations: {:?}", self.owners.lock().unwrap())
    }

    pub(crate) fn hold_next_open(&self) -> OpeningHold {
        let gate = Arc::new(OpeningGate::default());
        assert!(self
            .next_open
            .lock()
            .unwrap()
            .replace(gate.clone())
            .is_none());
        OpeningHold(gate)
    }

    pub(crate) fn drift(
        self: &Arc<Self>,
        id: &str,
        kind: SavedDrift,
    ) -> Result<SavedRestore, String> {
        let binding = self
            .env
            .registry
            .bindings()?
            .remove(id)
            .ok_or("missing binding")?;
        let pin = std::fs::read(self.env.paths().pin_file(id)).map_err(|e| e.to_string())?;
        let restore = SavedRestore {
            machines: self.clone(),
            id: id.into(),
            binding: Some(binding),
            pin,
        };
        match kind {
            SavedDrift::Binding => {
                self.env.registry.update_bindings(|all| {
                    let saved = all.get_mut(id).expect("saved binding");
                    saved.auto_connect = !saved.auto_connect;
                })?;
            }
            SavedDrift::Pin => {
                let other = self
                    .ids
                    .iter()
                    .find(|other| other.as_str() != id)
                    .ok_or("missing other host")?;
                let other_pin =
                    std::fs::read(self.env.paths().pin_file(other)).map_err(|e| e.to_string())?;
                std::fs::write(self.env.paths().pin_file(id), other_pin)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(restore)
    }
}
