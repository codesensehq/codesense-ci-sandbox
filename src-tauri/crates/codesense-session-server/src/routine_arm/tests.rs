// SPDX-License-Identifier: MIT
//! Machine-armed routines over a fake host port and an injected
//! clock; then the engine on a real in-process session server, armed over its
//! socket and firing with no client connected.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
use codesense_host_proto::reverse_mcp::AgentStartup;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::domain::WorkspaceKey;
use codesense_pty_core::routine_arm::{
    arm_run_id, ArmAnswer, ArmFireAnswer, ArmLaunch, ArmMissed, ArmMode, ArmOverlap, ArmRefusal,
    ArmRun, ArmRunStatus, ArmSchedule, ArmTarget, RoutineArm,
};
use codesense_pty_core::session_wake::{
    DeliverRequest, DeliveryStatus, GapPolicy, Originator, UndeliveredReason, WakeLimits,
    WakeOutcome,
};

use super::{ArmCandidate, ArmPort, LaunchFailure, RoutineArms};
use crate::session_wake::Clock;

const MIN: u64 = 60_000;
/// 2026-10-06T14:58:00Z = 08:58 in Denver (MDT).
const WALL: u64 = 1_791_298_680_000;
/// 09:00 Denver that day.
const NINE_DENVER: u64 = WALL + 2 * MIN;

struct FakeClock {
    wall: AtomicU64,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            wall: AtomicU64::new(WALL),
        }
    }
    fn set(&self, wall: u64) {
        self.wall.store(wall, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn mono_ms(&self) -> u64 {
        self.wall.load(Ordering::SeqCst)
    }
    fn wall_ms(&self) -> u64 {
        self.wall.load(Ordering::SeqCst)
    }
    fn sleep_ms(&self, ms: u64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
    }
}

/// A fake host: the sessions of the routine's agent, a wake queue that
/// answers as told, and a spawn path that records what it started.
#[derive(Default)]
struct Fake {
    candidates: Mutex<Vec<ArmCandidate>>,
    live: Mutex<HashSet<String>>,
    /// The status a delivery is answered with.
    answer: Mutex<Option<DeliveryStatus>>,
    /// Final statuses by delivery key, for polls.
    settled: Mutex<Vec<(String, DeliveryStatus)>>,
    delivered: Mutex<Vec<DeliverRequest>>,
    cancelled: Mutex<Vec<String>>,
    launched: Mutex<Vec<(PtySessionId, String)>>,
    /// Whether each launch asked for if-absent.
    if_absent: Mutex<Vec<bool>>,
    killed: Mutex<Vec<PtySessionId>>,
    launch_fails: Mutex<Option<LaunchFailure>>,
    /// A clock each launch moves forward by `launch_ms` (a slow spawn).
    slow: Mutex<Option<(Arc<FakeClock>, u64)>>,
    /// Sessions that come up once polled this many times (starting).
    coming_up: Mutex<Vec<(String, u32)>>,
    /// Paths that exist in the workspace.
    prepared: Mutex<Option<Result<(), String>>>,
    /// The notice a delivery's outcome carries.
    notice: Mutex<Option<String>>,
}

impl Fake {
    fn outcome(&self, request: &DeliverRequest, status: DeliveryStatus) -> WakeOutcome {
        WakeOutcome {
            seq: 0,
            delivery_id: format!("d-{}", request.idempotency_key),
            idempotency_key: request.idempotency_key.clone(),
            session: request.session.clone(),
            marker: request.marker.clone(),
            originator: request.originator.clone(),
            timer_id: None,
            status,
            host_boot_id: "boot".into(),
            at_ms: 0,
            notice: self.notice.lock().unwrap().clone(),
        }
    }
    fn idle(&self, session: &str) {
        self.live.lock().unwrap().insert(session.into());
        self.candidates.lock().unwrap().push(ArmCandidate {
            session: PtySessionId::new(session),
            idle: true,
            recency: 1,
            starting: false,
        });
    }
    fn busy(&self, session: &str, recency: u64) {
        self.live.lock().unwrap().insert(session.into());
        self.candidates.lock().unwrap().push(ArmCandidate {
            session: PtySessionId::new(session),
            idle: false,
            recency,
            starting: false,
        });
    }
    /// A session of the agent being started: up after `polls` liveness checks.
    fn starting(&self, session: &str, polls: u32) {
        self.coming_up.lock().unwrap().push((session.into(), polls));
        self.candidates.lock().unwrap().push(ArmCandidate {
            session: PtySessionId::new(session),
            idle: false,
            recency: u64::MAX,
            starting: true,
        });
    }
}

impl ArmPort for Fake {
    fn candidates(&self, _: &ArmTarget) -> Vec<ArmCandidate> {
        self.candidates.lock().unwrap().clone()
    }
    fn session_live(&self, session: &PtySessionId) -> bool {
        let mut coming = self.coming_up.lock().unwrap();
        if let Some(entry) = coming.iter_mut().find(|(s, _)| s == session.as_str()) {
            if entry.1 == 0 {
                self.live.lock().unwrap().insert(entry.0.clone());
            } else {
                entry.1 -= 1;
            }
        }
        drop(coming);
        self.live.lock().unwrap().contains(session.as_str())
    }
    fn prepared(&self, _: &ArmTarget, _: &ArmLaunch) -> Result<(), String> {
        self.prepared.lock().unwrap().clone().unwrap_or(Ok(()))
    }
    fn launch(
        &self,
        _: &ArmTarget,
        _: &ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        if_absent: bool,
        _: &str,
    ) -> Result<(), LaunchFailure> {
        self.if_absent.lock().unwrap().push(if_absent);
        if let Some((clock, ms)) = self.slow.lock().unwrap().clone() {
            clock.sleep_ms(ms);
        }
        if let Some(failure) = self.launch_fails.lock().unwrap().clone() {
            return Err(failure);
        }
        self.launched
            .lock()
            .unwrap()
            .push((session.clone(), prompt.to_string()));
        self.live.lock().unwrap().insert(session.as_str().into());
        Ok(())
    }
    fn kill(&self, session: &PtySessionId) {
        self.killed.lock().unwrap().push(session.clone());
        self.live.lock().unwrap().remove(session.as_str());
    }
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String> {
        let status = self
            .answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(DeliveryStatus::Delivered { at_ms: 7 });
        self.delivered.lock().unwrap().push(request.clone());
        Ok(self.outcome(&request, status))
    }
    fn delivery(
        &self,
        originator: &Originator,
        session: &PtySessionId,
        key: &str,
    ) -> Option<WakeOutcome> {
        let request = DeliverRequest {
            session: session.clone(),
            text: String::new(),
            header: None,
            marker: String::new(),
            max_wait_ms: 1,
            idempotency_key: key.into(),
            originator: originator.clone(),
            limits: None,
        };
        let settled = self
            .settled
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, status)| status.clone());
        Some(self.outcome(
            &request,
            settled.unwrap_or(DeliveryStatus::Queued { deadline_ms: 1 }),
        ))
    }
    fn cancel(&self, _: &Originator, _: &PtySessionId, key: &str) {
        self.cancelled.lock().unwrap().push(key.into());
        self.settled.lock().unwrap().push((
            key.into(),
            DeliveryStatus::Undelivered {
                reason: UndeliveredReason::Archived,
            },
        ));
    }
}

struct Rig {
    fake: Arc<Fake>,
    clock: Arc<FakeClock>,
    arms: Arc<RoutineArms>,
}

fn rig() -> Rig {
    rig_at(
        None,
        "boot-1",
        Arc::new(FakeClock::new()),
        Arc::new(Fake::default()),
    )
}

fn rig_at(store: Option<PathBuf>, boot: &str, clock: Arc<FakeClock>, fake: Arc<Fake>) -> Rig {
    let arms = RoutineArms::open_with(
        fake.clone(),
        clock.clone(),
        boot.into(),
        store,
        None,
        std::time::Duration::from_millis(500),
    );
    Rig { fake, clock, arms }
}

fn launch() -> ArmLaunch {
    ArmLaunch {
        launch: NativeLaunch::Cli(AgentCli::Claude),
        agent_startup: AgentStartup {
            args: vec![],
            settings: serde_json::json!({}),
            prompt: None,
            env: Default::default(),
        },
        prepared_paths: vec!["docs/current.md".into()],
        prepared_digest: "digest-1".into(),
        workspace: WorkspaceKey::new("org:team|objects"),
        descriptor: None,
        tags: Default::default(),
    }
}

fn arm(mode: ArmMode, epoch: u64) -> RoutineArm {
    RoutineArm {
        key: "acme/core/triage".into(),
        epoch,
        revision: 1,
        name: "Triage".into(),
        mode,
        schedules: vec![ArmSchedule {
            cron: "0 9 * * *".into(),
            tz: "America/Denver".into(),
        }],
        task: "triage for {{date}} ({{last_run_summary}})".into(),
        target: ArmTarget {
            agent: "acme:core/barb".into(),
            workspace_id: "objects".into(),
            cwd: "/home/dev/work".into(),
            owner: Some("41".into()),
            allow_wake: false,
        },
        max_wait_ms: 30 * MIN,
        overlap: ArmOverlap::Coalesce,
        missed: ArmMissed::Skip,
        launch: Some(launch()),
        originator: Originator {
            desktop: "forged".into(),
            routine_host: Some("mac-a".into()),
        },
        author: "sam".into(),
        max_runtime_ms: 30 * MIN,
        last_fired_ms: None,
        last_run_summary: None,
        last_run_at_ms: None,
        source_digest: "agent-1".into(),
        limits: None,
    }
}

fn armed(rig: &Rig, arm: RoutineArm) {
    match rig.arms.arm(arm, "app") {
        ArmAnswer::Armed { .. } => {}
        other => panic!("{other:?}"),
    }
}

/// The newest entry of each run, in log order.
fn current(rig: &Rig) -> Vec<ArmRun> {
    let mut runs: Vec<ArmRun> = Vec::new();
    for run in rig.arms.runs(0).runs {
        match runs.iter_mut().find(|r| r.run_id == run.run_id) {
            Some(slot) => *slot = run,
            None => runs.push(run),
        }
    }
    runs
}

fn statuses(rig: &Rig) -> Vec<ArmRunStatus> {
    current(rig).into_iter().map(|run| run.status).collect()
}

// Armed on the machine, with no desktop anywhere, a wake
// fires at 09:00 in the routine's zone into the agent's idle session, and its
// run is in the log a desktop reads when it comes back.
#[test]
fn an_armed_wake_fires_on_the_machines_clock_and_its_run_waits_in_the_log() {
    let rig = rig();
    rig.fake.idle("barb-1");
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER - 1);
    rig.arms.tick();
    assert!(
        rig.fake.delivered.lock().unwrap().is_empty(),
        "not yet 09:00"
    );
    rig.clock.set(NINE_DENVER + 3_000);
    rig.arms.tick();
    let delivered = rig.fake.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    let run_id = arm_run_id("acme/core/triage", 1, NINE_DENVER);
    let request = &delivered[0];
    assert_eq!(request.session.as_str(), "barb-1");
    assert_eq!(request.idempotency_key, run_id, "keyed by the run");
    assert_eq!(
        request.header.as_deref(),
        Some(format!("[routine: Triage | run {run_id}]").as_str())
    );
    assert_eq!(
        request.text, "triage for 2026-10-06 ()",
        "the date renders in the routine's zone, and no summary yet is empty"
    );
    assert_eq!(request.marker, "Triage (last edited by sam)");
    assert_eq!(request.originator.desktop, "app", "stamped, never claimed");
    assert_eq!(request.originator.routine_host.as_deref(), Some("mac-a"));
    let log = rig.arms.runs(0);
    assert_eq!(log.runs.len(), 2, "firing, then its outcome");
    assert_eq!(log.runs[0].status, ArmRunStatus::Firing);
    assert_eq!(
        log.runs[1].status,
        ArmRunStatus::Delivered {
            session: PtySessionId::new("barb-1"),
            at_ms: 7
        }
    );
    assert_eq!(log.runs[1].scheduled_at_ms, NINE_DENVER);
    assert_eq!(log.runs[1].source, "schedule");
    // A desktop that read up to the first entry gets only the rest.
    assert_eq!(rig.arms.runs(log.runs[0].seq).runs.len(), 1);
    // The next fire is tomorrow at 09:00 in Denver.
    assert_eq!(
        rig.arms.list().armed[0].next_at_ms,
        Some(NINE_DENVER + 24 * 60 * MIN)
    );
    rig.arms.tick();
    assert_eq!(rig.fake.delivered.lock().unwrap().len(), 1, "fired once");
}

// A queued wake (busy agent) settles in the log when its delivery does.
#[test]
fn a_queued_wake_settles_when_its_delivery_does() {
    let rig = rig();
    rig.fake.busy("barb-1", 1);
    *rig.fake.answer.lock().unwrap() = Some(DeliveryStatus::Queued { deadline_ms: 99 });
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(matches!(
        statuses(&rig)[0],
        ArmRunStatus::Queued {
            deadline_ms: 99,
            ..
        }
    ));
    rig.arms.tick();
    assert_eq!(rig.arms.runs(0).runs.len(), 2, "still queued: nothing new");
    let run_id = arm_run_id("acme/core/triage", 1, NINE_DENVER);
    rig.fake
        .settled
        .lock()
        .unwrap()
        .push((run_id, DeliveryStatus::Delivered { at_ms: 5 }));
    rig.arms.tick();
    assert_eq!(
        statuses(&rig)[0],
        ArmRunStatus::Delivered {
            session: PtySessionId::new("barb-1"),
            at_ms: 5
        }
    );
}

// R1: idle over busy, then the most recent.
#[test]
fn a_wake_prefers_an_idle_session_then_the_most_recent() {
    let rig = rig();
    rig.fake.busy("busy-new", 9);
    rig.fake.idle("idle-old");
    rig.fake.busy("busy-old", 2);
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(
        rig.fake.delivered.lock().unwrap()[0].session.as_str(),
        "idle-old"
    );
}

// R1: with no live instance a wake starts exactly one, with the task as its
// first prompt; the next fire wakes that one instead of starting another,
// even before the layout shows it.
#[test]
fn a_wake_with_no_live_agent_starts_one_and_the_next_fire_wakes_it() {
    let rig = rig();
    let mut every_minute = arm(ArmMode::Wake, 1);
    every_minute.schedules[0].cron = "* * * * *".into();
    armed(&rig, every_minute);
    rig.clock.set(WALL + MIN);
    rig.arms.tick();
    let launched = rig.fake.launched.lock().unwrap().clone();
    assert_eq!(launched.len(), 1);
    let run_id = arm_run_id("acme/core/triage", 1, WALL + MIN);
    assert!(launched[0]
        .1
        .starts_with(&format!("[routine: Triage | run {run_id}]\n")));
    assert!(matches!(statuses(&rig)[0], ArmRunStatus::Launched { .. }));
    rig.clock.set(WALL + 2 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 1, "never a second");
    let delivered = rig.fake.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].session, launched[0].0, "the one it started");
}

// A fresh fire with nothing prepared on the machine, or a workspace
// whose prepared files are gone, starts nothing.
#[test]
fn a_fresh_fire_into_an_unprepared_workspace_is_needs_preparation() {
    let rig = rig();
    let mut no_recipe = arm(ArmMode::Fresh, 1);
    no_recipe.launch = None;
    armed(&rig, no_recipe);
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    let ArmRunStatus::Undelivered { reason, detail } = statuses(&rig)[0].clone() else {
        panic!("{:?}", statuses(&rig))
    };
    assert_eq!(reason, UndeliveredReason::NeedsPreparation);
    assert!(detail.unwrap().contains("prepared"));
    assert!(rig.fake.launched.lock().unwrap().is_empty());

    let rig = rig_at(
        None,
        "b",
        Arc::new(FakeClock::new()),
        Arc::new(Fake::default()),
    );
    *rig.fake.prepared.lock().unwrap() = Some(Err("docs/current.md is missing".into()));
    armed(&rig, arm(ArmMode::Fresh, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(
        statuses(&rig)[0],
        ArmRunStatus::Undelivered {
            reason: UndeliveredReason::NeedsPreparation,
            detail: Some("docs/current.md is missing".into())
        }
    );
    assert!(rig.fake.launched.lock().unwrap().is_empty());
}

// The real preparation check: the workspace and every prepared path.
#[test]
fn a_workspace_counts_as_prepared_only_with_everything_its_preparation_wrote() {
    use super::host_port::prepared_in;
    let dir = tempfile::tempdir().unwrap();
    let recipe = launch();
    assert!(prepared_in(&dir.path().join("gone"), &recipe)
        .unwrap_err()
        .contains("no longer on this machine"));
    assert!(prepared_in(dir.path(), &recipe)
        .unwrap_err()
        .contains("docs/current.md is missing"));
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/current.md"), "x").unwrap();
    assert_eq!(prepared_in(dir.path(), &recipe), Ok(()));
    let mut escaping = recipe;
    escaping.prepared_paths = vec!["../outside".into()];
    assert!(prepared_in(dir.path(), &escaping).is_err());
}

// A fresh fire into a prepared workspace starts the agent with the task
// first; a fire while it still runs coalesces (or skips), never a second.
#[test]
fn a_fresh_fire_starts_the_agent_and_overlap_never_starts_a_second() {
    let rig = rig();
    let mut fresh = arm(ArmMode::Fresh, 1);
    fresh.schedules[0].cron = "* * * * *".into();
    armed(&rig, fresh.clone());
    rig.clock.set(WALL + MIN);
    rig.arms.tick();
    let first = arm_run_id("acme/core/triage", 1, WALL + MIN);
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 1);
    rig.clock.set(WALL + 2 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 1);
    assert_eq!(statuses(&rig)[1], ArmRunStatus::Coalesced { into: first });
    fresh.overlap = ArmOverlap::Skip;
    fresh.revision = 2;
    armed(&rig, fresh);
    rig.clock.set(WALL + 3 * MIN);
    rig.arms.tick();
    assert!(matches!(statuses(&rig)[2], ArmRunStatus::Skipped { .. }));
    // Its agent ended: the next fire starts a new one.
    rig.fake.live.lock().unwrap().clear();
    rig.clock.set(WALL + 4 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 2);
}

// R5: a launch that may have started is unknown, and never retried.
#[test]
fn a_launch_that_may_have_started_is_unknown() {
    let rig = rig();
    *rig.fake.launch_fails.lock().unwrap() = Some(LaunchFailure::Unknown("lost".into()));
    armed(&rig, arm(ArmMode::Fresh, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(
        statuses(&rig)[0],
        ArmRunStatus::Unknown {
            detail: "lost".into()
        }
    );
}

// Moved back to the desktop, the machine stops firing it
// at once; an arm at that epoch or older (a stale definition) is refused; a
// new move onto the machine (a newer epoch) arms it again.
#[test]
fn a_disarm_stops_the_fires_and_no_older_epoch_can_bring_it_back() {
    let rig = rig();
    rig.fake.idle("barb-1");
    armed(&rig, arm(ArmMode::Wake, 1));
    assert_eq!(
        rig.arms.disarm("acme/core/triage", 2),
        ArmAnswer::Disarmed {
            was_armed: true,
            last_fired_ms: None
        }
    );
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(rig.fake.delivered.lock().unwrap().is_empty(), "never fired");
    assert!(rig.arms.runs(0).runs.is_empty());
    assert!(matches!(
        rig.arms.arm(arm(ArmMode::Wake, 1), "app"),
        ArmAnswer::Refused {
            refusal: ArmRefusal::Stale { held_epoch: 2, .. }
        }
    ));
    assert!(matches!(
        rig.arms.disarm("acme/core/triage", 1),
        ArmAnswer::Refused { .. }
    ));
    let list = rig.arms.list();
    assert!(list.armed.is_empty());
    assert_eq!(list.disarmed[0].epoch, 2);
    armed(&rig, arm(ArmMode::Wake, 3));
    assert!(rig.arms.list().disarmed.is_empty());
    rig.clock.set(NINE_DENVER + 24 * 60 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.delivered.lock().unwrap().len(), 1);
}

// A disarm cancels the wake the machine still had queued, so the
// desktop that takes over never doubles it.
#[test]
fn a_disarm_cancels_a_queued_wake() {
    let rig = rig();
    rig.fake.busy("barb-1", 1);
    *rig.fake.answer.lock().unwrap() = Some(DeliveryStatus::Queued { deadline_ms: 9 });
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    rig.arms.disarm("acme/core/triage", 2);
    let run_id = arm_run_id("acme/core/triage", 1, NINE_DENVER);
    assert_eq!(*rig.fake.cancelled.lock().unwrap(), vec![run_id]);
    assert_eq!(
        statuses(&rig)[0],
        ArmRunStatus::Undelivered {
            reason: UndeliveredReason::Archived,
            detail: None
        }
    );
}

// A disarm without a move (paused, archived) is undone by the same
// placement at the same epoch; anything older stays refused.
#[test]
fn a_paused_routine_comes_back_at_its_own_epoch() {
    let rig = rig();
    armed(&rig, arm(ArmMode::Wake, 3));
    assert_eq!(
        rig.arms.disarm("acme/core/triage", 3),
        ArmAnswer::Disarmed {
            was_armed: true,
            last_fired_ms: None
        }
    );
    assert!(matches!(
        rig.arms.arm(arm(ArmMode::Wake, 2), "app"),
        ArmAnswer::Refused { .. }
    ));
    armed(&rig, arm(ArmMode::Wake, 3));
    assert_eq!(rig.arms.list().armed[0].epoch, 3);
}

// The epoch is the placement fence: a different managing Mac at the same
// epoch can replace an arm and can re-arm its tombstone. Callers must bump
// it when they change the managing Mac.
#[test]
fn two_managing_macs_at_one_epoch_can_replace_and_rearm_each_other() {
    let rig = rig();
    let mut first = arm(ArmMode::Wake, 3);
    first.originator.routine_host = Some("mac-a".into());
    let mut second = first.clone();
    second.originator.routine_host = Some("mac-b".into());
    armed(&rig, first.clone());
    armed(&rig, second);
    assert_eq!(
        rig.arms.list().armed[0].originator.routine_host.as_deref(),
        Some("mac-b")
    );
    assert!(matches!(
        rig.arms.disarm(&first.key, 3),
        ArmAnswer::Disarmed { .. }
    ));
    armed(&rig, first);
    assert_eq!(
        rig.arms.list().armed[0].originator.routine_host.as_deref(),
        Some("mac-a")
    );
}

#[test]
fn a_new_epoch_reuses_a_launch_only_for_the_same_agent_and_workspace() {
    for change in [
        "manager",
        "agent",
        "workspace",
        "directory",
        "task",
        "manifest",
    ] {
        let rig = rig();
        let first = arm(ArmMode::Fresh, 1);
        armed(&rig, first.clone());
        let mut replacement = first;
        replacement.epoch = 2;
        replacement.originator.routine_host = Some("mac-b".into());
        replacement.launch = None;
        match change {
            "agent" => replacement.target.agent = "acme:core/ricky".into(),
            "workspace" => replacement.target.workspace_id = "different".into(),
            "directory" => replacement.target.cwd = "/home/dev/other".into(),
            "task" => replacement.task = "a different task".into(),
            "manifest" => replacement.source_digest = "agent-2".into(),
            _ => {}
        }
        armed(&rig, replacement);
        assert_eq!(
            rig.arms.list().armed[0].has_launch,
            change == "manager",
            "{change}"
        );
    }
}

#[test]
fn a_takeover_keeps_the_launch_and_uses_the_new_author() {
    let rig = rig();
    rig.fake.idle("barb-1");
    let mut first = arm(ArmMode::Wake, 1);
    first.author = "triage-agent".into();
    armed(&rig, first.clone());
    let mut second = first;
    second.epoch = 2;
    second.author = "alice".into();
    second.originator.routine_host = Some("mac-b".into());
    second.launch = None;
    armed(&rig, second);
    assert!(rig.arms.list().armed[0].has_launch);
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(
        rig.fake.delivered.lock().unwrap()[0].marker,
        "Triage (last edited by alice)"
    );
}

// Fencing within an epoch: an older revision is refused; an edit that keeps
// the schedule keeps its cursor (nothing fired twice, nothing lost).
#[test]
fn an_older_revision_is_refused_and_an_edit_keeps_its_cursor() {
    let rig = rig();
    let mut v2 = arm(ArmMode::Wake, 4);
    v2.revision = 2;
    armed(&rig, v2.clone());
    let next = rig.arms.list().armed[0].next_at_ms;
    let mut v1 = v2.clone();
    v1.revision = 1;
    assert!(matches!(
        rig.arms.arm(v1, "app"),
        ArmAnswer::Refused {
            refusal: ArmRefusal::Stale {
                held_epoch: 4,
                held_revision: 2
            }
        }
    ));
    assert!(matches!(
        rig.arms.arm(arm(ArmMode::Wake, 3), "app"),
        ArmAnswer::Refused { .. }
    ));
    let mut v3 = v2;
    v3.revision = 3;
    v3.task = "edited".into();
    armed(&rig, v3);
    assert_eq!(rig.arms.list().armed[0].next_at_ms, next);
    assert_eq!(rig.arms.list().armed[0].revision, 3);
}

// Missed occurrences while the machine was down: skipped once, or fired once.
#[test]
fn missed_occurrences_are_skipped_or_fired_once_by_policy() {
    let rig = rig();
    rig.fake.idle("barb-1");
    armed(&rig, arm(ArmMode::Wake, 1));
    // Down from before 09:00 until three days later at noon.
    rig.clock
        .set(NINE_DENVER + 3 * 24 * 60 * MIN + 3 * 60 * MIN);
    rig.arms.tick();
    assert!(rig.fake.delivered.lock().unwrap().is_empty());
    let runs = current(&rig);
    assert_eq!(runs.len(), 1, "one skipped run for the gap");
    assert!(matches!(runs[0].status, ArmRunStatus::Skipped { .. }));

    let rig = rig_at(
        None,
        "b",
        Arc::new(FakeClock::new()),
        Arc::new(Fake::default()),
    );
    rig.fake.idle("barb-1");
    let mut once = arm(ArmMode::Wake, 1);
    once.missed = ArmMissed::RunOnce;
    armed(&rig, once);
    rig.clock
        .set(NINE_DENVER + 3 * 24 * 60 * MIN + 3 * 60 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.delivered.lock().unwrap().len(), 1, "fired once");
    assert_eq!(current(&rig)[0].source, "catch_up");
}

// Arms are routines, not session state: they survive a host restart and keep
// firing. A fire claimed but unfinished when the host died is `unknown` and
// its occurrence is never fired again (R5).
#[test]
fn arms_survive_a_restart_and_a_fire_caught_mid_way_is_unknown_never_refired() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new());
    let rig = rig_at(
        Some(dir.path().to_path_buf()),
        "boot-1",
        clock.clone(),
        Arc::new(Fake::default()),
    );
    armed(&rig, arm(ArmMode::Fresh, 1));
    clock.set(NINE_DENVER);
    // The host dies right after the claim, before the launch.
    let claimed = rig.arms.claim_next(rig.clock.wall_ms()).expect("due");
    assert_eq!(claimed.run.status, ArmRunStatus::Firing);
    drop(rig);
    let fake = Arc::new(Fake::default());
    let rig = rig_at(
        Some(dir.path().to_path_buf()),
        "boot-2",
        clock.clone(),
        fake,
    );
    assert_eq!(rig.arms.list().armed.len(), 1, "still armed");
    let runs = current(&rig);
    assert_eq!(runs.len(), 1);
    assert!(matches!(runs[0].status, ArmRunStatus::Unknown { .. }));
    assert_eq!(runs[0].host_boot_id, "boot-2");
    rig.arms.tick();
    assert!(
        rig.fake.launched.lock().unwrap().is_empty(),
        "the claimed occurrence is never fired again"
    );
    clock.set(NINE_DENVER + 24 * 60 * MIN);
    rig.arms.tick();
    assert_eq!(
        rig.fake.launched.lock().unwrap().len(),
        1,
        "the next one fires"
    );
}

// Nothing durable, nothing armed; nothing claimed, nothing fired.
#[test]
fn a_store_that_cannot_be_written_arms_and_fires_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new());
    let rig = rig_at(
        Some(dir.path().to_path_buf()),
        "boot-1",
        clock.clone(),
        Arc::new(Fake::default()),
    );
    rig.arms.fail_store(true);
    assert!(matches!(
        rig.arms.arm(arm(ArmMode::Fresh, 1), "app"),
        ArmAnswer::Refused {
            refusal: ArmRefusal::Unavailable { .. }
        }
    ));
    assert!(rig.arms.list().armed.is_empty());
    rig.arms.fail_store(false);
    armed(&rig, arm(ArmMode::Fresh, 1));
    rig.arms.fail_store(true);
    clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(rig.fake.launched.lock().unwrap().is_empty());
    rig.arms.fail_store(false);
    rig.arms.tick();
    assert_eq!(
        rig.fake.launched.lock().unwrap().len(),
        1,
        "fired once it can claim"
    );
}

type Breakage = Box<dyn Fn(&mut RoutineArm)>;

#[test]
fn malformed_arms_are_refused() {
    let rig = rig();
    let cases: Vec<(&str, Breakage)> = vec![
        ("epoch", Box::new(|a| a.epoch = 0)),
        (
            "zone",
            Box::new(|a| a.schedules[0].tz = "Mars/Olympus".into()),
        ),
        (
            "seconds",
            Box::new(|a| a.schedules[0].cron = "* * * *".into()),
        ),
        ("none", Box::new(|a| a.schedules.clear())),
        ("task", Box::new(|a| a.task = "\x1b".into())),
        ("wait", Box::new(|a| a.max_wait_ms = 0)),
        ("cwd", Box::new(|a| a.target.cwd = "work".into())),
        (
            "escape",
            Box::new(|a| {
                a.launch.as_mut().unwrap().prepared_paths = vec!["../x".into()];
            }),
        ),
        (
            "args",
            Box::new(|a| {
                a.launch.as_mut().unwrap().agent_startup.args = vec!["--mcp-config".into()];
            }),
        ),
    ];
    for (name, mutate) in cases {
        let mut bad = arm(ArmMode::Wake, 1);
        mutate(&mut bad);
        assert!(
            matches!(
                rig.arms.arm(bad, "app"),
                ArmAnswer::Refused {
                    refusal: ArmRefusal::Invalid { .. }
                }
            ),
            "{name}"
        );
    }
    assert!(rig.arms.list().armed.is_empty());
}

mod wire {
    use codesense_host_proto::Capability;
    use codesense_pty_core::protocol::{Reply, Request};

    use super::*;
    use crate::session_wake::tests::wire::Client;
    use crate::testing::{wait_until, TestServer};

    #[test]
    fn routine_arm_v1_is_advertised_only_where_it_runs() {
        let plain = TestServer::start();
        plain.enable_wake();
        assert!(!Client::open(plain.root(), "v")
            .hello
            .supports(&Capability::RoutineArmV1));
        let server = TestServer::start();
        server.enable_wake();
        server.enable_arms();
        assert!(Client::open(server.root(), "v")
            .hello
            .supports(&Capability::RoutineArmV1));
    }

    // Over the socket: armed by a desktop that then goes away, the
    // routine fires on the host's own clock with nobody connected; the
    // desktop that comes back reads the run from the log. With nothing
    // prepared on the machine it is `undelivered(needs_preparation)`.
    #[test]
    fn armed_over_the_socket_it_fires_with_no_desktop_and_the_returning_one_reads_the_run() {
        let server = TestServer::start();
        server.enable_wake();
        let clock = Arc::new(FakeClock::new());
        server.enable_arms_with(clock.clone());
        let mut desk = Client::open(server.root(), "desk-a");
        let mut fresh = arm(ArmMode::Fresh, 1);
        fresh.launch = None;
        let Reply::RoutineArmAnswer { answer } = desk
            .call(Request::RoutineArm {
                arm: Box::new(fresh),
            })
            .unwrap()
        else {
            panic!()
        };
        let ArmAnswer::Armed { info } = *answer else {
            panic!("{answer:?}")
        };
        assert_eq!(info.originator.desktop, "desk-a");
        assert_eq!(info.next_at_ms, Some(NINE_DENVER));
        drop(desk);
        clock.set(NINE_DENVER + 1_000);
        let arms = server.host().arms().unwrap();
        // The host's own ticker fires it (no client connected).
        assert!(wait_until(|| arms.runs(0).runs.len() == 2));
        let mut back = Client::open(server.root(), "desk-a");
        let Reply::RoutineArmRuns { log } =
            back.call(Request::RoutineArmRuns { since: 0 }).unwrap()
        else {
            panic!()
        };
        let last = log.runs.last().unwrap();
        assert_eq!(last.run_id, arm_run_id("acme/core/triage", 1, NINE_DENVER));
        assert!(matches!(
            last.status,
            ArmRunStatus::Undelivered {
                reason: UndeliveredReason::NeedsPreparation,
                ..
            }
        ));
        // Moved back to the desktop: disarmed, and listed as a tombstone.
        let Reply::RoutineArmAnswer { answer } = back
            .call(Request::RoutineDisarm {
                key: "acme/core/triage".into(),
                epoch: 2,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            *answer,
            ArmAnswer::Disarmed {
                was_armed: true,
                last_fired_ms: Some(NINE_DENVER)
            }
        );
        let Reply::RoutineArms { list } = back.call(Request::RoutineArms).unwrap() else {
            panic!()
        };
        assert!(list.armed.is_empty());
        assert_eq!(list.disarmed[0].epoch, 2);
    }
}

// Coordinator scope: with no desktop to relay its CodeSense MCP,
// the machine answers the agent's routine_done / signal_done itself, for the
// runs it fired into that session, and every other tool a typed
// `desktop_offline`, so the MCP server never goes dead.
mod machine_mcp {
    use super::*;
    use crate::routine_arm::local_mcp::{respond, Offline, DESKTOP_OFFLINE, DESKTOP_OFFLINE_CODE};

    fn call(id: u64, tool: &str, args: serde_json::Value) -> String {
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}}).to_string()
    }

    fn answer(line: &str, session: &str, arms: Option<&RoutineArms>) -> serde_json::Value {
        serde_json::from_str(
            &respond(line, session, arms, true, Offline::DesktopGone).expect("answered"),
        )
        .unwrap()
    }

    /// A fresh routine the machine fired and started `session` for.
    fn fired(rig: &Rig) -> (String, String) {
        let mut every_minute = arm(ArmMode::Fresh, 1);
        every_minute.schedules[0].cron = "* * * * *".into();
        armed(rig, every_minute);
        rig.clock.set(WALL + MIN);
        rig.arms.tick();
        let (session, _) = rig.fake.launched.lock().unwrap()[0].clone();
        (
            session.as_str().to_string(),
            arm_run_id("acme/core/triage", 1, WALL + MIN),
        )
    }

    #[test]
    fn routine_done_finishes_the_run_it_names_in_its_own_session_only() {
        let rig = rig();
        let (session, run_id) = fired(&rig);
        let forged = answer(
            &call(
                1,
                "routine_done",
                serde_json::json!({"runId": run_id, "outcome": "success"}),
            ),
            "some-other-session",
            Some(&rig.arms),
        );
        assert!(forged["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not started or woken in this session"));
        let done = answer(
            &call(
                2,
                "routine_done",
                serde_json::json!({"runId": run_id, "outcome": "success", "summary": "triaged 3"}),
            ),
            &session,
            Some(&rig.arms),
        );
        assert_eq!(done["id"], 2);
        assert!(done["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"recordedOn\":\"machine\""));
        let last = rig.arms.runs(0).runs.pop().unwrap();
        assert_eq!(last.run_id, run_id);
        assert!(matches!(
            last.status,
            ArmRunStatus::Done { ref outcome, ref summary, .. } if outcome == "success" && summary == "triaged 3"
        ));
        let again = answer(
            &call(
                3,
                "routine_done",
                serde_json::json!({"runId": run_id, "outcome": "failed"}),
            ),
            &session,
            Some(&rig.arms),
        );
        assert!(again["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already finished"));
    }

    #[test]
    fn signal_done_finishes_the_sessions_latest_run() {
        let rig = rig();
        let (session, run_id) = fired(&rig);
        let done = answer(
            &call(
                1,
                "signal_done",
                serde_json::json!({"status": "needs-attention", "summary": "which repo?"}),
            ),
            &session,
            Some(&rig.arms),
        );
        assert!(done.get("result").is_some(), "{done}");
        let last = rig.arms.runs(0).runs.pop().unwrap();
        assert_eq!(last.run_id, run_id);
        assert!(
            matches!(last.status, ArmRunStatus::Done { ref outcome, .. } if outcome == "needs-attention")
        );
        // A session this machine fired nothing into: the desktop owns it.
        let elsewhere = answer(
            &call(2, "signal_done", serde_json::json!({"status": "success"})),
            "a-hand-opened-tab",
            Some(&rig.arms),
        );
        assert_eq!(elsewhere["error"]["data"]["type"], DESKTOP_OFFLINE);
    }

    #[test]
    fn every_other_tool_is_a_typed_desktop_offline_and_the_server_stays_up() {
        let rig = rig();
        let init = answer(
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            "s",
            Some(&rig.arms),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(respond(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "s",
            Some(&rig.arms),
            true,
            Offline::DesktopGone,
        )
        .is_none());
        let list = answer(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "s",
            None,
        );
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["routine_done", "signal_done"]);
        for tool in ["list_machines", "send_message", "schedule_wakeup"] {
            let offline = answer(&call(2, tool, serde_json::json!({})), "s", Some(&rig.arms));
            assert_eq!(offline["error"]["code"], DESKTOP_OFFLINE_CODE);
            assert_eq!(offline["error"]["data"]["type"], DESKTOP_OFFLINE);
            assert!(offline["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("desktop_offline:"));
        }
    }
}

// An agent that finishes its run the moment it starts (before the engine
// recorded the start) still finishes it, and its verdict stays the last word.
#[test]
fn an_agent_finishing_its_run_while_it_is_being_started_keeps_its_verdict() {
    struct Racing {
        inner: Fake,
        arms: Mutex<Option<std::sync::Weak<RoutineArms>>>,
    }
    impl ArmPort for Racing {
        fn candidates(&self, t: &ArmTarget) -> Vec<ArmCandidate> {
            self.inner.candidates(t)
        }
        fn session_live(&self, s: &PtySessionId) -> bool {
            self.inner.session_live(s)
        }
        fn prepared(&self, t: &ArmTarget, l: &ArmLaunch) -> Result<(), String> {
            self.inner.prepared(t, l)
        }
        fn launch(
            &self,
            t: &ArmTarget,
            l: &ArmLaunch,
            session: &PtySessionId,
            prompt: &str,
            if_absent: bool,
            desktop: &str,
        ) -> Result<(), LaunchFailure> {
            self.inner
                .launch(t, l, session, prompt, if_absent, desktop)?;
            let arms = self
                .arms
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .upgrade()
                .unwrap();
            let run = prompt
                .split("| run ")
                .nth(1)
                .unwrap()
                .split(']')
                .next()
                .unwrap();
            arms.complete(session.as_str(), Some(run), "success", "fast")
                .expect("its own run, while starting");
            Ok(())
        }
        fn kill(&self, s: &PtySessionId) {
            self.inner.kill(s)
        }
        fn deliver(&self, r: DeliverRequest) -> Result<WakeOutcome, String> {
            self.inner.deliver(r)
        }
        fn delivery(&self, o: &Originator, s: &PtySessionId, k: &str) -> Option<WakeOutcome> {
            self.inner.delivery(o, s, k)
        }
        fn cancel(&self, o: &Originator, s: &PtySessionId, k: &str) {
            self.inner.cancel(o, s, k)
        }
    }
    let clock = Arc::new(FakeClock::new());
    let port = Arc::new(Racing {
        inner: Fake::default(),
        arms: Mutex::new(None),
    });
    let arms = RoutineArms::open(port.clone(), clock.clone(), "b".into(), None);
    *port.arms.lock().unwrap() = Some(Arc::downgrade(&arms));
    assert!(matches!(
        arms.arm(arm(ArmMode::Fresh, 1), "app"),
        ArmAnswer::Armed { .. }
    ));
    clock.set(NINE_DENVER);
    arms.tick();
    let log = arms.runs(0).runs;
    assert!(
        matches!(log.last().unwrap().status, ArmRunStatus::Done { ref summary, .. } if summary == "fast"),
        "{log:?}"
    );
    assert!(!log
        .iter()
        .any(|r| matches!(r.status, ArmRunStatus::Launched { .. })));
}

/// A fresh routine that fires every minute.
fn every_minute() -> RoutineArm {
    let mut fresh = arm(ArmMode::Fresh, 1);
    fresh.schedules[0].cron = "* * * * *".into();
    fresh
}

// Overlap is decided on the routine's open run, never on whether its
// session lives. An agent that finished its run with routine_done and idles
// at its prompt does not hold the next fire: a daily fresh routine starts
// again every day. Its summary is the next run's {{last_run_summary}}.
#[test]
fn a_fresh_run_its_agent_finished_frees_the_next_fire_while_its_session_idles() {
    let rig = rig();
    armed(&rig, every_minute());
    rig.clock.set(WALL + MIN);
    rig.arms.tick();
    let (first, _) = rig.fake.launched.lock().unwrap()[0].clone();
    let run = arm_run_id("acme/core/triage", 1, WALL + MIN);
    rig.arms
        .complete(
            first.as_str(),
            Some(&run),
            "success",
            "three issues triaged",
        )
        .unwrap();
    assert!(rig.fake.session_live(&first), "its session idles on");
    rig.clock.set(WALL + 2 * MIN);
    rig.arms.tick();
    let launched = rig.fake.launched.lock().unwrap().clone();
    assert_eq!(launched.len(), 2, "{:?}", statuses(&rig));
    assert!(matches!(statuses(&rig)[1], ArmRunStatus::Launched { .. }));
    assert!(
        launched[1].1.contains("(three issues triaged)"),
        "{}",
        launched[1].1
    );
    // While that second run is open, a third fire coalesces into it.
    rig.clock.set(WALL + 3 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 2);
    assert_eq!(
        statuses(&rig)[2],
        ArmRunStatus::Coalesced {
            into: arm_run_id("acme/core/triage", 1, WALL + 2 * MIN)
        }
    );
}

// A fresh run that passes its max_runtime times out on the machine
// (its session is stopped), and the next fire starts the agent again.
#[test]
fn a_fresh_run_past_its_max_runtime_times_out_and_its_session_is_stopped() {
    let rig = rig();
    armed(&rig, arm(ArmMode::Fresh, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    let (session, _) = rig.fake.launched.lock().unwrap()[0].clone();
    rig.clock.set(NINE_DENVER + 29 * MIN);
    rig.arms.tick();
    assert!(matches!(statuses(&rig)[0], ArmRunStatus::Launched { .. }));
    rig.clock.set(NINE_DENVER + 30 * MIN);
    rig.arms.tick();
    assert_eq!(
        statuses(&rig)[0],
        ArmRunStatus::TimedOut {
            at_ms: NINE_DENVER + 30 * MIN
        }
    );
    assert_eq!(*rig.fake.killed.lock().unwrap(), vec![session]);
    // Finished, it can no longer be finished again.
    assert!(rig
        .arms
        .complete(
            "any",
            Some(&arm_run_id("acme/core/triage", 1, NINE_DENVER)),
            "success",
            ""
        )
        .is_err());
}

// A fresh run whose session ended without a verdict is `exited`.
#[test]
fn a_fresh_run_whose_session_ended_is_exited() {
    let rig = rig();
    armed(&rig, every_minute());
    rig.clock.set(WALL + MIN);
    rig.arms.tick();
    rig.fake.live.lock().unwrap().clear();
    rig.clock.set(WALL + MIN + 10_000);
    rig.arms.tick();
    assert!(matches!(statuses(&rig)[0], ArmRunStatus::Exited { .. }));
    rig.clock.set(WALL + 2 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 2);
}

// A session of the agent that is being started (by a desktop's
// launch-if-absent, or this host) is the wake's session: waited for, woken,
// never launched over.
#[test]
fn a_wake_waits_for_a_session_still_starting_and_never_launches_a_second() {
    let rig = rig();
    rig.fake.starting("desk-launched", 2);
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(rig.fake.launched.lock().unwrap().is_empty());
    let delivered = rig.fake.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].session.as_str(), "desk-launched");
}

#[test]
fn a_session_that_never_comes_up_is_not_launched_over() {
    let rig = rig();
    rig.fake.starting("stuck", u32::MAX);
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(rig.fake.launched.lock().unwrap().is_empty());
    assert!(matches!(
        statuses(&rig)[0],
        ArmRunStatus::Undelivered {
            reason: UndeliveredReason::Unavailable,
            ..
        }
    ));
}

// A wake with no session launches if absent; losing the host's
// single flight to another launcher, it wakes that launcher's session.
#[test]
fn a_wake_that_loses_the_launch_flight_wakes_the_winner() {
    let rig = rig();
    *rig.fake.launch_fails.lock().unwrap() =
        Some(LaunchFailure::Busy(PtySessionId::new("desk-won")));
    rig.fake
        .coming_up
        .lock()
        .unwrap()
        .push(("desk-won".into(), 1));
    armed(&rig, arm(ArmMode::Wake, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(*rig.fake.if_absent.lock().unwrap(), vec![true]);
    assert!(rig.fake.launched.lock().unwrap().is_empty());
    let delivered = rig.fake.delivered.lock().unwrap().clone();
    assert_eq!(delivered[0].session.as_str(), "desk-won");
    assert!(matches!(statuses(&rig)[0], ArmRunStatus::Delivered { .. }));
}

// A fresh fire starts a new instance by design: not if-absent.
#[test]
fn a_fresh_fire_is_not_a_launch_if_absent() {
    let rig = rig();
    armed(&rig, arm(ArmMode::Fresh, 1));
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(*rig.fake.if_absent.lock().unwrap(), vec![false]);
}

// `routine_run_now` on a machine-armed routine fires on the machine,
// through its overlap policy and single flight, and leaves its schedule.
#[test]
fn firing_now_goes_through_the_policy_and_leaves_the_schedule_alone() {
    let rig = rig();
    armed(&rig, arm(ArmMode::Fresh, 1));
    let ArmFireAnswer::Fired { run } =
        rig.arms
            .fire_now("acme/core/triage", 1, Some("by hand on {{date}}".into()))
    else {
        panic!()
    };
    assert!(
        matches!(run.status, ArmRunStatus::Launched { .. }),
        "{run:?}"
    );
    assert_eq!(run.source, "manual");
    let (_, prompt) = rig.fake.launched.lock().unwrap()[0].clone();
    assert!(prompt.contains("by hand on 2026-10-06"), "{prompt}");
    assert_eq!(rig.arms.list().armed[0].next_at_ms, Some(NINE_DENVER));
    let ArmFireAnswer::Fired { run: second } = rig.arms.fire_now("acme/core/triage", 1, None)
    else {
        panic!()
    };
    assert_eq!(
        second.status,
        ArmRunStatus::Coalesced {
            into: run.run_id.clone()
        }
    );
    assert!(matches!(
        rig.arms.fire_now("acme/core/triage", 2, None),
        ArmFireAnswer::Refused {
            refusal: ArmRefusal::Stale { held_epoch: 1, .. }
        }
    ));
}

// Machine-armed, desktop closed: a wake fire carries the
// routine's limits into the delivery it makes on this machine's own queue,
// which enforces them (session_wake tests); what the queue decides is the
// run's status: `max_wakes` and `rate_limited` (its notice kept as the run's
// detail) undelivered, a fire inside `min_gap` skipped.
#[test]
fn an_armed_wake_carries_its_limits_and_the_machines_queue_decides() {
    let rig = rig();
    rig.fake.idle("barb-1");
    let mut limited = arm(ArmMode::Wake, 1);
    limited.limits = Some(WakeLimits {
        budget: "routine:acme/core/triage#1".into(),
        max_wakes: Some(3),
        max_wakes_per_day: Some(2),
        min_gap_ms: Some(120 * MIN),
        gap: GapPolicy::Skip,
    });
    armed(&rig, limited.clone());
    let fire = |rig: &Rig| {
        let ArmFireAnswer::Fired { run } = rig.arms.fire_now("acme/core/triage", 1, None) else {
            panic!()
        };
        run.status
    };
    assert!(matches!(fire(&rig), ArmRunStatus::Delivered { .. }));
    assert_eq!(rig.fake.delivered.lock().unwrap()[0].limits, limited.limits);
    *rig.fake.answer.lock().unwrap() = Some(DeliveryStatus::Undelivered {
        reason: UndeliveredReason::MaxWakes,
    });
    assert_eq!(
        fire(&rig),
        ArmRunStatus::Undelivered {
            reason: UndeliveredReason::MaxWakes,
            detail: None
        }
    );
    *rig.fake.answer.lock().unwrap() = Some(DeliveryStatus::Undelivered {
        reason: UndeliveredReason::MinGap,
    });
    assert!(matches!(fire(&rig), ArmRunStatus::Skipped { why } if why.contains("min_gap")));
    *rig.fake.answer.lock().unwrap() = Some(DeliveryStatus::Undelivered {
        reason: UndeliveredReason::RateLimited,
    });
    *rig.fake.notice.lock().unwrap() = Some("Most came from Loop A".into());
    assert_eq!(
        fire(&rig),
        ArmRunStatus::Undelivered {
            reason: UndeliveredReason::RateLimited,
            detail: Some("Most came from Loop A".into())
        }
    );
}

// The last occurrence fired where it was armed before travels
// with the arm and the tombstone: nothing at or before it fires again.
#[test]
fn a_machine_never_fires_an_occurrence_already_fired_elsewhere() {
    let rig = rig();
    let mut moved = arm(ArmMode::Fresh, 1);
    // The desktop fired 09:00 today; its clock ran ahead of the machine's.
    moved.last_fired_ms = Some(NINE_DENVER);
    armed(&rig, moved);
    assert_eq!(
        rig.arms.list().armed[0].next_at_ms,
        Some(NINE_DENVER + 24 * 60 * MIN)
    );
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert!(rig.fake.launched.lock().unwrap().is_empty());
    rig.clock.set(NINE_DENVER + 24 * 60 * MIN);
    rig.arms.tick();
    assert_eq!(rig.fake.launched.lock().unwrap().len(), 1);
    let day_two = NINE_DENVER + 24 * 60 * MIN;
    assert_eq!(
        rig.arms.disarm("acme/core/triage", 2),
        ArmAnswer::Disarmed {
            was_armed: true,
            last_fired_ms: Some(day_two)
        }
    );
    assert_eq!(rig.arms.list().disarmed[0].last_fired_ms, Some(day_two));
}

// Whether an occurrence was missed is judged against the tick's
// start, so a slow fire never makes the one due beside it look missed.
#[test]
fn a_slow_fire_never_makes_the_next_due_occurrence_look_missed() {
    let rig = rig();
    *rig.fake.slow.lock().unwrap() = Some((rig.clock.clone(), 3 * MIN));
    armed(&rig, arm(ArmMode::Fresh, 1));
    let mut other = arm(ArmMode::Fresh, 1);
    other.key = "acme/core/digest".into();
    armed(&rig, other);
    rig.clock.set(NINE_DENVER);
    rig.arms.tick();
    assert_eq!(
        rig.fake.launched.lock().unwrap().len(),
        2,
        "{:?}",
        statuses(&rig)
    );
}

// macOS machines: no reverse MCP, so no routine without a desktop. The host
// says why, and refuses every arm with it.
#[test]
fn a_host_that_cannot_run_routines_without_a_desktop_refuses_arms_and_says_why() {
    let arms = RoutineArms::open_with(
        Arc::new(Fake::default()),
        Arc::new(FakeClock::new()),
        "b".into(),
        None,
        Some("Linux machines only".into()),
        std::time::Duration::from_millis(1),
    );
    assert_eq!(
        arms.arm(arm(ArmMode::Fresh, 1), "app"),
        ArmAnswer::Refused {
            refusal: ArmRefusal::Invalid {
                detail: "Linux machines only".into()
            }
        }
    );
    assert_eq!(
        arms.list().unsupported.as_deref(),
        Some("Linux machines only")
    );
}

mod machine_mcp_servers {
    use super::*;
    use crate::routine_arm::local_mcp::{respond, Offline, DESKTOP_OFFLINE};

    fn ask(line: &str, codesense: bool, why: Offline) -> serde_json::Value {
        let rig = rig();
        serde_json::from_str(&respond(line, "s", Some(&rig.arms), codesense, why).unwrap()).unwrap()
    }

    // A desktop-relayed MCP server other than CodeSense lists no
    // tools on the machine and answers every call desktop_offline.
    #[test]
    fn another_mcp_server_lists_no_tools_and_answers_every_call_offline() {
        let list = ask(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            false,
            Offline::DesktopGone,
        );
        assert_eq!(list["result"]["tools"], serde_json::json!([]));
        let call = ask(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"routine_done","arguments":{"runId":"r","outcome":"success"}}}"#,
            false,
            Offline::DesktopGone,
        );
        assert_eq!(call["error"]["data"]["type"], DESKTOP_OFFLINE);
    }

    // The error says why: the desktop went away, or the machine
    // started the session while none could relay it.
    #[test]
    fn the_offline_error_says_why_no_desktop_relays_the_session() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_machines","arguments":{}}}"#;
        let gone = ask(line, true, Offline::DesktopGone);
        assert!(gone["error"]["message"]
            .as_str()
            .unwrap()
            .contains("which is not connected to this machine"));
        let started = ask(line, true, Offline::StartedByMachine);
        let message = started["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("was not connected when this machine started this session"),
            "{message}"
        );
        assert!(message.starts_with("desktop_offline:"));
    }
}

// An arm that reached the machine before its desktop
// named itself was stamped with the plain viewer; when that desktop (the
// same install) names itself, the arm is its, and no other install's is.
#[test]
fn an_arm_sent_before_its_desktop_named_itself_is_restamped() {
    let rig = rig();
    let mut ours = arm(ArmMode::Fresh, 1);
    ours.originator.routine_host = Some("host-1".into());
    armed(&rig, ours);
    let mut theirs = arm(ArmMode::Fresh, 1);
    theirs.key = "acme/core/other".into();
    theirs.originator.routine_host = Some("host-2".into());
    armed(&rig, theirs);
    assert_eq!(
        rig.arms.restamp("app", "host-1", "host-1.41"),
        vec!["acme/core/triage".to_string()]
    );
    let list = rig.arms.list();
    let desktop = |key: &str| {
        list.armed
            .iter()
            .find(|info| info.key == key)
            .unwrap()
            .originator
            .desktop
            .clone()
    };
    assert_eq!(desktop("acme/core/triage"), "host-1.41");
    assert_eq!(desktop("acme/core/other"), "app");
    assert!(
        rig.arms.restamp("app", "host-1", "host-1.41").is_empty(),
        "once"
    );
}
