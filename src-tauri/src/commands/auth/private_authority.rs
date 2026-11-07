// SPDX-License-Identifier: MIT
//! Private access leases share one short synchronous publication boundary.

use crate::domain::Principal;
use chrono::{DateTime, Utc};
use codesense_session_server::machine_owner::{BootstrapOwner, MachineOwner};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct State {
    owner: Option<u64>,
    local_identity: Option<(Principal, MachineOwner)>,
    expires: Option<DateTime<Utc>>,
    epoch: u64,
    exhausted: bool,
    changes: tokio::sync::watch::Sender<PrivateAuthorityStatus>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            owner: None,
            local_identity: None,
            expires: None,
            epoch: 0,
            exhausted: false,
            changes: tokio::sync::watch::channel(PrivateAuthorityStatus {
                epoch: "0".into(),
                available: false,
            })
            .0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PrivateAuthority(Arc<Mutex<State>>);

/// Presentation fence only; this is never accepted as backend authority.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateAuthorityStatus {
    pub epoch: String,
    pub available: bool,
}

#[derive(Clone, Debug)]
pub struct PrivateAuthoritySnapshot {
    lease: PrivateAuthorityLease,
}

#[derive(Clone, Debug)]
pub struct PrivateAuthorityLease {
    authority: Arc<Mutex<State>>,
    owner: u64,
    local_identity: Option<(Principal, MachineOwner)>,
    epoch: u64,
    changes: tokio::sync::watch::Receiver<PrivateAuthorityStatus>,
}

impl State {
    fn presentation(&self) -> PrivateAuthorityStatus {
        PrivateAuthorityStatus {
            epoch: self.epoch.to_string(),
            available: self.owner.is_some() && !self.exhausted,
        }
    }

    fn notify(&self) {
        self.changes.send_replace(self.presentation());
    }

    fn expire_at(&mut self, now: DateTime<Utc>) {
        if self.expires.is_some_and(|expires| expires <= now) && self.owner.is_some() {
            self.owner = None;
            match self.epoch.checked_add(1) {
                Some(epoch) => self.epoch = epoch,
                None => self.exhausted = true,
            }
            self.notify();
        }
    }
}

impl PrivateAuthority {
    pub(crate) fn publish_local(&self, identity: Option<(Principal, MachineOwner)>) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        if state.exhausted || state.local_identity == identity {
            return;
        }
        match state.epoch.checked_add(1) {
            Some(epoch) => state.epoch = epoch,
            None => {
                state.exhausted = true;
                state.owner = None;
                state.notify();
                return;
            }
        }
        state.owner = identity
            .as_ref()
            .and_then(|(principal, _)| principal.numeric_id().ok());
        state.local_identity = identity;
        state.expires = None;
        state.notify();
    }

    pub(crate) fn presentation(&self) -> Option<PrivateAuthorityStatus> {
        let mut state = self.0.lock().ok()?;
        state.expire_at(Utc::now());
        Some(state.presentation())
    }

    /// The channel is updated synchronously before auth publication. The
    /// asynchronous bridge carries only a retirement floor, never a grant.
    pub(crate) fn bridge_retirement(&self, app: tauri::AppHandle) {
        use tauri::Emitter;
        let Ok(state) = self.0.lock() else {
            return;
        };
        let mut changes = state.changes.subscribe();
        drop(state);
        tauri::async_runtime::spawn(async move {
            while changes.changed().await.is_ok() {
                let retirement = changes.borrow_and_update().clone();
                let _ = app.emit("private-authority-retired", retirement);
            }
        });
    }

    /// Called before publishing changed auth, while its write guard is held.
    #[cfg(test)]
    pub(super) fn publish(&self, identity: Option<(u64, Option<DateTime<Utc>>)>) {
        let (owner, expires) =
            identity.map_or((None, None), |(owner, expires)| (Some(owner), expires));
        let Ok(mut state) = self.0.lock() else {
            return; // Poisoned authority is permanently unavailable to readers.
        };
        state.expire_at(Utc::now());
        if state.exhausted {
            return;
        }
        let expiry_changed = state.expires != expires;
        state.expires = expires;
        if state.owner == owner {
            if expiry_changed {
                state.notify();
            }
            return;
        }
        match state.epoch.checked_add(1) {
            Some(epoch) => {
                state.epoch = epoch;
                state.owner = owner;
            }
            None => {
                state.exhausted = true;
                state.owner = None;
            }
        }
        state.notify();
    }

    pub(crate) fn snapshot(&self) -> Result<PrivateAuthoritySnapshot, String> {
        let mut state = self.0.lock().map_err(|_| "private authority unavailable")?;
        state.expire_at(Utc::now());
        let owner = state.owner.ok_or("private authority unavailable")?;
        Ok(PrivateAuthoritySnapshot {
            lease: PrivateAuthorityLease {
                authority: Arc::clone(&self.0),
                owner,
                local_identity: state.local_identity.clone(),
                epoch: state.epoch,
                changes: state.changes.subscribe(),
            },
        })
    }
}

impl PrivateAuthoritySnapshot {
    pub(crate) fn principal(&self) -> Principal {
        self.lease.principal()
    }
    pub(crate) fn machine_owner(&self) -> MachineOwner {
        self.lease.machine_owner()
    }
    pub(crate) fn bootstrap_owner(&self) -> BootstrapOwner {
        BootstrapOwner {
            machine_owner: self.machine_owner(),
            principal: self.owner_id(),
        }
    }

    pub fn owner_id(&self) -> u64 {
        self.lease.owner
    }

    pub fn lease(&self) -> PrivateAuthorityLease {
        self.lease.clone()
    }
}

impl PrivateAuthorityLease {
    pub(crate) fn principal(&self) -> Principal {
        self.local_identity
            .as_ref()
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| Principal::from(self.owner))
    }
    pub(crate) fn machine_owner(&self) -> MachineOwner {
        self.local_identity
            .as_ref()
            .map(|(_, o)| o.clone())
            .unwrap_or(MachineOwner::LegacyGithub { id: self.owner })
    }

    pub(crate) fn owner_id(&self) -> u64 {
        self.owner
    }
    pub(crate) fn epoch(&self) -> String {
        self.epoch.to_string()
    }
    /// Observe retirement of this exact lease without holding the publication
    /// gate while waiting or detaching its transport. Expiry is observed even
    /// when no later auth mutation occurs.
    pub async fn retired(&self) {
        let mut changes = self.changes.clone();
        loop {
            changes.borrow_and_update();
            let expires = {
                let Ok(mut state) = self.authority.lock() else {
                    return;
                };
                state.expire_at(Utc::now());
                if state.exhausted || state.owner != Some(self.owner) || state.epoch != self.epoch {
                    return;
                }
                state.expires
            };
            if let Some(expires) = expires {
                let duration = (expires - Utc::now()).to_std().unwrap_or_default();
                tokio::select! {
                    _ = tokio::time::sleep(duration) => {},
                    changed = changes.changed() => if changed.is_err() { return; },
                }
            } else if changes.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.commit(|| ())
    }

    /// Commit a short local effect atomically with lease validation. The closure
    /// must not perform I/O, await, or acquire authentication locks.
    pub fn commit<T>(&self, effect: impl FnOnce() -> T) -> Result<T, String> {
        let mut state = self
            .authority
            .lock()
            .map_err(|_| "private authority unavailable")?;
        state.expire_at(Utc::now());
        if state.exhausted || state.owner != Some(self.owner) || state.epoch != self.epoch {
            return Err("private authority expired".into());
        }
        Ok(effect())
    }
}

#[cfg(test)]
pub(crate) fn test_snapshot(owner: u64) -> (PrivateAuthoritySnapshot, impl Fn()) {
    let authority = PrivateAuthority::default();
    authority.publish(Some((owner, None)));
    (authority.snapshot().unwrap(), move || {
        authority.publish(None)
    })
}

#[cfg(test)]
pub(crate) fn test_lease(owner: u64) -> (PrivateAuthorityLease, impl Fn()) {
    let authority = PrivateAuthority::default();
    authority.publish(Some((owner, None)));
    let lease = authority.snapshot().unwrap().lease();
    (lease, move || authority.publish(None))
}

#[cfg(test)]
pub(crate) fn test_switching_lease(
    owner: u64,
) -> (PrivateAuthorityLease, impl Fn(u64) -> PrivateAuthorityLease) {
    let authority = PrivateAuthority::default();
    authority.publish(Some((owner, None)));
    let lease = authority.snapshot().unwrap().lease();
    (lease, move |owner| {
        authority.publish(Some((owner, None)));
        authority.snapshot().unwrap().lease()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_retirement_payload_matches_consumer_contract() {
        let authority = PrivateAuthority::default();
        let mut changes = authority.0.lock().unwrap().changes.subscribe();
        let mut emitted = Vec::new();
        let mut capture = || {
            assert!(changes.has_changed().unwrap());
            emitted.push(serde_json::to_value(changes.borrow_and_update().clone()).unwrap());
        };
        authority.publish(Some((1, None)));
        capture();
        authority.publish(Some((1, Some(Utc::now() + chrono::Duration::hours(1)))));
        capture(); // same owner refresh wakes the timer but does not retire
        authority.publish(Some((2, None)));
        capture();
        authority.publish(Some((1, None)));
        capture();
        authority.publish(None);
        capture();
        authority.publish(Some((1, Some(Utc::now() + chrono::Duration::seconds(1)))));
        capture();
        authority
            .0
            .lock()
            .unwrap()
            .expire_at(Utc::now() + chrono::Duration::seconds(2));
        capture();
        {
            let mut state = authority.0.lock().unwrap();
            state.epoch = u64::MAX;
            state.owner = Some(1);
            state.expires = None;
        }
        authority.publish(Some((2, None)));
        capture(); // exhaustion retires MAX itself, despite no larger floor
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/private-authority-retirement.json"
        ))
        .unwrap();
        assert_eq!(serde_json::Value::Array(emitted), contract);
    }

    #[tokio::test]
    async fn retirement_observer_follows_exact_epoch_and_does_not_revive() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, None)));
        let old = authority.snapshot().unwrap().lease();
        authority.publish(Some((2, None)));
        authority.publish(Some((1, None)));
        tokio::time::timeout(std::time::Duration::from_secs(1), old.retired())
            .await
            .unwrap();
        let fresh = authority.snapshot().unwrap().lease();
        assert!(fresh.validate().is_ok());
    }

    #[tokio::test]
    async fn retirement_observer_wakes_for_publication_and_credential_expiry() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, None)));
        let lease = authority.snapshot().unwrap().lease();
        let retirement = lease.retired();
        tokio::pin!(retirement);
        assert!(futures_util::poll!(&mut retirement).is_pending());
        authority.publish(Some((1, Some(Utc::now() - chrono::Duration::seconds(1)))));
        tokio::time::timeout(std::time::Duration::from_secs(1), retirement)
            .await
            .unwrap();
        assert!(lease.validate().is_err());
    }

    #[tokio::test]
    async fn retirement_observer_expires_without_another_auth_publication() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((
            1,
            Some(Utc::now() + chrono::Duration::milliseconds(30)),
        )));
        let lease = authority.snapshot().unwrap().lease();
        tokio::time::timeout(std::time::Duration::from_secs(1), lease.retired())
            .await
            .unwrap();
        assert!(lease.validate().is_err());
    }

    #[tokio::test]
    async fn same_owner_expiry_extension_preserves_lease_and_reschedules_observer() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, Some(Utc::now() + chrono::Duration::hours(1)))));
        let lease = authority.snapshot().unwrap().lease();
        let retirement = lease.retired();
        tokio::pin!(retirement);
        assert!(futures_util::poll!(&mut retirement).is_pending());
        authority.publish(Some((1, Some(Utc::now() + chrono::Duration::hours(2)))));
        assert!(futures_util::poll!(&mut retirement).is_pending());
        assert!(lease.validate().is_ok());
        authority.publish(None);
        retirement.await;
    }

    #[test]
    fn expired_credentials_retire_existing_lease_permanently() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, Some(Utc::now() + chrono::Duration::hours(1)))));
        let lease = authority.snapshot().unwrap().lease();
        authority.0.lock().unwrap().expires = Some(Utc::now() - chrono::Duration::seconds(1));
        assert!(lease.validate().is_err());
        authority.publish(Some((1, None)));
        assert!(lease.validate().is_err());
        assert!(authority.snapshot().unwrap().lease().validate().is_ok());
    }

    #[test]
    fn exhaustion_never_reuses_an_epoch() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, None)));
        let old = authority.snapshot().unwrap().lease();
        authority.0.lock().unwrap().epoch = u64::MAX;
        authority.publish(Some((2, None)));
        authority.publish(Some((1, None)));
        assert!(old.validate().is_err());
        assert!(authority.snapshot().is_err());
    }

    #[test]
    fn retired_lease_cannot_commit_effect() {
        let authority = PrivateAuthority::default();
        authority.publish(Some((1, None)));
        let lease = authority.snapshot().unwrap().lease();
        authority.publish(None);
        let mut committed = false;
        assert!(lease.commit(|| committed = true).is_err());
        assert!(!committed);
    }
}
