// SPDX-License-Identifier: MIT
//! The app's host adapters for the `codesense-pty-core` session registry: the
//! ONE [`TerminalEventSink`] every runtime side effect flows through (Tauri
//! events, OSC 133 dispatch, ask-user cancellation, warm-pool self-heal,
//! reverse-sync reconcile, and teardown bookkeeping), the client-side
//! [`StreamIds`] it re-emits viewport streams under, and the warm pool's
//! spawn/kill host.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, SubscribeResult, TeardownReason, TerminalDomainEvent,
    TerminalEventSink, ViewportId,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::runner::ExitInfo;
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::SessionMeta;
use log::trace;
use tauri::{Emitter, Manager};

use crate::commands::agent_audit;
use crate::commands::dashboard_launch::{DashboardLaunchState, ReleasedSession};
use crate::commands::pty::{kill_session, spawn_pty_inner, SpawnPtySize};
use crate::mcp::state::AskUserRegistry;
use crate::terminal::session_server::{
    EventNamespace, NamespacedTerminalEvent, NamespacedTerminalEventSink,
};
use crate::terminal::warm_pool::{MemberSpawnParams, WarmPoolHost, WarmPoolState};
use crate::terminal::{Osc133Bus, Osc133Event};

/// The frontend event a terminal-layout change is re-emitted on.
pub const TERMINAL_LAYOUT_CHANGED_EVENT: &str = "terminal-layout-changed";

/// Quarantined remote projection ingress. RM-5a may consume this sealed
/// namespace-bearing event; legacy local frontend schemas remain unchanged.
pub const REMOTE_TERMINAL_DOMAIN_EVENT: &str = "remote-terminal-domain-event";

/// Runtime-only ownership of legacy records; never accepted from IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalEffectClaim {
    pub target: super::machines::RuntimeMemberTarget,
    pub session: String,
    stamp: u64,
    pub(crate) pool_member: bool,
    authorizing: bool,
}

#[derive(Default)]
struct EffectClaims {
    next: u64,
    claims: HashMap<(super::machines::RuntimeMemberSlot, String), EffectClaimState>,
    // A retained predecessor must not hide terminal facts from its replacement.
    // These records never authorize registration and survive until explicit reuse.
    terminal: Vec<EffectClaimState>,
    // None is sticky ambiguity, retained even after both sessions disappear.
    history: HashMap<String, Option<super::machines::RuntimeMemberTarget>>,
}

struct EffectClaimState {
    claim: LocalEffectClaim,
    retired: bool,
    exit_complete: bool,
    removed_complete: bool,
    active_legacy: usize,
    pending_settlements: usize,
    terminal_outcomes: [Option<TerminalDomainEvent>; 2],
    cleanup_owner: Option<LocalEffectClaim>,
    cleanup_failed: bool,
}

#[derive(Clone)]
pub(crate) struct EffectPredecessor {
    pub owner: LocalEffectClaim,
    target: super::machines::RuntimeMemberTarget,
    evidence: Vec<LocalEffectClaim>,
}

#[derive(Default)]
pub(crate) struct LocalEffectOwners(
    Mutex<EffectClaims>,
    Condvar,
    #[cfg(test)] std::sync::atomic::AtomicUsize,
);

impl EffectClaims {
    fn record(&self, claim: &LocalEffectClaim) -> Option<&EffectClaimState> {
        self.claims
            .values()
            .chain(self.terminal.iter())
            .find(|record| &record.claim == claim)
    }

    fn predecessor(
        &self,
        target: &super::machines::RuntimeMemberTarget,
        session: &str,
    ) -> Result<EffectPredecessor, String> {
        let current = self
            .claims
            .get(&(target.member.slot, session.into()))
            .ok_or("predecessor unavailable")?;
        let evidence: Vec<_> = self
            .claims
            .values()
            .chain(self.terminal.iter())
            .filter(|record| same_effect_lifetime(&record.claim, target, session))
            .collect();
        if !evidence.iter().any(|record| record.exit_complete)
            || !evidence.iter().any(|record| record.removed_complete)
            || evidence.iter().any(|record| {
                record.pending_settlements != 0
                    || record.active_legacy != 0
                    || record.cleanup_failed
            })
            || current.active_legacy != 0
            || current.pending_settlements != 0
            || current.cleanup_failed
        {
            return Err("predecessor terminal cleanup is not drained".into());
        }
        let associated = evidence
            .iter()
            .find_map(|record| record.cleanup_owner.as_ref());
        let owner = if let Some(owner) = associated {
            if owner != &current.claim
                || evidence.iter().any(|record| {
                    record
                        .cleanup_owner
                        .as_ref()
                        .is_some_and(|other| other != owner)
                })
            {
                return Err("terminal predecessor association changed".into());
            }
            owner.clone()
        } else if current.claim.target == *target
            || (!current.claim.authorizing && same_effect_lifetime(&current.claim, target, session))
        {
            current.claim.clone()
        } else if same_effect_lifetime(&current.claim, target, session) {
            return Err("terminal predecessor has not established cleanup continuity".into());
        } else {
            evidence
                .first()
                .ok_or("terminal predecessor unavailable")?
                .claim
                .clone()
        };
        Ok(EffectPredecessor {
            owner,
            target: target.clone(),
            evidence: evidence
                .into_iter()
                .map(|record| record.claim.clone())
                .collect(),
        })
    }

    fn validate_predecessor(&self, proof: &EffectPredecessor) -> Result<(), String> {
        let current = self.predecessor(&proof.target, &proof.owner.session)?;
        if current.owner != proof.owner || current.evidence != proof.evidence {
            return Err("terminal predecessor proof changed".into());
        }
        Ok(())
    }

    fn record_mut(&mut self, claim: &LocalEffectClaim) -> Option<&mut EffectClaimState> {
        self.claims
            .values_mut()
            .chain(self.terminal.iter_mut())
            .find(|record| &record.claim == claim)
    }

    fn observe(&mut self, target: &super::machines::RuntimeMemberTarget, session: &str) {
        self.history
            .entry(session.into())
            .and_modify(|origin| {
                if origin.as_ref().is_some_and(|old| {
                    old.member != target.member
                        || old.namespace.server_id != target.namespace.server_id
                        || old.namespace.epoch != target.namespace.epoch
                }) {
                    *origin = None;
                }
            })
            .or_insert_with(|| Some(target.clone()));
    }
}

fn same_effect_lifetime(
    claim: &LocalEffectClaim,
    target: &super::machines::RuntimeMemberTarget,
    session: &str,
) -> bool {
    claim.session == session
        && claim.target.member == target.member
        && claim.target.namespace.server_id == target.namespace.server_id
        && claim.target.namespace.epoch == target.namespace.epoch
}

impl LocalEffectOwners {
    fn capture_event(
        &self,
        target: &super::machines::RuntimeMemberTarget,
        event: &TerminalDomainEvent,
    ) -> Option<LocalEffectClaim> {
        self.ingress(
            target,
            local_event_session(event)?,
            matches!(
                event,
                TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
            ),
        )
    }
    pub(crate) fn reserve(
        &self,
        target: super::machines::RuntimeMemberTarget,
        session: &str,
    ) -> Result<LocalEffectClaim, String> {
        self.reserve_inner(target, session, None, None, None)
            .map(|(claim, _)| claim)
    }

    #[cfg(test)]
    pub(crate) fn reserve_spawn(
        &self,
        target: super::machines::RuntimeMemberTarget,
        session: &str,
        pool: bool,
    ) -> Result<LocalEffectClaim, String> {
        self.reserve_inner(target, session, Some(pool), None, None)
            .map(|(claim, _)| claim)
    }

    /// Only spawn's blocking worker waits. Sink ingress merely updates facts
    /// and notifies; no admission or resource lock is held across this wait.
    #[cfg(test)]
    pub(crate) fn reserve_spawn_until(
        &self,
        target: super::machines::RuntimeMemberTarget,
        session: &str,
        pool: bool,
        deadline: std::time::Instant,
    ) -> Result<LocalEffectClaim, String> {
        self.reserve_inner(target, session, Some(pool), Some(deadline), None)
            .map(|(claim, _)| claim)
    }

    pub(crate) fn reserve_spawn_with_predecessor(
        &self,
        target: super::machines::RuntimeMemberTarget,
        session: &str,
        pool: bool,
        deadline: std::time::Instant,
        predecessor: Option<&EffectPredecessor>,
    ) -> Result<(LocalEffectClaim, Option<LocalEffectClaim>), String> {
        self.reserve_inner(target, session, Some(pool), Some(deadline), predecessor)
    }

    fn reserve_inner(
        &self,
        target: super::machines::RuntimeMemberTarget,
        session: &str,
        spawn: Option<bool>,
        deadline: Option<std::time::Instant>,
        predecessor: Option<&EffectPredecessor>,
    ) -> Result<(LocalEffectClaim, Option<LocalEffectClaim>), String> {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        core.observe(&target, session);
        let key = (target.member.slot, session.to_string());
        loop {
            if let Some(proof) = predecessor {
                if proof.target != target || proof.owner.session != session {
                    return Err("spawn predecessor belongs to another target".into());
                }
                if let Err(error) = core.validate_predecessor(proof) {
                    if let Some(remaining) = deadline.and_then(|deadline| {
                        deadline.checked_duration_since(std::time::Instant::now())
                    }) {
                        core = self
                            .1
                            .wait_timeout(core, remaining)
                            .unwrap_or_else(|error| error.into_inner())
                            .0;
                        continue;
                    }
                    return Err(error);
                }
            }
            let observed = core
                .terminal
                .iter()
                .any(|record| same_effect_lifetime(&record.claim, &target, session));
            if observed && (spawn.is_none() || core.predecessor(&target, session).is_err()) {
                return Err(
                    "observed terminal lifetime cannot authorize discovery or early reuse".into(),
                );
            }
            if let Some(previous) = core.claims.get(&key) {
                let claim = &previous.claim;
                if previous.cleanup_failed {
                    return Err("predecessor cleanup failed or is unknown".into());
                }
                if claim.target != target
                    && (previous.active_legacy != 0 || previous.pending_settlements != 0)
                {
                    return Err("old lifetime still has an in-flight legacy effect".into());
                }
                if claim.target == target {
                    if spawn.is_some() {
                        if !previous.exit_complete
                            || !previous.removed_complete
                            || previous.active_legacy != 0
                            || previous.pending_settlements != 0
                        {
                            if previous.retired {
                                if let Some(remaining) = deadline.and_then(|deadline| {
                                    deadline.checked_duration_since(std::time::Instant::now())
                                }) {
                                    core = self
                                        .1
                                        .wait_timeout(core, remaining)
                                        .unwrap_or_else(|e| e.into_inner())
                                        .0;
                                    continue;
                                }
                            }
                            return Err(
                                "runtime claim is draining; retry after terminal ingress completes"
                                    .into(),
                            );
                        }
                    } else {
                        return if previous.retired || !claim.authorizing {
                            Err("session effect claim already retired".into())
                        } else {
                            Ok((claim.clone(), None))
                        };
                    }
                } else if claim.target.namespace.server_id == target.namespace.server_id
                    && claim.target.namespace.epoch == target.namespace.epoch
                    && !(spawn.is_some() && previous.retired && (observed || predecessor.is_some()))
                {
                    return Err("same-lifetime claim requires exact recovery before reuse".into());
                }
            }
            break;
        }
        let prior = predecessor.map(|proof| proof.owner.clone()).or_else(|| {
            core.claims
                .get(&key)
                .filter(|record| record.claim.target == target)
                .map(|record| record.claim.clone())
        });
        core.next = core
            .next
            .checked_add(1)
            .ok_or("effect claim space exhausted")?;
        let claim = LocalEffectClaim {
            target,
            session: session.into(),
            stamp: core.next,
            pool_member: spawn.unwrap_or(false),
            authorizing: true,
        };
        core.terminal
            .retain(|record| !same_effect_lifetime(&record.claim, &claim.target, session));
        core.claims.insert(
            key,
            EffectClaimState {
                claim: claim.clone(),
                retired: false,
                exit_complete: false,
                removed_complete: false,
                active_legacy: 0,
                pending_settlements: 0,
                terminal_outcomes: [None, None],
                cleanup_owner: None,
                cleanup_failed: false,
            },
        );
        Ok((claim, prior))
    }

    pub(crate) fn retire_claim(&self, claim: &LocalEffectClaim) {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = core
            .claims
            .get_mut(&(claim.target.member.slot, claim.session.clone()))
        {
            if &current.claim == claim {
                current.retired = true;
            }
        }
    }

    pub(crate) fn finish_warm_handoff(
        &self,
        source: &LocalEffectClaim,
        target: &LocalEffectClaim,
        result: &Result<(), PortUnavailable>,
    ) {
        self.retire_claim(source);
        if result.is_err() {
            self.retire_claim(target);
        }
    }

    pub(crate) fn claims(&self, slot: super::machines::RuntimeMemberSlot) -> Vec<LocalEffectClaim> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .claims
            .values()
            .filter(|record| record.claim.target.member.slot == slot && record.claim.authorizing)
            .map(|record| record.claim.clone())
            .collect()
    }

    /// Caller proves the same live server/session off-lock. The closure only
    /// compares/moves existing metadata; no I/O or callbacks under this lock.
    pub(crate) fn transfer(
        &self,
        admission: &crate::terminal::session_server::LocalRuntimeAdmission,
        old: &LocalEffectClaim,
        target: super::machines::RuntimeMemberTarget,
        move_records: impl FnOnce(&LocalEffectClaim) -> Result<(), String>,
    ) -> Result<LocalEffectClaim, String> {
        if !old.authorizing
            || old.target.member != target.member
            || old.target.namespace.server_id != target.namespace.server_id
            || old.target.namespace.epoch != target.namespace.epoch
        {
            return Err("effect continuity requires the exact original server lifetime".into());
        }
        let deadline =
            std::time::Instant::now() + crate::terminal::session_server::EXIT_REAP_TIMEOUT;
        let key = (old.target.member.slot, old.session.clone());
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if core
                .terminal
                .iter()
                .any(|record| same_effect_lifetime(&record.claim, &target, &old.session))
            {
                return Err("observed terminal lifetime cannot authorize recovery".into());
            }
            let record = core
                .claims
                .get(&key)
                .filter(|record| &record.claim == old)
                .ok_or("effect predecessor changed")?;
            if record.active_legacy == 0 && record.pending_settlements == 0 {
                break;
            }
            let left = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or("effect predecessor settlement still pending")?;
            #[cfg(test)]
            self.2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            core = self
                .1
                .wait_timeout(core, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            #[cfg(test)]
            self.2.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
        let stamp = core
            .next
            .checked_add(1)
            .ok_or("effect claim space exhausted")?;
        let replacement = LocalEffectClaim {
            target,
            stamp,
            ..old.clone()
        };
        admission.commit_current(&replacement.target, || {
            move_records(&replacement)?;
            core.next = stamp;
            core.claims
                .get_mut(&key)
                .expect("predecessor checked under lock")
                .claim = replacement.clone();
            Ok(())
        })?;
        Ok(replacement)
    }

    fn settled(&self, claim: &LocalEffectClaim) {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = core.record_mut(claim) {
            record.pending_settlements = record.pending_settlements.saturating_sub(1);
        }
        self.1.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn drained_predecessor(
        &self,
        target: &super::machines::RuntimeMemberTarget,
        session: &str,
        deadline: std::time::Instant,
    ) -> Result<LocalEffectClaim, String> {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let prior = core
                .claims
                .get(&(target.member.slot, session.into()))
                .filter(|prior| &prior.claim.target == target)
                .ok_or("respawn predecessor ownership is unknown")?;
            if prior.exit_complete && prior.removed_complete {
                return Ok(prior.claim.clone());
            }
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or("respawn predecessor is still draining")?;
            core = self
                .1
                .wait_timeout(core, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub(crate) fn drained_spawn_predecessor(
        &self,
        target: &super::machines::RuntimeMemberTarget,
        session: &str,
        deadline: std::time::Instant,
    ) -> Result<EffectPredecessor, String> {
        let mut core = self.0.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            match core.predecessor(target, session) {
                Ok(proof) => return Ok(proof),
                Err(error) => {
                    let Some(left) = deadline.checked_duration_since(std::time::Instant::now())
                    else {
                        return Err(error);
                    };
                    core = self
                        .1
                        .wait_timeout(core, left)
                        .unwrap_or_else(|error| error.into_inner())
                        .0;
                }
            }
        }
    }

    pub(crate) fn associate_terminal<'a>(
        &'a self,
        admission: &crate::terminal::session_server::LocalRuntimeAdmission,
        old: &LocalEffectClaim,
        target: &super::machines::RuntimeMemberTarget,
    ) -> Result<Option<TerminalCleanup<'a>>, String> {
        if !same_effect_lifetime(old, target, &old.session) {
            return Ok(None);
        }
        let mut core = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if !core
            .terminal
            .iter()
            .any(|record| same_effect_lifetime(&record.claim, target, &old.session))
        {
            return Ok(None);
        }
        let deadline =
            std::time::Instant::now() + crate::terminal::session_server::EXIT_REAP_TIMEOUT;
        loop {
            let current = core
                .claims
                .get(&(old.target.member.slot, old.session.clone()))
                .filter(|record| &record.claim == old)
                .ok_or("cleanup predecessor changed")?;
            if current.cleanup_failed {
                return Err("cleanup predecessor failed or is unknown".into());
            }
            if current.active_legacy == 0 && current.pending_settlements == 0 {
                break;
            }
            let left = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or("cleanup predecessor still pending")?;
            core = self
                .1
                .wait_timeout(core, left)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
        let mut outcomes = Vec::new();
        admission.commit_current(target, || {
            for record in &core.terminal {
                if same_effect_lifetime(&record.claim, target, &old.session)
                    && record
                        .cleanup_owner
                        .as_ref()
                        .is_some_and(|owner| owner != old)
                {
                    return Err("terminal cleanup owner changed".into());
                }
            }
            let current = core
                .claims
                .get_mut(&(old.target.member.slot, old.session.clone()))
                .expect("predecessor checked under lock");
            current.retired = true;
            current.pending_settlements += 1;
            for record in &mut core.terminal {
                if same_effect_lifetime(&record.claim, target, &old.session) {
                    record.cleanup_owner = Some(old.clone());
                    outcomes.extend(record.terminal_outcomes.iter().flatten().cloned());
                }
            }
            Ok(())
        })?;
        Ok(Some(TerminalCleanup {
            owners: self,
            owner: old.clone(),
            outcomes,
            completed: false,
        }))
    }

    fn associated_cleanup<'a>(
        &'a self,
        ticket: &LocalEffectClaim,
        event: &TerminalDomainEvent,
    ) -> Option<TerminalCleanup<'a>> {
        let mut core = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let owner = core.record(ticket)?.cleanup_owner.clone()?;
        let current = core
            .claims
            .get_mut(&(owner.target.member.slot, owner.session.clone()))
            .filter(|record| record.claim == owner)?;
        current.pending_settlements += 1;
        Some(TerminalCleanup {
            owners: self,
            owner,
            outcomes: vec![event.clone()],
            completed: false,
        })
    }

    /// The short commit closure may only mutate a registry under its own lock.
    /// Ingress uses the same lock, so no registration can commit after retirement.
    pub(crate) fn commit<T>(
        &self,
        claim: &LocalEffectClaim,
        mutate: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !core
            .claims
            .get(&(claim.target.member.slot, claim.session.clone()))
            .is_some_and(|current| &current.claim == claim && claim.authorizing && !current.retired)
        {
            return Err("session effect claim changed or retired".into());
        }
        mutate()
    }

    fn ingress(
        &self,
        target: &super::machines::RuntimeMemberTarget,
        session: &str,
        terminal: bool,
    ) -> Option<LocalEffectClaim> {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        core.observe(target, session);
        let key = (target.member.slot, session.into());
        if terminal
            && core
                .claims
                .get(&key)
                .is_some_and(|record| &record.claim.target != target)
        {
            if !core
                .terminal
                .iter()
                .any(|record| record.claim.target == *target && record.claim.session == session)
            {
                let cleanup_owner = core
                    .terminal
                    .iter()
                    .find(|record| same_effect_lifetime(&record.claim, target, session))
                    .and_then(|record| record.cleanup_owner.clone())
                    .filter(|owner| {
                        core.claims
                            .get(&key)
                            .is_some_and(|record| &record.claim == owner)
                    });
                core.next = core.next.checked_add(1)?;
                let stamp = core.next;
                core.terminal.push(EffectClaimState {
                    claim: LocalEffectClaim {
                        target: target.clone(),
                        session: session.into(),
                        stamp,
                        pool_member: false,
                        authorizing: false,
                    },
                    retired: true,
                    exit_complete: false,
                    removed_complete: false,
                    active_legacy: 0,
                    pending_settlements: 0,
                    terminal_outcomes: [None, None],
                    cleanup_owner,
                    cleanup_failed: false,
                });
            }
            let observed = core
                .terminal
                .iter_mut()
                .find(|record| record.claim.target == *target && record.claim.session == session)?;
            observed.pending_settlements = observed.pending_settlements.saturating_add(1);
            return Some(observed.claim.clone());
        }
        if terminal && !core.claims.contains_key(&key) {
            // An unregistered exit still prevents a late discovered-session commit.
            core.next = core.next.checked_add(1)?;
            let claim = LocalEffectClaim {
                target: target.clone(),
                session: session.into(),
                stamp: core.next,
                pool_member: false,
                authorizing: false,
            };
            core.claims.insert(
                key.clone(),
                EffectClaimState {
                    claim,
                    retired: true,
                    exit_complete: false,
                    removed_complete: false,
                    active_legacy: 0,
                    pending_settlements: 0,
                    terminal_outcomes: [None, None],
                    cleanup_owner: None,
                    cleanup_failed: false,
                },
            );
        }
        let current = core.claims.get_mut(&key)?;
        if &current.claim.target != target {
            return None;
        }
        current.retired |= terminal;
        if terminal {
            current.pending_settlements = current.pending_settlements.saturating_add(1);
        }
        Some(current.claim.clone())
    }

    fn complete_ingress(&self, claim: &LocalEffectClaim, event: &TerminalDomainEvent) {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = core.record_mut(claim) {
            current.exit_complete |= matches!(event, TerminalDomainEvent::Exit { .. });
            current.removed_complete |= matches!(event, TerminalDomainEvent::SessionRemoved { .. });
            match event {
                TerminalDomainEvent::Exit { .. } => {
                    current.terminal_outcomes[0] = Some(event.clone())
                }
                TerminalDomainEvent::SessionRemoved { .. } => {
                    current.terminal_outcomes[1] = Some(event.clone())
                }
                _ => {}
            }
            self.1.notify_all();
        }
    }

    pub(crate) fn never_started(&self, claim: &LocalEffectClaim) {
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = core
            .claims
            .get_mut(&(claim.target.member.slot, claim.session.clone()))
        {
            if &current.claim == claim {
                current.retired = true;
                current.exit_complete = true;
                current.removed_complete = true;
                self.1.notify_all();
            }
        }
    }

    pub(crate) fn spawn_failed(
        &self,
        claim: &LocalEffectClaim,
        disposition: crate::terminal::session_server::SpawnDisposition,
    ) {
        if disposition == crate::terminal::session_server::SpawnDisposition::NeverStarted {
            self.never_started(claim);
        } else {
            self.retire_claim(claim);
        }
    }

    fn unambiguous(&self, claim: &LocalEffectClaim) -> bool {
        if !claim.authorizing {
            return false;
        }
        let core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if core
            .claims
            .get(&(claim.target.member.slot, claim.session.clone()))
            .is_none_or(|current| &current.claim != claim)
        {
            return false;
        }
        core.history.get(&claim.session).is_some_and(|origin| {
            origin.as_ref().is_some_and(|origin| {
                origin.member == claim.target.member
                    && origin.namespace.server_id == claim.target.namespace.server_id
                    && origin.namespace.epoch == claim.target.namespace.epoch
            })
        })
    }

    /// Exact recorded-owner operation: retain its lifetime while doing I/O
    /// off-lock. Unlike a legacy event, qualified authority permits peer IDs.
    pub(crate) fn owned_permit(
        &self,
        claim: &LocalEffectClaim,
        validate_binding: impl FnOnce() -> Result<(), String>,
    ) -> Result<LegacyEffectPermit<'_>, String> {
        let mut core = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let current = core
            .claims
            .get_mut(&(claim.target.member.slot, claim.session.clone()))
            .filter(|current| current.claim == *claim && claim.authorizing && !current.retired)
            .ok_or("session effect claim changed or retired")?;
        validate_binding()?;
        current.active_legacy += 1;
        Ok(LegacyEffectPermit {
            owners: self,
            claim: claim.clone(),
        })
    }

    pub(crate) fn legacy_permit(&self, claim: &LocalEffectClaim) -> Option<LegacyEffectPermit<'_>> {
        if !claim.authorizing {
            return None;
        }
        let mut core = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !core.history.get(&claim.session).is_some_and(|origin| {
            origin.as_ref().is_some_and(|origin| {
                origin.member == claim.target.member
                    && origin.namespace.server_id == claim.target.namespace.server_id
                    && origin.namespace.epoch == claim.target.namespace.epoch
            })
        }) {
            return None;
        }
        let current = core
            .claims
            .get_mut(&(claim.target.member.slot, claim.session.clone()))?;
        if &current.claim != claim {
            return None;
        }
        current.active_legacy += 1;
        Some(LegacyEffectPermit {
            owners: self,
            claim: claim.clone(),
        })
    }
}

pub(crate) struct LegacyEffectPermit<'a> {
    owners: &'a LocalEffectOwners,
    claim: LocalEffectClaim,
}

pub(crate) struct TerminalCleanup<'a> {
    owners: &'a LocalEffectOwners,
    owner: LocalEffectClaim,
    outcomes: Vec<TerminalDomainEvent>,
    completed: bool,
}

impl Drop for TerminalCleanup<'_> {
    fn drop(&mut self) {
        let mut core = self
            .owners
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(record) = core.record_mut(&self.owner) {
            record.pending_settlements = record.pending_settlements.saturating_sub(1);
            record.cleanup_failed |= !self.completed;
        }
        self.owners.1.notify_all();
    }
}

pub(crate) fn settle_terminal_cleanup<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    mut cleanup: TerminalCleanup<'_>,
) -> Result<(), String> {
    for event in &cleanup.outcomes {
        if let Some((audit, released)) = settle_owned_app_event(app, &cleanup.owner, event, true)? {
            // drain any stamped kill reason for this session so the
            // Kill audit line carries it. `None` for every non-sweep kill.
            let reason = if matches!(audit, agent_audit::AuditEvent::Kill) {
                app.try_state::<DashboardLaunchState>()
                    .and_then(|state| state.take_kill_reason(&cleanup.owner.session))
            } else {
                None
            };
            agent_audit::append_with_reason(
                audit,
                &released.handle,
                Some(&cleanup.owner.session),
                released.kind,
                reason,
            );
        }
    }
    cleanup.completed = true;
    Ok(())
}

impl Drop for LegacyEffectPermit<'_> {
    fn drop(&mut self) {
        let mut core = self.owners.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = core
            .claims
            .get_mut(&(self.claim.target.member.slot, self.claim.session.clone()))
        {
            if current.claim == self.claim {
                current.active_legacy -= 1;
            }
        }
        self.owners.1.notify_all();
    }
}

/// This instance's terminal-host identity: its id persisted under the
/// instance's CodeSense home (so a restart keeps it), named after the machine.
/// Fails open to an identity for this run only when the home is unusable.
pub fn local_server_identity() -> ServerIdentity {
    let name = ServerIdentity::local_host_name();
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        log::warn!(target: "pty", "no home directory; terminal host id is per-run");
        return ServerIdentity::ephemeral(name);
    };
    ServerIdentity::load_or_ephemeral(&home, name)
}

/// Which of the app's terminal hosts a viewport stream lives in. Each host
/// numbers its own streams.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StreamHost {
    InProcess,
    Sidecar,
    LocalSidecar(EventNamespace),
    /// A remotely admitted server, keyed only by its verified greeting.
    Remote {
        server_id: String,
        epoch: String,
        generation: u64,
    },
}

impl StreamHost {
    #[cfg(test)]
    pub(crate) fn remote(server_id: impl Into<String>, epoch: impl Into<String>) -> Self {
        Self::Remote {
            server_id: server_id.into(),
            epoch: epoch.into(),
            generation: 0,
        }
    }

    pub(crate) fn remote_namespace(namespace: &EventNamespace) -> Self {
        Self::Remote {
            server_id: namespace.server_id.clone(),
            epoch: namespace.epoch.clone(),
            generation: namespace.generation(),
        }
    }
}

/// One attached viewport stream: where it lives, what it views, and the byte
/// watermark and resize count the frontend has been sent up to.
struct Attached {
    host: StreamHost,
    remote: StreamId,
    session: PtySessionId,
    viewport: ViewportId,
    seq: u64,
    resize_gen: u32,
}

/// A PTY resize on a viewport stream, in order with its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct StreamResize {
    pub(crate) at_seq: u64,
    pub(crate) resize_gen: u32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

/// One item of a viewport stream: output ending at the byte watermark `seq`,
/// or a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamItem<'a> {
    Output { data: &'a str, seq: u64 },
    Resized(StreamResize),
}

/// An owned [`StreamItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnedStreamItem {
    Output { data: String, seq: u64 },
    Resized(StreamResize),
}

impl OwnedStreamItem {
    /// The stream position this item takes a viewport to.
    fn position(&self) -> (u64, u32) {
        match self {
            Self::Output { seq, .. } => (*seq, 0),
            Self::Resized(resize) => (resize.at_seq, resize.resize_gen),
        }
    }

    pub(crate) fn as_item(&self) -> StreamItem<'_> {
        match self {
            Self::Output { data, seq } => StreamItem::Output { data, seq: *seq },
            Self::Resized(resize) => StreamItem::Resized(*resize),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Output { data, .. } => data.len(),
            Self::Resized(_) => 0,
        }
    }
}

impl StreamItem<'_> {
    pub(crate) fn to_owned_item(self) -> OwnedStreamItem {
        match self {
            Self::Output { data, seq } => OwnedStreamItem::Output {
                data: data.to_string(),
                seq,
            },
            Self::Resized(resize) => OwnedStreamItem::Resized(resize),
        }
    }
}

/// Output or a resize of a stream no viewport claims yet (its host pushed it
/// ahead of the reply naming the stream). A re-point, or an attach to a
/// reserved id (whose frontend listener is already up), sends it on past the
/// snapshot; an attach that allocates its id drops it, as no listener can be
/// up for it yet.
struct Early {
    host: StreamHost,
    remote: StreamId,
    item: OwnedStreamItem,
}

/// What re-pointing a stream at its host's new stream did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repoint {
    /// It continues on the new stream.
    Moved,
    /// It was detached or re-pointed meanwhile.
    Gone,
    /// The new stream starts past what the frontend was sent: the stream is
    /// detached, and its display must be rebuilt.
    Behind,
}

/// A stream's early items, claimed as a viewport binds it, and the furthest
/// position of any that were dropped before.
struct Claimed {
    items: VecDeque<Early>,
    dropped: Option<(u64, u32)>,
}

/// Why a reserved stream id could not be bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachReservedError {
    /// The id is not reserved (never handed out, released, or already bound).
    NotReserved(String),
    /// Output the host pushed ahead of the subscribe reply overflowed the
    /// early buffer past the snapshot. The reservation is kept: subscribe
    /// again on it.
    Overflowed,
}

impl std::fmt::Display for AttachReservedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReserved(message) => f.write_str(message),
            Self::Overflowed => {
                f.write_str("terminal stream output overflowed before its subscribe reply")
            }
        }
    }
}

/// How much unclaimed output is kept, across every stream.
const EARLY_BYTES: usize = 1024 * 1024;

/// How many retired host streams are remembered: enough to drop the late
/// output that trails a detach or a re-point.
const RETIRED_STREAMS: usize = 1024;

/// Where one emitted stream item goes: the frontend stream `local` and the
/// item. `false` when it could not be delivered.
pub(crate) type Emit<'a> = dyn FnMut(u64, StreamItem<'_>) -> bool + 'a;

#[derive(Default)]
struct StreamTable {
    next: u64,
    locals: HashMap<(StreamHost, StreamId), u64>,
    attached: HashMap<u64, Attached>,
    /// Ids handed out by `reserve` that no subscribe has bound yet.
    reserved: HashSet<u64>,
    /// The latest host streams no viewport will claim again (detached or
    /// re-pointed), oldest first: their late output is dropped, never kept as
    /// early.
    retired: VecDeque<(StreamHost, StreamId)>,
    early: VecDeque<Early>,
    early_bytes: usize,
    /// Unclaimed streams whose oldest early items were dropped to keep
    /// `early` bounded: the furthest position dropped, as (byte watermark,
    /// resize count). A viewport bound past it has a gap and is rebuilt.
    overflowed: HashMap<(StreamHost, StreamId), (u64, u32)>,
    retired_lifetimes: HashSet<StreamHost>,
    origins: HashMap<StreamHost, Weak<crate::terminal::session_server::SessionServerClient>>,
}

impl StreamTable {
    /// Take the early output of `host`'s stream `remote`, in arrival order.
    fn claim_early(&mut self, host: StreamHost, remote: StreamId) -> Claimed {
        let dropped = self.overflowed.remove(&(host.clone(), remote));
        let (items, kept) = std::mem::take(&mut self.early)
            .into_iter()
            .partition(|e| (&e.host, e.remote) == (&host, remote));
        self.early = kept;
        self.early_bytes = self.early.iter().map(|e| e.item.bytes()).sum();
        Claimed { items, dropped }
    }

    /// No viewport will claim `host`'s stream `remote` again.
    fn retire(&mut self, key: (StreamHost, StreamId)) {
        self.overflowed.remove(&key);
        self.locals.remove(&key);
        if self.retired.len() == RETIRED_STREAMS {
            self.retired.pop_front();
        }
        self.retired.push_back(key);
    }

    /// Point `local` at `host`'s stream `remote`, and return its early output.
    fn bind(&mut self, local: u64, host: StreamHost, remote: StreamId) -> Claimed {
        assert!(
            !self.retired_lifetimes.contains(&host),
            "cannot bind a retired host lifetime"
        );
        self.retired.retain(|key| key != &(host.clone(), remote));
        self.locals.insert((host.clone(), remote), local);
        self.claim_early(host, remote)
    }

    /// Emit the part of a batch past stream `local`'s watermark, or a resize
    /// past its resize count, and advance the stream once it is delivered.
    fn send(&mut self, local: u64, item: StreamItem<'_>, emit: &mut Emit<'_>) -> bool {
        let Some(entry) = self.attached.get_mut(&local) else {
            return true;
        };
        match item {
            StreamItem::Output { data, seq } => {
                let Some(unsent) = past(data, seq, entry.seq) else {
                    return true;
                };
                let delivered = emit(local, StreamItem::Output { data: unsent, seq });
                if delivered {
                    entry.seq = seq;
                }
                delivered
            }
            StreamItem::Resized(resize) => {
                if resize.resize_gen <= entry.resize_gen {
                    return true;
                }
                let delivered = emit(local, item);
                if delivered {
                    entry.resize_gen = resize.resize_gen;
                }
                delivered
            }
        }
    }

    /// Send each of `items` to stream `local` until one is not delivered.
    fn send_all(&mut self, local: u64, items: &VecDeque<Early>, emit: &mut Emit<'_>) -> bool {
        items
            .iter()
            .all(|early| self.send(local, early.item.as_item(), emit))
    }

    /// Whether early items past stream position `(seq, resize_gen)` were
    /// dropped: a viewport there would miss them.
    fn gap_past(dropped: Option<(u64, u32)>, seq: u64, resize_gen: u32) -> bool {
        dropped.is_some_and(|(at, generation)| at > seq || generation > resize_gen)
    }
}

/// The app's client-side viewport stream ids. Every host numbers its own
/// streams, so two hosts may both report a `StreamId`; the app gives each
/// attached stream a client-unique id, and `pty-output-stream-{id}` under it
/// is the only name the frontend sees. Every output is re-emitted through this
/// one table, never under a host's own number, and only past the stream's
/// watermark, so a stream re-pointed to a new host stream after a reconnect
/// continues without a gap or a duplicate. Emits run under the table's lock,
/// which keeps each stream's batches in order.
#[derive(Default)]
pub struct StreamIds(Mutex<StreamTable>);

impl StreamIds {
    pub(crate) fn record_origin(
        &self,
        host: StreamHost,
        client: &Arc<crate::terminal::session_server::SessionServerClient>,
    ) {
        self.table().origins.insert(host, Arc::downgrade(client));
    }

    pub(crate) fn origin(
        &self,
        host: &StreamHost,
    ) -> Option<Arc<crate::terminal::session_server::SessionServerClient>> {
        self.table().origins.get(host).and_then(Weak::upgrade)
    }

    pub(crate) fn local_sidecar_hosts(&self) -> Vec<StreamHost> {
        let table = self.table();
        table
            .attached
            .values()
            .filter_map(|entry| {
                matches!(entry.host, StreamHost::LocalSidecar(_)).then_some(entry.host.clone())
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
    fn table(&self) -> std::sync::MutexGuard<'_, StreamTable> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A client-unique id for `attached`, a viewport of `session` just
    /// subscribed on `host`: its stream's live output begins after its
    /// snapshot's `seq`.
    pub fn attach(
        &self,
        host: StreamHost,
        session: &PtySessionId,
        viewport: &ViewportId,
        attached: &SubscribeResult,
    ) -> u64 {
        let mut table = self.table();
        table.next += 1;
        let local = table.next;
        Self::insert_attached(&mut table, local, host, session, viewport, attached);
        local
    }

    /// Hand out a stream id before its subscribe, so the frontend can listen on
    /// `pty-output-stream-{id}` first and miss nothing between the subscribe
    /// reply and its listener. Released by `detach` if never bound.
    pub fn reserve(&self) -> u64 {
        let mut table = self.table();
        table.next += 1;
        let local = table.next;
        table.reserved.insert(local);
        local
    }

    /// Bind the reserved id `local` to `attached`, as `attach` would, and emit
    /// the stream's early output past the snapshot: its listener is already up.
    /// An id that is not reserved (never handed out, released, or already
    /// bound) is an error.
    pub fn attach_reserved(
        &self,
        local: u64,
        host: StreamHost,
        session: &PtySessionId,
        viewport: &ViewportId,
        attached: &SubscribeResult,
        emit: &mut Emit<'_>,
    ) -> Result<u64, AttachReservedError> {
        let mut table = self.table();
        if !table.reserved.remove(&local) {
            return Err(AttachReservedError::NotReserved(format!(
                "terminal stream {local} is not reserved"
            )));
        }
        let early =
            Self::insert_attached(&mut table, local, host.clone(), session, viewport, attached);
        if StreamTable::gap_past(
            early.dropped,
            attached.snapshot.seq,
            attached.snapshot.resize_gen,
        ) {
            table.attached.remove(&local);
            table.retire((host, attached.stream_id));
            table.reserved.insert(local);
            return Err(AttachReservedError::Overflowed);
        }
        let _ = table.send_all(local, &early.items, emit);
        Ok(local)
    }

    fn insert_attached(
        table: &mut StreamTable,
        local: u64,
        host: StreamHost,
        session: &PtySessionId,
        viewport: &ViewportId,
        attached: &SubscribeResult,
    ) -> Claimed {
        assert!(
            !table.retired_lifetimes.contains(&host),
            "cannot attach a retired host lifetime"
        );
        table.attached.insert(
            local,
            Attached {
                host: host.clone(),
                remote: attached.stream_id,
                session: session.clone(),
                viewport: viewport.clone(),
                seq: attached.snapshot.seq,
                resize_gen: attached.snapshot.resize_gen,
            },
        );
        table.bind(local, host, attached.stream_id)
    }

    /// Forget stream `local`: the host stream to detach, if it was attached. A
    /// reservation never bound is released.
    pub fn detach(&self, local: u64) -> Option<(StreamHost, StreamId)> {
        let mut table = self.table();
        if table.reserved.remove(&local) {
            return None;
        }
        let attached = table.attached.remove(&local)?;
        let key = (attached.host, attached.remote);
        table.retire(key.clone());
        Some(key)
    }

    /// The streams attached on `host`: each one's id, session and viewport.
    #[cfg(test)]
    pub fn attached_on(&self, host: StreamHost) -> Vec<(u64, PtySessionId, ViewportId)> {
        self.table()
            .attached
            .iter()
            .filter(|(_, a)| a.host == host)
            .map(|(local, a)| (*local, a.session.clone(), a.viewport.clone()))
            .collect()
    }

    /// Re-subscribe every retained viewport on `host`. The old remote stream
    /// id is an optimistic token: detach or another reconnect winning while
    /// `subscribe` blocks makes this result stale and it is unsubscribed.
    #[cfg(test)]
    pub(crate) fn resume_on<E>(
        &self,
        host: StreamHost,
        current: impl FnMut() -> bool,
        subscribe: impl FnMut(&PtySessionId, &ViewportId) -> Result<SubscribeResult, E>,
        unsubscribe: impl FnMut(StreamId),
        emit: &mut Emit<'_>,
    ) -> Vec<u64> {
        self.resume_from_to(host.clone(), host, current, subscribe, unsubscribe, emit)
    }

    /// Re-subscribe retained viewports from one exact connection namespace to
    /// its replacement namespace. The ownership change and remote stream
    /// re-point happen under the stream-table lock.
    pub(crate) fn resume_from_to<E>(
        &self,
        previous: StreamHost,
        replacement: StreamHost,
        mut current: impl FnMut() -> bool,
        mut subscribe: impl FnMut(&PtySessionId, &ViewportId) -> Result<SubscribeResult, E>,
        mut unsubscribe: impl FnMut(StreamId),
        emit: &mut Emit<'_>,
    ) -> Vec<u64> {
        let table = self.table();
        if table.retired_lifetimes.contains(&replacement) {
            return Vec::new();
        }
        let retained: Vec<_> = table
            .attached
            .iter()
            .filter(|(_, attached)| attached.host == previous)
            .map(|(local, attached)| {
                (
                    *local,
                    attached.remote,
                    attached.session.clone(),
                    attached.viewport.clone(),
                )
            })
            .collect();
        drop(table);
        let mut lost = Vec::new();
        for (local, expected, session, viewport) in retained {
            if !current() {
                break;
            }
            let again = match subscribe(&session, &viewport) {
                Ok(again) => again,
                Err(_) => {
                    if current() && self.detach_from(local, &previous, expected) {
                        lost.push(local);
                    }
                    continue;
                }
            };
            if !current() {
                unsubscribe(again.stream_id);
                continue;
            }
            match self.repoint_from(
                local,
                &previous,
                expected,
                replacement.clone(),
                &again,
                emit,
            ) {
                Repoint::Moved => {}
                Repoint::Gone => unsubscribe(again.stream_id),
                Repoint::Behind => {
                    unsubscribe(again.stream_id);
                    lost.push(local);
                }
            }
        }
        lost
    }

    fn detach_from(&self, local: u64, expected_host: &StreamHost, expected: StreamId) -> bool {
        let mut table = self.table();
        if !table
            .attached
            .get(&local)
            .is_some_and(|attached| attached.host == *expected_host && attached.remote == expected)
        {
            return false;
        }
        let attached = table.attached.remove(&local).expect("checked above");
        table.retire((attached.host, attached.remote));
        true
    }

    fn repoint_from(
        &self,
        local: u64,
        expected_host: &StreamHost,
        expected: StreamId,
        replacement: StreamHost,
        attached: &SubscribeResult,
        emit: &mut Emit<'_>,
    ) -> Repoint {
        let mut table = self.table();
        let Some(entry) = table.attached.get_mut(&local) else {
            return Repoint::Gone;
        };
        if entry.host != *expected_host || entry.remote != expected {
            return Repoint::Gone;
        }
        let old = (entry.host.clone(), entry.remote);
        // The new stream carries only what follows its snapshot. Output or a
        // resize the frontend was not sent before that cannot be cut out of a
        // serialized terminal: the display is rebuilt instead.
        if attached.snapshot.seq > entry.seq || attached.snapshot.resize_gen > entry.resize_gen {
            table.attached.remove(&local);
            table.retire(old);
            return Repoint::Behind;
        }
        let (seq, resize_gen) = (entry.seq, entry.resize_gen);
        entry.host = replacement.clone();
        entry.remote = attached.stream_id;
        table.retire(old);
        let early = table.bind(local, replacement.clone(), attached.stream_id);
        if StreamTable::gap_past(early.dropped, seq, resize_gen) {
            table.attached.remove(&local);
            table.retire((replacement, attached.stream_id));
            return Repoint::Behind;
        }
        let _ = table.send_all(local, &early.items, emit);
        Repoint::Moved
    }

    /// Forget every stream on `host` (its host lifetime ended).
    pub fn forget_host(&self, host: StreamHost) {
        let mut table = self.table();
        table.locals.retain(|(on, _), _| *on != host);
        table.retired.retain(|(on, _)| *on != host);
        table.early.retain(|e| e.host != host);
        table.early_bytes = table.early.iter().map(|e| e.item.bytes()).sum();
        table.overflowed.retain(|(on, _), _| *on != host);
        table.attached.retain(|_, a| a.host != host);
    }

    /// Atomically retire one verified host lifetime. Active stream keys become
    /// tombstones so late frames stay dead; unclaimed early frames are purged.
    pub(crate) fn retire_lifetime_if(
        &self,
        host: StreamHost,
        take_permit: impl FnOnce() -> bool,
    ) -> Option<Vec<PtySessionId>> {
        let mut table = self.table();
        if table.retired_lifetimes.contains(&host) || !take_permit() {
            return None;
        }
        table.retired_lifetimes.insert(host.clone());
        let retired: Vec<_> = table
            .attached
            .iter()
            .filter(|(_, attached)| attached.host == host)
            .map(|(local, attached)| (*local, attached.session.clone(), attached.remote))
            .collect();
        let mut sessions = HashSet::new();
        for (local, session, remote) in retired {
            table.attached.remove(&local);
            table.retire((host.clone(), remote));
            sessions.insert(session);
        }
        table.early.retain(|early| early.host != host);
        table.early_bytes = table.early.iter().map(|early| early.item.bytes()).sum();
        table.overflowed.retain(|(on, _), _| *on != host);
        Some(sessions.into_iter().collect())
    }

    #[cfg(test)]
    pub(crate) fn retire_lifetime(&self, host: StreamHost) -> Option<Vec<PtySessionId>> {
        self.retire_lifetime_if(host, || true)
    }

    /// Re-point stream `local` at its host's new stream `attached` (the same
    /// viewport subscribed again over a new connection) and `emit` the output
    /// of the new stream that arrived first, past the stream's watermark —
    /// unless the new stream starts past it ([`Repoint::Behind`]).
    #[cfg(test)]
    fn repoint(&self, local: u64, attached: &SubscribeResult, emit: &mut Emit<'_>) -> Repoint {
        let Some((expected, replacement)) = self
            .table()
            .attached
            .get(&local)
            .map(|entry| (entry.remote, entry.host.clone()))
        else {
            return Repoint::Gone;
        };
        self.repoint_from(
            local,
            &replacement.clone(),
            expected,
            replacement,
            attached,
            emit,
        )
    }

    /// [`deliver`](Self::deliver) an output batch to an emitter of output.
    #[cfg(test)]
    pub(crate) fn deliver_output(
        &self,
        host: StreamHost,
        remote: StreamId,
        data: String,
        seq: u64,
        emit: &mut dyn FnMut(u64, &str, u64) -> bool,
    ) -> bool {
        self.deliver(
            host,
            remote,
            StreamItem::Output { data: &data, seq },
            &mut |local, item| match item {
                StreamItem::Output { data, seq } => emit(local, data, seq),
                StreamItem::Resized(_) => true,
            },
        )
    }

    /// Re-emit one output batch or resize of `host`'s stream `remote` through
    /// `emit`, under its client-unique id and only past its watermark. Items
    /// of a stream nothing claims yet are kept (bounded) for a re-point; those
    /// of a retired stream are dropped. `false` when `emit` failed.
    pub(crate) fn deliver(
        &self,
        host: StreamHost,
        remote: StreamId,
        item: StreamItem<'_>,
        emit: &mut Emit<'_>,
    ) -> bool {
        let mut table = self.table();
        if table.retired_lifetimes.contains(&host) {
            return true;
        }
        if let Some(local) = table.locals.get(&(host.clone(), remote)).copied() {
            return table.send(local, item, emit);
        }
        if table.retired.contains(&(host.clone(), remote)) {
            return true;
        }
        let item = item.to_owned_item();
        table.early_bytes += item.bytes();
        table.early.push_back(Early { host, remote, item });
        while table.early_bytes > EARLY_BYTES {
            let Some(dropped) = table.early.pop_front() else {
                break;
            };
            table.early_bytes -= dropped.item.bytes();
            let (seq, resize_gen) = dropped.item.position();
            let furthest = table
                .overflowed
                .entry((dropped.host, dropped.remote))
                .or_default();
            *furthest = (furthest.0.max(seq), furthest.1.max(resize_gen));
        }
        true
    }
}

/// The part of `data` — bytes ending at the byte watermark `seq` — past the
/// watermark `sent`, from the first whole character; `None` when nothing is
/// past it.
fn past(data: &str, seq: u64, sent: u64) -> Option<&str> {
    if seq <= sent {
        return None;
    }
    let start = seq.saturating_sub(data.len() as u64);
    let skip = usize::try_from(sent.saturating_sub(start)).unwrap_or(usize::MAX);
    let from = (skip.min(data.len())..=data.len()).find(|&i| data.is_char_boundary(i))?;
    Some(&data[from..]).filter(|unsent| !unsent.is_empty())
}

/// Re-emit one viewport output batch of `host` to the frontend, under the
/// stream's client-unique id — the one path every host's stream output takes.
pub(crate) fn reemit_stream_item<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    host: StreamHost,
    stream: StreamId,
    item: StreamItem<'_>,
) -> Result<(), PortUnavailable> {
    reemit_stream_item_with_epoch(app, host, stream, item, None)
}

fn reemit_stream_item_with_epoch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    host: StreamHost,
    stream: StreamId,
    item: StreamItem<'_>,
    private_epoch: Option<&str>,
) -> Result<(), PortUnavailable> {
    let Some(streams) = app.try_state::<StreamIds>() else {
        return Ok(());
    };
    if streams.deliver(host, stream, item, &mut |local, item| {
        emit_stream_with_epoch(app, local, item, private_epoch)
    }) {
        Ok(())
    } else {
        Err(PortUnavailable(format!(
            "fan-out emit failed for stream {}",
            stream.0
        )))
    }
}

/// The app's side-effect adapter for the PTY session registry of one host.
pub struct AppTerminalSink<R: tauri::Runtime = tauri::Wry> {
    app: tauri::AppHandle<R>,
    host: StreamHost,
    namespace: OnceLock<EventNamespace>,
    delivery: Arc<LocalEffectDelivery>,
    ingress: Arc<Mutex<()>>,
    #[cfg(test)]
    expire_checks: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
}

struct QueuedLocalEffect {
    claim: LocalEffectClaim,
    event: TerminalDomainEvent,
    ready: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct LocalEffectQueue {
    pending: VecDeque<QueuedLocalEffect>,
    presentation_count: usize,
    running: bool,
    closed: bool,
    #[cfg(test)]
    before_delivery: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}

#[derive(Default)]
struct LocalEffectDelivery(Mutex<LocalEffectQueue>);

const LOCAL_EFFECT_EVENTS: usize = 256;

/// A frontend presentation effect (bounded, droppable on overload), as opposed
/// to exact terminal cleanup.
///
/// `CwdChanged` is deliberately NOT presentation (R2-B1): the FE revision
/// counter is bump-only (no polling, no reconvergence), so a dropped event
/// would leave the editor + diff anchored to the pre-`cd` cwd until the
/// next real `cd` or a session restart. OSC 7 fires at most once per
/// command boundary — nothing like `PromptMarker`'s per-prompt volume — so
/// it doesn't belong in the bounded presentation budget.
fn is_presentation(event: &TerminalDomainEvent) -> bool {
    matches!(event, TerminalDomainEvent::PromptMarker { .. })
}

impl LocalEffectDelivery {
    fn reserve(
        &self,
        claim: LocalEffectClaim,
        event: TerminalDomainEvent,
    ) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        // Output has no local effect: it reaches the frontend on its streams.
        if matches!(event, TerminalDomainEvent::Output { .. }) {
            return None;
        }
        let mut queue = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if is_presentation(&event) {
            if queue.closed || queue.presentation_count >= LOCAL_EFFECT_EVENTS {
                log::warn!(target: "pty", "local compatibility queue full/closed; sealed event retained");
                return None;
            }
            queue.presentation_count += 1;
        } else if queue.pending.iter().any(|pending| {
            pending.claim == claim
                && std::mem::discriminant(&pending.event) == std::mem::discriminant(&event)
        }) {
            // At most one Exit and one Removed per claim may wait outside the
            // presentation budget. Exact cleanup is never dropped on overload.
            return None;
        }
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        queue.pending.push_back(QueuedLocalEffect {
            claim,
            event,
            ready: ready.clone(),
        });
        Some(ready)
    }

    fn start<R: tauri::Runtime>(self: &Arc<Self>, app: tauri::AppHandle<R>, host: StreamHost) {
        {
            let mut queue = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if queue.running
                || queue
                    .pending
                    .front()
                    .is_none_or(|entry| !entry.ready.load(std::sync::atomic::Ordering::Acquire))
            {
                return;
            }
            queue.running = true;
        }
        let delivery = self.clone();
        tauri::async_runtime::spawn_blocking(move || loop {
            let entry = {
                let mut queue = delivery.0.lock().unwrap_or_else(|e| e.into_inner());
                if queue
                    .pending
                    .front()
                    .is_none_or(|entry| !entry.ready.load(std::sync::atomic::Ordering::Acquire))
                {
                    queue.running = false;
                    return;
                }
                let entry = queue.pending.pop_front().expect("ready front exists");
                if is_presentation(&entry.event) {
                    queue.presentation_count -= 1;
                }
                entry
            };
            #[cfg(test)]
            {
                let pause = delivery
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .before_delivery
                    .take();
                if let Some((reached, resume)) = pause {
                    let _ = reached.send(());
                    let _ = resume.recv_timeout(codesense_session_server::testing::WAIT);
                }
            }
            let QueuedLocalEffect { claim, event, .. } = entry;
            let state = app.state::<crate::terminal::session_server::SessionServerState>();
            let associated = state.admission.effects.associated_cleanup(&claim, &event);
            if let Some(cleanup) = associated {
                let _ = settle_terminal_cleanup(&app, cleanup);
            } else {
                if let Ok(Some((audit, released))) =
                    settle_owned_app_event(&app, &claim, &event, false)
                {
                    let reason = if matches!(audit, agent_audit::AuditEvent::Kill) {
                        app.try_state::<DashboardLaunchState>()
                            .and_then(|state| state.take_kill_reason(&claim.session))
                    } else {
                        None
                    };
                    agent_audit::append_with_reason(
                        audit,
                        &released.handle,
                        Some(&claim.session),
                        released.kind,
                        reason,
                    );
                }
            }
            if matches!(
                event,
                TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
            ) {
                app.state::<crate::terminal::session_server::SessionServerState>()
                    .admission
                    .effects
                    .settled(&claim);
            }
            let closed = delivery.0.lock().unwrap_or_else(|e| e.into_inner()).closed;
            if !closed && local_legacy_effect_is_safe(&app, &claim) {
                // Reserve the final stamp without holding its lock over emit.
                // Same-ID spawn cannot pass while this old bare effect runs.
                let state = app.state::<crate::terminal::session_server::SessionServerState>();
                if let Some(_permit) = state.admission.effects.legacy_permit(&claim) {
                    let _ = emit_local_legacy(&app, &host, event, claim.pool_member);
                };
            }
        });
    }
}

impl<R: tauri::Runtime> Drop for AppTerminalSink<R> {
    fn drop(&mut self) {
        // No join on the reader. Already captured owned cleanup drains, while
        // bare presentation stops; no new work can arrive after the final drop.
        self.delivery
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed = true;
    }
}

impl<R: tauri::Runtime> AppTerminalSink<R> {
    pub fn new(app: tauri::AppHandle<R>, host: StreamHost) -> Self {
        Self {
            app,
            host,
            namespace: OnceLock::new(),
            delivery: Arc::new(LocalEffectDelivery::default()),
            ingress: Arc::new(Mutex::new(())),
            #[cfg(test)]
            expire_checks: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn bind_namespace(&self, namespace: EventNamespace) {
        let _ = self.namespace.set(namespace);
    }
}

/// Emit `item` on the frontend's stream `local`.
pub(crate) fn emit_stream<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    local: u64,
    item: StreamItem<'_>,
) -> bool {
    emit_stream_with_epoch(app, local, item, None)
}

/// Retain the exact client's stamp for its early replay items.
pub(crate) fn remote_replay_emitter<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    lease: Option<crate::commands::auth::private_authority::PrivateAuthorityLease>,
) -> impl FnMut(u64, StreamItem<'_>) -> bool + '_ {
    let epoch = lease.map(|lease| lease.epoch());
    move |to, item| emit_stream_with_epoch(app, to, item, epoch.as_deref())
}

/// A stream item as the frontend's `pty-output-stream-{id}` event carries it:
/// `{data, seq}` for output, `{resized: {at_seq, resize_gen, cols, rows}}` for
/// a resize.
fn stream_payload(item: StreamItem<'_>) -> serde_json::Value {
    match item {
        StreamItem::Output { data, seq } => serde_json::json!({ "data": data, "seq": seq }),
        StreamItem::Resized(resize) => serde_json::json!({ "resized": resize }),
    }
}

fn emit_stream_with_epoch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    local: u64,
    item: StreamItem<'_>,
    private_epoch: Option<&str>,
) -> bool {
    let event_name = stream_event_name(local);
    let mut payload = stream_payload(item);
    if let Some(epoch) = private_epoch {
        payload["privateAuthorityEpoch"] = epoch.into();
    }
    match app.emit(&event_name, payload) {
        Ok(()) => true,
        Err(e) => {
            trace!(target: "pty", "fan-out emit failed for {}: {}", event_name, e);
            false
        }
    }
}

/// SW-WA-F11 — PTY output pipeline hook: on the first non-empty `Output`
/// batch for a session whose launch is currently in flight, stamp
/// `pty_first_byte_ms` on the tracker.
///
/// Fast-path gating protects AC-5: the hot path (every `Output` on every
/// session system-wide) is a single `Relaxed` atomic load
/// ([`has_in_flight`](crate::commands::launch_telemetry::LaunchTelemetryTracker::has_in_flight)),
/// and only acquires the tracker Mutex when something is actually launching.
///
/// Non-`Output` events are a no-op; empty-data batches are skipped (an empty
/// batch is still an `Output` event — e.g. a flush — but carries no byte to
/// time).
pub(crate) fn observe_output_first_byte<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: &TerminalDomainEvent,
) {
    let TerminalDomainEvent::Output { session, data, .. } = event else {
        return;
    };
    if data.is_empty() {
        return;
    }
    let Some(tracker) =
        app.try_state::<crate::commands::launch_telemetry::LaunchTelemetryTracker>()
    else {
        return;
    };
    if !tracker.has_in_flight() {
        return;
    }
    tracker.observe_pty_first_byte(session.as_str(), std::time::Instant::now());
}

impl<R: tauri::Runtime> TerminalEventSink for AppTerminalSink<R> {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        // Capture the event's admission before deferring it: a reconnect must not
        // make an old-lifetime removal disappear or target its replacement.
        let target = self.namespace.get().and_then(|namespace| {
            let slot = if self.host == StreamHost::InProcess {
                super::machines::RuntimeMemberSlot::InProcess
            } else {
                super::machines::RuntimeMemberSlot::Sidecar
            };
            self.app
                .try_state::<crate::terminal::session_server::SessionServerState>()
                .and_then(|state| state.admission.target_for_namespace(slot, namespace))
        });
        if self.namespace.get().is_some() && target.is_none() {
            return Ok(());
        }
        if matches!(
            event,
            TerminalDomainEvent::SessionRemoved {
                reason: TeardownReason::Expire,
                ..
            }
        ) {
            // Architecture: the reader also delivers RPC replies. Check liveness off
            // that reader and off tokio workers, before Expire retires any claim.
            let (app, host, delivery, ingress) = (
                self.app.clone(),
                self.host.clone(),
                self.delivery.clone(),
                self.ingress.clone(),
            );
            let task = tauri::async_runtime::spawn_blocking(move || {
                let _ =
                    emit_local_received(&app, &host, target.as_ref(), &delivery, &ingress, event);
            });
            #[cfg(test)]
            self.expire_checks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(task);
            #[cfg(not(test))]
            drop(task);
            return Ok(());
        }
        emit_local_received(
            &self.app,
            &self.host,
            target.as_ref(),
            &self.delivery,
            &self.ingress,
            event,
        )
    }
}

#[cfg(test)]
impl<R: tauri::Runtime> AppTerminalSink<R> {
    pub(crate) fn wait_for_expire_checks(&self) {
        let tasks = std::mem::take(&mut *self.expire_checks.lock().unwrap());
        for task in tasks {
            tauri::async_runtime::block_on(task).unwrap();
        }
        // Architecture: tests retain their isolated HOME through queued audit and
        // scope cleanup, not only through the off-reader liveness query.
        assert!(codesense_session_server::testing::wait_until(|| {
            let queue = self.delivery.0.lock().unwrap_or_else(|e| e.into_inner());
            !queue.running && queue.pending.is_empty()
        }));
    }
}

fn emit_local_received<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    host: &StreamHost,
    target: Option<&super::machines::RuntimeMemberTarget>,
    delivery: &Arc<LocalEffectDelivery>,
    ingress: &Mutex<()>,
    event: TerminalDomainEvent,
) -> Result<(), PortUnavailable> {
    observe_output_first_byte(app, &event);
    let mut queued = None;
    if let Some(target) = target {
        if retain_live_expire(app, target, &event) {
            return Ok(());
        }
        let ingress = ingress.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = app
            .state::<crate::terminal::session_server::SessionServerState>()
            .admission
            .effects
            .capture_event(target, &event);
        if let Some(claim) = &ticket {
            queued = delivery.reserve(claim.clone(), event.clone());
        }
        drop(ingress);
        let _ = app.emit(
            "terminal-runtime-event",
            serde_json::json!({"target": target, "event": &event}),
        );
        if let Some(claim) = &ticket {
            app.state::<crate::terminal::session_server::SessionServerState>()
                .admission
                .effects
                .complete_ingress(claim, &event);
            if queued.is_none()
                && matches!(
                    event,
                    TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
                )
            {
                app.state::<crate::terminal::session_server::SessionServerState>()
                    .admission
                    .effects
                    .settled(claim);
            }
        }
    }
    if local_event_session(&event).is_some() {
        if let Some(ready) = queued {
            ready.store(true, std::sync::atomic::Ordering::Release);
            delivery.start(app.clone(), host.clone());
        }
        return Ok(());
    }
    // SW-XS-3 (F7): a session going live in a worktree writes nothing into the
    // workspace; SW-XS-17: nor does it feed an idle clock. Nothing to deliver.
    if matches!(event, TerminalDomainEvent::WorktreeLive { .. }) {
        return Ok(());
    }
    emit_local_legacy(app, host, event, false)
}

fn local_event_session(event: &TerminalDomainEvent) -> Option<&str> {
    match event {
        TerminalDomainEvent::Output { session, .. }
        | TerminalDomainEvent::PromptMarker { session, .. }
        | TerminalDomainEvent::CwdChanged { session, .. }
        | TerminalDomainEvent::Exit { session, .. }
        | TerminalDomainEvent::SessionRemoved { session, .. } => Some(session.as_str()),
        _ => None,
    }
}

// Architecture: only the original server lifetime may prove a session still live.
// Tags and a replacement server with a coincidentally equal session id are not evidence.
fn retain_live_expire<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &super::machines::RuntimeMemberTarget,
    event: &TerminalDomainEvent,
) -> bool {
    use super::machines::RuntimeMemberSlot;
    use codesense_pty_core::domain::TerminalDomain;
    let TerminalDomainEvent::SessionRemoved {
        session,
        reason: TeardownReason::Expire,
    } = event
    else {
        return false;
    };
    let report = match target.member.slot {
        RuntimeMemberSlot::Sidecar => app
            .try_state::<crate::terminal::session_server::SessionServerState>()
            .and_then(|state| state.connected())
            .filter(|client| {
                client.namespace().server_id == target.namespace.server_id
                    && client.namespace().epoch == target.namespace.epoch
            })
            .and_then(|client| client.alive_session_ids(std::slice::from_ref(session)).ok()),
        RuntimeMemberSlot::InProcess => super::pty_backend::in_process_host_handle(app)
            .filter(|host| {
                host.identity().is_ok_and(|id| {
                    id.server.id.as_str() == target.namespace.server_id
                        && id.epoch == target.namespace.epoch
                })
            })
            .and_then(|host| host.alive_session_ids(std::slice::from_ref(session)).ok()),
        RuntimeMemberSlot::Remote => None,
    };
    // Policy (open): an unavailable liveness report keeps the existing release/prune
    // behavior. Retention requires positive live proof, never an inferred heartbeat.
    let retain = report.is_some_and(|live| live.contains(session));
    if retain {
        log::info!(target: "agents", "ignored Expire for live session {}; retained launch facts and scope", session.as_str());
    }
    retain
}

// Architecture: the receiving boundary settles scope, launch facts and asks together.
// Callers already run on blocking delivery/reconnect threads, never tokio workers.
pub(crate) fn settle_owned_app_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    claim: &LocalEffectClaim,
    event: &TerminalDomainEvent,
    exact_scope: bool,
) -> Result<Option<(agent_audit::AuditEvent, ReleasedSession)>, String> {
    if retain_live_expire(app, &claim.target, event) {
        return Ok(None);
    }
    if exact_scope {
        if let TerminalDomainEvent::SessionRemoved { reason, .. } = event {
            if removal_effects(*reason).remove_scope {
                super::mcp_scope::settle_owned_scope(claim)?;
            }
        }
    }
    let released = settle_owned_event(
        app.try_state::<DashboardLaunchState>().as_deref(),
        app.try_state::<AskUserRegistry>().as_deref(),
        claim,
        event,
    );
    // Exact settlement may retain a home awaiting its record. Do not retry its capture here;
    // the session's asks, launch slot and account provenance still finish normally.
    settle_owned_resources_with_scope(
        app.try_state::<super::agent_accounts::AgentAccountRuntimeState>()
            .as_deref(),
        claim,
        event,
        !exact_scope,
    );
    Ok(released)
}

pub(crate) fn settle_owned_event(
    launch: Option<&DashboardLaunchState>,
    asks: Option<&AskUserRegistry>,
    claim: &LocalEffectClaim,
    event: &TerminalDomainEvent,
) -> Option<(agent_audit::AuditEvent, ReleasedSession)> {
    // Deliberate removal precedes child EOF/Exit. Either terminal notification
    // closes the owned asks promptly; the later duplicate is consume-once.
    if matches!(
        event,
        TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
    ) {
        if let Some(asks) = asks {
            asks.cancel_owned(claim);
        }
    }
    match event {
        TerminalDomainEvent::SessionRemoved { reason, .. } if *reason != TeardownReason::Reap => {
            let audit = if *reason == TeardownReason::Kill {
                agent_audit::AuditEvent::Kill
            } else {
                agent_audit::AuditEvent::Expire
            };
            launch?
                .release_owned(claim)
                .map(|released| (audit, released))
        }
        _ => None,
    }
}

pub(crate) fn settle_owned_resources(
    accounts: Option<&super::agent_accounts::AgentAccountRuntimeState>,
    claim: &LocalEffectClaim,
    event: &TerminalDomainEvent,
) {
    settle_owned_resources_with_scope(accounts, claim, event, true);
}

fn settle_owned_resources_with_scope(
    accounts: Option<&super::agent_accounts::AgentAccountRuntimeState>,
    claim: &LocalEffectClaim,
    event: &TerminalDomainEvent,
    remove_scope: bool,
) {
    if let TerminalDomainEvent::SessionRemoved { reason, .. } = event {
        let effects = removal_effects(*reason);
        if remove_scope && effects.remove_scope {
            super::mcp_scope::remove_owned_scope(claim);
        }
        if effects.clear_provenance {
            if let Some(accounts) = accounts {
                accounts.take_owned_provenance(claim);
            }
        }
    }
}

fn local_legacy_effect_is_safe<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    claim: &LocalEffectClaim,
) -> bool {
    use super::machines::RuntimeMemberSlot;
    use crate::terminal::session_server::{Presence, SessionServerState};
    use codesense_pty_core::domain::TerminalDomain;
    let Some(state) = app.try_state::<SessionServerState>() else {
        return false;
    };
    if !state.admission.effects.unambiguous(claim)
        || super::pty_backend::validate_member_target(app, &claim.target).is_err()
    {
        return false;
    }
    let absent = match claim.target.member.slot {
        RuntimeMemberSlot::InProcess => match state.presence() {
            Presence::Absent => true,
            Presence::Unknown(_) => false,
            Presence::Connected(client) => {
                client.has_session(&PtySessionId::new(&claim.session)) == Ok(false)
                    && state
                        .connected()
                        .is_some_and(|current| Arc::ptr_eq(&current, &client))
            }
        },
        RuntimeMemberSlot::Sidecar => {
            super::pty_backend::in_process_lacks_session(app, &claim.session)
        }
        RuntimeMemberSlot::Remote => false,
    };
    absent
        && state.admission.effects.unambiguous(claim)
        && super::pty_backend::validate_member_target(app, &claim.target).is_ok()
}

fn emit_local_legacy<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    host: &StreamHost,
    event: TerminalDomainEvent,
    pool_owner: bool,
) -> Result<(), PortUnavailable> {
    match event {
        // Output reaches the frontend only on its viewports' streams.
        TerminalDomainEvent::Output { .. } => {}
        TerminalDomainEvent::StreamOutput { stream, data, seq } => {
            reemit_stream_item(
                app,
                host.clone(),
                stream,
                StreamItem::Output { data: &data, seq },
            )?;
        }
        TerminalDomainEvent::StreamResized {
            stream,
            at_seq,
            resize_gen,
            cols,
            rows,
        } => {
            reemit_stream_item(
                app,
                host.clone(),
                stream,
                StreamItem::Resized(StreamResize {
                    at_seq,
                    resize_gen,
                    cols,
                    rows,
                }),
            )?;
        }
        // PT-5: publish an OSC 133 marker to internal Rust subscribers (PT-8's
        // warm-pool handoff, via [`Osc133Bus`]) and emit the thin `pty-osc133`
        // frontend event.
        TerminalDomainEvent::PromptMarker { session, marker } => {
            if let Some(event) = Osc133Event::from_osc(session.as_str(), &marker) {
                if event.kind == crate::terminal::osc_dispatch::Osc133Kind::CommandEnd {
                    let changed = app
                        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
                        .is_some_and(|launch| {
                            launch.turns().record_exit(
                                session.as_str(),
                                event.exit_code.map(i64::from),
                                false,
                                false,
                                false,
                            )
                        });
                    if changed {
                        if let Some(hub) = app.try_state::<crate::live_hub::LiveHub>() {
                            hub.invalidate_sessions();
                        }
                    }
                }
                if let Some(bus) = app.try_state::<Osc133Bus>() {
                    bus.publish(&event);
                }
                let _ = app.emit("pty-osc133", &event);
            }
        }
        // SW-VE-14: an OSC 7 cwd mutation (deduped at the batcher). The thin
        // `pty-osc7` frontend event feeds `useSessionCwdRevision` so the
        // editor + diff retarget on `cd` without a session restart. A
        // non-UTF-8 cwd (vanishingly rare — OSC 7 carries percent-encoded
        // bytes, and shells report their own cwd) is skipped rather than
        // mangled through `to_string_lossy`: the FE's cwd resolver would
        // read a lie and retarget to the wrong path.
        TerminalDomainEvent::MachineStats { sample } => {
            crate::live_hub::machine_stats::note(app, "local", Some(sample), true);
        }
        TerminalDomainEvent::CwdChanged { session, cwd } => {
            if let Some(cwd_str) = cwd.to_str() {
                let _ = app.emit(
                    "pty-osc7",
                    serde_json::json!({
                        "session_id": session.as_str(),
                        "cwd": cwd_str,
                    }),
                );
            }
        }
        TerminalDomainEvent::Exit {
            session,
            status,
            pool_member,
            requested,
            ..
        } => {
            let session_id = session.as_str();
            crate::session_usage::timing::note_session_end(session_id);
            crate::session_conversation::note_end(session_id);
            if let Some(launch) =
                app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            {
                launch.turns().record_exit(
                    session_id,
                    status
                        .as_ref()
                        .and_then(|exit| exit.exit_code)
                        .map(i64::from),
                    status.as_ref().is_some_and(|exit| exit.signal.is_some()),
                    true,
                    requested,
                );
            }
            let _ = app.emit("pty-exit", pty_exit_payload(session_id, status, requested));
            let pool = app.try_state::<WarmPoolState>();
            settle_exited_session(None, pool.as_deref(), session_id, pool_member && pool_owner);
        }
        // RF-perf-17 (AC-4): a session became live for a worktree — fire the
        // on-become-live catch-up reconcile so edits made while the worktree
        // was dormant converge (fire-and-forget; a no-op when the dir is under
        // no attached worktree).
        TerminalDomainEvent::WorktreeLive { .. } => {}
        TerminalDomainEvent::LayoutChanged {
            epoch,
            rev,
            workspace,
            layout,
        } => {
            let _ = app.emit(
                    TERMINAL_LAYOUT_CHANGED_EVENT,
                    serde_json::json!({ "epoch": epoch, "rev": rev, "workspace": workspace, "layout": layout }),
                );
        }
        TerminalDomainEvent::SessionRemoved { session, .. } => {
            crate::session_usage::timing::note_session_end(session.as_str());
            crate::session_conversation::note_end(session.as_str());
            let _ = app.emit("sessions-source-changed", ());
        }
        TerminalDomainEvent::Activity { snapshot } => {
            crate::session_usage::timing::note_snapshot(&snapshot);
            let _ = app.emit("sessions-source-changed", ());
        }
        // InboxWaitV1: a notice's fate or a waiter's state from this Mac's hosts.
        event @ (TerminalDomainEvent::InboxAck { .. }
        | TerminalDomainEvent::InboxRelease { .. }
        | TerminalDomainEvent::InboxUncertain { .. }
        | TerminalDomainEvent::InboxWaiter { .. }) => {
            crate::commands::host_inbox::on_local_pushed(app, event);
        }
        // SW-AGR-9: a wake outcome from this Mac's session server (pushed only
        // to a connection that asked for them) lands in the delivery core:
        // the scrollback annotation, the `Woken by` caption, the tab's wake
        // history and the run row all follow from it.
        TerminalDomainEvent::Wake { outcome } => {
            crate::delivery::app::on_local_wake(app, *outcome);
        }
    }
    Ok(())
}
impl<R: tauri::Runtime> NamespacedTerminalEventSink for AppTerminalSink<R> {
    fn emit(&self, envelope: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        self.emit_remote(envelope, None)
    }

    fn emit_private(
        &self,
        envelope: NamespacedTerminalEvent,
        lease: &super::auth::private_authority::PrivateAuthorityLease,
    ) -> Result<(), PortUnavailable> {
        let epoch = lease.epoch();
        let envelope = lease.commit(|| envelope).map_err(PortUnavailable)?;
        // No callbacks or Tauri I/O under the authority gate. The recipient
        // rejects retired stamps at consumption, including queued output.
        self.emit_remote(envelope, Some(&epoch))
    }
}

impl<R: tauri::Runtime> AppTerminalSink<R> {
    fn emit_remote(
        &self,
        envelope: NamespacedTerminalEvent,
        private_epoch: Option<&str>,
    ) -> Result<(), PortUnavailable> {
        if matches!(
            &envelope.event,
            TerminalDomainEvent::LayoutChanged { .. } | TerminalDomainEvent::Activity { .. }
        ) {
            let _ = self.app.emit("sessions-source-changed", ());
            let manager = self
                .app
                .try_state::<super::host_connection_manager::HostConnectionManager>();
            if let Some((manager, target)) = manager.as_ref().and_then(|manager| {
                manager
                    .target_for_namespace(&envelope.namespace)
                    .map(|target| (manager, target))
            }) {
                if let TerminalDomainEvent::LayoutChanged {
                    workspace, layout, ..
                } = &envelope.event
                {
                    manager.note_remote_workspace_tabs(
                        &target.member.machine_id,
                        workspace.as_str(),
                        layout.as_ref().map_or(0, |layout| layout.tabs.len()),
                    );
                }
                let _ = self.app.emit(
                    "terminal-runtime-event",
                    serde_json::json!({"target": target, "event": &envelope.event, "privateAuthorityEpoch": private_epoch}),
                );
            }
        }
        if let TerminalDomainEvent::MachineStats { sample } = &envelope.event {
            if let Some(manager) = self
                .app
                .try_state::<super::host_connection_manager::HostConnectionManager>()
            {
                if let Some(target) = manager.target_for_namespace(&envelope.namespace) {
                    crate::live_hub::machine_stats::note(
                        &self.app,
                        &target.member.machine_id,
                        Some(sample.clone()),
                        true,
                    );
                }
            }
            return Ok(());
        }
        match envelope.event {
            TerminalDomainEvent::MachineStats { .. } => {}
            TerminalDomainEvent::StreamOutput { stream, data, seq } => {
                reemit_stream_item_with_epoch(
                    &self.app,
                    StreamHost::remote_namespace(&envelope.namespace),
                    stream,
                    StreamItem::Output { data: &data, seq },
                    private_epoch,
                )?;
            }
            TerminalDomainEvent::StreamResized {
                stream,
                at_seq,
                resize_gen,
                cols,
                rows,
            } => {
                reemit_stream_item_with_epoch(
                    &self.app,
                    StreamHost::remote_namespace(&envelope.namespace),
                    stream,
                    StreamItem::Resized(StreamResize {
                        at_seq,
                        resize_gen,
                        cols,
                        rows,
                    }),
                    private_epoch,
                )?;
            }
            TerminalDomainEvent::WorktreeLive { .. } => {
                // Remote worktrees never enter local content reconciliation.
            }
            // SW-AGR-9: a remote machine's wake outcome, keyed to the machine
            // its admitted connection belongs to.
            TerminalDomainEvent::Wake { outcome } => {
                crate::delivery::app::on_remote_wake(&self.app, &envelope.namespace, *outcome);
            }
            // InboxWaitV1: a notice's fate or a waiter's state from a machine.
            event @ (TerminalDomainEvent::InboxAck { .. }
            | TerminalDomainEvent::InboxRelease { .. }
            | TerminalDomainEvent::InboxUncertain { .. }
            | TerminalDomainEvent::InboxWaiter { .. }) => {
                crate::commands::host_inbox::on_pushed(&self.app, &envelope.namespace, event);
            }
            event => {
                self.app
                    .emit(
                        REMOTE_TERMINAL_DOMAIN_EVENT,
                        serde_json::json!({
                            "namespace": envelope.namespace,
                            "event": event,
                            "privateAuthorityEpoch": private_epoch,
                        }),
                    )
                    .map_err(|error| {
                        PortUnavailable(format!("remote terminal envelope emit failed: {error}"))
                    })?;
            }
        }
        Ok(())
    }
}

/// The per-viewport live-byte event the frontend listens on for its stream
/// `local`.
pub(crate) fn stream_event_name(local: u64) -> String {
    format!("pty-output-stream-{local}")
}

/// The `pty-exit` payload: the child's exit status as its host saw it (the
/// in-process runtime or the session-server sidecar, which owns the child) —
/// `exit_code` for a normal exit, `signal` when a signal killed it, both `null`
/// when the child outlived its terminal — and whether CodeSense asked for the
/// exit. The frontend classifies it.
pub(crate) type PtyExitPayload = serde_json::Value;

pub(crate) fn pty_exit_payload(
    session_id: &str,
    status: Option<ExitInfo>,
    requested: bool,
) -> PtyExitPayload {
    let ExitInfo { exit_code, signal } = status.unwrap_or_default();
    serde_json::json!({
        "session_id": session_id,
        "exit_code": exit_code,
        "signal": signal,
        "requested": requested,
    })
}

/// The in-app consequences of a session's death. SW-AW-4 AC-3: resolve any
/// blocking `ask_user` awaiting it with a typed `cancel`, so the agent isn't
/// wedged for the full ASK_USER_TIMEOUT after its own PTY exits (non-agent/pool
/// sessions have no pending asks → a no-op). PT-8: a pool member that exits on
/// its own (e.g. an external `kill -9`) is reclaimed + refilled by the pool,
/// not retained.
fn settle_exited_session(
    asks: Option<&AskUserRegistry>,
    pool: Option<&WarmPoolState>,
    session_id: &str,
    pool_member: bool,
) {
    // Forget the session's dot ledger first, so the ask the sweep resolves
    // cannot restore a dot on this dead tab.
    crate::mcp::agent_dot::ledger().forget(session_id);
    if let Some(asks) = asks {
        asks.cancel_by_session(session_id);
    }
    if pool_member {
        if let Some(pool) = pool {
            pool.notify_member_exited(session_id);
        }
    }
}

/// SW-DX-7 AC-8: a dashboard-launched session that left the registry releases
/// its SpawnCap slot and drops its storyId↔sessionId binding in lockstep; the
/// returned audit event records it as a deliberate `Kill` or a background
/// `Expire`. A plain terminal / warm-pool member releases nothing → `None` → no
/// audit.
#[cfg(test)]
fn release_dashboard_binding(
    launch: Option<&DashboardLaunchState>,
    session_id: &str,
    reason: TeardownReason,
) -> Option<(agent_audit::AuditEvent, ReleasedSession)> {
    let audit = match reason {
        TeardownReason::Kill => agent_audit::AuditEvent::Kill,
        TeardownReason::Expire => agent_audit::AuditEvent::Expire,
        // A reap keeps the tab and its binding: nothing to release or audit.
        TeardownReason::Reap => return None,
    };
    let released = launch?.release_for_session(session_id)?;
    Some((audit, released))
}

/// What a session leaving the registry takes with it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RemovalEffects {
    /// Release its dashboard binding (audited as a kill or an expiry).
    pub release_binding: bool,
    /// Remove its scoped MCP config.
    pub remove_scope: bool,
    /// Drop its staged launch provenance.
    pub clear_provenance: bool,
}

/// A deliberate close takes everything; a background reclaim everything but
/// the launch provenance. A reap takes nothing: the session ended by itself and
/// its id is about to host a new process (`terminal_respawn_tab`), which
/// removes the old scope config itself before projecting a fresh one —
/// removing it here, late, could delete the fresh one. The binding and
/// provenance belong to the tab, which stays; nothing was killed, so nothing is
/// audited.
pub(crate) fn removal_effects(reason: TeardownReason) -> RemovalEffects {
    match reason {
        TeardownReason::Kill => RemovalEffects {
            release_binding: true,
            remove_scope: true,
            clear_provenance: true,
        },
        TeardownReason::Expire => RemovalEffects {
            release_binding: true,
            remove_scope: true,
            clear_provenance: false,
        },
        TeardownReason::Reap => RemovalEffects {
            release_binding: false,
            remove_scope: false,
            clear_provenance: false,
        },
    }
}

/// OC-10: the `AppHandle`-backed adapter that performs the warm pool's PTY-side
/// spawn/kill off the pool's registry lock, and owns the async orchestration +
/// pool re-entry the operations require. Keeps `terminal::warm_pool` free of any
/// Tauri coupling — the pool decides which members to spawn/kill; this executes.
pub struct AppWarmPoolHost {
    app: tauri::AppHandle,
}

impl AppWarmPoolHost {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

/// SV-7: whether the warm pool may spawn a member now: not while the terminal
/// server was stopped on purpose and nothing a person did started it since —
/// a refill would start it again behind the person's back.
pub(crate) fn pool_may_spawn<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    !app.try_state::<crate::terminal::session_server::SessionServerState>()
        .is_some_and(|state| state.user_stopped())
}

impl WarmPoolHost for AppWarmPoolHost {
    fn spawn_member(&self, id: String, params: MemberSpawnParams) {
        let app = self.app.clone();
        if !pool_may_spawn(&app) {
            if let Some(pool) = app.try_state::<WarmPoolState>() {
                pool.remove_member(&id);
            }
            return;
        }
        // Fire-and-forget off the request path; the spawn's blocking prep and
        // fork run on the blocking pool, never parking a runtime worker.
        tauri::async_runtime::spawn(async move {
            let res = spawn_pty_inner(
                &app,
                id.clone(),
                String::new(),
                // SW-WA-F10: a warm-pool shell runs the user's login shell;
                // no argv-direct spawn, no native-launch binding.
                None,
                params.home,
                SpawnPtySize {
                    cols: params.cols,
                    rows: params.rows,
                },
                Some(true),
                params.preset,
                // RF-15: a pool member spawns with no correlation keys — the
                // all-`None` `SessionMeta::default()` is the sentinel the
                // persistence classifier uses to exclude unattached members (gains
                // real keys only at handoff — PT-8 D5).
                SessionMeta::default(),
                None,
                params.shell_integration,
                None,
                None,
                None,
                None,
                true,
                crate::commands::pty_backend::SpawnPlacement::ResolveLocal,
            )
            .await;
            match res {
                // Reconcile through the pool: it counts the warm spawn only if the
                // reserved slot survived a concurrent drain/reconfigure (L3). If it
                // didn't, reclaim the now-live PTY here so it can't leak as an
                // orphaned pool shell invisible to acquire/TTL/drain.
                Ok(()) => {
                    if let Some(pool) = app.try_state::<WarmPoolState>() {
                        if !pool.on_member_spawned(&id) {
                            kill_session(&app, &id);
                        }
                    }
                }
                Err(e) => {
                    log::warn!(target: "warm_pool", "member spawn failed for {id}: {e}");
                    if let Some(pool) = app.try_state::<WarmPoolState>() {
                        pool.remove_member(&id);
                    }
                }
            }
        });
    }

    fn kill_member(&self, id: &str) {
        kill_session(&self.app, id);
    }

    fn reclaim_exited_and_refill(&self, id: String) {
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            // The shell already exited; prune the retained dead session, refill.
            kill_session(&app, &id);
            if let Some(pool) = app.try_state::<WarmPoolState>() {
                pool.refill();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm5aa_owned_permit_rejects_changed_binding_and_releases_on_unwind() {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
            RuntimeNamespace,
        };
        let owners = LocalEffectOwners::default();
        let target = RuntimeMemberTarget {
            member: RuntimeMemberAddress::local(RuntimeMemberSlot::Sidecar),
            admission_revision: RuntimeCounter::new(1),
            namespace: RuntimeNamespace {
                server_id: "owner".into(),
                epoch: "epoch".into(),
                generation: RuntimeCounter::new(1),
            },
        };
        let claim = owners.reserve(target.clone(), "permit-session").unwrap();
        let count = || {
            owners
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .claims[&(RuntimeMemberSlot::Sidecar, claim.session.clone())]
                .active_legacy
        };
        assert!(owners
            .owned_permit(&claim, || Err("binding changed".into()))
            .is_err());
        assert_eq!(count(), 0);
        let mut replacement = target;
        replacement.namespace.generation = RuntimeCounter::new(2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _permit = owners.owned_permit(&claim, || Ok(())).unwrap();
            assert_eq!(count(), 1);
            assert!(owners
                .reserve_spawn(replacement, &claim.session, false)
                .is_err());
            panic!("operation failed while its exact owner was retained");
        }));
        assert!(result.is_err());
        assert_eq!(count(), 0);
        owners.retire_claim(&claim);
        let mut checked_binding = false;
        assert!(owners
            .owned_permit(&claim, || {
                checked_binding = true;
                Ok(())
            })
            .is_err());
        assert!(!checked_binding);
        assert_eq!(count(), 0);
    }
    use tauri::Listener;

    #[test]
    fn rm5aa_terminal_cleanup_reservation_blocks_same_target_and_retains_failure() {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
            RuntimeNamespace,
        };
        for outcome in ["success", "error", "unwind"] {
            let owners = LocalEffectOwners::default();
            let target = RuntimeMemberTarget {
                member: RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
                admission_revision: RuntimeCounter::new(1),
                namespace: RuntimeNamespace {
                    server_id: "cleanup".into(),
                    epoch: "epoch".into(),
                    generation: RuntimeCounter::new(1),
                },
            };
            let claim = owners.reserve(target.clone(), "cleanup-session").unwrap();
            for event in [
                TerminalDomainEvent::Exit {
                    session: PtySessionId::new(&claim.session),
                    status: None,
                    pool_member: false,
                    requested: false,
                    incarnation: 0,
                },
                TerminalDomainEvent::SessionRemoved {
                    session: PtySessionId::new(&claim.session),
                    reason: TeardownReason::Reap,
                },
            ] {
                let ticket = owners.capture_event(&target, &event).unwrap();
                owners.complete_ingress(&ticket, &event);
                owners.settled(&ticket);
            }
            // Exercise the actual RAII settlement independently of runtime/socket admission.
            owners
                .0
                .lock()
                .unwrap()
                .record_mut(&claim)
                .unwrap()
                .pending_settlements += 1;
            let mut cleanup = TerminalCleanup {
                owners: &owners,
                owner: claim.clone(),
                outcomes: Vec::new(),
                completed: false,
            };
            assert!(owners
                .reserve_spawn(target.clone(), &claim.session, false)
                .is_err());
            if outcome == "unwind" {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _held = cleanup;
                    panic!("cleanup interrupted");
                }));
                assert!(result.is_err());
            } else {
                cleanup.completed = outcome == "success";
                drop(cleanup);
            }
            {
                let core = owners.0.lock().unwrap();
                let record = core.record(&claim).unwrap();
                assert_eq!(record.pending_settlements, 0);
                assert_eq!(record.cleanup_failed, outcome != "success");
            }
            assert_eq!(
                owners.reserve_spawn(target, &claim.session, false).is_ok(),
                outcome == "success"
            );
        }
    }

    #[test]
    fn rm5aa_replacement_terminal_evidence_preserves_predecessor_and_blocks_discovery() {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
            RuntimeNamespace,
        };
        for removed_first in [false, true] {
            let owners = LocalEffectOwners::default();
            let old_target = RuntimeMemberTarget {
                member: RuntimeMemberAddress::local(RuntimeMemberSlot::Sidecar),
                admission_revision: RuntimeCounter::new(1),
                namespace: RuntimeNamespace {
                    server_id: "server".into(),
                    epoch: "old".into(),
                    generation: RuntimeCounter::new(1),
                },
            };
            let old = owners.reserve(old_target.clone(), "shared").unwrap();
            let permit = owners.owned_permit(&old, || Ok(())).unwrap();
            owners.retire_claim(&old);
            let mut target = old_target;
            target.namespace.epoch = "replacement".into();
            target.namespace.generation = RuntimeCounter::new(2);
            target.admission_revision = RuntimeCounter::new(2);
            let exit = TerminalDomainEvent::Exit {
                session: PtySessionId::new("shared"),
                status: None,
                pool_member: false,
                requested: false,
                incarnation: 0,
            };
            let removed = TerminalDomainEvent::SessionRemoved {
                session: PtySessionId::new("shared"),
                reason: TeardownReason::Kill,
            };
            let events = if removed_first {
                [removed, exit]
            } else {
                [exit, removed]
            };
            let first = owners.capture_event(&target, &events[0]).unwrap();
            assert!(!first.authorizing);
            owners.complete_ingress(&first, &events[0]);
            assert!(owners.reserve(target.clone(), "shared").is_err());
            assert!(owners
                .reserve_spawn(target.clone(), "shared", false)
                .is_err());
            let mut readmitted = target.clone();
            readmitted.namespace.generation = RuntimeCounter::new(3);
            readmitted.admission_revision = RuntimeCounter::new(3);
            assert!(owners.reserve(readmitted, "shared").is_err());
            assert_eq!(owners.claims(RuntimeMemberSlot::Sidecar), vec![old.clone()]);
            let last = owners.capture_event(&target, &events[1]).unwrap();
            assert_eq!(first, last);
            owners.complete_ingress(&last, &events[1]);
            owners.settled(&first);
            assert!(owners
                .reserve_spawn(target.clone(), "shared", false)
                .is_err());
            owners.settled(&last);
            assert!(owners
                .reserve_spawn(target.clone(), "shared", false)
                .is_err());
            drop(permit);
            let replacement = owners
                .reserve_spawn(target.clone(), "shared", false)
                .unwrap();
            assert!(replacement.authorizing);
            owners.settled(&last);
            owners.complete_ingress(&first, &events[0]);
            assert!(owners.commit(&replacement, || Ok(())).is_ok());
            assert!(owners
                .commit::<()>(&old, || panic!("old owner cannot register"))
                .is_err());
            let mut peer = target;
            peer.member = RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess);
            assert!(owners.reserve(peer, "shared").is_ok());
        }
    }

    #[derive(Default)]
    struct RecordingHost {
        spawned: std::sync::Mutex<Vec<String>>,
        reclaimed: std::sync::Mutex<Vec<String>>,
    }
    impl WarmPoolHost for RecordingHost {
        fn spawn_member(&self, id: String, _params: MemberSpawnParams) {
            self.spawned.lock().unwrap().push(id);
        }
        fn kill_member(&self, _id: &str) {}
        fn reclaim_exited_and_refill(&self, id: String) {
            self.reclaimed.lock().unwrap().push(id);
        }
    }

    type TestSink = AppTerminalSink<tauri::test::MockRuntime>;
    type ExitPause = Arc<
        Mutex<
            Option<(
                std::sync::mpsc::SyncSender<()>,
                std::sync::mpsc::Receiver<()>,
            )>,
        >,
    >;

    struct BeforeExitSink {
        inner: Weak<TestSink>,
        pause: ExitPause,
    }

    impl TerminalEventSink for BeforeExitSink {
        fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            if matches!(event, TerminalDomainEvent::Exit { .. }) {
                let pause = self.pause.lock().unwrap().take();
                if let Some((reached, resume)) = pause {
                    let _ = reached.send(());
                    let _ = resume.recv_timeout(codesense_session_server::testing::WAIT);
                }
            }
            self.inner
                .upgrade()
                .map_or(Ok(()), |sink| TerminalEventSink::emit(&*sink, event))
        }
    }

    struct EffectSink {
        admission: Arc<crate::terminal::session_server::LocalRuntimeAdmission>,
        slot: super::super::machines::RuntimeMemberSlot,
        namespace: OnceLock<EventNamespace>,
        sealed: Arc<
            Mutex<
                Vec<(
                    super::super::machines::RuntimeMemberTarget,
                    TerminalDomainEvent,
                )>,
            >,
        >,
        deferred: Arc<Mutex<Vec<(LocalEffectClaim, TerminalDomainEvent)>>>,
    }

    impl TerminalEventSink for EffectSink {
        fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            if let Some(target) = self
                .namespace
                .get()
                .and_then(|namespace| self.admission.target_for_namespace(self.slot, namespace))
            {
                let ticket = self.admission.effects.capture_event(&target, &event);
                self.sealed.lock().unwrap().push((target, event.clone()));
                if let Some(ticket) = ticket {
                    self.admission.effects.complete_ingress(&ticket, &event);
                    self.deferred.lock().unwrap().push((ticket, event));
                }
            }
            Ok(())
        }
    }

    struct EffectFixture {
        session: String,
        runtime: &'static tokio::runtime::Runtime,
        app: tauri::App<tauri::test::MockRuntime>,
        local: Arc<codesense_pty_core::TerminalHost>,
        sidecar: Arc<codesense_session_server::testing::TestServer>,
        admission: Arc<crate::terminal::session_server::LocalRuntimeAdmission>,
        sealed: Arc<
            Mutex<
                Vec<(
                    super::super::machines::RuntimeMemberTarget,
                    TerminalDomainEvent,
                )>,
            >,
        >,
        deferred: Arc<Mutex<Vec<(LocalEffectClaim, TerminalDomainEvent)>>>,
        launch: DashboardLaunchState,
        asks: AskUserRegistry,
        real_sinks: Arc<Mutex<Vec<Arc<TestSink>>>>,
        pause: ExitPause,
        _serial: Option<std::sync::MutexGuard<'static, ()>>,
    }

    // Register before a handoff can re-key a child, so assertion unwinding
    // cannot strand the new ID in the process-lifetime test server.
    struct ExactTestSession(
        Arc<dyn codesense_pty_core::domain::TerminalDomain>,
        PtySessionId,
    );

    impl Drop for ExactTestSession {
        fn drop(&mut self) {
            if self.0.has_session(&self.1) == Ok(true) {
                let _ = self.0.kill(&self.1);
            }
        }
    }

    impl EffectFixture {
        fn new() -> Self {
            Self::with_session("shared".into())
        }

        fn with_session(session: String) -> Self {
            Self::build(session, false)
        }

        fn actual(session: &str) -> Self {
            Self::build(format!("{session}-{}", rand::random::<u64>()), true)
        }

        fn build(session: String, actual: bool) -> Self {
            use super::super::machines::RuntimeMemberSlot;
            use crate::terminal::session_server::{mint_event_namespace, SessionServerState};
            static SERIAL: Mutex<()> = Mutex::new(());
            static SERVER: OnceLock<Arc<codesense_session_server::testing::TestServer>> =
                OnceLock::new();
            // Tauri's mock app retains managed state through its core plugin
            // AppHandles/PathResolver. Match the application's process-lifetime
            // runtime, not one retained set of reactor descriptors per mock.
            // Apps, admission, claims and sessions remain fixture-local; Drop
            // explicitly drains their work rather than relying on runtime drop.
            static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
            let serial = actual.then(|| SERIAL.lock().unwrap_or_else(|error| error.into_inner()));
            let runtime = RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap());
            let app = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            let state = SessionServerState::default();
            let admission = state.admission.clone();
            let sealed = Arc::new(Mutex::new(Vec::new()));
            let deferred = Arc::new(Mutex::new(Vec::new()));
            let real_sinks = Arc::new(Mutex::new(Vec::new()));
            let pause: ExitPause = Arc::new(Mutex::new(None));
            let real_sink = Arc::new(TestSink::new(app.handle().clone(), StreamHost::InProcess));
            if actual {
                real_sinks.lock().unwrap().push(real_sink.clone());
            }
            let sink = Arc::new(EffectSink {
                admission: admission.clone(),
                slot: RuntimeMemberSlot::InProcess,
                namespace: OnceLock::new(),
                sealed: sealed.clone(),
                deferred: deferred.clone(),
            });
            let local = Arc::new(codesense_pty_core::TerminalHost::new(
                ServerIdentity::ephemeral("effects"),
                if actual {
                    Arc::new(BeforeExitSink {
                        inner: Arc::downgrade(&real_sink),
                        pause: pause.clone(),
                    })
                } else {
                    sink.clone()
                },
                runtime.handle().clone(),
            ));
            let identity = local.identity().unwrap();
            let namespace =
                mint_event_namespace(identity.server.id.as_str(), &identity.epoch).unwrap();
            admission.register_in_process(&namespace).unwrap();
            real_sink.bind_namespace(namespace.clone());
            sink.namespace.set(namespace).unwrap();
            let sink_admission = admission.clone();
            let sink_sealed = sealed.clone();
            let sink_deferred = deferred.clone();
            let actual_sinks = real_sinks.clone();
            let actual_pause = pause.clone();
            let sink_app = app.handle().clone();
            admission.set_sink_factory(Arc::new(move |namespace| {
                if actual {
                    let inner = Arc::new(TestSink::new(
                        sink_app.clone(),
                        StreamHost::LocalSidecar(namespace.clone()),
                    ));
                    inner.bind_namespace(namespace.clone());
                    actual_sinks.lock().unwrap().push(inner.clone());
                    return Arc::new(BeforeExitSink {
                        inner: Arc::downgrade(&inner),
                        pause: actual_pause.clone(),
                    });
                }
                let bound = OnceLock::new();
                bound.set(namespace.clone()).unwrap();
                Arc::new(EffectSink {
                    admission: sink_admission.clone(),
                    slot: RuntimeMemberSlot::Sidecar,
                    namespace: bound,
                    sealed: sink_sealed.clone(),
                    deferred: sink_deferred.clone(),
                })
            }));
            let sidecar = if actual {
                SERVER
                    .get_or_init(
                        || Arc::new(codesense_session_server::testing::TestServer::start()),
                    )
                    .clone()
            } else {
                Arc::new(codesense_session_server::testing::TestServer::start())
            };
            state
                .connect_or_launch(
                    sidecar.root(),
                    Arc::new(codesense_session_server::testing::RecordingSink::default()),
                    |_| Err("no launch".into()),
                )
                .unwrap();
            app.manage(local.clone());
            app.manage(state);
            app.manage(super::super::agent_accounts::AgentAccountRuntimeState::default());
            app.manage(DashboardLaunchState::new(1));
            app.manage(AskUserRegistry::new());
            app.manage(StreamIds::default());
            Self {
                session,
                runtime,
                app,
                local,
                sidecar,
                admission,
                sealed,
                deferred,
                launch: DashboardLaunchState::new(1),
                asks: AskUserRegistry::new(),
                real_sinks,
                pause,
                _serial: serial,
            }
        }

        fn spawn(&self, sidecar: bool) {
            self.spawn_command(sidecar, "sleep 60");
        }

        fn spawn_command(&self, sidecar: bool, command: &str) {
            use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
            use codesense_session_server::testing::{registration, sh_spec};
            let pty = self
                .runtime
                .block_on(LocalPtyRunner.spawn(sh_spec(&self.session, command)))
                .unwrap();
            let pool = self
                .admission
                .effects
                .claims(if sidecar {
                    super::super::machines::RuntimeMemberSlot::Sidecar
                } else {
                    super::super::machines::RuntimeMemberSlot::InProcess
                })
                .iter()
                .any(|claim| claim.session == self.session && claim.pool_member);
            if sidecar {
                self.sidecar.track(&self.session);
                // Deliberately external/discovered: not spawned through the app client ledger.
                self.sidecar
                    .host()
                    .terminal()
                    .register(pty, registration(&self.session, None, pool))
                    .unwrap();
            } else {
                self.local
                    .register(pty, registration(&self.session, None, pool))
                    .unwrap();
            }
        }

        fn reserve(&self, sidecar: bool) -> LocalEffectClaim {
            self.reserve_pool(sidecar, false)
        }

        fn reserve_pool(&self, sidecar: bool, pool: bool) -> LocalEffectClaim {
            use super::super::machines::{
                RuntimeAdmissionState, RuntimeMemberSlot, RuntimeMemberTarget,
            };
            let slot = if sidecar {
                RuntimeMemberSlot::Sidecar
            } else {
                RuntimeMemberSlot::InProcess
            };
            let row = self
                .admission
                .baseline()
                .rows
                .into_iter()
                .find(|row| row.member.slot == slot)
                .unwrap();
            let RuntimeAdmissionState::Live { namespace } = row.state else {
                panic!("live")
            };
            self.admission
                .effects
                .reserve_spawn(
                    RuntimeMemberTarget {
                        member: row.member,
                        namespace,
                        admission_revision: row.revision,
                    },
                    &self.session,
                    pool,
                )
                .unwrap()
        }

        fn register(
            &self,
        ) -> (
            LocalEffectClaim,
            tokio::sync::oneshot::Receiver<crate::mcp::state::AskUserOutcome>,
        ) {
            let claim = super::super::pty_backend::capture_local_effect_claim(
                self.app.handle(),
                &self.session,
            )
            .unwrap();
            assert!(self.launch.begin("story").unwrap());
            let (_, rx) = super::super::pty_backend::commit_local_effect_claim(
                self.app.handle(),
                &claim,
                || {
                    self.launch.bind_owned(
                        "story",
                        &self.session,
                        None,
                        None,
                        Some(claim.clone()),
                    )?;
                    Ok(self.asks.register_owned(&self.session, Some(claim.clone())))
                },
            )
            .unwrap();
            (claim, rx)
        }

        fn kill(&self, sidecar: bool) {
            use codesense_pty_core::domain::TerminalDomain;
            let session = PtySessionId::new(&self.session);
            if sidecar {
                self.sidecar.host().terminal().kill(&session).unwrap();
            } else {
                self.local.kill(&session).unwrap();
            }
        }

        fn wait_removed(&self, count: usize) {
            assert!(codesense_session_server::testing::wait_until(|| self
                .sealed
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, event)| matches!(event, TerminalDomainEvent::SessionRemoved { .. }))
                .count()
                >= count
                && self.deferred.lock().unwrap().iter().any(
                    |(_, event)| matches!(event, TerminalDomainEvent::SessionRemoved { .. })
                )));
        }
    }

    impl Drop for EffectFixture {
        fn drop(&mut self) {
            use codesense_pty_core::domain::TerminalDomain;
            let session = PtySessionId::new(&self.session);
            if self.local.has_session(&session) == Ok(true) {
                let _ = self.local.kill(&session);
            }
            if self.sidecar.host().terminal().has_session(&session) == Ok(true) {
                let _ = self.sidecar.host().terminal().kill(&session);
            }
            let claims: Vec<_> = self
                .admission
                .effects
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .claims
                .values()
                .filter(|record| record.claim.session == self.session && record.claim.authorizing)
                .map(|record| record.claim.clone())
                .collect();
            for claim in claims {
                let _ = self.admission.effects.drained_predecessor(
                    &claim.target,
                    &claim.session,
                    std::time::Instant::now() + crate::terminal::session_server::EXIT_REAP_TIMEOUT,
                );
            }
            if !std::thread::panicking() {
                assert_eq!(self.local.has_session(&session), Ok(false));
                assert_eq!(
                    self.sidecar.host().terminal().has_session(&session),
                    Ok(false)
                );
            }
            let state = self
                .app
                .state::<crate::terminal::session_server::SessionServerState>();
            let client = state.connected();
            state.close_test_connection();
            let sinks = self.real_sinks.lock().unwrap().clone();
            for sink in &sinks {
                sink.delivery.0.lock().unwrap().closed = true;
            }
            let quiescent = codesense_session_server::testing::wait_until(|| {
                client.as_ref().is_none_or(|client| !client.is_alive())
                    && sinks.iter().all(|sink| {
                        let queue = sink.delivery.0.lock().unwrap();
                        !queue.running && queue.pending.is_empty()
                    })
            });
            self.real_sinks.lock().unwrap().clear();
            if !std::thread::panicking() {
                assert!(quiescent, "fixture client and ordered effects must stop");
                assert!(state.connected().is_none());
            }
        }
    }

    #[test]
    fn rm5aa_waited_recovery_rechecks_admission_and_scope_cas() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        for refusal in ["none", "superseded", "scope-replaced"] {
            let fixture = EffectFixture::actual("waited-recovery");
            let old = fixture.reserve(true);
            fixture.spawn(true);
            create_owned_resources(&fixture, &old);
            let launch = fixture.app.state::<DashboardLaunchState>();
            launch.begin("waiting").unwrap();
            launch
                .bind_owned("waiting", &old.session, None, None, Some(old.clone()))
                .unwrap();
            let (reached, paused) = std::sync::mpsc::sync_channel(1);
            let (resume, receive) = std::sync::mpsc::sync_channel(1);
            let sink = fixture.real_sinks.lock().unwrap()[1].clone();
            sink.delivery.0.lock().unwrap().before_delivery = Some((reached, receive));
            // Controlled terminal ingress exercises the actual ordered sink and
            // its pending-settlement gate; the real server remains queryable.
            TerminalEventSink::emit(
                &*sink,
                TerminalDomainEvent::SessionRemoved {
                    session: PtySessionId::new(&old.session),
                    reason: TeardownReason::Reap,
                },
            )
            .unwrap();
            paused
                .recv_timeout(codesense_session_server::testing::WAIT)
                .unwrap();
            let state = fixture.app.state::<SessionServerState>();
            let prior = state.connected().unwrap();
            prior.sever();
            assert!(codesense_session_server::testing::wait_until(
                || !prior.is_alive()
            ));
            let client = state.reconnected(fixture.sidecar.root()).unwrap();
            let app = fixture.app.handle().clone();
            let current = client.clone();
            let worker = std::thread::spawn(move || {
                let state = app.state::<SessionServerState>();
                pty_backend::carry_across(&app, &state, &current);
            });
            assert!(codesense_session_server::testing::wait_until(|| fixture
                .admission
                .effects
                .2
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0));
            if refusal == "superseded" {
                client.sever();
                assert!(codesense_session_server::testing::wait_until(
                    || !client.is_alive()
                ));
                state.reconnected(fixture.sidecar.root()).unwrap();
            } else if refusal == "scope-replaced" {
                assert!(mcp_scope::remove_owned_scope(&old));
                create_owned_resources(&fixture, &old);
            }
            resume.send(()).unwrap();
            worker.join().unwrap();
            let actual = fixture
                .admission
                .effects
                .claims(old.target.member.slot)
                .into_iter()
                .find(|claim| claim.session == old.session)
                .unwrap();
            let accounts = fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>();
            if refusal == "none" {
                assert_ne!(actual, old);
                assert!(launch.release_owned(&old).is_none());
                assert!(!mcp_scope::remove_owned_scope(&old));
                assert!(accounts.owned_provenance(&old).is_none());
                assert!(mcp_scope::owned_creation(&actual).unwrap().is_some());
            } else {
                assert_eq!(actual, old, "refusal moves neither claim nor records");
                assert!(mcp_scope::owned_creation(&old).unwrap().is_some());
                assert_eq!(accounts.owned_provenance(&old), Some(resource_provenance()));
                assert!(launch.release_owned(&old).is_some());
                // Explicit fixture cleanup, not synthetic admission/drain proof.
                assert!(mcp_scope::remove_owned_scope(&old));
                accounts.take_owned_provenance(&old);
            }
            fixture.kill(true);
        }
    }

    #[test]
    fn rm5aa_acquired_pool_member_refusal_cleans_exact_source_and_refills() {
        use super::super::pty_backend;
        use codesense_pty_core::domain::TerminalDomain;
        let mut fixture = EffectFixture::actual("pool-drain");
        let host = Arc::new(RecordingHost::default());
        let pool = WarmPoolState::default();
        pool.init(host.clone());
        pool.reconfigure(1, None, None);
        fixture.session = host.spawned.lock().unwrap()[0].clone();
        let source = fixture.reserve_pool(false, true);
        fixture.spawn(false);
        pool.mark_ready(&source.session);
        let target_id = format!("{}-target", source.session);
        let domain: Arc<dyn TerminalDomain> = fixture.local.clone();
        let _target_cleanup = ExactTestSession(domain.clone(), PtySessionId::new(&target_id));
        fixture.session = target_id.clone();
        let predecessor = fixture.reserve(false);
        fixture.spawn_command(false, "true");
        fixture.session = source.session.clone();
        assert!(codesense_session_server::testing::wait_until(|| {
            let owners = fixture.admission.effects.0.lock().unwrap();
            owners.claims.values().any(|record| {
                record.claim == predecessor && record.exit_complete && !record.removed_complete
            })
        }));
        let member = pool.acquire_ready().unwrap();
        assert_eq!(member, source.session);
        let exact =
            pty_backend::resolve_member_backend(fixture.app.handle(), &source.target, false)
                .unwrap();
        assert!(pty_backend::prepare_popped_warm(
            fixture.app.handle(),
            &domain,
            &member,
            || pty_backend::reserve_spawn_effect_claim(
                fixture.app.handle(),
                &exact,
                &target_id,
                false
            ),
            || pool.refill(),
        )
        .is_err());
        assert_eq!(
            domain.has_session(&PtySessionId::new(&source.session)),
            Ok(false)
        );
        assert_eq!(
            domain.has_session(&PtySessionId::new(&target_id)),
            Ok(true),
            "ended predecessor retained; no handoff or cold spawn"
        );
        assert_eq!(
            host.spawned.lock().unwrap().len(),
            2,
            "one replacement refill"
        );
        assert!(fixture
            .admission
            .effects
            .reserve_spawn(predecessor.target.clone(), &target_id, false)
            .is_err());
    }

    #[test]
    fn rm5aa_fallback_scope_cas_failure_releases_only_never_started_target() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{mcp_scope, pty};
        let fixture = EffectFixture::actual("fallback-cas");
        let old = fixture.reserve(true);
        let original = create_owned_resources(&fixture, &old);
        assert!(mcp_scope::remove_owned_scope(&old));
        let replacement = create_owned_resources(&fixture, &old);
        assert_ne!(original, replacement);
        let fallback = fixture.reserve(false);
        assert!(
            pty::transfer_fallback_scope(fixture.app.handle(), &original, &old, &fallback).is_err()
        );
        assert_eq!(mcp_scope::owned_creation(&old).unwrap(), Some(replacement));
        assert!(fixture
            .admission
            .effects
            .reserve_spawn(old.target.clone(), &old.session, false)
            .is_err());
        let retry = fixture
            .admission
            .effects
            .reserve_spawn(fallback.target.clone(), &fallback.session, false)
            .unwrap();
        fixture.admission.effects.never_started(&retry);
        assert!(mcp_scope::remove_owned_scope(&old));
    }

    #[test]
    fn rm5aa_popped_warm_preflight_cleans_only_exact_owner_and_refills() {
        use super::super::pty_backend;
        use codesense_pty_core::domain::TerminalDomain;
        for refusal in ["draining", "not-pool", "superseded"] {
            let fixture = EffectFixture::actual("warm-refusal");
            let source = fixture.reserve_pool(false, refusal != "not-pool");
            fixture.spawn(false);
            let domain: Arc<dyn TerminalDomain> = fixture.local.clone();
            let refills = std::cell::Cell::new(0);
            let reserve_called = std::cell::Cell::new(false);
            if refusal == "superseded" {
                fixture.admission.effects.never_started(&source);
                let replacement = fixture
                    .admission
                    .effects
                    .reserve_spawn(source.target.clone(), &source.session, true)
                    .unwrap();
                assert!(
                    pty_backend::cleanup_popped_warm(fixture.app.handle(), &domain, &source)
                        .is_err()
                );
                assert_eq!(
                    fixture
                        .local
                        .has_session(&PtySessionId::new(&source.session)),
                    Ok(true)
                );
                fixture.admission.effects.retire_claim(&replacement);
            }
            let result = pty_backend::prepare_popped_warm(
                fixture.app.handle(),
                &domain,
                &source.session,
                || {
                    reserve_called.set(true);
                    Err("target is draining".into())
                },
                || refills.set(refills.get() + 1),
            );
            assert!(result.is_err());
            assert_eq!(refills.get(), 1);
            assert_eq!(reserve_called.get(), refusal == "draining");
            assert_eq!(
                fixture
                    .local
                    .has_session(&PtySessionId::new(&source.session)),
                Ok(refusal == "superseded")
            );
            fixture.kill(false);
        }
    }

    #[test]
    fn rm5aa_real_pool_handoff_preserves_target_and_unknown_reply_guards() {
        use super::super::pty_backend;
        use codesense_pty_core::domain::TerminalDomain;
        for sidecar in [false, true] {
            for lost_reply in [false, true] {
                let fixture = EffectFixture::actual("pool-handoff");
                let source = fixture.reserve_pool(sidecar, true);
                fixture.spawn(sidecar);
                let domain: Arc<dyn TerminalDomain> = if sidecar {
                    fixture
                        .app
                        .state::<crate::terminal::session_server::SessionServerState>()
                        .connected()
                        .unwrap()
                } else {
                    fixture.local.clone()
                };
                let target_id = format!("{}-attached", source.session);
                let _target_cleanup =
                    ExactTestSession(domain.clone(), PtySessionId::new(&target_id));
                let refills = std::cell::Cell::new(0);
                let (captured, target) = pty_backend::prepare_popped_warm(
                    fixture.app.handle(),
                    &domain,
                    &source.session,
                    || {
                        fixture.admission.effects.reserve_spawn(
                            source.target.clone(),
                            &target_id,
                            false,
                        )
                    },
                    || refills.set(refills.get() + 1),
                )
                .unwrap();
                assert_eq!(captured, source);
                assert_eq!(refills.get(), 0);
                let outcome = domain.handoff_warm_member(
                    &PtySessionId::new(&source.session),
                    &PtySessionId::new(&target_id),
                    "/tmp",
                    80,
                    24,
                    SessionMeta::default(),
                );
                assert!(outcome.is_ok(), "real core/sidecar handoff");
                // Losing the success reply is indistinguishable at the caller
                // from a re-key that did not happen; neither authorizes retry.
                let observed = if lost_reply {
                    Err(PortUnavailable("lost handoff reply".into()))
                } else {
                    outcome
                };
                fixture
                    .admission
                    .effects
                    .finish_warm_handoff(&source, &target, &observed);
                assert_eq!(
                    domain.has_session(&PtySessionId::new(&source.session)),
                    Ok(false)
                );
                assert_eq!(domain.has_session(&PtySessionId::new(&target_id)), Ok(true));
                assert!(fixture
                    .admission
                    .effects
                    .commit(&source, || Ok(()))
                    .is_err());
                assert_eq!(
                    fixture
                        .admission
                        .effects
                        .commit(&target, || Ok(()))
                        .is_err(),
                    lost_reply
                );
                assert!(!target.pool_member);
                assert!(fixture
                    .admission
                    .effects
                    .reserve_spawn(source.target.clone(), &source.session, true)
                    .is_err());
                assert!(fixture
                    .admission
                    .effects
                    .reserve_spawn(target.target.clone(), &target_id, false)
                    .is_err());
                domain.kill(&PtySessionId::new(&target_id)).unwrap();
                fixture
                    .admission
                    .effects
                    .drained_predecessor(
                        &target.target,
                        &target_id,
                        std::time::Instant::now() + codesense_session_server::testing::WAIT,
                    )
                    .unwrap();
            }
        }
    }

    /// Set in a re-executed test binary that runs exactly one test alone.
    const ISOLATED_TEST_CHILD: &str = "CODESENSE_ISOLATED_TEST_CHILD";

    /// Runs `test` in a fresh process of this test binary unless already in
    /// one. Returns true when the caller is that child and should run the
    /// body; false when the child already ran it (and passed).
    fn run_isolated(test: &str) -> bool {
        if std::env::var_os(ISOLATED_TEST_CHILD).is_some() {
            return true;
        }
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, path)| path);
        // The child's session ids come from its own fresh counters, so they
        // repeat ids this process (or another test process) uses. A private
        // temp dir keeps its per-session scope dirs from colliding with them.
        let tmp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                &format!("{module}::{test}"),
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ISOLATED_TEST_CHILD, "1")
            .env("TMPDIR", tmp.path())
            .output()
            .expect("spawn isolated test child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated {test} failed\nstdout: {stdout}\nstderr: {stderr}"
        );
        false
    }

    #[test]
    fn rm5aa_same_epoch_recovery_moves_exact_existing_records() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // The descriptor-leak check counts `/dev/fd`, which is process-wide:
        // every test running alongside opens and closes descriptors and moves
        // the count. Run alone in its own process so the count is this
        // fixture's.
        if !run_isolated("rm5aa_same_epoch_recovery_moves_exact_existing_records") {
            return;
        }
        use super::super::{agent_accounts, mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::TerminalDomain;
        for (iteration, natural) in [true, false, true].into_iter().enumerate() {
            let before = std::fs::read_dir("/dev/fd").unwrap().count();
            {
                let mut fixture = EffectFixture::actual("recover-owned");
                let pool_host = Arc::new(RecordingHost::default());
                let pool = WarmPoolState::default();
                pool.init(pool_host.clone());
                pool.reconfigure(1, None, None);
                fixture.session = pool_host.spawned.lock().unwrap()[0].clone();
                fixture.app.manage(pool);
                let old = fixture.reserve_pool(true, true);
                fixture.spawn_command(true, "read -r rm5aa_exit");
                create_owned_resources(&fixture, &old);
                let launch = fixture.app.state::<DashboardLaunchState>();
                let asks = fixture.app.state::<AskUserRegistry>();
                launch.begin("recover").unwrap();
                launch
                    .bind_owned("recover", &old.session, None, None, Some(old.clone()))
                    .unwrap();
                let (_, mut answer) = asks.register_owned(&old.session, Some(old.clone()));
                let state = fixture.app.state::<SessionServerState>();
                let prior = state.connected().unwrap();
                prior.sever();
                assert!(codesense_session_server::testing::wait_until(
                    || !prior.is_alive()
                ));
                let client = state.reconnected(fixture.sidecar.root()).unwrap();
                pty_backend::carry_across(fixture.app.handle(), &state, &client);
                let next = fixture
                    .admission
                    .effects
                    .claims(super::super::machines::RuntimeMemberSlot::Sidecar)
                    .into_iter()
                    .find(|claim| claim.session == old.session)
                    .unwrap();
                assert_ne!(old, next);
                assert_eq!(old.target.namespace.epoch, next.target.namespace.epoch);
                assert_eq!(next.target.namespace, client.namespace().into());
                assert_eq!(old.pool_member, next.pool_member);
                assert!(fixture.admission.effects.commit(&old, || Ok(())).is_err());
                assert!(launch.release_owned(&old).is_none());
                assert_eq!(asks.cancel_owned(&old), 0);
                assert!(!mcp_scope::remove_owned_scope(&old));
                let accounts = fixture
                    .app
                    .state::<agent_accounts::AgentAccountRuntimeState>();
                assert!(accounts.owned_provenance(&old).is_none());
                assert_eq!(
                    accounts.owned_provenance(&next),
                    Some(resource_provenance())
                );
                assert!(mcp_scope::owned_creation(&next).unwrap().is_some());
                if natural {
                    client
                        .write(&PtySessionId::new(&old.session), b"done\n")
                        .unwrap();
                    assert!(codesense_session_server::testing::wait_until(|| pool_host
                        .reclaimed
                        .lock()
                        .unwrap()
                        .as_slice()
                        == [old.session.as_str()]));
                }
                fixture.kill(true);
                assert!(codesense_session_server::testing::wait_until(|| launch
                    .resolve("recover")
                    .is_none()));
                assert_eq!(
                    answer.try_recv().unwrap().action,
                    crate::mcp::state::AskUserAction::Cancel
                );
                assert!(codesense_session_server::testing::wait_until(|| {
                    !mcp_scope::mcp_scope_session_dir(&old.session).exists()
                }));
                assert!(codesense_session_server::testing::wait_until(|| accounts
                    .owned_provenance(&next)
                    .is_none()));
                fixture
                    .admission
                    .effects
                    .drained_predecessor(
                        &next.target,
                        &next.session,
                        std::time::Instant::now() + codesense_session_server::testing::WAIT,
                    )
                    .unwrap();
                assert_eq!(
                    pool_host.reclaimed.lock().unwrap().len(),
                    usize::from(natural),
                    "deliberate kill must not request refill"
                );
            }
            // First iteration initializes shared sidecar/runtime/global async
            // resources. Every subsequent fixture must release its own FDs.
            if iteration > 0 {
                assert!(
                    codesense_session_server::testing::wait_until(|| {
                        std::fs::read_dir("/dev/fd").unwrap().count() <= before
                    }),
                    "fixture retained new descriptors after shared initialization"
                );
            }
            eprintln!(
                "recovery fixture {iteration}: descriptors before={before}, after={}",
                std::fs::read_dir("/dev/fd").unwrap().count()
            );
        }
    }

    #[test]
    fn rm5aa_missing_recovery_settles_old_owner_without_inventing_drain() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        let fixture = EffectFixture::actual("missing-owned");
        let old = fixture.reserve(true);
        create_owned_resources(&fixture, &old);
        let asks = fixture.app.state::<AskUserRegistry>();
        let (_, mut answer) = asks.register_owned(&old.session, Some(old.clone()));
        let state = fixture.app.state::<SessionServerState>();
        let prior = state.connected().unwrap();
        prior.sever();
        assert!(codesense_session_server::testing::wait_until(
            || !prior.is_alive()
        ));
        let client = state.reconnected(fixture.sidecar.root()).unwrap();
        pty_backend::carry_across(fixture.app.handle(), &state, &client);
        assert_eq!(
            answer.try_recv().unwrap().action,
            crate::mcp::state::AskUserAction::Cancel
        );
        assert!(!mcp_scope::mcp_scope_session_dir(&old.session).exists());
        assert!(fixture.admission.effects.commit(&old, || Ok(())).is_err());
        assert!(fixture
            .admission
            .effects
            .reserve_spawn(old.target.clone(), &old.session, false)
            .is_err());
        let mut other_epoch = old.target.clone();
        other_epoch.namespace.epoch.push_str("-replacement");
        assert!(fixture
            .admission
            .effects
            .transfer(&fixture.admission, &old, other_epoch, |_| panic!(
                "must not move records"
            ))
            .is_err());
    }

    #[test]
    fn rm5aa_observed_only_terminal_ingress_never_authorizes_effects() {
        let fixture = EffectFixture::actual("observed-only");
        let sealed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = sealed.clone();
        fixture.app.listen("terminal-runtime-event", move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        fixture.spawn(false);
        fixture.kill(false);
        assert!(codesense_session_server::testing::wait_until(|| sealed
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2));
        let owners = &fixture.admission.effects;
        let observed = owners
            .0
            .lock()
            .unwrap()
            .claims
            .values()
            .find(|record| record.claim.session == fixture.session)
            .unwrap()
            .claim
            .clone();
        owners
            .drained_predecessor(
                &observed.target,
                &observed.session,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
            )
            .unwrap();
        assert!(!observed.authorizing);
        assert!(!observed.pool_member);
        assert!(owners.claims(observed.target.member.slot).is_empty());
        assert!(!owners.unambiguous(&observed));
        assert!(owners.legacy_permit(&observed).is_none());
        assert!(owners
            .commit::<()>(&observed, || panic!("must not commit records"))
            .is_err());
        assert!(owners
            .reserve(observed.target.clone(), &observed.session)
            .is_err());
        let replacement = owners
            .reserve_spawn(observed.target.clone(), &observed.session, false)
            .unwrap();
        assert!(replacement.authorizing);
        owners.never_started(&replacement);
    }

    #[test]
    fn rm5aa_real_replacement_exit_refuses_held_discovery_without_settling_old_owner() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{
            agent_accounts, machines::RuntimeSessionReference, mcp_scope, pty_backend,
        };
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::TerminalDomain;
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
        use codesense_session_server::testing::{registration, sh_spec, wait_until, TestServer};

        for natural in [false, true] {
            let fixture = EffectFixture::actual("replacement-discovery");
            let old = fixture.reserve(true);
            fixture.spawn(true);
            create_owned_resources(&fixture, &old);
            let asks = fixture.app.state::<AskUserRegistry>();
            let (_, mut answer) = asks.register_owned(&old.session, Some(old.clone()));
            let state = fixture.app.state::<SessionServerState>();
            let prior = state.connected().unwrap();
            prior.sever();
            assert!(wait_until(|| !prior.is_alive()));
            let replacement = TestServer::start();
            let client = state.reconnected(replacement.root()).unwrap();
            assert_ne!(client.epoch(), prior.epoch());
            let session = PtySessionId::new(&fixture.session);
            replacement.track(&fixture.session);
            let child = fixture
                .runtime
                .block_on(LocalPtyRunner.spawn(sh_spec(&fixture.session, "read line; exit 0")))
                .unwrap();
            replacement
                .host()
                .terminal()
                .register(child, registration(&fixture.session, None, false))
                .unwrap();
            let held = pty_backend::resolve_session_reference(
                fixture.app.handle(),
                &RuntimeSessionReference::Legacy(fixture.session.clone()),
                false,
            )
            .unwrap();
            let target = held.admission_target().unwrap().clone();
            if natural {
                replacement
                    .host()
                    .terminal()
                    .write(&session, b"done\n")
                    .unwrap();
                assert!(wait_until(|| fixture
                    .admission
                    .effects
                    .0
                    .lock()
                    .unwrap()
                    .terminal
                    .iter()
                    .any(
                        |record| record.claim.target == target && record.exit_complete
                    )));
            }
            replacement.host().terminal().kill(&session).unwrap();
            assert!(wait_until(|| fixture
                .admission
                .effects
                .0
                .lock()
                .unwrap()
                .terminal
                .iter()
                .any(|record| record.claim.target == target
                    && record.exit_complete
                    && record.removed_complete
                    && record.pending_settlements == 0)));
            assert!(pty_backend::capture_resolved_local_effect_claim(
                fixture.app.handle(),
                &fixture.session,
                held,
            )
            .is_err());
            // The new owner's unregistered terminal events must not consume A's records.
            assert!(answer.try_recv().is_err());
            assert!(mcp_scope::owned_creation(&old).unwrap().is_some());
            let accounts = fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>();
            assert_eq!(accounts.owned_provenance(&old), Some(resource_provenance()));
            assert_eq!(
                fixture.admission.effects.claims(old.target.member.slot),
                vec![old.clone()]
            );
            // Existing real recovery establishes A's different lifetime and settles A only.
            pty_backend::carry_across(fixture.app.handle(), &state, &client);
            assert_eq!(
                answer.try_recv().unwrap().action,
                crate::mcp::state::AskUserAction::Cancel
            );
            assert!(mcp_scope::owned_creation(&old).unwrap().is_none());
            assert_eq!(
                accounts.owned_provenance(&old),
                Some(resource_provenance()),
                "existing Expire semantics retain provenance; a new epoch must not inherit it"
            );
            let next = fixture
                .admission
                .effects
                .reserve_spawn(target, &fixture.session, false)
                .unwrap();
            fixture.admission.effects.never_started(&next);
        }
    }

    #[test]
    fn rm5aa_real_recovery_terminal_race_settles_original_records_after_refused_transfer() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, machines::RuntimeMemberSlot, mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::TerminalDomain;
        use codesense_session_server::testing::wait_until;
        let fixture = EffectFixture::actual("recovery-terminal-race");
        let old = fixture.reserve(true);
        fixture.spawn(true);
        create_owned_resources(&fixture, &old);
        let asks = fixture.app.state::<AskUserRegistry>();
        let (_, mut answer) = asks.register_owned(&old.session, Some(old.clone()));
        let state = fixture.app.state::<SessionServerState>();
        let prior = state.connected().unwrap();
        prior.sever();
        assert!(wait_until(|| !prior.is_alive()));
        let client = state.reconnected(fixture.sidecar.root()).unwrap();
        assert_eq!(client.epoch(), prior.epoch());
        let target = fixture
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
            .unwrap();
        assert_ne!(target, old.target);
        let session = PtySessionId::new(&fixture.session);
        // Hold the actual positive observations used by the production recovery loop.
        let presence = client.has_session(&session);
        let was_live = client
            .alive_session_ids(std::slice::from_ref(&session))
            .unwrap()
            .contains(&session);
        assert_eq!(presence, Ok(true));
        assert!(was_live);
        fixture.kill(true);
        assert!(wait_until(|| fixture
            .admission
            .effects
            .0
            .lock()
            .unwrap()
            .terminal
            .iter()
            .any(|record| record.claim.target == target
                && record.exit_complete
                && record.removed_complete
                && record.pending_settlements == 0)));
        assert!(fixture
            .admission
            .effects
            .transfer(&fixture.admission, &old, target.clone(), |_| panic!(
                "terminal evidence must refuse transfer"
            ))
            .is_err());
        assert!(answer.try_recv().is_err());
        assert!(mcp_scope::owned_creation(&old).unwrap().is_some());
        pty_backend::recover_observed_local_effect(
            fixture.app.handle(),
            &state,
            &client,
            &target,
            old.clone(),
            presence,
            was_live,
        );
        assert_eq!(
            answer.try_recv().unwrap().action,
            crate::mcp::state::AskUserAction::Cancel
        );
        assert!(mcp_scope::owned_creation(&old).unwrap().is_none());
        assert!(fixture
            .app
            .state::<agent_accounts::AgentAccountRuntimeState>()
            .owned_provenance(&old)
            .is_none());
        {
            let owners = fixture.admission.effects.0.lock().unwrap();
            let retained = &owners.claims[&(RuntimeMemberSlot::Sidecar, fixture.session.clone())];
            assert_eq!(retained.claim, old);
            assert!(retained.retired);
            assert!(
                !retained.exit_complete && !retained.removed_complete,
                "lookup must not invent old ingress"
            );
        }
        let next = fixture
            .admission
            .effects
            .reserve_spawn(target, &fixture.session, false)
            .unwrap();
        fixture.admission.effects.never_started(&next);
    }

    #[test]
    fn rm5aa_reconnected_terminal_evidence_allows_actual_respawn_and_owner_transfer() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, machines::RuntimeMemberSlot, mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::{ClientBootId, TerminalDomain, WorkspaceKey};
        use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};
        use codesense_session_server::testing::{registration, sh, wait_until};
        for reconnect_again in [false, true] {
            let fixture = EffectFixture::actual("terminal-recovery-respawn");
            let old = fixture.reserve(true);
            let state = fixture.app.state::<SessionServerState>();
            let prior = state.connected().unwrap();
            let viewer = super::super::pty_layout::app_viewer();
            prior
                .layout_snapshot(&viewer, &ClientBootId::new("terminal-recovery"))
                .unwrap();
            fixture.sidecar.track(&fixture.session);
            prior
                .open_tab(
                    OpenTab {
                        viewer,
                        workspace: WorkspaceKey::new("root:acme/app"),
                        session: PtySessionId::new(&fixture.session),
                        descriptor: Some(WorkspaceDescriptor {
                            display_name: "app".into(),
                            kind: "root".into(),
                            ..Default::default()
                        }),
                        name: None,
                        role: None,
                        tags: Default::default(),
                        placement: Placement::Tab,
                        index: None,
                        activate: true,
                    },
                    Box::new(|| {
                        prior
                            .spawn(
                                sh("read line; exit 0"),
                                registration(&fixture.session, None, false),
                            )
                            .map(|_| ())
                            .map_err(|failure| failure.message)
                    }),
                )
                .unwrap();
            create_owned_resources(&fixture, &old);
            let launch = fixture.app.state::<DashboardLaunchState>();
            launch.begin("respawn-owner").unwrap();
            launch
                .bind_owned(
                    "respawn-owner",
                    &fixture.session,
                    Some("test"),
                    None,
                    Some(old.clone()),
                )
                .unwrap();
            prior.sever();
            assert!(wait_until(|| !prior.is_alive()));
            let mut client = state.reconnected(fixture.sidecar.root()).unwrap();
            let mut target = fixture
                .admission
                .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
                .unwrap();
            let session = PtySessionId::new(&fixture.session);
            let paused = if reconnect_again {
                let (reached, receive_reached) = std::sync::mpsc::sync_channel(1);
                let (resume, receive_resume) = std::sync::mpsc::sync_channel(1);
                fixture
                    .real_sinks
                    .lock()
                    .unwrap()
                    .last()
                    .unwrap()
                    .delivery
                    .0
                    .lock()
                    .unwrap()
                    .before_delivery = Some((reached, receive_resume));
                Some((receive_reached, resume))
            } else {
                None
            };
            client.write(&session, b"done\n").unwrap();
            assert!(wait_until(|| fixture
                .admission
                .effects
                .0
                .lock()
                .unwrap()
                .terminal
                .iter()
                .any(|record| record.claim.target == target
                    && record.exit_complete
                    && !record.removed_complete)));
            pty_backend::carry_across(fixture.app.handle(), &state, &client);
            if reconnect_again {
                let (reached, _) = paused.as_ref().unwrap();
                reached
                    .recv_timeout(codesense_session_server::testing::WAIT)
                    .unwrap();
                let observed = target.clone();
                client.sever();
                assert!(wait_until(|| !client.is_alive()));
                client = state.reconnected(fixture.sidecar.root()).unwrap();
                target = fixture
                    .admission
                    .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
                    .unwrap();
                assert_ne!(target, observed);
                pty_backend::carry_across(fixture.app.handle(), &state, &client);
                assert!(fixture
                    .admission
                    .effects
                    .0
                    .lock()
                    .unwrap()
                    .terminal
                    .iter()
                    .any(|record| record.claim.target == observed && record.exit_complete));
                assert!(
                    fixture
                        .admission
                        .effects
                        .drained_spawn_predecessor(
                            &target,
                            &fixture.session,
                            std::time::Instant::now()
                        )
                        .is_err(),
                    "pending B settlement cannot be erased by C admission"
                );
                paused.as_ref().unwrap().1.send(()).unwrap();
            }
            let backend =
                pty_backend::resolve_member_backend(fixture.app.handle(), &target, false).unwrap();
            let mut spawned = None;
            let entry = pty_backend::respawn_tab_admitted(
                fixture.app.handle(),
                backend,
                &session,
                |backend| {
                    assert!(
                        !mcp_scope::mcp_scope_session_dir(&fixture.session).exists(),
                        "the true predecessor's scope must be removed before spawn"
                    );
                    let next = pty_backend::reserve_spawn_effect_claim(
                        fixture.app.handle(),
                        &backend,
                        &fixture.session,
                        false,
                    )?;
                    assert_eq!(
                        launch.resolve_owned_pty("respawn-owner"),
                        Some(next.clone())
                    );
                    assert_eq!(
                        fixture
                            .app
                            .state::<agent_accounts::AgentAccountRuntimeState>()
                            .owned_provenance(&next),
                        Some(resource_provenance())
                    );
                    backend
                        .client()
                        .unwrap()
                        .spawn(sh("sleep 60"), registration(&fixture.session, None, false))
                        .map_err(|failure| failure.message)?;
                    spawned = Some(next);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(entry.session, session);
            let next = spawned.expect("actual production respawn callback must execute once");
            assert!(fixture.admission.effects.commit(&next, || Ok(())).is_ok());
            assert!(client
                .alive_session_ids(std::slice::from_ref(&session))
                .unwrap()
                .contains(&session));
            fixture.kill(true);
        }
    }

    #[test]
    fn rm5aa_exit_association_preserves_owner_until_actual_later_removed() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, machines::RuntimeMemberSlot, mcp_scope, pty_backend};
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::TerminalDomain;
        use codesense_session_server::testing::wait_until;
        let fixture = EffectFixture::actual("terminal-later-remove");
        let old = fixture.reserve(true);
        fixture.spawn_command(true, "read line; exit 0");
        create_owned_resources(&fixture, &old);
        let launch = fixture.app.state::<DashboardLaunchState>();
        launch.begin("later-remove").unwrap();
        launch
            .bind_owned("later-remove", &old.session, None, None, Some(old.clone()))
            .unwrap();
        let asks = fixture.app.state::<AskUserRegistry>();
        let (_, mut answer) = asks.register_owned(&old.session, Some(old.clone()));
        let state = fixture.app.state::<SessionServerState>();
        let prior = state.connected().unwrap();
        prior.sever();
        assert!(wait_until(|| !prior.is_alive()));
        let client = state.reconnected(fixture.sidecar.root()).unwrap();
        let target = fixture
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
            .unwrap();
        let session = PtySessionId::new(&old.session);
        client.write(&session, b"done\n").unwrap();
        assert!(wait_until(|| fixture
            .admission
            .effects
            .0
            .lock()
            .unwrap()
            .terminal
            .iter()
            .any(|record| record.claim.target == target
                && record.exit_complete
                && record.pending_settlements == 0)));
        pty_backend::carry_across(fixture.app.handle(), &state, &client);
        assert_eq!(
            answer.try_recv().unwrap().action,
            crate::mcp::state::AskUserAction::Cancel
        );
        assert!(mcp_scope::owned_creation(&old).unwrap().is_some());
        assert_eq!(launch.resolve_owned_pty("later-remove"), Some(old.clone()));
        assert_eq!(
            fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>()
                .owned_provenance(&old),
            Some(resource_provenance())
        );
        client.kill(&session).unwrap();
        assert!(wait_until(
            || mcp_scope::owned_creation(&old).ok() == Some(None)
        ));
        assert!(wait_until(|| launch
            .resolve_owned_pty("later-remove")
            .is_none()));
        assert!(wait_until(|| fixture
            .app
            .state::<agent_accounts::AgentAccountRuntimeState>()
            .owned_provenance(&old)
            .is_none()));
        let proof = fixture
            .admission
            .effects
            .drained_spawn_predecessor(
                &target,
                &old.session,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
            )
            .unwrap();
        assert_eq!(proof.owner, old);
        // Teardown reservation only; the separate production-respawn oracle starts a real process.
        let (cleanup, _) = fixture
            .admission
            .effects
            .reserve_spawn_with_predecessor(
                target,
                &old.session,
                false,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
                Some(&proof),
            )
            .unwrap();
        fixture.admission.effects.never_started(&cleanup);
    }

    #[test]
    fn rm5aa_actual_association_cleanup_holds_reuse_and_retains_identity_failure() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, machines::RuntimeMemberSlot, mcp_scope};
        use crate::terminal::session_server::SessionServerState;
        use codesense_session_server::testing::wait_until;
        for identity_failure in [false, true] {
            let fixture = EffectFixture::actual("terminal-cleanup-held");
            let old = fixture.reserve(true);
            fixture.spawn(true);
            create_owned_resources(&fixture, &old);
            let state = fixture.app.state::<SessionServerState>();
            let prior = state.connected().unwrap();
            prior.sever();
            assert!(wait_until(|| !prior.is_alive()));
            let client = state.reconnected(fixture.sidecar.root()).unwrap();
            let target = fixture
                .admission
                .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
                .unwrap();
            fixture.kill(true);
            assert!(wait_until(|| fixture
                .admission
                .effects
                .0
                .lock()
                .unwrap()
                .terminal
                .iter()
                .any(|record| record.claim.target == target
                    && record.exit_complete
                    && record.removed_complete
                    && record.pending_settlements == 0)));
            let cleanup = fixture
                .admission
                .effects
                .associate_terminal(&fixture.admission, &old, &target)
                .unwrap()
                .unwrap();
            assert!(fixture
                .admission
                .effects
                .reserve_spawn(target.clone(), &old.session, false)
                .is_err());
            assert_eq!(
                fixture
                    .admission
                    .effects
                    .0
                    .lock()
                    .unwrap()
                    .record(&old)
                    .unwrap()
                    .pending_settlements,
                1
            );
            let path = mcp_scope::mcp_scope_session_dir(&old.session);
            let backup = path.with_extension(format!("fixture-{}", rand::random::<u64>()));
            if identity_failure {
                std::fs::rename(&path, &backup).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
            let result = if identity_failure {
                settle_terminal_cleanup(fixture.app.handle(), cleanup)
            } else {
                // The ordered sink may still deliver Removed after association
                // copied its completed receipt. Exercise both production cleanup
                // paths concurrently, retaining two exact-owner reservations.
                let (ticket, event) = {
                    let core = fixture.admission.effects.0.lock().unwrap();
                    let record = core
                        .terminal
                        .iter()
                        .find(|record| record.claim.target == target)
                        .unwrap();
                    (
                        record.claim.clone(),
                        record
                            .terminal_outcomes
                            .iter()
                            .flatten()
                            .find(|event| {
                                matches!(event, TerminalDomainEvent::SessionRemoved { .. })
                            })
                            .unwrap()
                            .clone(),
                    )
                };
                let duplicate = fixture
                    .admission
                    .effects
                    .associated_cleanup(&ticket, &event)
                    .unwrap();
                assert_eq!(
                    fixture
                        .admission
                        .effects
                        .0
                        .lock()
                        .unwrap()
                        .record(&old)
                        .unwrap()
                        .pending_settlements,
                    2
                );
                assert!(fixture
                    .admission
                    .effects
                    .reserve_spawn(target.clone(), &old.session, false)
                    .is_err());
                let barrier = std::sync::Barrier::new(3);
                let app = fixture.app.handle();
                std::thread::scope(|scope| {
                    let first = scope.spawn(|| {
                        barrier.wait();
                        settle_terminal_cleanup(app, cleanup)
                    });
                    let second = scope.spawn(|| {
                        barrier.wait();
                        settle_terminal_cleanup(app, duplicate)
                    });
                    barrier.wait();
                    first.join().unwrap().unwrap();
                    second.join().unwrap()
                })
            };
            assert_eq!(result.is_err(), identity_failure);
            {
                let core = fixture.admission.effects.0.lock().unwrap();
                let record = core.record(&old).unwrap();
                assert_eq!(record.pending_settlements, 0);
                assert_eq!(record.cleanup_failed, identity_failure);
            }
            let proof = fixture.admission.effects.drained_spawn_predecessor(
                &target,
                &old.session,
                std::time::Instant::now(),
            );
            assert_eq!(proof.is_err(), identity_failure);
            if identity_failure {
                assert!(
                    path.is_dir() && backup.is_dir(),
                    "unknown identity must not be removed"
                );
                assert_eq!(
                    fixture
                        .app
                        .state::<agent_accounts::AgentAccountRuntimeState>()
                        .owned_provenance(&old),
                    Some(resource_provenance())
                );
                std::fs::remove_dir(&path).unwrap();
                std::fs::rename(&backup, &path).unwrap();
                assert!(mcp_scope::remove_owned_scope(&old));
                fixture
                    .app
                    .state::<agent_accounts::AgentAccountRuntimeState>()
                    .take_owned_provenance(&old);
                // Fixture disposal after all failure assertions, not a production recovery.
                fixture
                    .admission
                    .effects
                    .0
                    .lock()
                    .unwrap()
                    .claims
                    .remove(&(old.target.member.slot, old.session.clone()));
            } else {
                let proof = proof.unwrap();
                let (claim, _) = fixture
                    .admission
                    .effects
                    .reserve_spawn_with_predecessor(
                        target,
                        &old.session,
                        false,
                        std::time::Instant::now(),
                        Some(&proof),
                    )
                    .unwrap();
                fixture.admission.effects.never_started(&claim);
            }
        }
    }

    #[test]
    fn rm5aa_ownerless_terminal_predecessor_preserves_actual_source_across_reconnect() {
        use super::super::machines::RuntimeMemberSlot;
        use crate::terminal::session_server::SessionServerState;
        use codesense_session_server::testing::wait_until;
        let fixture = EffectFixture::actual("ownerless-terminal-continuity");
        fixture.spawn(true);
        let state = fixture.app.state::<SessionServerState>();
        let prior = state.connected().unwrap();
        let source = fixture
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, prior.namespace())
            .unwrap();
        fixture.kill(true);
        let initial = fixture
            .admission
            .effects
            .drained_spawn_predecessor(
                &source,
                &fixture.session,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
            )
            .unwrap();
        assert!(!initial.owner.authorizing);
        prior.sever();
        assert!(wait_until(|| !prior.is_alive()));
        let client = state.reconnected(fixture.sidecar.root()).unwrap();
        let current = fixture
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, client.namespace())
            .unwrap();
        assert_ne!(current, source);
        assert_eq!(current.namespace.epoch, source.namespace.epoch);
        let proof = fixture
            .admission
            .effects
            .drained_spawn_predecessor(&current, &fixture.session, std::time::Instant::now())
            .unwrap();
        assert_eq!(proof.owner, initial.owner);
        assert_eq!(proof.evidence, initial.evidence);
        assert_eq!(proof.owner.target, source);
        assert!(fixture
            .admission
            .effects
            .reserve(current.clone(), &fixture.session)
            .is_err());
        let mut unrelated = current.clone();
        unrelated.namespace.epoch.push_str("-other");
        assert!(fixture
            .admission
            .effects
            .drained_spawn_predecessor(&unrelated, &fixture.session, std::time::Instant::now())
            .is_err());
        let (spawn, predecessor) = fixture
            .admission
            .effects
            .reserve_spawn_with_predecessor(
                current,
                &fixture.session,
                false,
                std::time::Instant::now(),
                Some(&proof),
            )
            .unwrap();
        assert_eq!(predecessor, Some(initial.owner));
        fixture.admission.effects.never_started(&spawn);
    }

    #[test]
    fn rm5aa_no_send_retry_is_distinct_from_partial_or_cancelled_spawn() {
        use crate::terminal::session_server::{SessionServerState, SpawnDisposition};
        use codesense_session_server::testing::{registration, sh};
        let fixture = EffectFixture::actual("spawn-disposition");
        let old = fixture.reserve(true);
        let client = fixture
            .app
            .state::<SessionServerState>()
            .connected()
            .unwrap();
        client.sever();
        assert!(codesense_session_server::testing::wait_until(
            || !client.is_alive()
        ));
        let failure = client
            .spawn(sh("sleep 60"), registration(&old.session, None, false))
            .unwrap_err();
        assert_eq!(failure.disposition, SpawnDisposition::NeverStarted);
        assert!(failure.may_host_elsewhere);
        let owners = &fixture.admission.effects;
        owners.spawn_failed(&old, failure.disposition);
        let retry = owners
            .reserve_spawn(old.target.clone(), &old.session, true)
            .unwrap();
        assert!(retry.pool_member);
        for disposition in [
            SpawnDisposition::RejectedUndrained,
            SpawnDisposition::CancelledUndrained,
            SpawnDisposition::Unknown,
        ] {
            owners.spawn_failed(&retry, disposition);
            assert!(owners
                .reserve_spawn(retry.target.clone(), &retry.session, true)
                .is_err());
        }
        // Only a positively known no-send outcome—not generic failure or a
        // cancellation reply—can release this reservation without ingress.
        owners.never_started(&retry);
    }

    #[test]
    fn rm5aa_sidecar_layout_event_keeps_emitting_connection_namespace() {
        use super::super::pty_backend;
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::{TerminalDomain, WorkspaceKey};
        use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};
        let fixture = EffectFixture::actual("layout-origin");
        let old = fixture.reserve(true);
        let payloads = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen = payloads.clone();
        fixture.app.listen("terminal-runtime-event", move |event| {
            seen.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        let state = fixture.app.state::<SessionServerState>();
        let client = state.connected().unwrap();
        let workspace = WorkspaceKey::new(format!("layout-{}", fixture.session));
        client
            .open_tab(
                OpenTab {
                    viewer: super::super::pty_layout::app_viewer(),
                    workspace: workspace.clone(),
                    session: PtySessionId::new(&fixture.session),
                    descriptor: Some(WorkspaceDescriptor::default()),
                    name: None,
                    role: None,
                    tags: Default::default(),
                    placement: Placement::Tab,
                    index: None,
                    activate: true,
                },
                Box::new(|| {
                    fixture.spawn(true);
                    Ok(())
                }),
            )
            .unwrap();
        assert!(codesense_session_server::testing::wait_until(|| !payloads
            .lock()
            .unwrap()
            .is_empty()));
        let old_sink = fixture.real_sinks.lock().unwrap()[1].clone();
        let before = payloads.lock().unwrap().clone();
        assert!(before.iter().any(|value| matches!(
            serde_json::from_value::<TerminalDomainEvent>(value["event"].clone()).unwrap(),
            TerminalDomainEvent::LayoutChanged { .. }
        )));
        assert!(before
            .iter()
            .all(|value| value["target"] == serde_json::to_value(&old.target).unwrap()));
        client.sever();
        assert!(codesense_session_server::testing::wait_until(
            || !client.is_alive()
        ));
        let next = state.reconnected(fixture.sidecar.root()).unwrap();
        pty_backend::carry_across(fixture.app.handle(), &state, &next);
        let target = state
            .admission
            .target_for_namespace(
                super::super::machines::RuntimeMemberSlot::Sidecar,
                next.namespace(),
            )
            .unwrap();
        payloads.lock().unwrap().clear();
        next.set_descriptor(
            &workspace,
            WorkspaceDescriptor {
                display_name: "replacement".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(codesense_session_server::testing::wait_until(|| !payloads
            .lock()
            .unwrap()
            .is_empty()));
        assert!(payloads
            .lock()
            .unwrap()
            .iter()
            .all(|value| value["target"] == serde_json::to_value(&target).unwrap()));
        let count = payloads.lock().unwrap().len();
        TerminalEventSink::emit(
            &*old_sink,
            TerminalDomainEvent::LayoutChanged {
                epoch: client.epoch().into(),
                rev: u64::MAX,
                workspace,
                layout: None,
            },
        )
        .unwrap();
        assert_eq!(
            payloads.lock().unwrap().len(),
            count,
            "late old layout cannot adopt new admission"
        );
    }

    #[test]
    fn rm5aa_actual_sink_orders_legacy_and_keeps_reader_and_sealed_events_live() {
        use codesense_pty_core::domain::TerminalDomain;
        for sidecar in [false, true] {
            let fixture = EffectFixture::actual("ordered");
            let claim = fixture.reserve(sidecar);
            fixture.spawn(sidecar);
            let sink = fixture.real_sinks.lock().unwrap()[usize::from(sidecar)].clone();
            let observed = Arc::new(Mutex::new(Vec::<String>::new()));
            let (paused, reached) = std::sync::mpsc::sync_channel(1);
            let (resume, receive) = std::sync::mpsc::sync_channel(1);
            sink.delivery.0.lock().unwrap().before_delivery = Some((paused, receive));
            let order = observed.clone();
            fixture.app.listen("pty-osc133", move |_| {
                order.lock().unwrap().push("osc".into())
            });
            let order = observed.clone();
            fixture.app.listen("pty-exit", move |_| {
                order.lock().unwrap().push("exit".into())
            });
            let sealed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let sealed_count = sealed.clone();
            let payloads = Arc::new(Mutex::new(Vec::new()));
            let captured = payloads.clone();
            fixture.app.listen("terminal-runtime-event", move |event| {
                let value: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                captured.lock().unwrap().push(value);
                sealed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            let marker = || TerminalDomainEvent::PromptMarker {
                session: PtySessionId::new(&fixture.session),
                marker: codesense_pty_core::osc::OscEvent::PromptStart,
            };
            TerminalEventSink::emit(&*sink, marker()).unwrap();
            reached
                .recv_timeout(codesense_session_server::testing::WAIT)
                .unwrap();
            // Output has no local effect: it is sealed, never queued.
            TerminalEventSink::emit(
                &*sink,
                TerminalDomainEvent::Output {
                    session: PtySessionId::new(&fixture.session),
                    data: "out".into(),
                    seq: 3,
                },
            )
            .unwrap();
            TerminalEventSink::emit(&*sink, marker()).unwrap();
            // This reply needs the actual sidecar reader while compatibility is paused.
            let client = fixture
                .app
                .state::<crate::terminal::session_server::SessionServerState>()
                .connected()
                .unwrap();
            assert_eq!(
                client.has_session(&PtySessionId::new(&fixture.session)),
                Ok(sidecar)
            );
            assert!(sealed.load(std::sync::atomic::Ordering::SeqCst) >= 3);
            assert!(
                payloads.lock().unwrap().iter().all(|payload| {
                    payload["target"] == serde_json::to_value(&claim.target).unwrap()
                }),
                "sealed local output/OSC must retain its exact admitted origin"
            );
            assert!(sink
                .delivery
                .0
                .lock()
                .unwrap()
                .pending
                .iter()
                .all(|queued| !matches!(queued.event, TerminalDomainEvent::Output { .. })));
            let local_stream = fixture.app.state::<StreamIds>().attach(
                sink.host.clone(),
                &PtySessionId::new(&fixture.session),
                &ViewportId::new("view"),
                &subscribed(91, "", 0),
            );
            let streamed = Arc::new(Mutex::new(String::new()));
            let stream_seen = streamed.clone();
            fixture
                .app
                .listen(stream_event_name(local_stream), move |event| {
                    *stream_seen.lock().unwrap() = event.payload().into()
                });
            TerminalEventSink::emit(
                &*sink,
                TerminalDomainEvent::StreamOutput {
                    stream: StreamId(91),
                    data: "direct".into(),
                    seq: 6,
                },
            )
            .unwrap();
            assert!(
                streamed.lock().unwrap().contains("direct"),
                "exact stream delivery bypasses paused compatibility work"
            );
            fixture.kill(sidecar);
            fixture
                .admission
                .effects
                .drained_predecessor(
                    &claim.target,
                    &claim.session,
                    std::time::Instant::now() + codesense_session_server::testing::WAIT,
                )
                .unwrap();
            assert!(observed.lock().unwrap().is_empty());
            resume.send(()).unwrap();
            assert!(codesense_session_server::testing::wait_until(|| observed
                .lock()
                .unwrap()
                .last()
                .is_some_and(|event| event == "exit")));
            assert_eq!(*observed.lock().unwrap(), vec!["osc", "osc", "exit"]);
            let sealed = payloads.lock().unwrap();
            assert!(sealed
                .iter()
                .all(|payload| payload["target"] == serde_json::to_value(&claim.target).unwrap()));
            assert!(sealed.iter().any(|payload| matches!(
                serde_json::from_value::<TerminalDomainEvent>(payload["event"].clone()).unwrap(),
                TerminalDomainEvent::Exit { .. }
            )));
        }
    }

    #[test]
    fn rm5aa_actual_sink_collision_suppresses_bare_but_keeps_owned_cleanup() {
        let fixture = EffectFixture::actual("sink-collision");
        let local = fixture.reserve(false);
        fixture.spawn(false);
        let asks = fixture.app.state::<AskUserRegistry>();
        let (_, mut answer) = asks.register_owned(&fixture.session, Some(local.clone()));
        let peer = fixture.reserve(true);
        fixture.spawn(true);
        let bare = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for event in ["pty-exit", "pty-osc133"] {
            let bare = bare.clone();
            fixture.app.listen(event, move |_| {
                bare.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let sealed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = sealed.clone();
        fixture.app.listen("terminal-runtime-event", move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        for sink in fixture.real_sinks.lock().unwrap().iter() {
            TerminalEventSink::emit(
                &**sink,
                TerminalDomainEvent::PromptMarker {
                    session: PtySessionId::new(&fixture.session),
                    marker: codesense_pty_core::osc::OscEvent::PromptStart,
                },
            )
            .unwrap();
        }
        fixture.kill(true);
        fixture
            .admission
            .effects
            .drained_predecessor(
                &peer.target,
                &peer.session,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
            )
            .unwrap();
        assert!(
            matches!(
                answer.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "peer removal cannot cancel local ask"
        );
        fixture.kill(false);
        let answer = Mutex::new(answer);
        assert!(codesense_session_server::testing::wait_until(|| answer
            .lock()
            .unwrap()
            .try_recv()
            .is_ok()));
        for sink in fixture.real_sinks.lock().unwrap().iter() {
            assert!(codesense_session_server::testing::wait_until(|| {
                let queue = sink.delivery.0.lock().unwrap();
                queue.pending.is_empty() && !queue.running
            }));
        }
        assert_eq!(bare.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(sealed.load(std::sync::atomic::Ordering::SeqCst) >= 4);
        assert!(!fixture.admission.effects.unambiguous(&local));
    }

    /// SW-XS-3 (F7, AC1): a session going live in a worktree — the PTY attach
    /// stimulus — writes nothing into the worktree. Before SW-XS-3 this handler
    /// enqueued an `Auto` reconcile wave (which forward-materialized and
    /// reverse-promoted).
    #[test]
    fn worktree_live_writes_nothing() {
        let coord = std::sync::Arc::new(crate::content_engine::sync::WaveCoordinator::new());
        let app = tauri::test::mock_builder()
            .manage(std::sync::Arc::clone(&coord))
            .manage(crate::commands::watch::WatcherState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(worktree.path().join("README.md"), b"unchanged").unwrap();
        std::fs::create_dir(worktree.path().join("src")).unwrap();
        let before: Vec<_> = std::fs::read_dir(worktree.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let sink = AppTerminalSink::new(app.handle().clone(), StreamHost::InProcess);

        TerminalEventSink::emit(
            &sink,
            TerminalDomainEvent::WorktreeLive {
                cwd: worktree.path().join("src"),
            },
        )
        .unwrap();

        assert_eq!(
            coord.enqueued_count(),
            0,
            "a terminal attach enqueues no wave"
        );
        assert_eq!(
            coord.entry_attempts(),
            0,
            "no coordinator entry point is reached"
        );
        let after: Vec<_> = std::fs::read_dir(worktree.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(before, after, "a terminal attach writes nothing");
        assert_eq!(
            std::fs::read(worktree.path().join("README.md")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn rm5aa_actual_sink_overload_and_close_retain_exact_terminal_cleanup() {
        let fixture = EffectFixture::actual("bounded");
        let claim = fixture.reserve(false);
        fixture.spawn(false);
        let sink = TestSink::new(fixture.app.handle().clone(), StreamHost::InProcess);
        sink.bind_namespace(
            fixture.real_sinks.lock().unwrap()[0]
                .namespace
                .get()
                .unwrap()
                .clone(),
        );
        let delivery = sink.delivery.clone();
        let asks = fixture.app.state::<AskUserRegistry>();
        let (_, answer) = asks.register_owned(&fixture.session, Some(claim.clone()));
        let (paused, reached) = std::sync::mpsc::sync_channel(1);
        let (resume, receive) = std::sync::mpsc::sync_channel(1);
        sink.delivery.0.lock().unwrap().before_delivery = Some((paused, receive));
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        fixture.app.listen("pty-osc133", move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let emit = || {
            TerminalEventSink::emit(
                &sink,
                TerminalDomainEvent::PromptMarker {
                    session: PtySessionId::new(&fixture.session),
                    marker: codesense_pty_core::osc::OscEvent::PromptStart,
                },
            )
            .unwrap()
        };
        emit();
        reached
            .recv_timeout(codesense_session_server::testing::WAIT)
            .unwrap();
        for _ in 0..LOCAL_EFFECT_EVENTS + 10 {
            emit();
        }
        let terminal = TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new(&fixture.session),
            reason: TeardownReason::Kill,
        };
        TerminalEventSink::emit(&sink, terminal.clone()).unwrap();
        TerminalEventSink::emit(&sink, terminal).unwrap();
        {
            let queue = delivery.0.lock().unwrap();
            assert_eq!(queue.presentation_count, LOCAL_EFFECT_EVENTS);
            assert_eq!(
                queue.pending.len(),
                LOCAL_EFFECT_EVENTS + 1,
                "duplicate terminal settlement coalesces per claim"
            );
        }
        drop(sink);
        resume.send(()).unwrap();
        let answer = Mutex::new(answer);
        assert!(codesense_session_server::testing::wait_until(|| answer
            .lock()
            .unwrap()
            .try_recv()
            .is_ok()));
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "closed queue suppresses pending bare effects"
        );
        assert!(codesense_session_server::testing::wait_until(|| delivery
            .0
            .lock()
            .unwrap()
            .pending
            .is_empty()));
        fixture.kill(false);
    }

    /// SW-VE-14 R2-B1: `CwdChanged` is NOT a presentation effect — a
    /// `PromptMarker` storm must not drop a `cd` event, since the FE
    /// revision counter is bump-only (no polling, no reconvergence). Fill
    /// the presentation queue past its cap with prompt markers, slip a
    /// single `CwdChanged` in among them, resume delivery, and verify the
    /// `pty-osc7` event still fired.
    #[test]
    fn rm5aa_cwd_changed_still_emits_under_prompt_marker_storm() {
        let fixture = EffectFixture::actual("cwd-storm");
        let _claim = fixture.reserve(false);
        fixture.spawn(false);
        let sink = TestSink::new(fixture.app.handle().clone(), StreamHost::InProcess);
        sink.bind_namespace(
            fixture.real_sinks.lock().unwrap()[0]
                .namespace
                .get()
                .unwrap()
                .clone(),
        );
        let delivery = sink.delivery.clone();
        let (paused, reached) = std::sync::mpsc::sync_channel(1);
        let (resume, receive) = std::sync::mpsc::sync_channel(1);
        sink.delivery.0.lock().unwrap().before_delivery = Some((paused, receive));
        let cwd_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_cwd = cwd_count.clone();
        fixture.app.listen("pty-osc7", move |_| {
            seen_cwd.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let marker = || {
            TerminalEventSink::emit(
                &sink,
                TerminalDomainEvent::PromptMarker {
                    session: PtySessionId::new(&fixture.session),
                    marker: codesense_pty_core::osc::OscEvent::PromptStart,
                },
            )
            .unwrap()
        };
        // Fire the first marker so the delivery worker starts and parks on
        // the before-delivery barrier — every emit after this one queues.
        marker();
        reached
            .recv_timeout(codesense_session_server::testing::WAIT)
            .unwrap();
        // Saturate the presentation queue past its cap. A new PromptMarker
        // at this point is dropped on overload (the existing bounded test
        // locks that behavior); a CwdChanged must NOT be.
        for _ in 0..LOCAL_EFFECT_EVENTS + 10 {
            marker();
        }
        TerminalEventSink::emit(
            &sink,
            TerminalDomainEvent::CwdChanged {
                session: PtySessionId::new(&fixture.session),
                cwd: std::path::PathBuf::from("/tmp/under-storm"),
            },
        )
        .unwrap();
        // Resume delivery and let the worker drain.
        resume.send(()).unwrap();
        assert!(codesense_session_server::testing::wait_until(|| cwd_count
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1));
        assert_eq!(
            cwd_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one CwdChanged in → one pty-osc7 out, even with a PromptMarker storm"
        );
        drop(sink);
        assert!(codesense_session_server::testing::wait_until(|| delivery
            .0
            .lock()
            .unwrap()
            .pending
            .is_empty()));
        fixture.kill(false);
    }

    #[test]
    fn rm5aa_real_exit_and_sealed_completion_gate_same_id_reservation() {
        for sidecar in [false, true] {
            for sealed_publication in [false, true] {
                let fixture = EffectFixture::actual("draining");
                let old = fixture.reserve(sidecar);
                fixture.spawn(sidecar);
                let (paused, reached) = std::sync::mpsc::sync_channel(1);
                let (resume, receive) = std::sync::mpsc::sync_channel(1);
                if sealed_publication {
                    let receive = Mutex::new(Some(receive));
                    fixture.app.listen("terminal-runtime-event", move |event| {
                        let value: serde_json::Value =
                            serde_json::from_str(event.payload()).unwrap();
                        if value["event"].get("Exit").is_some() {
                            if let Some(receive) = receive.lock().unwrap().take() {
                                paused.send(()).unwrap();
                                receive
                                    .recv_timeout(codesense_session_server::testing::WAIT)
                                    .unwrap();
                            }
                        }
                    });
                } else {
                    *fixture.pause.lock().unwrap() = Some((paused, receive));
                }
                fixture.kill(sidecar);
                reached
                    .recv_timeout(codesense_session_server::testing::WAIT)
                    .unwrap();
                assert!(
                    fixture
                        .admission
                        .effects
                        .reserve_spawn(old.target.clone(), &old.session, false)
                        .is_err(),
                    "Removed and even captured Exit cannot bypass incomplete sealed delivery"
                );
                assert!(fixture.admission.effects.commit(&old, || Ok(())).is_err());
                resume.send(()).unwrap();
                let replacement = fixture
                    .admission
                    .effects
                    .reserve_spawn_until(
                        old.target.clone(),
                        &old.session,
                        false,
                        std::time::Instant::now() + codesense_session_server::testing::WAIT,
                    )
                    .unwrap();
                assert_ne!(old, replacement);
                assert!(
                    !fixture.admission.effects.unambiguous(&old),
                    "old queued bare work cannot emit into replacement"
                );
                assert!(
                    fixture
                        .admission
                        .effects
                        .reserve_spawn(old.target.clone(), &old.session, false)
                        .is_err(),
                    "second spawn cannot share the atomic reservation"
                );
                fixture.spawn(sidecar);
                assert!(fixture
                    .admission
                    .effects
                    .commit(&replacement, || Ok(()))
                    .is_ok());
                fixture.kill(sidecar);
            }
        }
    }

    #[test]
    fn rm5aa_natural_exit_reap_removes_only_predecessor_scope_before_respawn() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, machines, mcp_scope, pty_backend};
        use codesense_pty_core::domain::{ClientBootId, TerminalDomain, ViewerId, WorkspaceKey};
        use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};
        for sidecar in [false, true] {
            let fixture =
                EffectFixture::actual(&format!("rm5aa-respawn-{}", rand::random::<u64>()));
            let old = fixture.reserve(sidecar);
            create_owned_resources(&fixture, &old);
            // This oracle is a scoped launch without an account binding; account
            // continuity is separately exercised by the recovery/failure package.
            fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>()
                .take_owned_provenance(&old);
            let domain: Arc<dyn TerminalDomain> = if sidecar {
                fixture
                    .app
                    .state::<crate::terminal::session_server::SessionServerState>()
                    .connected()
                    .unwrap()
            } else {
                fixture.local.clone()
            };
            let session = PtySessionId::new(&fixture.session);
            let viewer = ViewerId::new("respawn-viewer");
            let request = OpenTab {
                viewer: viewer.clone(),
                workspace: WorkspaceKey::new("root:rm5aa"),
                session: session.clone(),
                descriptor: Some(WorkspaceDescriptor {
                    display_name: "rm5aa".into(),
                    kind: "root".into(),
                    ..Default::default()
                }),
                name: Some("kept tab".into()),
                role: None,
                tags: Default::default(),
                placement: Placement::Tab,
                index: None,
                activate: true,
            };
            let before = domain
                .open_tab(
                    request,
                    Box::new(|| {
                        fixture.spawn_command(sidecar, "printf old-scrollback; exit 0");
                        Ok(())
                    }),
                )
                .unwrap();
            assert!(codesense_session_server::testing::wait_until(|| fixture
                .admission
                .effects
                .0
                .lock()
                .unwrap()
                .claims
                .get(&(old.target.member.slot, old.session.clone()))
                .is_some_and(|record| record.exit_complete)));
            assert!(
                fixture
                    .admission
                    .effects
                    .reserve_spawn(old.target.clone(), &old.session, false)
                    .is_err(),
                "natural Exit alone does not reap the retained tab"
            );
            let backend = pty_backend::resolve_session_reference(
                fixture.app.handle(),
                &machines::RuntimeSessionReference::Admitted(Box::new(
                    machines::RuntimeSessionTarget {
                        runtime: old.target.clone(),
                        session: session.clone(),
                    },
                )),
                false,
            )
            .unwrap();
            let path = mcp_scope::mcp_scope_session_dir(&fixture.session);
            let replacement = Mutex::new(None);
            let after =
                pty_backend::respawn_tab_admitted(fixture.app.handle(), backend, &session, |_| {
                    assert!(
                        !path.exists(),
                        "exact old resource is removed before new projection"
                    );
                    let next = fixture.reserve(sidecar);
                    create_owned_resources(&fixture, &next);
                    fixture.spawn(sidecar);
                    *replacement.lock().unwrap() = Some(next);
                    Ok(())
                })
                .unwrap();
            assert_eq!(before.session, after.session);
            assert_eq!(before.name, after.name);
            let next = replacement.into_inner().unwrap().unwrap();
            assert!(!mcp_scope::remove_owned_scope(&old));
            assert!(path.join(".mcp.json").exists());
            assert!(domain
                .layout_snapshot(&viewer, &ClientBootId::new("test"))
                .is_ok());
            fixture.kill(sidecar);
            assert!(codesense_session_server::testing::wait_until(
                || !path.exists()
            ));
            assert!(fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>()
                .owned_provenance(&next)
                .is_none());
        }
    }

    #[test]
    fn rm5aa_drained_concurrent_reservations_have_one_winner_and_unknown_stays_closed() {
        let fixture = EffectFixture::actual("one-winner");
        let old = fixture.reserve(false);
        fixture.spawn(false);
        fixture.kill(false);
        fixture
            .admission
            .effects
            .drained_predecessor(
                &old.target,
                &old.session,
                std::time::Instant::now() + codesense_session_server::testing::WAIT,
            )
            .unwrap();
        // Drained proves exit and removal only; a settlement can still be in
        // flight, and a reservation without a deadline refuses until it lands.
        assert!(codesense_session_server::testing::wait_until(|| {
            fixture
                .admission
                .effects
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .claims
                .get(&(old.target.member.slot, old.session.clone()))
                .is_some_and(|record| record.active_legacy == 0 && record.pending_settlements == 0)
        }));
        let winners = std::thread::scope(|scope| {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let mut threads = Vec::new();
            for _ in 0..2 {
                let barrier = barrier.clone();
                let admission = fixture.admission.clone();
                let target = old.target.clone();
                let session = fixture.session.clone();
                threads.push(scope.spawn(move || {
                    barrier.wait();
                    admission.effects.reserve_spawn(target, &session, false)
                }));
            }
            threads
                .into_iter()
                .filter_map(|thread| thread.join().unwrap().ok())
                .collect::<Vec<_>>()
        });
        assert_eq!(winners.len(), 1);
        let pending = &winners[0];
        fixture.admission.effects.retire_claim(pending); // unknown child outcome
        assert!(fixture
            .admission
            .effects
            .reserve_spawn(old.target.clone(), &old.session, false)
            .is_err());
        fixture.admission.effects.never_started(pending); // explicit proof, not generic retirement
        let retry = fixture.reserve(false);
        assert_ne!(&retry, pending);
        fixture.spawn(false);
        fixture.kill(false);
    }

    /// SV-PBF-138: an unbounded bind of a launch holding a workspace writes the
    /// launch registry, but never while the shared effects mutex is held.
    #[test]
    fn pbf138_gated_bind_does_not_wait_on_the_registry_write() {
        use tauri::Manager;
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let handle = fixture.app.handle().clone();
        let claim =
            super::super::pty_backend::capture_local_effect_claim(&handle, &fixture.session)
                .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch-registry.json");
        let launch = handle.state::<DashboardLaunchState>();
        launch.install_persistence(path.clone());
        launch.begin("story").unwrap();
        launch.set_worktree_dir_if_absent("story", "/worktree/a".into());

        launch.set_async_persist_for_test(true);
        let hold = launch.hold_persistence_for_test();
        let (done, bound) = std::sync::mpsc::channel();
        let worker = {
            let handle = handle.clone();
            let session = fixture.session.clone();
            let claim = claim.clone();
            std::thread::spawn(move || {
                let result =
                    super::super::pty_backend::commit_local_effect_claim(&handle, &claim, || {
                        handle.state::<DashboardLaunchState>().bind_owned(
                            "story",
                            &session,
                            None,
                            None,
                            Some(claim.clone()),
                        )
                    });
                done.send(result).unwrap();
            })
        };
        let outcome = bound.recv_timeout(std::time::Duration::from_secs(3));
        let waited_on_disk = outcome.is_err();
        drop(hold);
        worker.join().unwrap();
        assert!(
            !waited_on_disk,
            "the effects gate waited on the launch-registry write"
        );
        outcome.unwrap().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let saved = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .map(|registry| registry["launches"]["story"].clone());
            if saved.is_some_and(|value| {
                value["sessionId"] == fixture.session.as_str()
                    && value["worktreeDir"] == "/worktree/a"
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the bind's snapshot was never flushed"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        fixture.kill(false);
    }

    /// Binds the fixture's session under handle "story" through the effects gate,
    /// after stamping `/worktree/a`. The registry write of the bind pauses once it
    /// has read its snapshot; returns once it is paused, with the way to resume it.
    fn pbf138_pause_gated_bind(
        fixture: &EffectFixture,
        path: &std::path::Path,
    ) -> std::sync::mpsc::Sender<()> {
        use tauri::Manager;
        let handle = fixture.app.handle().clone();
        let claim =
            super::super::pty_backend::capture_local_effect_claim(&handle, &fixture.session)
                .unwrap();
        let launch = handle.state::<DashboardLaunchState>();
        launch.install_persistence(path.to_path_buf());
        launch.set_async_persist_for_test(true);
        launch.begin("story").unwrap();
        launch.set_worktree_dir_if_absent("story", "/worktree/a".into());
        assert!(launch.flush_and_wait(std::time::Duration::from_secs(5)));
        let (reached_tx, reached) = std::sync::mpsc::channel();
        let (resume, resume_rx) = std::sync::mpsc::channel::<()>();
        let resume_rx = std::sync::Mutex::new(resume_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        launch.set_persist_hook_for_test(move || {
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                reached_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
            }
        });
        super::super::pty_backend::commit_local_effect_claim(&handle, &claim, || {
            handle.state::<DashboardLaunchState>().bind_owned(
                "story",
                &fixture.session,
                None,
                None,
                Some(claim.clone()),
            )
        })
        .unwrap();
        reached
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the bind's registry write never started");
        resume
    }

    /// "story"'s saved record: `None` when the file or the record is absent; any
    /// other read or parse failure panics.
    fn pbf138_saved(path: &std::path::Path) -> Option<serde_json::Value> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("registry unreadable: {error}"),
        };
        let registry: serde_json::Value =
            serde_json::from_str(&text).expect("registry file does not parse");
        registry["launches"].get("story").cloned()
    }

    /// Quit through the real coordinator while the bind's registry write is paused:
    /// quit must stay held (a second quit request included), then exit once the
    /// write is released and acknowledged.
    fn pbf138_quit_while_paused(fixture: &EffectFixture, resume: std::sync::mpsc::Sender<()>) {
        use crate::exit_coordinator::{request, ExitDisposition};
        use tauri::Manager;
        let manager = std::sync::Arc::new(
            crate::commands::host_connection_manager::HostConnectionManager::default(),
        );
        assert_eq!(request(&manager), ExitDisposition::StartDrain);
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (exited_tx, exited) = std::sync::mpsc::channel();
        let app = fixture.app.handle().clone();
        let quitting = manager.clone();
        let quit = std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
                .block_on(crate::exit_coordinator::drain_then(
                    &quitting,
                    || {},
                    move || {
                        entered_tx.send(()).unwrap();
                        app.state::<DashboardLaunchState>()
                            .flush_and_wait(std::time::Duration::from_secs(10));
                    },
                    || exited_tx.send(()).unwrap(),
                ));
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("quit never reached the registry flush");
        assert_ne!(
            request(&manager),
            ExitDisposition::Allow,
            "a second quit was allowed before the registry flush finished"
        );
        assert!(
            exited.try_recv().is_err(),
            "quit did not wait for the pending registry write"
        );
        resume.send(()).unwrap();
        exited
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("quit never completed");
        quit.join().unwrap();
        assert_eq!(request(&manager), ExitDisposition::Allow);
    }

    /// SV-PBF-138: quitting right after a launch must leave its bind on disk.
    #[test]
    fn pbf138_quit_waits_for_the_pending_bind_write() {
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch-registry.json");
        let resume = pbf138_pause_gated_bind(&fixture, &path);
        pbf138_quit_while_paused(&fixture, resume);

        let restarted = DashboardLaunchState::new(1);
        restarted.install_persistence(path.clone());
        assert_eq!(
            restarted.persisted_worktree_dir("story", &fixture.session),
            Some("/worktree/a".into())
        );
        fixture.kill(false);
    }

    /// SV-PBF-138: a bind's late registry write must not bring back a record the
    /// session's release already removed.
    #[test]
    fn pbf138_late_write_does_not_resurrect_a_released_launch() {
        use tauri::Manager;
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch-registry.json");
        let resume = pbf138_pause_gated_bind(&fixture, &path);
        let launch = fixture.app.handle().state::<DashboardLaunchState>();
        launch.release_for_session(&fixture.session).unwrap();
        pbf138_quit_while_paused(&fixture, resume);
        assert_eq!(pbf138_saved(&path), None);
        fixture.kill(false);
    }

    /// SV-PBF-138: a bind's late registry write must not overwrite the record of
    /// the session that replaced it under the same handle.
    #[test]
    fn pbf138_late_write_does_not_overwrite_the_replacement_launch() {
        use tauri::Manager;
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch-registry.json");
        let resume = pbf138_pause_gated_bind(&fixture, &path);
        let launch = fixture.app.handle().state::<DashboardLaunchState>();
        launch.release_for_session(&fixture.session).unwrap();
        launch.begin("story").unwrap();
        launch.set_worktree_dir_if_absent("story", "/worktree/b".into());
        launch.bind("story", "session-b", None).unwrap();
        pbf138_quit_while_paused(&fixture, resume);

        let saved = pbf138_saved(&path).expect("the replacement's record is missing");
        assert_eq!(saved["sessionId"], "session-b");
        let restarted = DashboardLaunchState::new(1);
        restarted.install_persistence(path.clone());
        assert_eq!(
            restarted.persisted_worktree_dir("story", "session-b"),
            Some("/worktree/b".into())
        );
        fixture.kill(false);
    }

    /// SW-AGR-19 PR-6: a terminal server update ends its sessions as expired
    /// (`end_sessions`): that frees an Agent's launch handle, so a resume
    /// relaunching it under the same handle starts fresh rather than reading
    /// as "a session already runs".
    #[test]
    fn agr19_a_session_ended_with_its_server_frees_its_launch_handle() {
        let fixture = EffectFixture::new();
        fixture.spawn(true);
        let (claim, _rx) = fixture.register();
        assert!(
            !fixture.launch.begin("story").unwrap(),
            "held while it runs"
        );
        let released = settle_owned_event(
            Some(&fixture.launch),
            Some(&fixture.asks),
            &claim,
            &TerminalDomainEvent::SessionRemoved {
                session: PtySessionId::new(&fixture.session),
                reason: TeardownReason::Expire,
            },
        )
        .map(|(_, released)| released.handle);
        assert_eq!(released.as_deref(), Some("story"));
        assert!(
            fixture.launch.begin("story").unwrap(),
            "the relaunch reserves it afresh"
        );
        assert!(fixture.launch.abort("story"));
        fixture.kill(true);
    }

    #[test]
    fn rm5aa_unique_registration_actual_exit_releases_dashboard_and_ask() {
        for sidecar in [false, true] {
            let fixture = EffectFixture::new();
            fixture.spawn(sidecar);
            let (claim, mut rx) = fixture.register();
            fixture.kill(sidecar);
            fixture.wait_removed(1);
            let tickets = std::mem::take(&mut *fixture.deferred.lock().unwrap());
            let mut released = Vec::new();
            for (ticket, event) in tickets {
                assert_eq!(ticket, claim);
                if let Some((audit, released_entry)) =
                    settle_owned_event(Some(&fixture.launch), Some(&fixture.asks), &ticket, &event)
                {
                    assert!(matches!(audit, agent_audit::AuditEvent::Kill));
                    released.push(released_entry.handle);
                }
            }
            assert_eq!(released, vec!["story"]);
            assert_eq!(
                rx.try_recv().unwrap().action,
                crate::mcp::state::AskUserAction::Cancel
            );
            assert_eq!(
                fixture.asks.cancel_owned(&claim),
                0,
                "terminal notifications consume the ask once"
            );
            assert!(fixture.launch.resolve("story").is_none());
            assert!(
                fixture.launch.begin("next").unwrap(),
                "checked release frees the real cap permit"
            );
            assert!(fixture.admission.effects.unambiguous(&claim));
        }
    }

    #[test]
    fn rm5aa_simultaneous_removal_keeps_collision_and_peer_records() {
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let (claim, mut rx) = fixture.register();
        fixture.spawn(true);
        assert!(super::super::pty_backend::capture_local_effect_claim(
            fixture.app.handle(),
            "shared"
        )
        .is_err());
        fixture.kill(true);
        fixture.kill(false);
        fixture.wait_removed(2);
        assert!(
            !fixture.admission.effects.unambiguous(&claim),
            "two empty layouts never erase the collision"
        );
        let captured = fixture.sealed.lock().unwrap().clone();
        let sidecar_target = captured
            .iter()
            .find(|(target, event)| {
                target.member.slot == super::super::machines::RuntimeMemberSlot::Sidecar
                    && matches!(event, TerminalDomainEvent::SessionRemoved { .. })
            })
            .unwrap()
            .0
            .clone();
        let peer = fixture
            .admission
            .effects
            .ingress(&sidecar_target, "shared", true)
            .unwrap();
        assert!(fixture.launch.release_owned(&peer).is_none());
        assert_eq!(fixture.asks.cancel_owned(&peer), 0);
        for event in [
            TerminalDomainEvent::Exit {
                session: PtySessionId::new("shared"),
                status: None,
                pool_member: false,
                requested: true,
                incarnation: 0,
            },
            TerminalDomainEvent::SessionRemoved {
                session: PtySessionId::new("shared"),
                reason: TeardownReason::Kill,
            },
        ] {
            assert!(
                settle_owned_event(Some(&fixture.launch), Some(&fixture.asks), &peer, &event)
                    .is_none()
            );
        }
        assert_eq!(fixture.launch.resolve("story").as_deref(), Some("shared"));
        assert!(rx.try_recv().is_err());
        for (ticket, event) in std::mem::take(&mut *fixture.deferred.lock().unwrap()) {
            settle_owned_event(Some(&fixture.launch), Some(&fixture.asks), &ticket, &event);
        }
        assert!(fixture.launch.resolve("story").is_none());
        assert_eq!(
            rx.try_recv().unwrap().action,
            crate::mcp::state::AskUserAction::Cancel
        );
    }

    #[test]
    fn rm5aa_terminal_ingress_prevents_late_registration_commit() {
        let fixture = EffectFixture::new();
        fixture.spawn(false);
        let pending =
            super::super::pty_backend::capture_local_effect_claim(fixture.app.handle(), "shared")
                .unwrap();
        fixture.kill(false);
        fixture.wait_removed(1);
        let result = super::super::pty_backend::commit_local_effect_claim(
            fixture.app.handle(),
            &pending,
            || {
                fixture
                    .launch
                    .register_owned("too-late", "shared", None, Some(pending.clone()))
            },
        );
        assert!(result.is_err());
        assert!(fixture.launch.resolve("too-late").is_none());
        assert!(fixture
            .admission
            .effects
            .reserve(pending.target, "shared")
            .is_err());
    }

    fn resource_provenance() -> super::super::agent_accounts::LaunchAccountProvenance {
        use crate::install::agent_account_store::{
            AccountResolutionReason, AccountResolutionSource, AgentProvider,
        };
        super::super::agent_accounts::LaunchAccountProvenance {
            runtime: codesense_pty_core::agent_cli::AgentCli::Claude,
            provider: Some(AgentProvider::Claude),
            source: AccountResolutionSource::Active,
            reason: AccountResolutionReason::ActiveSelection,
            account_id: Some("resource-test".into()),
            label: Some("Resource test".into()),
            generation: Some(3),
            home: None,
        }
    }

    fn create_owned_resources(
        fixture: &EffectFixture,
        claim: &LocalEffectClaim,
    ) -> super::super::mcp_scope::ScopeCreation {
        use super::super::{agent_accounts, mcp_scope};
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            worktree.path().join(".mcp.json"),
            r#"{"mcpServers":{"codesense":{"command":"bridge-test"}}}"#,
        )
        .unwrap();
        let descriptor = codesense_pty_core::agent_cli::McpScope {
            config_filename: ".mcp.json".into(),
            server_map_key: "mcpServers".into(),
            bridge_id: "codesense".into(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            format: codesense_pty_core::agent_cli::ConfigFragmentFormat::Json,
            injection: codesense_pty_core::agent_cli::ScopeInjection::ConfigFlag {
                flag_template: vec!["--mcp-config".into(), "{path}".into()],
                open_flag_template: None,
                settings_flag_template: None,
            },
            bridge_override: None,
            inventory: None,
        };
        let (outcome, creation) = mcp_scope::project_scope_with_creation(
            &fixture.session,
            worktree.path(),
            &[],
            Some(&descriptor),
            None,
            str::to_string,
        )
        .unwrap();
        assert!(!outcome.extra_args.is_empty());
        let creation = creation.unwrap();
        let token = agent_accounts::mint_launch_environment(
            fixture.app.handle(),
            agent_accounts::LaunchEnvironment {
                values: outcome.extra_env,
                scope: Some(creation.clone()),
                agent_env: Vec::new(),
                requires_spend_holds: false,
            },
        );
        assert!(
            token.is_some(),
            "a config-flag scope still carries its creation through the existing capability"
        );
        let carried =
            agent_accounts::consume_environment_binding(fixture.app.handle(), token.as_deref())
                .unwrap()
                .scope;
        assert_eq!(carried.as_ref(), Some(&creation));
        assert!(agent_accounts::consume_environment_binding(
            fixture.app.handle(),
            token.as_deref()
        )
        .is_err());
        mcp_scope::bind_scope_creation(&carried.unwrap(), claim).unwrap();
        fixture
            .app
            .state::<agent_accounts::AgentAccountRuntimeState>()
            .stage_owned_provenance(
                &fixture.session,
                Some(resource_provenance()),
                Some(claim.clone()),
            )
            .unwrap();
        creation
    }

    #[test]
    fn pbf210_owned_removal_retains_pending_scope_and_retries_after_record_write() {
        use crate::session_conversation::{nth_uuid, TestWriter, CODEX_FIXTURE};
        use crate::session_usage::conversation::State;
        let _home_env = crate::routines::test_support::TempHome::new();
        let fixture = EffectFixture::with_session(format!("pbf210-owned-{}", rand::random::<u64>()));
        let claim = fixture.reserve(false);
        create_owned_resources(&fixture, &claim);
        let scope = super::super::mcp_scope::mcp_scope_session_dir(&fixture.session);
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        let native = nth_uuid(15);
        w.seed_codex(&fixture.session, &native);
        let store = crate::session_usage::UsageStore::open(&w.store).unwrap();
        let mut record = store.read_session_record(&fixture.session).unwrap().unwrap();
        record.config_home = Some(scope.to_string_lossy().into_owned());
        let logs = scope.join("sessions/2026/10/27");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join(format!("rollout-2026-10-27T10-00-00-{native}.jsonl")), CODEX_FIXTURE).unwrap();
        rusqlite::Connection::open(&w.store).unwrap()
            .execute("DELETE FROM session_record WHERE session_id = ?1", [&fixture.session]).unwrap();
        let launch = fixture.app.state::<DashboardLaunchState>();
        assert!(launch.begin("pending-owned").unwrap());
        launch.bind_owned("pending-owned", &fixture.session, None, None, Some(claim.clone())).unwrap();
        let event = TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new(&fixture.session), reason: TeardownReason::Kill,
        };
        let (_, mut pending_ask) = fixture.app.state::<AskUserRegistry>()
            .register_owned(&fixture.session, Some(claim.clone()));
        let owners = &fixture.admission.effects;
        owners.0.lock().unwrap().record_mut(&claim).unwrap().pending_settlements += 1;
        settle_terminal_cleanup(fixture.app.handle(), TerminalCleanup {
            owners, owner: claim.clone(), outcomes: vec![event.clone()], completed: false,
        }).unwrap();
        assert!(scope.exists(), "a pending record must retain its native log");
        assert!(launch.resolve("pending-owned").is_none(), "capture timeout must still free the launch slot");
        assert_eq!(pending_ask.try_recv().unwrap().action, crate::mcp::state::AskUserAction::Cancel);
        assert!(fixture.app.state::<super::super::agent_accounts::AgentAccountRuntimeState>()
            .owned_provenance(&claim).is_none());
        {
            let core = owners.0.lock().unwrap();
            let settled = core.record(&claim).unwrap();
            assert!(!settled.cleanup_failed, "a capture timeout must not poison terminal settlement");
            assert_eq!(settled.pending_settlements, 0);
        }
        let unavailable = store.read_conversation_meta(&fixture.session).unwrap().unwrap();
        assert_eq!(unavailable.state, State::Unavailable);
        assert!(unavailable.unavailable["conversation"].starts_with("session_record_timeout"));
        crate::session_usage::record::persist_record_in(&w.store, &record).unwrap();
        assert!(settle_owned_app_event(fixture.app.handle(), &claim, &event, true).unwrap().is_none());
        assert!(!scope.exists(), "retry removes the home after its conversation is stored");
        assert!(launch.resolve("pending-owned").is_none());
        assert_eq!(w.stored(&fixture.session).unwrap().1.len(), 3);
        assert_eq!(store.read_conversation_meta(&fixture.session).unwrap().unwrap().state, State::Present);
    }

    #[test]
    fn rm5aa_created_scope_and_account_cleanup_requires_exact_registration() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, mcp_scope};
        for sidecar in [false, true] {
            let fixture =
                EffectFixture::with_session(format!("rm5aa-resource-{}", rand::random::<u64>()));
            fixture.spawn(sidecar);
            let (claim, _rx) = fixture.register();
            create_owned_resources(&fixture, &claim);
            let path = mcp_scope::mcp_scope_session_dir(&fixture.session);
            assert!(path.join(".mcp.json").is_file());
            let accounts = fixture
                .app
                .state::<agent_accounts::AgentAccountRuntimeState>();
            assert_eq!(
                accounts.owned_provenance(&claim),
                Some(resource_provenance())
            );
            fixture.kill(sidecar);
            fixture.wait_removed(1);
            for (ticket, event) in std::mem::take(&mut *fixture.deferred.lock().unwrap()) {
                settle_owned_resources(Some(&accounts), &ticket, &event);
            }
            assert!(
                !path.exists(),
                "the exact departed registration owns its created scope"
            );
            assert!(accounts.owned_provenance(&claim).is_none());
        }
    }

    #[test]
    fn rm5aa_old_resource_ticket_preserves_replacement_creation_and_provenance() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use super::super::{agent_accounts, mcp_scope};
        let fixture =
            EffectFixture::with_session(format!("rm5aa-replacement-{}", rand::random::<u64>()));
        fixture.spawn(false);
        let (old, _rx) = fixture.register();
        let old_creation = create_owned_resources(&fixture, &old);
        fixture.kill(false);
        fixture.wait_removed(1);
        // Sealed publication precedes deferred delivery; reuse requires both
        // terminal tickets to have reached the cleanup queue.
        assert!(codesense_session_server::testing::wait_until(|| {
            let pending = fixture.deferred.lock().unwrap();
            pending.iter().any(|(claim, event)| {
                claim == &old && matches!(event, TerminalDomainEvent::Exit { .. })
            }) && pending.iter().any(|(claim, event)| {
                claim == &old && matches!(event, TerminalDomainEvent::SessionRemoved { .. })
            })
        }));
        let old_tickets = std::mem::take(&mut *fixture.deferred.lock().unwrap());
        let accounts = fixture
            .app
            .state::<agent_accounts::AgentAccountRuntimeState>();
        for (ticket, event) in &old_tickets {
            settle_owned_resources(Some(&accounts), ticket, event);
            if matches!(
                event,
                TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
            ) {
                fixture.admission.effects.settled(ticket);
            }
        }
        let replacement = fixture
            .admission
            .effects
            .reserve_spawn_until(
                old.target.clone(),
                &fixture.session,
                false,
                std::time::Instant::now() + crate::terminal::session_server::EXIT_REAP_TIMEOUT,
            )
            .unwrap();
        assert_ne!(replacement, old);
        fixture.spawn(false);
        let new_creation = create_owned_resources(&fixture, &replacement);
        assert_ne!(new_creation, old_creation);
        let path = mcp_scope::mcp_scope_session_dir(&fixture.session);
        let before = std::fs::read(path.join(".mcp.json")).unwrap();
        assert!(mcp_scope::bind_scope_creation(&old_creation, &replacement).is_err());
        assert!(accounts
            .stage_owned_provenance(
                &fixture.session,
                Some(resource_provenance()),
                Some(old.clone())
            )
            .is_err());
        for (ticket, event) in &old_tickets {
            settle_owned_resources(Some(&accounts), ticket, event);
        }
        assert_eq!(std::fs::read(path.join(".mcp.json")).unwrap(), before);
        assert_eq!(
            accounts.owned_provenance(&replacement),
            Some(resource_provenance())
        );
        assert!(accounts.take_owned_provenance(&old).is_none());
        fixture.kill(false);
        fixture.wait_removed(2);
        for (ticket, event) in std::mem::take(&mut *fixture.deferred.lock().unwrap()) {
            settle_owned_resources(Some(&accounts), &ticket, &event);
        }
        assert!(!path.exists());
        assert!(accounts.owned_provenance(&replacement).is_none());
    }

    /// A session leaving the registry — here a failed background start, reclaimed
    /// as `Expire` — releases its dashboard binding and SpawnCap permit, audited
    /// as a background reclaim; a deliberate close audits as a kill.
    #[test]
    fn failed_background_start_releases_pty_and_dashboard_registration() {
        let launch = DashboardLaunchState::new(1);
        assert!(launch.begin("story-handle").expect("reserve launch"));
        launch
            .bind("story-handle", "startup-failed", None)
            .expect("bind launch");

        let (audit, released) =
            release_dashboard_binding(Some(&launch), "startup-failed", TeardownReason::Expire)
                .expect("the bound session is released");
        assert!(matches!(audit, agent_audit::AuditEvent::Expire));
        assert_eq!(released.handle, "story-handle");
        assert!(
            launch.resolve("story-handle").is_none(),
            "failed startup must release its dashboard binding and permit"
        );
        assert!(
            launch.begin("next-handle").expect("permit is free again"),
            "the released SpawnCap slot is reusable"
        );
        launch
            .bind("next-handle", "closed", None)
            .expect("bind launch");
        let (audit, _) = release_dashboard_binding(Some(&launch), "closed", TeardownReason::Kill)
            .expect("the bound session is released");
        assert!(matches!(audit, agent_audit::AuditEvent::Kill));
        // A plain terminal has no binding: nothing released, nothing audited.
        assert!(release_dashboard_binding(Some(&launch), "plain", TeardownReason::Kill).is_none());
    }

    /// A reap (an ended session removed for a respawn in place) releases,
    /// audits, removes and clears nothing — a kill and an expiry do.
    #[test]
    fn a_reap_keeps_the_binding_the_scope_and_the_provenance() {
        assert_eq!(
            removal_effects(TeardownReason::Reap),
            RemovalEffects {
                release_binding: false,
                remove_scope: false,
                clear_provenance: false,
            }
        );
        assert!(removal_effects(TeardownReason::Kill).remove_scope);
        assert!(removal_effects(TeardownReason::Kill).clear_provenance);
        assert!(removal_effects(TeardownReason::Expire).release_binding);

        let launch = DashboardLaunchState::new(1);
        assert!(launch.begin("story-handle").expect("reserve launch"));
        launch
            .bind("story-handle", "respawned", None)
            .expect("bind launch");
        assert!(
            release_dashboard_binding(Some(&launch), "respawned", TeardownReason::Reap).is_none()
        );
        assert!(
            launch.resolve("story-handle").is_some(),
            "a reap keeps the dashboard binding"
        );
    }

    /// A session's death cancels its pending `ask_user` questions, and only a
    /// pool member's own exit is handed to the pool for reclaim + refill.
    #[test]
    fn exited_session_cancels_its_asks_and_self_heals_only_pool_members() {
        let asks = AskUserRegistry::default();
        let (_, mut pending) = asks.register("shell-1");
        let host = std::sync::Arc::new(RecordingHost::default());
        let pool = WarmPoolState::default();
        pool.init(host.clone());
        pool.reconfigure(1, None, None);
        let member = host.spawned.lock().unwrap()[0].clone();

        settle_exited_session(Some(&asks), Some(&pool), "shell-1", false);
        assert!(pending.try_recv().is_ok(), "the pending ask is cancelled");
        assert!(host.reclaimed.lock().unwrap().is_empty());

        settle_exited_session(Some(&asks), Some(&pool), &member, false);
        assert!(
            host.reclaimed.lock().unwrap().is_empty(),
            "a deliberately killed member is not self-healed"
        );
        settle_exited_session(Some(&asks), Some(&pool), &member, true);
        assert_eq!(*host.reclaimed.lock().unwrap(), vec![member]);
    }

    /// A session's death forgets its dot-ledger entry (before the ask sweep),
    /// so an ask resolved by the sweep cannot restore the dead tab's dot.
    #[test]
    fn settling_an_exited_session_forgets_its_dot_ledger_entry() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let ledger = crate::mcp::agent_dot::ledger();
        let sid = "pty-host-settle-forgets-dot";
        crate::mcp::agent_dot::on_signal_done(app.handle(), ledger, sid, "failed", |_, _| true);
        assert!(ledger.is_tracking(sid), "the failed finish latches");
        settle_exited_session(None, None, sid, false);
        assert!(!ledger.is_tracking(sid));
    }

    /// `pty-exit` carries the child's real exit status: a non-zero code, a signal
    /// death, and an unknown status each serialize distinctly.
    #[test]
    fn pty_exit_payload_carries_the_real_exit_status() {
        assert_eq!(
            pty_exit_payload(
                "shell-1",
                Some(ExitInfo {
                    exit_code: Some(3),
                    signal: None,
                }),
                false,
            ),
            serde_json::json!({
                "session_id": "shell-1", "exit_code": 3, "signal": null,
                "requested": false,
            })
        );
        assert_eq!(
            pty_exit_payload(
                "shell-2",
                Some(ExitInfo {
                    exit_code: None,
                    signal: Some("Killed: 9".into()),
                }),
                false,
            ),
            serde_json::json!({
                "session_id": "shell-2", "exit_code": null, "signal": "Killed: 9",
                "requested": false,
            })
        );
        assert_eq!(
            pty_exit_payload("shell-3", None, false),
            serde_json::json!({
                "session_id": "shell-3", "exit_code": null, "signal": null,
                "requested": false,
            })
        );
        assert_eq!(
            pty_exit_payload(
                "shell-4",
                Some(ExitInfo {
                    exit_code: None,
                    signal: Some("Hangup: 1".into()),
                }),
                true,
            )["requested"],
            true,
        );
    }

    /// Real children run by the in-process runtime, through to the `pty-exit`
    /// payload — the golden the frontend's exit seam test replays (a signal's
    /// platform name aside). The sidecar's payloads equal these
    /// (`exit_classification_is_identical_across_hosts`).
    #[test]
    fn real_children_produce_the_golden_pty_exit_payloads() {
        use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
        use codesense_pty_core::TerminalHost;
        use codesense_session_server::testing::{registration, sh_spec, wait_until, RecordingSink};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let events = std::sync::Arc::new(RecordingSink::default());
        let host = TerminalHost::new(
            ServerIdentity::ephemeral("test"),
            events.clone(),
            runtime.handle().clone(),
        );
        let cases = [
            ("clean", "exit 0"),
            ("failed", "exit 5"),
            ("sigkill", "kill -9 $$"),
            ("stopped", "sleep 30"),
        ];
        for (id, script) in cases {
            let pty = runtime
                .block_on(LocalPtyRunner.spawn(sh_spec(id, script)))
                .unwrap();
            host.register(pty, registration(id, None, false)).unwrap();
        }
        host.kill(&PtySessionId::new("stopped")).unwrap();
        let payload = |id: &str| {
            events.events().into_iter().find_map(|event| match event {
                TerminalDomainEvent::Exit {
                    session,
                    status,
                    requested,
                    ..
                } if session.as_str() == id => Some(pty_exit_payload(id, status, requested)),
                _ => None,
            })
        };
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/agent-state/pty-exit-payloads.json"
        ))
        .unwrap();
        for (id, _) in cases {
            assert!(wait_until(|| payload(id).is_some()), "{id} exited");
            let mut got = payload(id).unwrap();
            let want = &golden["payloads"][id];
            if got["signal"].is_string() && want["signal"].is_string() {
                got["signal"] = want["signal"].clone();
            }
            assert_eq!(&got, want, "{id}");
        }
        for (id, _) in cases {
            let _ = host.kill(&PtySessionId::new(id));
        }
    }

    #[test]
    fn stream_event_name_is_the_per_viewport_channel() {
        assert_eq!(stream_event_name(42), "pty-output-stream-42");
    }

    /// A subscribe whose snapshot holds `vt`, ending at `seq`.
    fn subscribed(stream: u64, vt: &str, seq: u64) -> SubscribeResult {
        SubscribeResult {
            snapshot: codesense_pty_core::emulator::TerminalSnapshot {
                cols: 80,
                rows: 24,
                seq,
                resize_gen: 0,
                scrollback: 1000,
                vt: vt.into(),
            },
            stream_id: StreamId(stream),
            story_id: None,
            workspace_id: None,
            epic_id: None,
        }
    }

    fn output(data: &str, seq: u64) -> StreamItem<'_> {
        StreamItem::Output { data, seq }
    }

    fn resized(at_seq: u64, resize_gen: u32) -> StreamItem<'static> {
        StreamItem::Resized(StreamResize {
            at_seq,
            resize_gen,
            cols: 100,
            rows: 30,
        })
    }

    /// Every item a recorder was handed: its stream, bytes and watermark (a
    /// resize as `resized:<resize_gen>` at its `at_seq`).
    type Sent = std::rc::Rc<std::cell::RefCell<Vec<(u64, String, u64)>>>;

    fn recorder() -> (Sent, impl FnMut(u64, StreamItem<'_>) -> bool) {
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let into = sent.clone();
        (sent, move |to: u64, item: StreamItem<'_>| {
            into.borrow_mut().push(match item {
                StreamItem::Output { data, seq } => (to, data.to_string(), seq),
                StreamItem::Resized(resize) => {
                    (to, format!("resized:{}", resize.resize_gen), resize.at_seq)
                }
            });
            true
        })
    }

    /// Two hosts reporting the same `StreamId` never share a frontend stream:
    /// each attached stream has its own client-unique id, output is re-emitted
    /// only under it, and detaching one routes to its own host.
    #[test]
    fn two_hosts_with_the_same_stream_id_never_collide() {
        let ids = StreamIds::default();
        let viewport = ViewportId::new("webview");
        let (sent, mut emit) = recorder();
        let local = ids.attach(
            StreamHost::InProcess,
            &PtySessionId::new("a"),
            &viewport,
            &subscribed(7, "", 0),
        );
        let hosted = ids.attach(
            StreamHost::Sidecar,
            &PtySessionId::new("b"),
            &viewport,
            &subscribed(7, "", 0),
        );
        assert_ne!(local, hosted);
        assert!(ids.deliver(
            StreamHost::InProcess,
            StreamId(7),
            output("in", 2),
            &mut emit
        ));
        assert!(ids.deliver(
            StreamHost::Sidecar,
            StreamId(7),
            output("side", 4),
            &mut emit
        ));
        assert_eq!(
            *sent.borrow(),
            [
                (local, "in".to_string(), 2),
                (hosted, "side".to_string(), 4)
            ]
        );
        assert_eq!(ids.detach(hosted), Some((StreamHost::Sidecar, StreamId(7))));
        assert_eq!(ids.detach(hosted), None, "detached once");
        assert_eq!(
            ids.detach(local),
            Some((StreamHost::InProcess, StreamId(7)))
        );
    }

    fn attach_reserved_on(
        ids: &StreamIds,
        local: u64,
        attached: &SubscribeResult,
        emit: &mut Emit<'_>,
    ) -> Result<u64, AttachReservedError> {
        ids.attach_reserved(
            local,
            StreamHost::Sidecar,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            attached,
            emit,
        )
    }

    #[test]
    fn bind_unreserved_stream_is_error() {
        let ids = StreamIds::default();
        let (_sent, mut emit) = recorder();
        assert!(attach_reserved_on(&ids, 9, &subscribed(3, "", 0), &mut emit).is_err());
        assert!(ids.table().attached.is_empty(), "nothing bound");
    }

    /// A reservation binds once: a second subscribe naming it is refused, and
    /// every reserve hands out a new id.
    #[test]
    fn reserve_twice_is_error() {
        let ids = StreamIds::default();
        let (_sent, mut emit) = recorder();
        let local = ids.reserve();
        assert_ne!(ids.reserve(), local);
        assert_eq!(
            attach_reserved_on(&ids, local, &subscribed(3, "", 0), &mut emit),
            Ok(local)
        );
        assert!(attach_reserved_on(&ids, local, &subscribed(4, "", 0), &mut emit).is_err());
        assert_eq!(ids.detach(local), Some((StreamHost::Sidecar, StreamId(3))));
    }

    #[test]
    fn unbound_reservation_released_on_unsubscribe() {
        let ids = StreamIds::default();
        let (_sent, mut emit) = recorder();
        let local = ids.reserve();
        assert_eq!(ids.detach(local), None, "no host stream to detach");
        assert!(ids.table().reserved.is_empty(), "released");
        assert!(attach_reserved_on(&ids, local, &subscribed(3, "", 0), &mut emit).is_err());
    }

    /// Without a reserved id, subscribe allocates one as it always has, and
    /// never hands out an id someone reserved.
    #[test]
    fn subscribe_without_stream_allocates() {
        let ids = StreamIds::default();
        let reserved = ids.reserve();
        let local = ids.attach(
            StreamHost::InProcess,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(3, "", 0),
        );
        assert_ne!(local, reserved);
        assert!(ids.table().attached.contains_key(&local));
        assert!(ids.table().reserved.contains(&reserved));
    }

    /// The host may push a stream's output ahead of the subscribe reply. A
    /// reserved stream's listener is already up, so that output reaches it past
    /// the snapshot instead of being dropped.
    #[test]
    fn a_reserved_attach_sends_early_output_past_the_snapshot() {
        let ids = StreamIds::default();
        let (sent, mut emit) = recorder();
        ids.deliver(StreamHost::Sidecar, StreamId(3), output("bc", 3), &mut emit);
        ids.deliver(StreamHost::Sidecar, StreamId(3), output("de", 5), &mut emit);
        let local = ids.reserve();
        attach_reserved_on(&ids, local, &subscribed(3, "ab", 2), &mut emit).unwrap();
        assert_eq!(
            *sent.borrow(),
            [(local, "c".to_string(), 3), (local, "de".to_string(), 5)]
        );
        assert!(ids.table().early.is_empty());
    }

    /// Resizes ride a stream in order with its output — held with it ahead of
    /// the attach, sent once each, and never one the snapshot already holds.
    #[test]
    fn resizes_ride_the_stream_in_order_and_once() {
        let ids = StreamIds::default();
        let (sent, mut emit) = recorder();
        for item in [
            output("ab", 2),
            resized(2, 1),
            output("cd", 4),
            resized(4, 2),
        ] {
            ids.deliver(StreamHost::Sidecar, StreamId(3), item, &mut emit);
        }
        let local = ids.reserve();
        let mut attached = subscribed(3, "", 0);
        attached.snapshot.resize_gen = 1;
        attach_reserved_on(&ids, local, &attached, &mut emit).unwrap();
        ids.deliver(StreamHost::Sidecar, StreamId(3), resized(4, 2), &mut emit);
        ids.deliver(StreamHost::Sidecar, StreamId(3), output("e", 5), &mut emit);
        ids.deliver(StreamHost::Sidecar, StreamId(3), resized(5, 3), &mut emit);
        assert_eq!(
            *sent.borrow(),
            [
                (local, "ab".to_string(), 2),
                (local, "cd".to_string(), 4),
                (local, "resized:2".to_string(), 4),
                (local, "e".to_string(), 5),
                (local, "resized:3".to_string(), 5),
            ],
            "the resize the snapshot holds is not sent, nor one twice"
        );
    }

    /// Early output that overflowed the bounded buffer past a snapshot is a
    /// gap: a reserved attach is refused (the reservation kept, to subscribe
    /// again) and a re-point reports the stream behind, never a silent hole.
    /// Output dropped before the snapshot is no gap.
    #[test]
    fn early_overflow_past_the_snapshot_is_never_a_silent_gap() {
        let big = "x".repeat(EARLY_BYTES / 2 + 1);
        let flood = |ids: &StreamIds, emit: &mut Emit<'_>| {
            for n in 1..=3u64 {
                ids.deliver(
                    StreamHost::Sidecar,
                    StreamId(3),
                    output(&big, n * 1000),
                    emit,
                );
            }
        };
        let ids = StreamIds::default();
        let (sent, mut emit) = recorder();
        flood(&ids, &mut emit);
        let local = ids.reserve();
        assert_eq!(
            attach_reserved_on(&ids, local, &subscribed(3, "", 500), &mut emit),
            Err(AttachReservedError::Overflowed)
        );
        assert!(sent.borrow().is_empty(), "nothing sent over the gap");
        assert!(ids.table().reserved.contains(&local), "reservation kept");
        assert_eq!(
            attach_reserved_on(&ids, local, &subscribed(4, "", 2000), &mut emit),
            Ok(local),
            "a snapshot past everything dropped has no gap"
        );

        let ids = StreamIds::default();
        let (_, mut emit) = recorder();
        let local = ids.attach(
            StreamHost::Sidecar,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(1, "", 500),
        );
        flood(&ids, &mut emit);
        assert_eq!(
            ids.repoint(local, &subscribed(3, "", 500), &mut emit),
            Repoint::Behind
        );
        assert!(ids.attached_on(StreamHost::Sidecar).is_empty());
    }

    #[test]
    fn verified_remote_server_ids_isolate_equal_stream_ids() {
        let ids = StreamIds::default();
        let viewport = ViewportId::new("webview");
        let (sent, mut emit) = recorder();
        let a_host = StreamHost::remote("server-a", "epoch");
        let b_host = StreamHost::remote("server-b", "epoch");
        let a = ids.attach(
            a_host.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        let b = ids.attach(
            b_host.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        ids.deliver(a_host, StreamId(1), output("a", 1), &mut emit);
        ids.deliver(b_host, StreamId(1), output("b", 1), &mut emit);
        assert_eq!(*sent.borrow(), [(a, "a".into(), 1), (b, "b".into(), 1)]);
    }

    #[test]
    fn rm4e_two_server_namespaces_reusing_ids_never_collide() {
        let ids = StreamIds::default();
        let viewport = ViewportId::new("webview");
        let (sent, mut emit) = recorder();
        let a_namespace =
            crate::terminal::session_server::mint_event_namespace("server-a", "shared-epoch")
                .unwrap();
        let b_namespace =
            crate::terminal::session_server::mint_event_namespace("server-b", "shared-epoch")
                .unwrap();
        let a_host = StreamHost::remote_namespace(&a_namespace);
        let b_host = StreamHost::remote_namespace(&b_namespace);
        let a = ids.attach(
            a_host.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        let b = ids.attach(
            b_host.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        ids.deliver(a_host, StreamId(1), output("sentinel-a", 10), &mut emit);
        ids.deliver(b_host, StreamId(1), output("sentinel-b", 10), &mut emit);
        assert_eq!(
            *sent.borrow(),
            [(a, "sentinel-a".into(), 10), (b, "sentinel-b".into(), 10)]
        );
    }

    #[test]
    fn private_reconnect_early_replay_keeps_exact_lease_stamp() {
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let ids = StreamIds::default();
        let old = StreamHost::remote("server", "old");
        let new = StreamHost::remote("server", "new");
        let local = ids.attach(
            old.clone(),
            &PtySessionId::new("s"),
            &ViewportId::new("v"),
            &subscribed(1, "", 0),
        );
        let payloads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = payloads.clone();
        app.listen(stream_event_name(local), move |event| {
            captured
                .lock()
                .unwrap()
                .push(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        let (lease, revoke) = crate::commands::auth::private_authority::test_lease(1);
        let mut emit = remote_replay_emitter(app.handle(), Some(lease));
        let lost = ids.resume_from_to(
            old,
            new.clone(),
            || true,
            |_, _| {
                ids.deliver(
                    new.clone(),
                    StreamId(2),
                    output("early", 13),
                    &mut |_, _| true,
                );
                revoke(); // the captured batch retains its original lease stamp
                Ok::<_, ()>(subscribed(2, "", 0))
            },
            |_| {},
            &mut emit,
        );
        assert!(lost.is_empty());
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/private-reconnect-replay.json"
        ))
        .unwrap();
        assert_eq!(
            serde_json::Value::Array(payloads.lock().unwrap().clone()),
            expected
        );
    }

    #[test]
    fn rm4e_same_lifetime_reconnect_moves_ownership_and_tombstones_old_generation() {
        let ids = StreamIds::default();
        let viewport = ViewportId::new("webview");
        let old = StreamHost::Remote {
            server_id: "server-a".into(),
            epoch: "epoch".into(),
            generation: 1,
        };
        let new = StreamHost::Remote {
            server_id: "server-a".into(),
            epoch: "epoch".into(),
            generation: 2,
        };
        let local = ids.attach(
            old.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "old", 3),
        );
        let (sent, mut emit) = recorder();
        let lost = ids.resume_from_to(
            old.clone(),
            new.clone(),
            || true,
            |_, _| Ok::<_, ()>(subscribed(1, "old", 3)),
            |_| {},
            &mut emit,
        );
        assert!(lost.is_empty());
        ids.deliver(old, StreamId(1), output("late", 11), &mut emit);
        ids.deliver(new.clone(), StreamId(1), output("!", 8), &mut emit);
        assert_eq!(
            ids.attached_on(new),
            vec![(local, PtySessionId::new("same"), viewport)]
        );
        assert_eq!(*sent.borrow(), [(local, "!".into(), 8)]);
    }

    #[test]
    fn rm4e_delayed_old_generation_result_cannot_repoint_or_detach_reused_stream_id() {
        let ids = StreamIds::default();
        let viewport = ViewportId::new("webview");
        let old = StreamHost::Remote {
            server_id: "server-a".into(),
            epoch: "epoch".into(),
            generation: 11,
        };
        let current = StreamHost::Remote {
            server_id: "server-a".into(),
            epoch: "epoch".into(),
            generation: 12,
        };
        let local = ids.attach(
            old.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(7, "", 0),
        );
        let (_, mut emit) = recorder();
        assert_eq!(
            ids.repoint_from(
                local,
                &old,
                StreamId(7),
                current.clone(),
                &subscribed(7, "", 0),
                &mut emit,
            ),
            Repoint::Moved
        );
        assert_eq!(
            ids.repoint_from(
                local,
                &old,
                StreamId(7),
                old.clone(),
                &subscribed(8, "stale", 5),
                &mut emit,
            ),
            Repoint::Gone
        );
        assert!(!ids.detach_from(local, &old, StreamId(7)));
        assert_eq!(
            ids.attached_on(current),
            vec![(local, PtySessionId::new("same"), viewport)]
        );
    }

    /// Output a host pushes ahead of its attach reply is not kept for it (the
    /// snapshot covers that window); a detached stream's late
    /// output is dropped, never kept; and only so many retired streams are
    /// remembered.
    #[test]
    fn early_output_is_dropped_at_attach_and_retired_streams_are_bounded() {
        let ids = StreamIds::default();
        let (sent, mut emit) = recorder();
        ids.deliver(StreamHost::Sidecar, StreamId(3), output("bc", 3), &mut emit);
        assert_eq!(ids.table().early.len(), 1, "held for a re-point");
        let local = ids.attach(
            StreamHost::Sidecar,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(3, "ab", 2),
        );
        assert!(ids.table().early.is_empty());
        ids.detach(local);
        ids.deliver(
            StreamHost::Sidecar,
            StreamId(3),
            output("late", 7),
            &mut emit,
        );
        assert!(ids.table().early.is_empty(), "a retired stream is not kept");
        assert!(sent.borrow().is_empty());
        for stream in 0..(RETIRED_STREAMS as u64 + 10) {
            let local = ids.attach(
                StreamHost::InProcess,
                &PtySessionId::new("s"),
                &ViewportId::new("webview"),
                &subscribed(100 + stream, "", 0),
            );
            ids.detach(local);
        }
        assert_eq!(ids.table().retired.len(), RETIRED_STREAMS);
    }

    /// A batch the frontend could not take is not counted as sent.
    #[test]
    fn an_undelivered_batch_does_not_advance_the_watermark() {
        let ids = StreamIds::default();
        let local = ids.attach(
            StreamHost::InProcess,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(1, "", 0),
        );
        assert!(!ids.deliver(
            StreamHost::InProcess,
            StreamId(1),
            output("ab", 2),
            &mut |_, _| false
        ));
        let (sent, mut emit) = recorder();
        ids.deliver(
            StreamHost::InProcess,
            StreamId(1),
            output("abc", 3),
            &mut emit,
        );
        assert_eq!(*sent.borrow(), [(local, "abc".to_string(), 3)]);
    }

    /// A stream re-pointed at a new host stream whose snapshot is where the
    /// frontend left off continues without a gap or a duplicate: output the
    /// new stream pushed ahead of the re-point, then live output.
    #[test]
    fn a_repointed_stream_continues_without_gap_or_duplicate() {
        let ids = StreamIds::default();
        let (sent, mut emit) = recorder();
        let local = ids.attach(
            StreamHost::Sidecar,
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(1, "ab", 2),
        );
        ids.deliver(StreamHost::Sidecar, StreamId(1), output("cd", 4), &mut emit);
        // The connection is lost; the new stream pushes "efgh" before the
        // re-attach reply names it.
        ids.deliver(
            StreamHost::Sidecar,
            StreamId(9),
            output("efgh", 8),
            &mut emit,
        );
        assert_eq!(
            ids.repoint(local, &subscribed(9, "abcd", 4), &mut emit),
            Repoint::Moved
        );
        ids.deliver(
            StreamHost::Sidecar,
            StreamId(9),
            output("fghij", 10),
            &mut emit,
        );
        ids.deliver(
            StreamHost::Sidecar,
            StreamId(1),
            output("stale", 99),
            &mut emit,
        );
        let joined: String = sent
            .borrow()
            .iter()
            .map(|(_, data, _)| data.as_str())
            .collect();
        assert_eq!(joined, "cdefghij");
        ids.forget_host(StreamHost::Sidecar);
        assert!(ids.attached_on(StreamHost::Sidecar).is_empty());
        assert_eq!(
            ids.repoint(local, &subscribed(10, "", 0), &mut emit),
            Repoint::Gone
        );
    }

    /// A new stream whose snapshot holds output or a resize the frontend was
    /// never sent cannot continue the display: the stream is detached, to be
    /// rebuilt, and nothing is sent on it.
    #[test]
    fn a_repoint_past_the_watermark_detaches_for_a_rebuild() {
        for behind in [subscribed(9, "abcdef", 6), {
            let mut resized = subscribed(9, "abcd", 4);
            resized.snapshot.resize_gen = 1;
            resized
        }] {
            let ids = StreamIds::default();
            let (sent, mut emit) = recorder();
            let local = ids.attach(
                StreamHost::Sidecar,
                &PtySessionId::new("s"),
                &ViewportId::new("webview"),
                &subscribed(1, "abcd", 4),
            );
            ids.deliver(StreamHost::Sidecar, StreamId(9), output("gh", 8), &mut emit);
            assert_eq!(ids.repoint(local, &behind, &mut emit), Repoint::Behind);
            assert!(ids.attached_on(StreamHost::Sidecar).is_empty());
            ids.deliver(
                StreamHost::Sidecar,
                StreamId(1),
                output("late", 9),
                &mut emit,
            );
            assert!(sent.borrow().is_empty());
        }
    }

    #[test]
    fn resume_is_exactly_once_and_stale_or_detached_results_are_unsubscribed() {
        let ids = StreamIds::default();
        let host = StreamHost::remote("verified", "epoch");
        let viewport = ViewportId::new("webview");
        let (sent, mut emit) = recorder();
        let local = ids.attach(
            host.clone(),
            &PtySessionId::new("s"),
            &viewport,
            &subscribed(1, "ab", 2),
        );
        ids.deliver(host.clone(), StreamId(1), output("cd", 4), &mut emit);
        ids.deliver(host.clone(), StreamId(9), output("efgh", 8), &mut emit);
        let mut unsubscribed = Vec::new();
        let lost = ids.resume_on(
            host.clone(),
            || true,
            |_, _| Ok::<_, ()>(subscribed(9, "abcd", 4)),
            |stream| unsubscribed.push(stream),
            &mut emit,
        );
        assert!(lost.is_empty());
        // A generation invalidated while subscribe is in flight cannot
        // repoint or duplicate bytes, and its allocated stream is released.
        let mut generation_checks = 0;
        ids.resume_on(
            host.clone(),
            || {
                generation_checks += 1;
                generation_checks == 1
            },
            |_, _| Ok::<_, ()>(subscribed(10, "abcdefgh", 8)),
            |stream| unsubscribed.push(stream),
            &mut emit,
        );
        ids.deliver(host.clone(), StreamId(1), output("late", 99), &mut emit);
        let joined: String = sent
            .borrow()
            .iter()
            .map(|(_, data, _)| data.as_str())
            .collect();
        assert_eq!(joined, "cdefgh");
        assert_eq!(unsubscribed, [StreamId(10)]);

        // Detach after subscribe but before the second generation check wins.
        let detached = std::cell::Cell::new(false);
        let mut checks = 0;
        ids.resume_on(
            host,
            || {
                checks += 1;
                if checks == 2 {
                    ids.detach(local);
                    detached.set(true);
                }
                true
            },
            |_, _| Ok::<_, ()>(subscribed(11, "", 8)),
            |stream| unsubscribed.push(stream),
            &mut emit,
        );
        assert!(detached.get());
        assert_eq!(unsubscribed, [StreamId(10), StreamId(11)]);
    }

    #[test]
    fn stale_failed_resume_cannot_detach_the_retained_stream() {
        let ids = StreamIds::default();
        let host = StreamHost::remote("verified", "epoch");
        let local = ids.attach(
            host.clone(),
            &PtySessionId::new("s"),
            &ViewportId::new("webview"),
            &subscribed(1, "", 0),
        );
        let mut checks = 0;
        let lost = ids.resume_on(
            host.clone(),
            || {
                checks += 1;
                checks == 1
            },
            |_, _| Err::<SubscribeResult, _>(()),
            |_| panic!("a failed subscribe allocated no stream"),
            &mut |_, _| true,
        );
        assert!(lost.is_empty());
        assert_eq!(ids.attached_on(host).len(), 1);
        assert!(ids.detach(local).is_some());
    }

    #[test]
    fn changed_epoch_retires_only_old_lifetime_and_keeps_late_frames_dead() {
        let ids = StreamIds::default();
        let old = StreamHost::remote("server-a", "old");
        let new = StreamHost::remote("server-a", "new");
        let other = StreamHost::remote("server-b", "old");
        let viewport = ViewportId::new("webview");
        let old_local = ids.attach(
            old.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        let new_local = ids.attach(
            new.clone(),
            &PtySessionId::new("new"),
            &viewport,
            &subscribed(1, "", 0),
        );
        let other_local = ids.attach(
            other.clone(),
            &PtySessionId::new("same"),
            &viewport,
            &subscribed(1, "", 0),
        );
        assert_eq!(
            ids.retire_lifetime(old.clone()),
            Some(vec![PtySessionId::new("same")])
        );
        assert!(ids.attached_on(old.clone()).is_empty());
        assert_eq!(ids.attached_on(new.clone()).len(), 1);
        assert_eq!(ids.attached_on(other.clone()).len(), 1);
        let (sent, mut emit) = recorder();
        ids.deliver(old, StreamId(1), output("late", 4), &mut emit);
        ids.deliver(
            StreamHost::remote("server-a", "old"),
            StreamId(999),
            output("unseen-late", 11),
            &mut emit,
        );
        assert!(ids.table().early.is_empty());
        let mut resubscribed = false;
        assert!(ids
            .resume_on(
                StreamHost::remote("server-a", "old"),
                || true,
                |_, _| {
                    resubscribed = true;
                    Ok::<_, ()>(subscribed(99, "resurrected", 11))
                },
                |_| panic!("a retired lifetime cannot allocate a replacement stream"),
                &mut emit,
            )
            .is_empty());
        assert!(!resubscribed);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ids.attach(
                StreamHost::remote("server-a", "old"),
                &PtySessionId::new("resurrected"),
                &viewport,
                &subscribed(99, "", 11),
            )
        }))
        .is_err());
        assert!(ids.table().early.is_empty());
        ids.deliver(new, StreamId(1), output("new", 3), &mut emit);
        ids.deliver(other, StreamId(1), output("other", 5), &mut emit);
        assert_eq!(
            *sent.borrow(),
            [
                (new_local, "new".into(), 3),
                (other_local, "other".into(), 5)
            ]
        );
        assert_eq!(
            ids.retire_lifetime(StreamHost::remote("server-a", "old")),
            None
        );
        assert!(ids.detach(old_local).is_none());
        let detached_host = StreamHost::remote("server-c", "old");
        let detached = ids.attach(
            detached_host.clone(),
            &PtySessionId::new("detached"),
            &viewport,
            &subscribed(7, "", 0),
        );
        assert!(ids.detach(detached).is_some());
        assert_eq!(ids.retire_lifetime(detached_host), Some(Vec::new()));
    }

    /// Only bytes past the watermark are sent, from a whole character.
    #[test]
    fn past_trims_to_the_watermark_on_a_character_boundary() {
        assert_eq!(past("abc", 3, 0), Some("abc"));
        assert_eq!(past("abc", 3, 1), Some("bc"));
        assert_eq!(past("abc", 3, 3), None);
        assert_eq!(past("abc", 2, 5), None);
        // "é" is two bytes: a watermark inside it resumes after it.
        assert_eq!(past("xéy", 4, 2), Some("y"));
    }

    /// SW-WA-F11 — the sink→tracker wire. A session whose launch is registered
    /// in-flight has `pty_first_byte_ms` stamped by the output pipeline on
    /// its first non-empty `Output`. Refactor regression guard: this is the
    /// one production wire that makes `pty_first_byte_ms` work end-to-end;
    /// dropping it from `AppTerminalSink::emit` must fail this test.
    #[test]
    fn f11_sink_output_hook_stamps_first_byte_on_the_tracker() {
        use crate::commands::launch_telemetry::LaunchTelemetryTracker;
        use codesense_pty_core::domain::PtySessionId;
        use std::time::Instant;
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(LaunchTelemetryTracker::new());

        let session = "sess-wire";
        app.state::<LaunchTelemetryTracker>()
            .register_in_flight(session, Instant::now());

        // The free function the sink delegates to — same path, no sink setup.
        super::observe_output_first_byte(
            app.handle(),
            &TerminalDomainEvent::Output {
                session: PtySessionId::new(session.to_string()),
                data: "hi".into(),
                seq: 2,
            },
        );

        let tracker = app.state::<LaunchTelemetryTracker>();
        assert!(
            tracker.take_first_byte(session).is_some(),
            "sink→tracker wire must stamp pty_first_byte on the first non-empty Output"
        );
    }

    /// SW-WA-F11 — empty-data `Output` is a flush, not a byte. The hook must
    /// skip it so `pty_first_byte_ms` only times real bytes.
    #[test]
    fn f11_sink_output_hook_skips_empty_data_batches() {
        use crate::commands::launch_telemetry::LaunchTelemetryTracker;
        use codesense_pty_core::domain::PtySessionId;
        use std::time::Instant;
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(LaunchTelemetryTracker::new());

        let session = "sess-flush";
        app.state::<LaunchTelemetryTracker>()
            .register_in_flight(session, Instant::now());
        super::observe_output_first_byte(
            app.handle(),
            &TerminalDomainEvent::Output {
                session: PtySessionId::new(session.to_string()),
                data: String::new(),
                seq: 0,
            },
        );
        let tracker = app.state::<LaunchTelemetryTracker>();
        assert!(
            tracker.take_first_byte(session).is_none(),
            "empty-data Output must not stamp first byte"
        );
    }

    /// SW-WA-F11 — fast path. When no launch is in flight the hook must not
    /// acquire the tracker Mutex (observe_pty_first_byte is a no-op in that
    /// case anyway, but the fast path means we never even enter the Mutex).
    #[test]
    fn f11_sink_output_hook_is_a_noop_without_in_flight_launches() {
        use crate::commands::launch_telemetry::LaunchTelemetryTracker;
        use codesense_pty_core::domain::PtySessionId;
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(LaunchTelemetryTracker::new());
        // No `register_in_flight` call → has_in_flight() reads false.
        super::observe_output_first_byte(
            app.handle(),
            &TerminalDomainEvent::Output {
                session: PtySessionId::new("sess-x".to_string()),
                data: "hi".into(),
                seq: 2,
            },
        );
        let tracker = app.state::<LaunchTelemetryTracker>();
        assert!(tracker.take_first_byte("sess-x").is_none());
    }

    #[test]
    fn pbf210_a_pty_end_before_the_record_write_still_stores_the_conversation() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        for (n, session, event) in [
            (11u8, "late-tab-exit", TerminalDomainEvent::Exit {
                session: PtySessionId::new("late-tab-exit"), status: None,
                pool_member: false, requested: false, incarnation: 0,
            }),
            (12, "late-tab-removed", TerminalDomainEvent::SessionRemoved {
                session: PtySessionId::new("late-tab-removed"), reason: TeardownReason::Kill,
            }),
        ] {
            w.seed_claude(session, &nth_uuid(n));
            let store = crate::session_usage::UsageStore::open(&w.store).unwrap();
            let record = store.read_session_record(session).unwrap().unwrap();
            rusqlite::Connection::open(&w.store).unwrap()
                .execute("DELETE FROM session_record WHERE session_id = ?1", [session]).unwrap();
            let path = w.store.clone();
            let writer = std::thread::spawn(move || {
                crate::session_usage::record::wait_for_record_waiter(&path, session);
                crate::session_usage::record::persist_record_in(&path, &record).unwrap();
            });
            emit_local_legacy(app.handle(), &StreamHost::InProcess, event, false).unwrap();
            writer.join().unwrap();
            assert_eq!(w.stored(session).expect("an end before record persistence must retain its conversation").1.len(), 3);
        }
    }

    /// The real local event handler, driven with a PTY exit and a session removal: each stores the
    /// session's conversation, and a second delivery of either changes nothing.
    #[test]
    fn a_pty_exit_and_a_session_removal_each_store_the_conversation() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        let exit = |id: &str| TerminalDomainEvent::Exit {
            session: PtySessionId::new(id),
            status: None,
            pool_member: false,
            requested: false,
            incarnation: 0,
        };
        let removed = |id: &str| TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new(id),
            reason: TeardownReason::Kill,
        };
        for (n, session, event) in [
            (1u8, "tab-exit", exit("tab-exit")),
            (2, "tab-removed", removed("tab-removed")),
        ] {
            w.seed_claude(session, &nth_uuid(n));
            assert!(w.stored(session).is_none());
            emit_local_legacy(app.handle(), &StreamHost::InProcess, event.clone(), false).unwrap();
            let first = w.stored(session).unwrap_or_else(|| panic!("{session} stored by its event"));
            emit_local_legacy(app.handle(), &StreamHost::InProcess, event, false).unwrap();
            let again = w.stored(session).unwrap();
            assert_eq!(first.0.transcript, again.0.transcript, "{session}: a repeat changes nothing");
            assert_eq!(first.1, again.1);
        }
    }

}
