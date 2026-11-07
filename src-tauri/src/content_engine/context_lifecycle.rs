// SPDX-License-Identifier: MIT
//! Workspace-wide admission leases: one context operation (a launch's Step 2,
//! an explicit hydrate or publish) per destination at a time. SW-XS-8: the
//! legacy projection engine's lease (`legacy_local`) is gone with that engine.
use super::destination::DestinationIdentity;
use super::sync_state_store::{default_store_path, SyncStateStore};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

#[derive(Default)]
struct State {
    selected: bool,
}
#[derive(Default)]
struct Gate {
    state: Mutex<State>,
    drained: Condvar,
}
type Gates = Mutex<HashMap<DestinationIdentity, Weak<Gate>>>;
fn gate(identity: &DestinationIdentity) -> Arc<Gate> {
    static GATES: OnceLock<Gates> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    gates.retain(|_, value| value.strong_count() > 0);
    gates
        .entry(identity.clone())
        .or_default()
        .upgrade()
        .unwrap_or_else(|| {
            let gate = Arc::new(Gate::default());
            gates.insert(identity.clone(), Arc::downgrade(&gate));
            gate
        })
}
pub(crate) fn local_identity(root: &Path) -> DestinationIdentity {
    DestinationIdentity::Local {
        root: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
    }
}

pub(crate) struct SelectedLease {
    gate: Arc<Gate>,
    pub generation: u64,
}
impl Drop for SelectedLease {
    fn drop(&mut self) {
        self.gate
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selected = false;
        self.gate.drained.notify_all();
    }
}
/// Runs on the blocking executor after the destination lock. Close admission first,
/// drain every admitted direct writer. Durable cutover belongs to the engine
/// transaction that accepts a completely validated bounded request snapshot.
pub(crate) fn reserve_selected(
    home: &Path,
    identity: &DestinationIdentity,
) -> Result<SelectedLease, String> {
    let gate = gate(identity);
    let mut state = gate.state.lock().unwrap_or_else(|e| e.into_inner());
    while state.selected {
        state = gate.drained.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    state.selected = true;
    drop(state);
    let mut lease = SelectedLease {
        gate,
        generation: 0,
    };
    lease.generation = SyncStateStore::open(default_store_path(home))
        .map_err(|e| e.to_string())?
        .context_generation(identity)
        .map_err(|e| e.to_string())?
        .unwrap_or(1);
    Ok(lease)
}

/// [`reserve_selected`] that gives up after `wait` (SW-XS-7): a launch's
/// Step 2 never waits without bound on another context operation on the same
/// destination. The lease carries no generation.
pub(crate) fn reserve_selected_within(
    home: &Path,
    identity: &DestinationIdentity,
    wait: std::time::Duration,
) -> Result<SelectedLease, String> {
    let busy = || {
        "another context operation on this workspace is still running; launch again in a moment"
            .to_string()
    };
    let deadline = std::time::Instant::now() + wait;
    let gate = gate(identity);
    let mut state = gate.state.lock().unwrap_or_else(|e| e.into_inner());
    while state.selected {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(busy());
        }
        state = gate
            .drained
            .wait_timeout(state, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    state.selected = true;
    drop(state);
    // Admission only: Step 2 keeps its own managed record and never reads the
    // selected engine's generation, so the store is not touched here (two
    // launches opening it at once would contend for nothing).
    let _ = home;
    Ok(SelectedLease {
        gate,
        generation: 0,
    })
}

/// Explicit test admission for fixtures that exercise receivers without an engine request.
#[cfg(test)]
pub(crate) fn selected(
    home: &Path,
    identity: &DestinationIdentity,
) -> Result<SelectedLease, String> {
    let mut lease = reserve_selected(home, identity)?;
    lease.generation = SyncStateStore::open(default_store_path(home))
        .map_err(|e| e.to_string())?
        .admit_explicit_context(identity)
        .map_err(|e| e.to_string())?;
    Ok(lease)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SW-XS-7: a held selected lease makes a bounded reservation give up
    /// after its wait instead of blocking forever, and frees the gate after.
    #[test]
    fn bounded_reservation_gives_up_while_another_holds_the_lease() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let identity = local_identity(root.path());
        let held = reserve_selected(home.path(), &identity).unwrap();
        let started = std::time::Instant::now();
        let error = reserve_selected_within(
            home.path(),
            &identity,
            std::time::Duration::from_millis(150),
        )
        .err()
        .unwrap();
        assert!(error.contains("still running"));
        assert!(started.elapsed() >= std::time::Duration::from_millis(150));
        drop(held);
        drop(
            reserve_selected_within(
                home.path(),
                &identity,
                std::time::Duration::from_millis(150),
            )
            .unwrap(),
        );
    }

    #[test]
    fn explicit_status_read_does_not_wait_for_transfer_lease() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let identity = local_identity(root.path());
        let _transfer = selected(home.path(), &identity).unwrap();
        let h = home.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(
                SyncStateStore::open(default_store_path(&h))
                    .unwrap()
                    .context_generation(&identity)
                    .unwrap(),
            )
            .unwrap();
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            Some(1)
        );
        worker.join().unwrap();
    }
}
