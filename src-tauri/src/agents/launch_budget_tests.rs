// SPDX-License-Identifier: MIT
//! SW-XS-2 AC4: a launch is bounded by the budgets of the phases it enters,
//! never a flat 120 s, and each phase it enters is reported. Time is tokio's
//! paused virtual clock: deterministic, not wall-clock.
use super::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MIB: u64 = 1024 * 1024;

#[test]
fn phase_budgets_follow_the_spec() {
    assert_eq!(transfer_budget(3 * MIB), Duration::from_secs(90));
    assert_eq!(transfer_budget(3 * MIB + 1), Duration::from_secs(120));
    assert_eq!(transfer_budget(1), Duration::from_secs(30));
    // A metadata-only or zero-byte apply still gets a floor, never zero.
    assert_eq!(transfer_budget(0), Duration::from_secs(10));
    assert_eq!(diff_budget(0), Duration::from_secs(10));
    assert_eq!(diff_budget(50 * MIB), Duration::from_secs(11));
    assert_eq!(diff_budget(50 * MIB + 1), Duration::from_secs(12));
    assert_eq!(SPAWN_BUDGET, Duration::from_secs(20));
    assert_eq!(ADMIT_BUDGET, Duration::from_secs(60));
    // Huge inputs saturate to the cap instead of overflowing.
    assert_eq!(transfer_budget(u64::MAX), PHASE_CAP);
    assert_eq!(diff_budget(u64::MAX), PHASE_CAP);
}

fn recording() -> (LaunchBudget, Arc<Mutex<Vec<LaunchProgress>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let budget = LaunchBudget::new(move |progress| sink.lock().unwrap().push(progress));
    (budget, seen)
}

fn phases(seen: &Mutex<Vec<LaunchProgress>>) -> Vec<LaunchPhase> {
    let mut phases: Vec<LaunchPhase> = seen.lock().unwrap().iter().map(|p| p.phase).collect();
    phases.dedup();
    phases
}

#[tokio::test(start_paused = true)]
async fn a_launch_starts_admitting_never_diffing() {
    let (budget, seen) = recording();
    assert_eq!(phases(&seen), [LaunchPhase::Admitting]);
    let started = tokio::time::Instant::now();
    let expired = budget.expired().await;
    assert_eq!(expired.phase, LaunchPhase::Admitting);
    assert_eq!(started.elapsed(), ADMIT_BUDGET);
}

#[tokio::test(start_paused = true)]
async fn a_3_mib_transfer_is_bounded_at_90_s_and_reported_as_transferring() {
    let (budget, seen) = recording();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(budget.scope(async {
        current()
            .expect("the open runs inside its budget")
            .enter(LaunchPhase::Transferring, 3 * MIB);
        std::future::pending::<()>().await
    }));
    let expired = budget.expired().await;
    task.abort();
    assert_eq!(expired.phase, LaunchPhase::Transferring);
    assert_eq!(expired.budget, Duration::from_secs(90));
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(90),
        "not a flat 120 s"
    );
    let last = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.phase, LaunchPhase::Transferring);
    assert_eq!(last.bytes, Some(3 * MIB));
    assert_eq!(last.budget_ms, 90_000);
}

#[tokio::test(start_paused = true)]
async fn each_phase_gets_its_own_budget_from_when_it_starts() {
    let (budget, seen) = recording();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(budget.scope(async {
        let budget = current().unwrap();
        // Admission (and waiting for the destination lock) may outlast a diff.
        tokio::time::sleep(Duration::from_secs(50)).await;
        budget.diff_progress(0);
        tokio::time::sleep(Duration::from_secs(9)).await;
        budget.enter(LaunchPhase::Transferring, MIB);
        tokio::time::sleep(Duration::from_secs(29)).await;
        budget.enter(LaunchPhase::Provisioning, 0);
        tokio::time::sleep(Duration::from_secs(59)).await;
        budget.enter(LaunchPhase::Spawning, 0);
        std::future::pending::<()>().await
    }));
    let expired = budget.expired().await;
    task.abort();
    assert_eq!(expired.phase, LaunchPhase::Spawning);
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(50 + 9 + 29 + 59 + 20)
    );
    assert_eq!(
        phases(&seen),
        [
            LaunchPhase::Admitting,
            LaunchPhase::Diffing,
            LaunchPhase::Transferring,
            LaunchPhase::Provisioning,
            LaunchPhase::Spawning,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_cold_diff_keeps_its_budget_while_progress_arrives_and_expires_when_quiet() {
    let (budget, _) = recording();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(budget.scope(async {
        let budget = current().unwrap();
        budget.diff_progress(0);
        // Ten reports, 8 s apart: 80 s of diff, never 10 s of silence.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(8)).await;
            budget.diff_progress(0);
        }
        std::future::pending::<()>().await
    }));
    let expired = budget.expired().await;
    task.abort();
    assert_eq!(expired.phase, LaunchPhase::Diffing);
    assert_eq!(started.elapsed(), Duration::from_secs(80 + 10));
}

#[tokio::test(start_paused = true)]
async fn a_diff_report_never_shortens_the_budget() {
    let (budget, _) = recording();
    let started = tokio::time::Instant::now();
    budget.diff_progress(500 * MIB); // 20 s
    tokio::time::advance(Duration::from_secs(1)).await;
    budget.diff_progress(0); // would be 10 s from now: 11 s < 20 s
    let expired = budget.expired().await;
    assert_eq!(started.elapsed(), Duration::from_secs(20));
    assert_eq!(expired.phase, LaunchPhase::Diffing);
}

#[test]
fn outside_a_launch_there_is_no_budget() {
    assert!(current().is_none());
}
