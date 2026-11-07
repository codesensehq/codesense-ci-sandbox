// SPDX-License-Identifier: MIT
//! The one lifecycle-owner registry and generation fence for an app-wide
//! Principal switch (SW-PRN-2 consumes this).
//!
//! Every process-global owner of principal-scoped state implements
//! [`LifecycleOwner`] and registers once. The registry drives the same
//! coordinated sequence the existing team switch already runs, at Principal
//! scope: preflight → quiesce at a new generation fence → commit the durable
//! pointer (caller-supplied, the single commit point) → hydrate. A failure
//! before commit rolls back via `recover`; a stale event from a prior generation
//! is rejected by [`LifecycleRegistry::accepts`].

use std::fmt;

use crate::domain::Principal;

/// Monotonic switch generation. Every switch mints the next value; owners tag
/// their in-flight work with it so a late event from an older generation is
/// detectably stale. The default (`0`) is the initial generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Generation(pub u64);

impl Generation {
    pub const INITIAL: Generation = Generation(0);

    pub fn next(self) -> Generation {
        // Saturating so the monotonic fence can never wrap to a prior value
        // (unreachable in practice — 2^64 switches — but keeps the invariant total).
        Generation(self.0.saturating_add(1))
    }
}

/// Non-rollbackable work a switch would disrupt. Surfaced by preflight so the UI
/// can obtain explicit consent (SW-PRN-2 AC-4) before any teardown begins —
/// CodeSense never silently abandons or reattributes this work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consequence {
    LivePty { count: usize },
    SpawnedAgent { count: usize },
    RunningAutomation { count: usize },
    UnsavedEdits { count: usize },
}

/// An immutable snapshot every reader (IPC, MCP, schedulers, windows) observes so
/// the whole app process agrees on the active Principal and its generation
/// (INV-2, mirroring `ActiveTeamState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalContext {
    pub principal: Principal,
    pub generation: Generation,
}

/// One process-global owner of principal-scoped state.
pub trait LifecycleOwner {
    fn name(&self) -> &'static str;

    /// Report non-rollbackable work that switching to `target` would disrupt.
    /// Read-only — must not begin teardown.
    fn preflight(&self, target: &Principal) -> Result<Vec<Consequence>, LifecycleError>;

    /// Stop accepting new work and drain/serialize in-flight work, tagging it
    /// with `fence` so later events can be recognized as stale.
    fn quiesce(&mut self, fence: Generation) -> Result<(), LifecycleError>;

    /// Rebuild state for `target` at `generation` after the pointer commit.
    fn hydrate(&mut self, target: &Principal, generation: Generation)
        -> Result<(), LifecycleError>;

    /// Restore the owner to the prior Principal after a pre-commit failure.
    fn recover(&mut self) -> Result<(), LifecycleError>;
}

/// Failure from an owner during a coordinated switch. Carries the owner name so
/// the coordinator can report exactly which subsystem blocked the switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleError {
    pub owner: &'static str,
    pub stage: LifecycleStage,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStage {
    Preflight,
    Quiesce,
    /// The coordinator's durable pointer commit (not an owner stage).
    Commit,
    Hydrate,
    Recover,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lifecycle owner {} failed at {:?}: {}",
            self.owner, self.stage, self.detail
        )
    }
}

impl std::error::Error for LifecycleError {}

/// Outcome of a failed switch: whether the prior Principal is still fully active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchFailure {
    /// Failure occurred before the commit point; the prior Principal is intact.
    RolledBack(LifecycleError),
    /// Failure occurred after the commit point; recovery must complete forward.
    RecoveryRequired(LifecycleError),
}

/// Owner label for a coordinator-level failure — the pointer commit itself,
/// which belongs to no registered owner.
const COORDINATOR: &str = "coordinator";

/// The single coordinator over all registered owners.
#[derive(Default)]
pub struct LifecycleRegistry {
    owners: Vec<Box<dyn LifecycleOwner>>,
    current: Generation,
}

impl LifecycleRegistry {
    pub fn new() -> Self {
        Self {
            owners: Vec::new(),
            current: Generation::INITIAL,
        }
    }

    pub fn register(&mut self, owner: Box<dyn LifecycleOwner>) {
        self.owners.push(owner);
    }

    pub fn generation(&self) -> Generation {
        self.current
    }

    /// Whether an event tagged `event_gen` belongs to the current generation. A
    /// late event from a prior switch is rejected (INV-2 generation fence).
    pub fn accepts(&self, event_gen: Generation) -> bool {
        event_gen == self.current
    }

    /// Aggregate preflight consequences across all owners for consent handling.
    pub fn preflight(&self, target: &Principal) -> Result<Vec<Consequence>, LifecycleError> {
        let mut all = Vec::new();
        for owner in &self.owners {
            all.extend(owner.preflight(target)?);
        }
        Ok(all)
    }

    /// Coordinated switch. `commit_pointer` performs the single durable commit
    /// (e.g. the catalog active-pointer write) between quiesce and hydrate.
    ///
    /// Pre-commit failure rolls every owner back and returns
    /// [`SwitchFailure::RolledBack`]; the current generation is unchanged.
    /// Post-commit failure returns [`SwitchFailure::RecoveryRequired`].
    pub fn switch(
        &mut self,
        target: &Principal,
        commit_pointer: impl FnOnce() -> Result<(), String>,
    ) -> Result<PrincipalContext, SwitchFailure> {
        let fence = self.current.next();

        // Quiesce every owner at the new fence. Any failure here is pre-commit:
        // roll back and leave the generation untouched.
        for i in 0..self.owners.len() {
            if let Err(e) = self.owners[i].quiesce(fence) {
                self.recover_all();
                return Err(SwitchFailure::RolledBack(e));
            }
        }

        // The single durable commit point.
        if let Err(detail) = commit_pointer() {
            self.recover_all();
            return Err(SwitchFailure::RolledBack(LifecycleError {
                owner: COORDINATOR,
                stage: LifecycleStage::Commit,
                detail,
            }));
        }

        // Past the commit point: advance the generation, then hydrate forward.
        self.current = fence;
        for i in 0..self.owners.len() {
            if let Err(e) = self.owners[i].hydrate(target, fence) {
                return Err(SwitchFailure::RecoveryRequired(e));
            }
        }

        Ok(PrincipalContext {
            principal: target.clone(),
            generation: fence,
        })
    }

    fn recover_all(&mut self) {
        for owner in &mut self.owners {
            // Best-effort restoration of the prior Principal; a recover failure
            // does not mask the original pre-commit error.
            let _ = owner.recover();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Log {
        calls: Rc<RefCell<Vec<String>>>,
    }
    impl Log {
        fn push(&self, s: &str) {
            self.calls.borrow_mut().push(s.to_string());
        }
        fn seq(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    #[derive(Default)]
    struct RecordingOwner {
        name: &'static str,
        log: Log,
        consequences: Vec<Consequence>,
        fail_quiesce: bool,
        fail_hydrate: bool,
        fail_recover: bool,
    }
    impl RecordingOwner {
        fn err(&self, stage: LifecycleStage) -> LifecycleError {
            LifecycleError {
                owner: self.name,
                stage,
                detail: "injected".to_string(),
            }
        }
    }
    impl LifecycleOwner for RecordingOwner {
        fn name(&self) -> &'static str {
            self.name
        }
        fn preflight(&self, _t: &Principal) -> Result<Vec<Consequence>, LifecycleError> {
            Ok(self.consequences.clone())
        }
        fn quiesce(&mut self, _fence: Generation) -> Result<(), LifecycleError> {
            self.log.push(&format!("quiesce:{}", self.name));
            if self.fail_quiesce {
                return Err(self.err(LifecycleStage::Quiesce));
            }
            Ok(())
        }
        fn hydrate(&mut self, _t: &Principal, _g: Generation) -> Result<(), LifecycleError> {
            self.log.push(&format!("hydrate:{}", self.name));
            if self.fail_hydrate {
                return Err(self.err(LifecycleStage::Hydrate));
            }
            Ok(())
        }
        fn recover(&mut self) -> Result<(), LifecycleError> {
            self.log.push(&format!("recover:{}", self.name));
            if self.fail_recover {
                return Err(self.err(LifecycleStage::Recover));
            }
            Ok(())
        }
    }

    fn owner(name: &'static str, log: &Log) -> Box<RecordingOwner> {
        Box::new(RecordingOwner {
            name,
            log: log.clone(),
            ..Default::default()
        })
    }

    #[test]
    fn happy_switch_quiesces_then_commits_then_hydrates() {
        let log = Log::default();
        let mut reg = LifecycleRegistry::new();
        reg.register(owner("a", &log));
        reg.register(owner("b", &log));
        let committed = Rc::new(RefCell::new(false));
        let c = committed.clone();

        let ctx = reg
            .switch(&Principal::from("t"), move || {
                *c.borrow_mut() = true;
                Ok(())
            })
            .unwrap();

        assert_eq!(ctx.principal, Principal::from("t"));
        assert_eq!(ctx.generation, Generation(1));
        assert!(*committed.borrow());
        // Quiesce both, then hydrate both — commit lands between the two groups.
        assert_eq!(
            log.seq(),
            vec!["quiesce:a", "quiesce:b", "hydrate:a", "hydrate:b"]
        );
    }

    #[test]
    fn pre_commit_quiesce_failure_rolls_back_and_keeps_generation() {
        let log = Log::default();
        let mut reg = LifecycleRegistry::new();
        reg.register(owner("a", &log));
        reg.register(Box::new(RecordingOwner {
            name: "b",
            log: log.clone(),
            fail_quiesce: true,
            ..Default::default()
        }));
        let committed = Rc::new(RefCell::new(false));
        let c = committed.clone();

        let err = reg
            .switch(&Principal::from("t"), move || {
                *c.borrow_mut() = true;
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(err, SwitchFailure::RolledBack(_)));
        assert!(!*committed.borrow(), "pointer never committed");
        assert_eq!(
            reg.generation(),
            Generation::INITIAL,
            "generation unchanged"
        );
        // Both owners recover after the failed quiesce.
        assert!(log.seq().contains(&"recover:a".to_string()));
        assert!(log.seq().contains(&"recover:b".to_string()));
    }

    #[test]
    fn commit_failure_rolls_back() {
        let log = Log::default();
        let mut reg = LifecycleRegistry::new();
        reg.register(owner("a", &log));

        let err = reg
            .switch(&Principal::from("t"), || Err("disk full".to_string()))
            .unwrap_err();

        match err {
            SwitchFailure::RolledBack(e) => assert_eq!(e.detail, "disk full"),
            other => panic!("expected rollback, got {other:?}"),
        }
        assert_eq!(reg.generation(), Generation::INITIAL);
    }

    #[test]
    fn stale_generation_events_are_rejected_after_switch() {
        let mut reg = LifecycleRegistry::new();
        reg.register(owner("a", &Log::default()));
        let before = reg.generation();
        reg.switch(&Principal::from("t"), || Ok(())).unwrap();

        assert!(!reg.accepts(before), "prior-generation event is stale");
        assert!(reg.accepts(reg.generation()));
    }

    #[test]
    fn preflight_aggregates_consequences_for_consent() {
        let mut reg = LifecycleRegistry::new();
        reg.register(Box::new(RecordingOwner {
            name: "pty",
            consequences: vec![Consequence::LivePty { count: 2 }],
            ..Default::default()
        }));
        reg.register(Box::new(RecordingOwner {
            name: "auto",
            consequences: vec![Consequence::RunningAutomation { count: 1 }],
            ..Default::default()
        }));

        let consequences = reg.preflight(&Principal::from("t")).unwrap();
        assert_eq!(
            consequences,
            vec![
                Consequence::LivePty { count: 2 },
                Consequence::RunningAutomation { count: 1 },
            ]
        );
    }

    #[test]
    fn post_commit_hydrate_failure_requires_recovery() {
        let log = Log::default();
        let mut reg = LifecycleRegistry::new();
        reg.register(owner("a", &log));
        reg.register(Box::new(RecordingOwner {
            name: "b",
            log: log.clone(),
            fail_hydrate: true,
            ..Default::default()
        }));
        let committed = Rc::new(RefCell::new(false));
        let c = committed.clone();

        let err = reg
            .switch(&Principal::from("t"), move || {
                *c.borrow_mut() = true;
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(err, SwitchFailure::RecoveryRequired(_)));
        assert!(*committed.borrow(), "commit lands before hydrate");
        // Past the commit point the generation has advanced; recovery is forward.
        assert_eq!(reg.generation(), Generation(1));
    }

    #[test]
    fn failing_recover_preserves_original_quiesce_error() {
        let mut reg = LifecycleRegistry::new();
        reg.register(Box::new(RecordingOwner {
            name: "a",
            fail_quiesce: true,
            fail_recover: true,
            ..Default::default()
        }));

        let err = reg.switch(&Principal::from("t"), || Ok(())).unwrap_err();

        match err {
            // The failed rollback does not mask the original quiesce failure.
            SwitchFailure::RolledBack(e) => {
                assert_eq!(e.stage, LifecycleStage::Quiesce);
                assert_eq!(e.owner, "a");
            }
            other => panic!("expected rollback with original error, got {other:?}"),
        }
        assert_eq!(reg.generation(), Generation::INITIAL);
    }
}
