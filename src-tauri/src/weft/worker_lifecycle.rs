// SPDX-License-Identifier: MIT
//! SW-WV-A3 — the formally-checked worker lifecycle state machine + the single
//! ordered `signal_done` hook registry.
//!
//! # State machine (built ON `wv_roster.status`, not a second column)
//!
//! A2 landed the worker lifecycle on the existing `wv_roster.status` column
//! ([`super::sqlite::WV_WORKER_LIVE_STATES`], `wv_provision_worker_row`,
//! `wv_dispatch_worker_row`). A3 canonicalises that ad-hoc string column into a
//! typed enum + a table-driven transition guard and adds the two missing
//! transitions (`signal`, `handoff`, `deprovision`) — it does NOT introduce a
//! second `lifecycle_state` column (that would be the shadow-state divergence
//! Task 7 forbids). The five wire values are the exact strings A2 already writes:
//!
//! ```text
//! provision → dispatch → signal → [handoff] → deprovision
//!                          └───────────────────→ (handoff optional)
//! ```
//!
//! Every transition is guarded twice: once by [`check_transition`] (the pure
//! table lookup, so an illegal step is refused with a typed error before the SQL)
//! and once by the CAS `WHERE status=<expected>` + `changes()==1` guard in the
//! store handler (the load-bearing one-winner belt, ARCHITECTURE §1.2). A
//! concurrent second transition on the same row loses the CAS and surfaces the
//! same [`LifecycleError::IllegalTransition`].
//!
//! # `SignalDoneHooks` registry (A3 sole owner — D6/D13)
//!
//! A3 CREATES the one ordered [`SignalDoneHooks`] registry invoked once at the end
//! of the `signal_done` handler, registers the roster-update hook FIRST, and owns
//! the canonical order-assertion test. A4/B4/C1/C3 register hooks only — they do
//! not define the registry or assert order. The registry dispatch is spawned
//! (`tokio::spawn`) from `signal_done` so the handler returns before any hook's
//! future resolves (non-blocking hot path); each hook is isolated behind its own
//! `Result` so a failure in hook N does not skip N+1.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The five reachable worker lifecycle states (AC-1). The wire form is the exact
/// string A2 already writes into `wv_roster.status` — this enum is the typed gate
/// over that column, never a second column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerLifecycleState {
    /// Slot allocated, worktree live, no task yet (`wv_worker_provision`, A2).
    Provisioned,
    /// 4-field brief sent, worker executing (`wv_worker_dispatch`, C1 wires the brief).
    Dispatched,
    /// `signal_done` received (success / failed / needs-attention).
    Signalled,
    /// Optional — worker wrote a handoff doc before deprovision.
    HandoffWritten,
    /// Worktree torn down, slot freed.
    Deprovisioned,
}

impl WorkerLifecycleState {
    /// The wire string stored in `wv_roster.status` — the value A2 already writes.
    pub fn as_wire(self) -> &'static str {
        match self {
            WorkerLifecycleState::Provisioned => "provision",
            WorkerLifecycleState::Dispatched => "dispatch",
            WorkerLifecycleState::Signalled => "signal",
            WorkerLifecycleState::HandoffWritten => "handoff",
            WorkerLifecycleState::Deprovisioned => "deprovision",
        }
    }

    /// Parse a `wv_roster.status` wire string; `None` for any unknown token.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "provision" => Some(WorkerLifecycleState::Provisioned),
            "dispatch" => Some(WorkerLifecycleState::Dispatched),
            "signal" => Some(WorkerLifecycleState::Signalled),
            "handoff" => Some(WorkerLifecycleState::HandoffWritten),
            "deprovision" => Some(WorkerLifecycleState::Deprovisioned),
            _ => None,
        }
    }
}

/// The complete set of legal `(from, to)` transition pairs (AC-1). Table-driven —
/// the guard is a single membership test over this slice, never scattered
/// if/else. Anything not listed here is an illegal transition.
///
/// ```text
/// provisioned → dispatched
/// dispatched  → signalled
/// signalled   → handoff_written   (optional handoff)
/// signalled   → deprovisioned     (handoff skipped)
/// handoff_written → deprovisioned
/// ```
pub const LEGAL_TRANSITIONS: &[(WorkerLifecycleState, WorkerLifecycleState)] = &[
    (
        WorkerLifecycleState::Provisioned,
        WorkerLifecycleState::Dispatched,
    ),
    (
        WorkerLifecycleState::Dispatched,
        WorkerLifecycleState::Signalled,
    ),
    (
        WorkerLifecycleState::Signalled,
        WorkerLifecycleState::HandoffWritten,
    ),
    (
        WorkerLifecycleState::Signalled,
        WorkerLifecycleState::Deprovisioned,
    ),
    (
        WorkerLifecycleState::HandoffWritten,
        WorkerLifecycleState::Deprovisioned,
    ),
];

/// The four closure-check conditions guarding a `deprovision` (AC-3). Each variant
/// names the one condition that was unmet so the caller can surface exactly which
/// gate refused the teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureCondition {
    /// No `signal_done` recorded — the worker is not in `signal`/`handoff` yet.
    SignalReceived,
    /// The operator did not confirm the packet was delivered to its canonical home.
    PacketDelivered,
    /// This worker's scope still has open (`state <> 'resolved'`) `wv_queue` rows.
    NoOpenFollowups,
    /// The worker is `lifecycle='offered'` — closing it needs an explicit override.
    NotOffered,
}

impl ClosureCondition {
    /// A stable wire token for the unsatisfied condition (surfaced in the error).
    pub fn as_wire(self) -> &'static str {
        match self {
            ClosureCondition::SignalReceived => "signal-received",
            ClosureCondition::PacketDelivered => "packet-delivered",
            ClosureCondition::NoOpenFollowups => "no-open-followups",
            ClosureCondition::NotOffered => "not-offered",
        }
    }
}

/// The typed refusals the lifecycle handlers raise (AC-1, AC-3). Distinct from a
/// storage error and from an ordinary CAS loss — a caller maps each to the right
/// MCP/Tauri outcome (`invalid_params` / a named refusal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// The requested `(from → to)` step is not in [`LEGAL_TRANSITIONS`], OR the row
    /// was not in the expected predecessor state when the CAS ran (a concurrent
    /// transition won). `from`/`to` are the wire strings for a legible message.
    IllegalTransition {
        from: &'static str,
        to: &'static str,
    },
    /// A `deprovision` was attempted with one of the four closure conditions unmet.
    ClosureCheckFailed { condition: ClosureCondition },
    /// A `deprovision` of an `lifecycle='offered'` worker was attempted without the
    /// explicit `force=true` override (AC-3).
    OfferedWorkerProtected,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::IllegalTransition { from, to } => {
                write!(f, "illegal worker lifecycle transition: {from} → {to}")
            }
            LifecycleError::ClosureCheckFailed { condition } => write!(
                f,
                "worker closure check failed: {} unsatisfied",
                condition.as_wire()
            ),
            LifecycleError::OfferedWorkerProtected => write!(
                f,
                "offered worker is protected — deprovision requires an explicit force override"
            ),
        }
    }
}

impl std::error::Error for LifecycleError {}

/// Table-driven transition guard (AC-1). Returns `Ok(())` iff `(from, to)` is a
/// member of [`LEGAL_TRANSITIONS`]; otherwise a typed
/// [`LifecycleError::IllegalTransition`] carrying the wire strings. Pure — no I/O.
/// The store handlers call this BEFORE the SQL so an illegal step is refused at the
/// boundary; the CAS guard is the concurrent-winner belt on top.
pub fn check_transition(
    from: WorkerLifecycleState,
    to: WorkerLifecycleState,
) -> Result<(), LifecycleError> {
    if LEGAL_TRANSITIONS.contains(&(from, to)) {
        Ok(())
    } else {
        Err(LifecycleError::IllegalTransition {
            from: from.as_wire(),
            to: to.as_wire(),
        })
    }
}

// ---------------------------------------------------------------------------
// SignalDoneHooks registry (AC-4, D6/D13 — A3 sole owner)
// ---------------------------------------------------------------------------

/// The boxed future a hook returns. `'static + Send` so it can be driven from the
/// `tokio::spawn`ed dispatch task off the `signal_done` hot path.
pub type HookFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;

/// One registered `signal_done` hook: a named, cloneable async closure. The name
/// is carried so the canonical order-assertion test (owned by A3) can assert the
/// exact execution order by name.
#[derive(Clone)]
pub struct SignalDoneHook {
    /// The canonical hook name (`"roster-update"`, `"dispatch-done-stamp"`, …).
    pub name: &'static str,
    run: Arc<dyn Fn(String) -> HookFuture + Send + Sync>,
}

impl SignalDoneHook {
    /// Build a hook from a name and an async factory taking the `session_id`.
    pub fn new<F>(name: &'static str, run: F) -> Self
    where
        F: Fn(String) -> HookFuture + Send + Sync + 'static,
    {
        SignalDoneHook {
            name,
            run: Arc::new(run),
        }
    }
}

impl std::fmt::Debug for SignalDoneHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalDoneHook")
            .field("name", &self.name)
            .finish()
    }
}

/// The ONE ordered `signal_done` hook registry (A3 sole owner — D6/D13).
///
/// Hooks execute in registration order; the canonical order A3 asserts is
/// `roster-update → dispatch-done-stamp → done-record → watchdog-cancel →
/// wv_signals-insert`. A4/B4/C1/C3 [`register`](Self::register) their hook into
/// this list — they never re-order or re-assert. Each hook runs behind its own
/// `Result` so a failure in one does not skip the next
/// ([`dispatch_hooks`](Self::dispatch_hooks)).
#[derive(Clone, Default, Debug)]
pub struct SignalDoneHooks {
    hooks: Vec<SignalDoneHook>,
}

impl SignalDoneHooks {
    /// A fresh empty registry.
    pub fn new() -> Self {
        SignalDoneHooks { hooks: Vec::new() }
    }

    /// Append a hook (registration order == execution order). Returns `self` so a
    /// call site can chain the canonical registration once at startup.
    pub fn register(mut self, hook: SignalDoneHook) -> Self {
        self.hooks.push(hook);
        self
    }

    /// The registered hook names in execution order (for the order-assertion test
    /// and diagnostics).
    pub fn names(&self) -> Vec<&'static str> {
        self.hooks.iter().map(|h| h.name).collect()
    }

    /// Number of registered hooks.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run every registered hook IN ORDER, awaiting each before the next, each
    /// isolated behind its own `Result` (a hook returning `Err` is logged and the
    /// next still runs — AC-4). Consumes a clone of the registry so the future is
    /// `'static` and drivable from the spawned dispatch task.
    ///
    /// This is the future the `signal_done` handler `tokio::spawn`s — the handler
    /// returns before this resolves, so the completion hot path is never blocked.
    pub async fn dispatch_hooks(self, session_id: String) {
        for hook in self.hooks {
            let name = hook.name;
            let fut = (hook.run)(session_id.clone());
            if let Err(e) = fut.await {
                log::warn!(
                    target: "weft",
                    "signal_done hook {name:?} failed (continuing to next hook): {e}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    // ---- AC-1: pure transition table (the store-seam illegal-transition tests
    // that exercise the CAS live in `sqlite.rs`; these cover the table itself). ----

    #[test]
    fn happy_path_transitions_are_all_legal() {
        use WorkerLifecycleState::*;
        assert!(check_transition(Provisioned, Dispatched).is_ok());
        assert!(check_transition(Dispatched, Signalled).is_ok());
        assert!(check_transition(Signalled, HandoffWritten).is_ok());
        assert!(check_transition(Signalled, Deprovisioned).is_ok());
        assert!(check_transition(HandoffWritten, Deprovisioned).is_ok());
    }

    #[test]
    fn illegal_transitions_are_refused_with_typed_error() {
        use WorkerLifecycleState::*;
        // The spine's named illegal examples + the obvious skips/backwards steps.
        let illegal = [
            (Provisioned, Deprovisioned), // skip dispatch
            (Provisioned, Signalled),     // skip dispatch
            (Dispatched, Provisioned),    // backwards
            (Signalled, Dispatched),      // re-dispatch without re-provision
            (Signalled, Provisioned),     // backwards
            (HandoffWritten, Dispatched), // backwards from handoff
            (HandoffWritten, Signalled),  // backwards from handoff
            (Deprovisioned, Dispatched),  // terminal → anything
            (Deprovisioned, Signalled),   // terminal → anything
            (Deprovisioned, Provisioned), // terminal → anything
            (Provisioned, Provisioned),   // self-loop
            (Dispatched, Dispatched),     // self-loop
        ];
        for (from, to) in illegal {
            let err = check_transition(from, to).unwrap_err();
            assert_eq!(
                err,
                LifecycleError::IllegalTransition {
                    from: from.as_wire(),
                    to: to.as_wire(),
                },
                "{from:?} → {to:?} must be refused"
            );
        }
    }

    #[test]
    fn wire_roundtrip_is_stable() {
        use WorkerLifecycleState::*;
        for s in [
            Provisioned,
            Dispatched,
            Signalled,
            HandoffWritten,
            Deprovisioned,
        ] {
            assert_eq!(WorkerLifecycleState::from_wire(s.as_wire()), Some(s));
        }
        assert_eq!(WorkerLifecycleState::from_wire("bogus"), None);
    }

    // ---- AC-4 (i): non-blocking dispatch + per-hook isolation. ----

    #[tokio::test]
    async fn dispatch_returns_before_a_blocking_hook_resolves_and_preserves_order() {
        // hook 1 blocks on a oneshot until the test releases it; hook 2 is a counter.
        let (tx, rx) = oneshot::channel::<()>();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let rx_cell = Arc::new(Mutex::new(Some(rx)));

        let order1 = order.clone();
        let hook1 = SignalDoneHook::new("blocker", move |_sid| {
            let order1 = order1.clone();
            let rx_cell = rx_cell.clone();
            Box::pin(async move {
                let rx = rx_cell.lock().unwrap().take().expect("rx once");
                let _ = rx.await;
                order1.lock().unwrap().push("blocker");
                Ok(())
            }) as HookFuture
        });

        let order2 = order.clone();
        let hook2 = SignalDoneHook::new("counter", move |_sid| {
            let order2 = order2.clone();
            Box::pin(async move {
                order2.lock().unwrap().push("counter");
                Ok(())
            }) as HookFuture
        });

        let registry = SignalDoneHooks::new().register(hook1).register(hook2);
        // Spawn the dispatch exactly as `signal_done` does — the handler (here, this
        // test body) proceeds immediately.
        let handle = tokio::spawn(registry.dispatch_hooks("sess-1".to_string()));

        // The dispatch is still parked on hook 1's receiver → nothing has run yet.
        tokio::task::yield_now().await;
        assert!(
            order.lock().unwrap().is_empty(),
            "dispatch must not have completed any hook while hook 1 blocks"
        );

        // Release hook 1; now both run, IN ORDER.
        tx.send(()).unwrap();
        handle.await.unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["blocker", "counter"]);
    }

    #[tokio::test]
    async fn a_failing_hook_does_not_skip_the_next() {
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let order_a = order.clone();
        let failing = SignalDoneHook::new("failing", move |_sid| {
            let order_a = order_a.clone();
            Box::pin(async move {
                order_a.lock().unwrap().push("failing");
                Err("boom".to_string())
            }) as HookFuture
        });
        let order_b = order.clone();
        let after = SignalDoneHook::new("after", move |_sid| {
            let order_b = order_b.clone();
            Box::pin(async move {
                order_b.lock().unwrap().push("after");
                Ok(())
            }) as HookFuture
        });

        SignalDoneHooks::new()
            .register(failing)
            .register(after)
            .dispatch_hooks("s".to_string())
            .await;

        assert_eq!(
            *order.lock().unwrap(),
            vec!["failing", "after"],
            "a failing hook is isolated; the next hook still runs"
        );
    }

    // ---- AC-4 (ii): the CANONICAL order-assertion test — owned by A3, NOT
    // duplicated by A4/B4/C1/C3. ----

    /// The canonical `signal_done` hook execution order (AC-4, ARCHITECTURE §4.3).
    /// Downstream stories register INTO this order; only A3 asserts it.
    const CANONICAL_HOOK_ORDER: &[&str] = &[
        "roster-update",
        "dispatch-done-stamp",
        "done-record",
        // SW-WV-C4 — the handoff-record hook advances `signal → handoff` when the worker
        // reported a packet; it slots after `done-record` (so `roster-update` has set
        // `signal`) and before the terminal watchdog/signals hooks.
        "handoff-record",
        "watchdog-cancel",
        "wv_signals-insert",
    ];

    #[tokio::test]
    async fn canonical_hook_order_is_enforced_a3_owns_this_test() {
        let seen = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let mut registry = SignalDoneHooks::new();
        for name in CANONICAL_HOOK_ORDER {
            let seen = seen.clone();
            let name: &'static str = name;
            registry = registry.register(SignalDoneHook::new(name, move |_sid| {
                let seen = seen.clone();
                Box::pin(async move {
                    seen.lock().unwrap().push(name);
                    Ok(())
                }) as HookFuture
            }));
        }
        // Registration order is the declared canonical order.
        assert_eq!(registry.names(), CANONICAL_HOOK_ORDER.to_vec());
        registry.dispatch_hooks("sess".to_string()).await;
        assert_eq!(
            *seen.lock().unwrap(),
            CANONICAL_HOOK_ORDER.to_vec(),
            "hooks must run in the canonical order"
        );
    }

    #[test]
    fn closure_condition_wire_tokens_are_stable() {
        assert_eq!(
            ClosureCondition::SignalReceived.as_wire(),
            "signal-received"
        );
        assert_eq!(
            ClosureCondition::PacketDelivered.as_wire(),
            "packet-delivered"
        );
        assert_eq!(
            ClosureCondition::NoOpenFollowups.as_wire(),
            "no-open-followups"
        );
        assert_eq!(ClosureCondition::NotOffered.as_wire(), "not-offered");
    }
}
