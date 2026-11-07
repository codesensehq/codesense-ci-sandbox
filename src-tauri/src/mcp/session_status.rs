// SPDX-License-Identifier: MIT
//! Retained task results alongside the MCP owners that produce them.
use crate::live_hub::effective_status::{awaiting_native_readiness, CompletionBoundary};
use codesense_pty_core::activity::{Snapshot, State};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use tokio::time::{Duration, Instant};

const QUIET_WINDOW: Duration = Duration::from_secs(45);
const LAUNCH_LIVENESS: Duration = Duration::from_secs(600);

struct SafetyNet {
    liveness_at: Instant,
    quiet_at: Option<Instant>,
    legacy_status: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ManagedOutcome {
    pub outcome: String,
    pub completion: Option<CompletionBoundary>,
    pub legacy_generation: u64,
    pub new_turn_seen: bool,
}
#[derive(Default)]
struct Results {
    outcomes: BTreeMap<String, ManagedOutcome>,
    turns: BTreeMap<String, CompletionBoundary>,
    nets: BTreeMap<String, SafetyNet>,
}
#[derive(Default)]
pub(crate) struct SessionOutcomes(Mutex<Results>);
impl SessionOutcomes {
    pub(crate) fn arm_launch(
        &self,
        session: &str,
        legacy_generation: u64,
        legacy_status: Option<&str>,
    ) {
        let mut results = self.0.lock().unwrap();
        let outcome = results
            .outcomes
            .entry(session.into())
            .or_insert_with(|| ManagedOutcome {
                outcome: "running".into(),
                completion: None,
                legacy_generation,
                new_turn_seen: false,
            });
        if matches!(outcome.outcome.as_str(), "running" | "awaiting-answer") {
            results.nets.entry(session.into()).or_insert(SafetyNet {
                liveness_at: Instant::now() + LAUNCH_LIVENESS,
                quiet_at: None,
                legacy_status: legacy_status.map(str::to_owned),
            });
        }
    }
    pub(crate) fn message_received(&self, session: &str) {
        if let Some(net) = self.0.lock().unwrap().nets.get_mut(session) {
            net.liveness_at = Instant::now() + LAUNCH_LIVENESS;
        }
    }
    pub(crate) fn session_died(&self, session: &str, legacy_generation: u64) {
        let mut results = self.0.lock().unwrap();
        let completion = results.turns.get(session).cloned();
        if let Some(outcome) = results.outcomes.get_mut(session) {
            if matches!(outcome.outcome.as_str(), "running" | "awaiting-answer") {
                outcome.outcome = "needs-attention".into();
                outcome.completion = completion;
                outcome.legacy_generation = legacy_generation;
                outcome.new_turn_seen = false;
            }
        }
        results.nets.remove(session);
    }
    /// The legacy viewer input changes only on admitted terminal-status events.
    /// An authoritative host clears it; host turns never reset launch liveness.
    pub(crate) fn legacy_status(&self, session: &str, status: Option<&str>) {
        if let Some(net) = self.0.lock().unwrap().nets.get_mut(session) {
            if net.legacy_status.as_deref() == status {
                return;
            }
            net.legacy_status = status.map(str::to_owned);
            net.quiet_at =
                matches!(status, Some("done" | "error")).then(|| Instant::now() + QUIET_WINDOW);
            if status == Some("running") {
                net.liveness_at = Instant::now() + LAUNCH_LIVENESS;
            }
        }
    }
    /// Architecture: these are the launch manager's event deadlines, sharing the
    /// hub's one sleep. Neither host turn state nor invocation exit produces them.
    pub(crate) fn reconcile_launch(
        &self,
        session: &str,
        snapshot: Option<&Snapshot>,
        legacy_generation: u64,
        pending_ask: bool,
    ) -> Option<Instant> {
        if snapshot.is_some_and(|s| s.observed || s.exit.is_some()) {
            self.legacy_status(session, None);
        }
        let now = Instant::now();
        let mut results = self.0.lock().unwrap();
        let net = results.nets.get_mut(session)?;
        let quiet_due = net.quiet_at.is_some_and(|at| at <= now);
        let liveness_due = net.liveness_at <= now;
        if quiet_due {
            net.quiet_at = None;
        }
        if liveness_due {
            net.liveness_at = now + LAUNCH_LIVENESS;
        }
        let deadline = net
            .quiet_at
            .map_or(net.liveness_at, |quiet| quiet.min(net.liveness_at));
        let outcome = results.outcomes.get_mut(session)?;
        // Policy: applyQuietSignal only acts from running, without working subagents.
        // A session still awaiting native readiness is not dropped: the startup
        // signal names it, sooner and in the right words.
        if (quiet_due || liveness_due)
            && outcome.outcome == "running"
            && !pending_ask
            && !snapshot.is_some_and(awaiting_native_readiness)
            && !snapshot.is_some_and(|s| s.subagents_open > 0)
        {
            outcome.outcome = "needs-attention".into();
            outcome.completion = snapshot.map(|s| CompletionBoundary {
                identity: s.identity.clone(),
                turn_generation: s.turn_generation,
                revision: s.revision,
            });
            outcome.legacy_generation = legacy_generation;
            outcome.new_turn_seen = false;
            results.nets.remove(session);
            return None;
        }
        Some(deadline)
    }
    pub(crate) fn record(
        &self,
        session: &str,
        status: &str,
        completion: Option<CompletionBoundary>,
        legacy_generation: u64,
    ) {
        let outcome = match status {
            "success" => "done",
            "failed" => "failed",
            "needs-attention" => "needs-attention",
            _ => return,
        };
        let mut results = self.0.lock().unwrap();
        if let Some((incoming, retained)) = completion.as_ref().zip(
            results
                .outcomes
                .get(session)
                .and_then(|o| o.completion.as_ref()),
        ) {
            if incoming.identity == retained.identity
                && (incoming.turn_generation, incoming.revision)
                    <= (retained.turn_generation, retained.revision)
            {
                return;
            }
        }
        let new_turn_seen = completion
            .as_ref()
            .zip(results.turns.get(session))
            .is_some_and(|(completed, turn)| {
                turn.identity == completed.identity
                    && turn.turn_generation > completed.turn_generation
                    && turn.revision > completed.revision
            });
        results.outcomes.insert(
            session.into(),
            ManagedOutcome {
                outcome: outcome.into(),
                completion,
                legacy_generation,
                new_turn_seen,
            },
        );
        results.nets.remove(session);
    }
    pub(crate) fn ask_opened(&self, session: &str) {
        let mut results = self.0.lock().unwrap();
        let outcome = results
            .outcomes
            .entry(session.into())
            .or_insert_with(|| ManagedOutcome {
                outcome: "running".into(),
                completion: None,
                legacy_generation: 0,
                new_turn_seen: false,
            });
        outcome.outcome = "awaiting-answer".into();
    }
    pub(crate) fn reconcile_asks(&self, pending: &BTreeSet<String>) {
        for (session, outcome) in &mut self.0.lock().unwrap().outcomes {
            if outcome.outcome == "awaiting-answer" && !pending.contains(session) {
                outcome.outcome = "running".into();
            }
        }
    }
    pub(crate) fn observe(&self, snapshot: &Snapshot) {
        let mut results = self.0.lock().unwrap();
        if snapshot.observed
            && snapshot.exit.is_none()
            && matches!(
                snapshot.state,
                State::Thinking | State::Working | State::NeedsPermission
            )
        {
            results.turns.insert(
                snapshot.session.clone(),
                CompletionBoundary {
                    identity: snapshot.identity.clone(),
                    turn_generation: snapshot.turn_generation,
                    revision: snapshot.revision,
                },
            );
        }
        if let Some(outcome) = results.outcomes.get_mut(&snapshot.session) {
            outcome.new_turn_seen |= outcome
                .completion
                .as_ref()
                .is_some_and(|c| c.followed_by(snapshot));
        }
    }
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> BTreeMap<String, ManagedOutcome> {
        self.0.lock().unwrap().outcomes.clone()
    }
    pub(crate) fn get(&self, session: &str) -> Option<ManagedOutcome> {
        self.0.lock().unwrap().outcomes.get(session).cloned()
    }
    pub(crate) fn retain(&self, sessions: &BTreeSet<String>) {
        let mut results = self.0.lock().unwrap();
        results.outcomes.retain(|id, _| sessions.contains(id));
        results.turns.retain(|id, _| sessions.contains(id));
        results.nets.retain(|id, _| sessions.contains(id));
    }
}
