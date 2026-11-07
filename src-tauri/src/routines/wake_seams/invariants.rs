// SPDX-License-Identifier: MIT
//! SW-AGR-7: the user's non-negotiables for `mode: wake`, locked in over the
//! same seam as the rest of `wake_seams` (real coordinator, admission, claim
//! loop, dispatch, run store and delivery core; fake roster and terminal
//! server).
//!
//! - **Wake never duplicates** (R1, story AC2): a routine fired five times
//!   against a live agent leaves exactly one instance, idle or busy, on this
//!   Mac or on a machine; five fires against an absent agent start exactly
//!   one per local routine; remote routines retain shared-agent matching.
//! - **No fire path launches while an instance is live** (the story's guard):
//!   every live state a roster can report (idle, busy, needs-you, unknown,
//!   offline, starting, quitting), on every target (`local`, a machine,
//!   `any`), redriven as the app would, launches nothing, here or there.
//! - **Needs-you is never typed into**, whatever the target.
//!
//! SW-AGR-8: a machine-armed fire is [`FirePath::MachineArmed`]. It never
//! touches this Mac's dispatch: the machine's own scheduler fires it, so its
//! cases (the same matrix: live in every state the machine can see, absent,
//! five fires, one tick) run in [`machine_armed`] through the machine's real
//! `RoutineArms` engine over a fake host that models Barb.

#![cfg(test)]

use super::*;

/// The fires a story counts: "a wake routine fired 5 times".
const FIRES: usize = 5;

/// How the fires arrive. Each one must keep R1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirePath {
    /// One routine fired again and again (`routine_run_now`, a schedule),
    /// each fire through admission and the claim loop, deferred ones
    /// redriven as the app's task does.
    OneAfterAnother,
    /// One routine per fire, all on the same agent, admitted in one tick and
    /// dispatched at the same time (a schedule, an `inbox_idle`, a person's
    /// run-now landing together).
    SameTick,
    /// SW-AGR-8: the machine's own scheduler firing a wake through its wake
    /// queue while the desktop is away. Its cases run in [`machine_armed`]
    /// (the desktop's dispatch never sees it).
    #[allow(dead_code)]
    MachineArmed,
}

/// The fire paths that go through this Mac's dispatch.
const FIRE_PATHS: [FirePath; 2] = [FirePath::OneAfterAnother, FirePath::SameTick];

/// Launches anywhere: this Mac's launcher plus Creates on any machine.
fn launches(w: &World) -> usize {
    w.stub.calls() + w.creates()
}

/// Redrive what was deferred, as the app does, until nothing is handed back
/// (bounded).
async fn settle(w: &World) {
    for _ in 0..4 {
        if w.redrive().await == 0 {
            break;
        }
    }
}

/// Save `n` wake routines on Barb aimed at `target` (one, for
/// [`FirePath::OneAfterAnother`]); their ids.
fn routines(w: &World, path: FirePath, target: &str, n: usize) -> Vec<String> {
    let ids: Vec<String> = match path {
        FirePath::OneAfterAnother => vec!["triage".to_string()],
        FirePath::SameTick => (0..n).map(|i| format!("triage-{i}")).collect(),
        FirePath::MachineArmed => unreachable!("machine-armed fires run in `machine_armed`"),
    };
    for id in &ids {
        save(&w.home(), wake_routine(id, target));
    }
    ids
}

/// Fire `n` times through `path`.
async fn fire_n(w: &World, path: FirePath, ids: &[String], n: usize) {
    match path {
        FirePath::OneAfterAnother => {
            for _ in 0..n {
                manual(&w.home(), &ids[0]);
                w.drain().await;
                settle(w).await;
            }
        }
        FirePath::SameTick => {
            let pointers: Vec<RoutinePointer> = ids
                .iter()
                .take(n)
                .map(|id| pointer_of(&w.home(), &manual(&w.home(), id).run_id))
                .collect();
            let launchers: Vec<Arc<dyn Launcher>> = pointers.iter().map(|_| w.launcher()).collect();
            let dispatched = futures_util::future::join_all(
                pointers
                    .into_iter()
                    .zip(launchers.iter())
                    .map(|(p, l)| super::super::dispatch::execute(w.home(), p, l.as_ref())),
            )
            .await;
            for result in dispatched {
                result.unwrap();
            }
            settle(w).await;
        }
        FirePath::MachineArmed => unreachable!("machine-armed fires run in `machine_armed`"),
    }
}

/// Every run of `ids`.
fn all_runs(w: &World, ids: &[String]) -> Vec<RoutineRun> {
    ids.iter().flat_map(|id| runs_of(&w.home(), id)).collect()
}

// ───────────────────────────── AC2: five fires, one instance ─────────────────────────────

/// Barb is live on `machine`, doing `state`; `FIRES` wake fires. Never a
/// launch; everything typed goes into Barb's one session, each run at most
/// once.
async fn five_fires_keep_one_session(machine: &str, state: SimState, path: FirePath) {
    let w = World::new();
    let ids = routines(&w, path, machine, FIRES);
    let sessions = if machine == LOCAL && matches!(path, FirePath::SameTick) {
        ids.clone()
    } else {
        vec![BARB.to_string()]
    };
    for (index, session) in sessions.iter().enumerate() {
        w.sim(machine).set(session, state);
        let mut live = candidate(
            machine,
            session,
            &barb_key(),
            match state {
                SimState::Idle => Activity::Idle,
                _ => Activity::Busy,
            },
        );
        live.routine_key = Some(key(&ids[index]).to_string());
        w.env.roster.lock().unwrap().push(live);
    }
    fire_n(&w, path, &ids, FIRES).await;
    let cell = format!("{machine} {state:?} {path:?}");
    assert_eq!(launches(&w), 0, "{cell}: Barb is live, nothing launches");
    for _ in 0..=FIRES {
        for session in &sessions {
            w.go(machine, session, SimState::Idle);
        }
    }
    let typed = w.typed();
    assert!(!typed.is_empty(), "{cell}: Barb was woken");
    assert!(
        typed.iter().all(|(session, _)| sessions.contains(session)),
        "{cell}: only Barb's session is typed into: {typed:?}"
    );
    let runs = all_runs(&w, &ids);
    assert_eq!(runs.len(), FIRES, "{cell}: every fire is a run: {runs:?}");
    // R5: no run is typed twice.
    let mut headers: Vec<&str> = typed
        .iter()
        .map(|(_, body)| body.lines().next().unwrap())
        .collect();
    headers.sort_unstable();
    let before = headers.len();
    headers.dedup();
    assert_eq!(
        before,
        headers.len(),
        "{cell}: a run typed twice: {typed:?}"
    );
    for run in &runs {
        assert!(
            run.session_id.is_none()
                || run
                    .session_id
                    .as_ref()
                    .is_some_and(|session| sessions.contains(session)),
            "{cell}: a run bound to another session: {run:?}"
        );
        assert_ne!(run.phase, phase::RUNNING, "{cell}: a run launched: {run:?}");
    }
    // Every fire got through: typed, or folded into one that was.
    assert!(
        runs.iter()
            .all(|r| r.phase == phase::DELIVERED || r.phase == phase::COALESCED),
        "{cell}: {:?}",
        runs.iter()
            .map(|r| (r.phase.as_str(), r.reason.as_deref()))
            .collect::<Vec<_>>()
    );
    assert_eq!(launches(&w), 0, "{cell}");
}

#[tokio::test]
async fn ac2_five_wake_fires_against_a_live_agent_keep_one_session() {
    for path in FIRE_PATHS {
        for machine in [LOCAL, REMOTE] {
            for state in [SimState::Idle, SimState::Busy] {
                five_fires_keep_one_session(machine, state, path).await;
            }
        }
    }
}

/// No Barb anywhere: `FIRES` wake fires start exactly one, on the routine's
/// machine; every other fire goes to that one.
async fn five_fires_start_one(machine: &str, path: FirePath) {
    let w = World::new();
    let ids = routines(&w, path, machine, FIRES);
    fire_n(&w, path, &ids, FIRES).await;
    let cell = format!("{machine} absent {path:?}");
    let expected = if machine == LOCAL { ids.len() } else { 1 };
    assert_eq!(launches(&w), expected, "{cell}");
    if machine == LOCAL {
        assert_eq!(w.creates(), 0, "{cell}");
        for run in all_runs(&w, &ids) {
            if let Some(session) = run.session_id {
                assert!(w
                    .stub
                    .launched()
                    .iter()
                    .any(|launch| launch.session_id == session
                        && launch.request.provenance.routine_key.as_deref()
                            == Some(run.routine_key.as_str())));
            }
        }
    } else {
        assert_eq!(w.stub.calls(), 0, "{cell}");
        for run in all_runs(&w, &ids) {
            assert!(
                run.session_id.is_none() || run.session_id.as_deref() == Some("r-new"),
                "{cell}: {run:?}"
            );
        }
    }
}

/// Concurrent local routines each own their launch; remote routines continue
/// sharing the machine's single agent instance.
#[tokio::test]
async fn same_tick_fires_interleaving_at_the_roster_read_start_exactly_one() {
    for machine in [LOCAL, REMOTE] {
        let w = World::new();
        w.env.roster_yields.store(true, Ordering::SeqCst);
        let ids = routines(&w, FirePath::SameTick, machine, FIRES);
        fire_n(&w, FirePath::SameTick, &ids, FIRES).await;
        assert_eq!(
            launches(&w),
            if machine == LOCAL { FIRES } else { 1 },
            "{machine}"
        );
    }
}

#[tokio::test]
async fn ac2_five_wake_fires_against_an_absent_agent_start_exactly_one() {
    for path in FIRE_PATHS {
        for machine in [LOCAL, REMOTE] {
            five_fires_start_one(machine, path).await;
        }
    }
}

// ───────────────────────────── the guard: live means no launch ─────────────────────────────

/// Every state in which a roster says Barb is live in the target.
const LIVE: [Activity; 7] = [
    Activity::Idle,
    Activity::Busy,
    Activity::NeedsYou,
    Activity::Unknown,
    Activity::Offline,
    Activity::Starting,
    Activity::Quitting,
];

fn sim_state(activity: Activity) -> SimState {
    match activity {
        Activity::Idle => SimState::Idle,
        Activity::NeedsYou => SimState::NeedsYou,
        _ => SimState::Busy,
    }
}

/// Barb is listed on `on`, `activity`; wake routines target `target`
/// (`local`, a machine, or `any`). Fired twice and redriven: nothing is
/// launched anywhere, and only Barb is ever typed into.
async fn live_never_launches(on: &str, target: &str, activity: Activity, path: FirePath) {
    let w = World::new();
    let ids = routines(&w, path, target, 2);
    let sessions = if on == LOCAL && matches!(path, FirePath::SameTick) {
        ids.clone()
    } else {
        vec![BARB.to_string()]
    };
    for (index, session) in sessions.iter().enumerate() {
        w.sim(on).set(session, sim_state(activity));
        let mut barb = candidate(on, session, &barb_key(), activity);
        barb.routine_key = Some(key(&ids[index]).to_string());
        if activity == Activity::Starting {
            barb.session = String::new();
        }
        w.env.roster.lock().unwrap().push(barb);
    }
    fire_n(&w, path, &ids, 2).await;
    let cell = format!("{activity:?} on {on}, target {target}, {path:?}");
    assert_eq!(launches(&w), 0, "{cell}: launched while Barb is live");
    assert!(
        w.typed()
            .iter()
            .all(|(session, _)| sessions.contains(session)),
        "{cell}: {:?}",
        w.typed()
    );
    if activity == Activity::NeedsYou {
        assert!(w.typed().is_empty(), "{cell}: typed into needs-you");
    }
    // Deferred ones (starting, quitting) past their cap end honestly,
    // still without a launch.
    for mut late in w.take_redispatched() {
        late.fired_at -= wake::DEFER_CAP_MS;
        super::super::dispatch::execute(w.home(), late, w.launcher().as_ref())
            .await
            .unwrap();
    }
    assert_eq!(launches(&w), 0, "{cell}: launched after the defer cap");
    for run in all_runs(&w, &ids) {
        assert_ne!(run.phase, phase::RUNNING, "{cell}: {run:?}");
    }
}

#[tokio::test]
async fn guard_no_wake_fire_path_launches_while_an_instance_is_live() {
    for path in FIRE_PATHS {
        for activity in LIVE {
            for (on, target) in [
                (LOCAL, LOCAL),
                (REMOTE, REMOTE),
                (REMOTE, "any"),
                (LOCAL, "any"),
            ] {
                live_never_launches(on, target, activity, path).await;
            }
        }
    }
}

// ───────────────────────────── fresh is a new instance ─────────────────────────────

/// `fresh` starts a new instance on purpose, even with Barb live and idle,
/// and never types into her (§4.2).
#[tokio::test]
async fn fresh_starts_a_new_instance_even_with_the_agent_live() {
    for machine in [LOCAL, REMOTE] {
        let w = World::new();
        w.barb(machine, BARB, SimState::Idle);
        let mut def = wake_routine("nightly", machine);
        def.mode = "fresh".into();
        def.task = "nightly sweep".into();
        save(&w.home(), def);
        manual(&w.home(), "nightly");
        w.drain().await;
        assert_eq!(launches(&w), 1, "{machine}: fresh starts one");
        assert!(w.typed().is_empty(), "{machine}: fresh never wakes Barb");
        let run = only_run(&w.home(), "nightly");
        assert_ne!(run.session_id.as_deref(), Some(BARB), "{machine}: {run:?}");
    }
}

/// [`FirePath::MachineArmed`]: the same R1 matrix, fired by the machine's own
/// scheduler (`RoutineArms`, the session server's engine) over a host that
/// models Barb as the machine sees her, with the host's single flight: a
/// launch-if-absent while anyone runs or is starting her is refused (Busy).
mod machine_armed {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use codesense_host_proto::agent_cli::NativeLaunch;
    use codesense_pty_core::agent_cli::AgentCli;
    use codesense_pty_core::domain::PtySessionId;
    use codesense_pty_core::routine_arm::{
        ArmLaunch, ArmMissed, ArmMode, ArmOverlap, ArmRunStatus, ArmSchedule, ArmTarget, RoutineArm,
    };
    use codesense_pty_core::session_wake::{
        DeliverRequest, DeliveryStatus, Originator, WakeOutcome,
    };
    use codesense_session_server::routine_arm::{
        ArmCandidate, ArmPort, LaunchFailure, RoutineArms,
    };
    use codesense_session_server::session_wake::Clock;

    use super::{Activity, FirePath, FIRES};

    const BARB: &str = "barb-1";
    const AGENT: &str = "acme:core/barb";
    /// 2026-10-06T09:00:00Z.
    const NINE: u64 = 1_791_277_200_000;

    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn mono_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
        fn wall_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
        fn sleep_ms(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    /// The machine as its engine sees it: Barb (if listed) in one state, and
    /// what was started and typed.
    #[derive(Default)]
    struct Host {
        barb: Mutex<Option<Activity>>,
        started: Mutex<Vec<String>>,
        typed: Mutex<Vec<(String, String)>>,
        queued: Mutex<Vec<DeliverRequest>>,
    }

    impl Host {
        fn live(&self) -> Vec<(String, bool, bool)> {
            let mut live: Vec<(String, bool, bool)> = self
                .barb
                .lock()
                .unwrap()
                .iter()
                .map(|a| {
                    (
                        BARB.to_string(),
                        *a == Activity::Idle,
                        *a == Activity::Starting,
                    )
                })
                .collect();
            // A session the engine started is up and busy with its first task.
            live.extend(
                self.started
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|s| (s.clone(), false, false)),
            );
            live
        }
    }

    impl ArmPort for Host {
        fn candidates(&self, _: &ArmTarget) -> Vec<ArmCandidate> {
            self.live()
                .into_iter()
                .map(|(session, idle, starting)| ArmCandidate {
                    session: PtySessionId::new(session),
                    idle,
                    recency: 1,
                    starting,
                })
                .collect()
        }
        fn session_live(&self, session: &PtySessionId) -> bool {
            self.live()
                .iter()
                .any(|(s, _, starting)| s == session.as_str() && !starting)
        }
        fn prepared(&self, _: &ArmTarget, _: &ArmLaunch) -> Result<(), String> {
            Ok(())
        }
        fn launch(
            &self,
            _: &ArmTarget,
            _: &ArmLaunch,
            session: &PtySessionId,
            _: &str,
            if_absent: bool,
            _: &str,
        ) -> Result<(), LaunchFailure> {
            // The host's single flight.
            if if_absent {
                if let Some((running, _, _)) = self.live().into_iter().next() {
                    return Err(LaunchFailure::Busy(PtySessionId::new(running)));
                }
            }
            self.started
                .lock()
                .unwrap()
                .push(session.as_str().to_string());
            Ok(())
        }
        fn kill(&self, _: &PtySessionId) {}
        fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String> {
            // Idle-gated, as the host's wake queue is: typed only into an
            // idle session, queued otherwise (needs-you is never typed into).
            let idle = self
                .live()
                .iter()
                .any(|(s, idle, _)| s == request.session.as_str() && *idle);
            let status = if idle {
                self.typed.lock().unwrap().push((
                    request.session.as_str().to_string(),
                    request.idempotency_key.clone(),
                ));
                DeliveryStatus::Delivered { at_ms: 1 }
            } else {
                self.queued.lock().unwrap().push(request.clone());
                DeliveryStatus::Queued {
                    deadline_ms: u64::MAX,
                }
            };
            Ok(WakeOutcome {
                notice: None,
                seq: 1,
                delivery_id: request.idempotency_key.clone(),
                idempotency_key: request.idempotency_key,
                session: request.session,
                marker: request.marker,
                originator: request.originator,
                timer_id: None,
                status,
                host_boot_id: "boot".into(),
                at_ms: 1,
            })
        }
        fn delivery(&self, _: &Originator, _: &PtySessionId, _: &str) -> Option<WakeOutcome> {
            None
        }
        fn cancel(&self, _: &Originator, _: &PtySessionId, _: &str) {}
    }

    fn arm(key: &str) -> RoutineArm {
        RoutineArm {
            limits: None,
            key: key.into(),
            epoch: 1,
            revision: 1,
            name: key.into(),
            mode: ArmMode::Wake,
            schedules: vec![ArmSchedule {
                cron: "* * * * *".into(),
                tz: "UTC".into(),
            }],
            task: "triage".into(),
            target: ArmTarget {
                agent: AGENT.into(),
                workspace_id: "objects".into(),
                cwd: "/home/dev/work".into(),
                owner: None,
                allow_wake: false,
            },
            max_wait_ms: 60_000,
            overlap: ArmOverlap::Coalesce,
            missed: ArmMissed::Skip,
            launch: Some(ArmLaunch {
                launch: NativeLaunch::Cli(AgentCli::Claude),
                agent_startup: codesense_host_proto::reverse_mcp::AgentStartup {
                    args: vec![],
                    settings: serde_json::json!({}),
                    prompt: None,
                    env: Default::default(),
                },
                prepared_paths: vec![],
                prepared_digest: String::new(),
                workspace: codesense_pty_core::domain::WorkspaceKey::new("ws"),
                descriptor: None,
                tags: Default::default(),
            }),
            originator: Originator {
                desktop: String::new(),
                routine_host: Some("mac".into()),
            },
            author: String::new(),
            max_runtime_ms: 0,
            last_fired_ms: None,
            last_run_summary: None,
            last_run_at_ms: None,
            source_digest: String::new(),
        }
    }

    /// `FIRES` fires of wake routines on Barb: one routine every minute, or
    /// (`same_tick`) `FIRES` routines due in one tick. What the host saw.
    fn fire(barb: Option<Activity>, same_tick: bool) -> (Arc<Host>, Arc<RoutineArms>) {
        let host = Arc::new(Host::default());
        *host.barb.lock().unwrap() = barb;
        let clock = Arc::new(TestClock(AtomicU64::new(NINE - 30_000)));
        // A starting Barb is waited for briefly, never launched over.
        let engine = RoutineArms::open_with(
            host.clone(),
            clock.clone(),
            "boot".into(),
            None,
            None,
            std::time::Duration::from_millis(20),
        );
        let keys: Vec<String> = if same_tick {
            (0..FIRES).map(|i| format!("triage-{i}")).collect()
        } else {
            vec!["triage".into()]
        };
        for key in &keys {
            assert!(
                matches!(
                    engine.arm(arm(key), "app"),
                    codesense_pty_core::routine_arm::ArmAnswer::Armed { .. }
                ),
                "{key} armed"
            );
        }
        let ticks = if same_tick { 1 } else { FIRES as u64 };
        for minute in 0..ticks {
            clock
                .0
                .store(NINE + minute * 60_000 + 1_000, Ordering::SeqCst);
            engine.tick();
        }
        (host, engine)
    }

    fn cell(barb: Option<Activity>, same_tick: bool) -> String {
        format!(
            "{barb:?} {:?} same_tick={same_tick}",
            FirePath::MachineArmed
        )
    }

    /// Every state the machine can see Barb in. (`Offline` is the desktop's
    /// view of a machine; on the machine itself she is listed or not.)
    const LIVE: [Activity; 6] = [
        Activity::Idle,
        Activity::Busy,
        Activity::NeedsYou,
        Activity::Unknown,
        Activity::Starting,
        Activity::Quitting,
    ];

    // The guard: live in any state, no machine-armed fire launches; only
    // Barb is ever delivered to, and needs-you is never typed into.
    #[test]
    fn guard_no_machine_armed_fire_launches_while_barb_is_live() {
        for same_tick in [false, true] {
            for activity in LIVE {
                let (host, engine) = fire(Some(activity), same_tick);
                let cell = cell(Some(activity), same_tick);
                assert!(host.started.lock().unwrap().is_empty(), "{cell}: launched");
                assert!(
                    host.typed.lock().unwrap().iter().all(|(s, _)| s == BARB)
                        && host
                            .queued
                            .lock()
                            .unwrap()
                            .iter()
                            .all(|r| r.session.as_str() == BARB),
                    "{cell}: delivered elsewhere"
                );
                if activity != Activity::Idle {
                    assert!(
                        host.typed.lock().unwrap().is_empty(),
                        "{cell}: typed into a non-idle Barb"
                    );
                }
                assert!(
                    engine
                        .runs(0)
                        .runs
                        .iter()
                        .all(|r| !matches!(r.status, ArmRunStatus::Launched { .. })),
                    "{cell}"
                );
            }
        }
    }

    // AC2: five fires against a live Barb keep one session, each run typed
    // at most once (R5).
    #[test]
    fn ac2_five_machine_armed_fires_against_a_live_agent_keep_one_session() {
        for same_tick in [false, true] {
            for activity in [Activity::Idle, Activity::Busy] {
                let (host, _) = fire(Some(activity), same_tick);
                let cell = cell(Some(activity), same_tick);
                assert!(host.started.lock().unwrap().is_empty(), "{cell}");
                let mut keys: Vec<String> = host
                    .typed
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(_, k)| k.clone())
                    .chain(
                        host.queued
                            .lock()
                            .unwrap()
                            .iter()
                            .map(|r| r.idempotency_key.clone()),
                    )
                    .collect();
                assert_eq!(keys.len(), FIRES, "{cell}: every fire reached Barb");
                keys.sort();
                keys.dedup();
                assert_eq!(keys.len(), FIRES, "{cell}: a run delivered twice");
            }
        }
    }

    // AC2: five fires against an absent Barb start exactly one; every later
    // fire goes to that one.
    #[test]
    fn ac2_five_machine_armed_fires_against_an_absent_agent_start_exactly_one() {
        for same_tick in [false, true] {
            let (host, _) = fire(None, same_tick);
            let cell = cell(None, same_tick);
            let started = host.started.lock().unwrap().clone();
            assert_eq!(started.len(), 1, "{cell}: exactly one Barb");
            assert!(
                host.queued
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|r| r.session.as_str() == started[0]),
                "{cell}: later fires go to the one started"
            );
            assert_eq!(host.queued.lock().unwrap().len(), FIRES - 1, "{cell}");
        }
    }
}
