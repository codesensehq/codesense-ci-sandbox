// SPDX-License-Identifier: MIT
//! SW-XS-2: how long a remote launch may take before its caller is answered
//! "not confirmed yet". There is no flat bound: each phase the launch enters
//! gets its own budget, starting when that phase starts, and each phase is
//! reported as launch progress.
//!
//! | Phase | What runs | Budget |
//! |---|---|---|
//! | Admitting | authorization, host home, intent lock, MCP capture, resolving the prepared workspace, waiting for the destination lock | 60 s |
//! | Diffing | Step 2's comparison | 10 s plus 1 s per 50 MiB to hash, renewed by every progress report (a cold cache that keeps reporting keeps its budget) |
//! | Transferring | Step 2's copy | 30 s per started MiB, at least 10 s |
//! | Provisioning | MCP provisioning and the runtime preflight | 60 s |
//! | Spawning | the PTY spawn (the destination lock is already released) | 20 s |
//!
//! Every budget is capped at [`PHASE_CAP`] and computed with saturating
//! arithmetic. The open runs inside [`LaunchBudget::scope`]; code on the
//! launch path reaches its budget with [`current`] (or captures it before going
//! to a blocking thread). Expiry only answers the caller; it never cancels the
//! open.
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

const MIB: u64 = 1024 * 1024;
/// No phase is ever granted more than this.
pub(crate) const PHASE_CAP: Duration = Duration::from_secs(60 * 60);
pub(crate) const ADMIT_BUDGET: Duration = Duration::from_secs(60);
/// The diff's (and the verification's) base budget.
pub(crate) const DIFF_BUDGET: Duration = Duration::from_secs(10);
/// Bytes hashed per extra second of diff or verification.
const HASH_BYTES_PER_SECOND: u64 = 50 * MIB;
const TRANSFER_SECONDS_PER_MIB: u64 = 30;
/// The least a transfer gets, so a metadata-only Apply is never budgeted zero.
pub(crate) const TRANSFER_FLOOR: Duration = Duration::from_secs(10);
pub(crate) const PROVISION_BUDGET: Duration = Duration::from_secs(60);
pub(crate) const SPAWN_BUDGET: Duration = Duration::from_secs(20);

fn capped(seconds: u64) -> Duration {
    Duration::from_secs(seconds).min(PHASE_CAP)
}

/// A diff that must hash `bytes`.
pub(crate) fn diff_budget(bytes: u64) -> Duration {
    capped(
        DIFF_BUDGET
            .as_secs()
            .saturating_add(bytes.div_ceil(HASH_BYTES_PER_SECOND)),
    )
}

/// A transfer of `bytes`.
pub(crate) fn transfer_budget(bytes: u64) -> Duration {
    capped(TRANSFER_SECONDS_PER_MIB.saturating_mul(bytes.div_ceil(MIB))).max(TRANSFER_FLOOR)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LaunchPhase {
    Admitting,
    Diffing,
    Transferring,
    Provisioning,
    Spawning,
}

impl LaunchPhase {
    fn budget(self, bytes: u64) -> Duration {
        match self {
            Self::Admitting => ADMIT_BUDGET,
            Self::Diffing => diff_budget(bytes),
            Self::Transferring => transfer_budget(bytes),
            Self::Provisioning => PROVISION_BUDGET,
            Self::Spawning => SPAWN_BUDGET,
        }
    }
}

impl std::fmt::Display for LaunchPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Admitting => "Admitting",
            Self::Diffing => "Diffing",
            Self::Transferring => "Transferring",
            Self::Provisioning => "Provisioning",
            Self::Spawning => "Spawning",
        })
    }
}

/// One phase report: what the launch is doing and the time it has left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchProgress {
    pub phase: LaunchPhase,
    /// Bytes this report covers (hashed or copied), when known.
    pub bytes: Option<u64>,
    /// Time left in the phase as of this report.
    pub budget_ms: u64,
}

/// The phase whose budget ran out, and the last budget it was granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Expired {
    pub phase: LaunchPhase,
    pub budget: Duration,
}

struct State {
    phase: LaunchPhase,
    deadline: Instant,
    granted: Duration,
}

type Report = Box<dyn Fn(LaunchProgress) + Send + Sync>;

struct Inner {
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    report: Report,
}

/// One launch's running phase budget.
#[derive(Clone)]
pub(crate) struct LaunchBudget(Arc<Inner>);

tokio::task_local! {
    static CURRENT: LaunchBudget;
}

/// The budget of the launch this task is running, if any.
pub(crate) fn current() -> Option<LaunchBudget> {
    CURRENT.try_with(LaunchBudget::clone).ok()
}

impl LaunchBudget {
    /// A launch (or an attach) starting now, admitting; `report` hears every
    /// phase.
    pub(crate) fn new(report: impl Fn(LaunchProgress) + Send + Sync + 'static) -> Self {
        let budget = Self(Arc::new(Inner {
            state: Mutex::new(State {
                phase: LaunchPhase::Admitting,
                deadline: Instant::now() + ADMIT_BUDGET,
                granted: ADMIT_BUDGET,
            }),
            changed: tokio::sync::Notify::new(),
            report: Box::new(report),
        }));
        budget.publish(None);
        budget
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn publish(&self, bytes: Option<u64>) {
        let progress = {
            let state = self.state();
            let left = state.deadline.saturating_duration_since(Instant::now());
            LaunchProgress {
                phase: state.phase,
                bytes,
                budget_ms: u64::try_from(left.as_millis()).unwrap_or(u64::MAX),
            }
        };
        (self.0.report)(progress);
        self.0.changed.notify_waiters();
    }

    /// Enter `phase` now, covering `bytes`; its budget starts now.
    pub(crate) fn enter(&self, phase: LaunchPhase, bytes: u64) {
        {
            let mut state = self.state();
            let granted = phase.budget(bytes);
            state.phase = phase;
            state.granted = granted;
            state.deadline = Instant::now() + granted;
        }
        self.publish(Some(bytes));
    }

    /// A diff progress report: `bytes` more to hash. The first report starts
    /// the diff; each later one renews its budget from now (never shortening
    /// it), so a cold diff that keeps reporting is not cut off, and one that
    /// goes quiet expires.
    pub(crate) fn diff_progress(&self, bytes: u64) {
        let entering = self.state().phase != LaunchPhase::Diffing;
        if entering {
            return self.enter(LaunchPhase::Diffing, bytes);
        }
        {
            let mut state = self.state();
            let granted = diff_budget(bytes);
            let renewed = Instant::now() + granted;
            if renewed > state.deadline {
                state.deadline = renewed;
                state.granted = granted;
            }
        }
        self.publish(Some(bytes));
    }

    /// Run `open` with this budget as its [`current`] one.
    pub(crate) fn scope<F: std::future::Future>(
        &self,
        open: F,
    ) -> impl std::future::Future<Output = F::Output> {
        CURRENT.scope(self.clone(), open)
    }

    /// Resolves once the running phase outlives its budget.
    pub(crate) async fn expired(&self) -> Expired {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let deadline = self.state().deadline;
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    let state = self.state();
                    if state.deadline <= Instant::now() {
                        return Expired { phase: state.phase, budget: state.granted };
                    }
                }
                () = &mut changed => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "launch_budget_tests.rs"]
mod tests;
