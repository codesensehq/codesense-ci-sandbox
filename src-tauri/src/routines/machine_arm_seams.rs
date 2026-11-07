// SPDX-License-Identifier: MIT
//! SW-AGR-8 seam tests: a routine armed on a machine, end to end between the
//! desktop's real service, scheduler pass, sync and run store, and the
//! machine's real `RoutineArms` engine (the session server's), over a fake
//! host port and an injected clock. "Offline" is the desktop's view of the
//! machine: the engine keeps firing on its own clock meanwhile.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
use codesense_host_proto::reverse_mcp::AgentStartup;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::routine_arm::{
    arm_run_id, ArmAnswer, ArmLaunch, ArmList, ArmRunLog, ArmTarget, RoutineArm,
};
use codesense_pty_core::session_wake::{DeliverRequest, DeliveryStatus, Originator, WakeOutcome};
use codesense_session_server::routine_arm::{ArmCandidate, ArmPort, LaunchFailure, RoutineArms};
use codesense_session_server::session_wake::Clock;

use super::discover::{discover_routines, DiscoveredRoutine};
use super::machine_arm::{
    handovers, pending_handovers, prepare_refresh, publish_refresh, stale_arms, sync_machine,
    AgentFacts, ArmEnv, ArmHost, ArmLookup, ArmPlace, ArmPrepare, Carried, SyncInput, SyncReport,
};
use super::model::{Author, RoutineDef, RoutineMode, RoutineTrigger};
use super::run_seams::{key, ms, routine, save, store, ticker};
use super::service::{self, ArmPlacement};
use super::test_support::{alice, stub_agent, StubLauncher, TempHome};

const MACHINE: &str = "m-test-1";
const DAY: i64 = 24 * 60 * 60 * 1000;

struct MachineClock(AtomicU64);

impl Clock for MachineClock {
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

/// The machine's host: no agent session yet; a start is recorded.
#[derive(Default)]
struct MachinePort {
    idle: Mutex<Vec<String>>,
    launched: Mutex<Vec<(String, String)>>,
    ended: Mutex<Vec<String>>,
    delivered: Mutex<Vec<DeliverRequest>>,
    /// What the machine's wake queue decides (SW-AGR-13: its limits).
    decide: Mutex<Option<DeliveryStatus>>,
}

impl ArmPort for MachinePort {
    fn candidates(&self, _: &ArmTarget) -> Vec<ArmCandidate> {
        self.idle
            .lock()
            .unwrap()
            .iter()
            .map(|session| ArmCandidate {
                session: PtySessionId::new(session),
                idle: true,
                recency: 1,
                starting: false,
            })
            .collect()
    }
    fn session_live(&self, session: &PtySessionId) -> bool {
        (self
            .launched
            .lock()
            .unwrap()
            .iter()
            .any(|(s, _)| s == session.as_str())
            && !self
                .ended
                .lock()
                .unwrap()
                .iter()
                .any(|s| s == session.as_str()))
            || self
                .idle
                .lock()
                .unwrap()
                .iter()
                .any(|s| s == session.as_str())
    }
    fn prepared(&self, _: &ArmTarget, _: &ArmLaunch) -> Result<(), String> {
        Ok(())
    }
    fn launch(
        &self,
        _: &ArmTarget,
        _: &ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        _: bool,
        _: &str,
    ) -> Result<(), LaunchFailure> {
        self.launched
            .lock()
            .unwrap()
            .push((session.as_str().to_string(), prompt.to_string()));
        Ok(())
    }
    fn kill(&self, session: &PtySessionId) {
        self.ended
            .lock()
            .unwrap()
            .push(session.as_str().to_string());
    }
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String> {
        self.delivered.lock().unwrap().push(request.clone());
        Ok(WakeOutcome {
            seq: 1,
            delivery_id: "d".into(),
            idempotency_key: request.idempotency_key,
            session: request.session,
            marker: request.marker,
            originator: request.originator,
            timer_id: None,
            status: self
                .decide
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(DeliveryStatus::Delivered { at_ms: 42 }),
            host_boot_id: "boot".into(),
            at_ms: 42,
            notice: None,
        })
    }
    fn delivery(&self, _: &Originator, _: &PtySessionId, _: &str) -> Option<WakeOutcome> {
        None
    }
    fn cancel(&self, _: &Originator, _: &PtySessionId, _: &str) {}
}

/// The machine: its engine, its clock, and whether the desktop reaches it.
struct Machine {
    engine: Arc<RoutineArms>,
    port: Arc<MachinePort>,
    clock: Arc<MachineClock>,
    online: AtomicBool,
    prepare_fails: AtomicBool,
    prepared_workspace: tempfile::TempDir,
    reject_arms: AtomicBool,
    runs_gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    prepare_gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    arm_gate: Mutex<
        Option<(
            bool,
            std::sync::mpsc::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    /// Its terminal server predates wake limits (proto minor 12).
    before_limits: AtomicBool,
    /// The routine's agent's manifest, as this Mac reads it.
    manifest: Mutex<String>,
    /// SW-XS-25: preparations this Mac ran into the machine's workspace.
    prepares: AtomicUsize,
    /// SW-XS-25: arms pushed with a launch recipe (only a preparation makes one).
    launch_pushes: AtomicUsize,
}

impl Machine {
    fn new(at: i64) -> Arc<Self> {
        let port = Arc::new(MachinePort::default());
        let clock = Arc::new(MachineClock(AtomicU64::new(at as u64)));
        let engine = RoutineArms::open(port.clone(), clock.clone(), "boot-1".into(), None);
        Arc::new(Self {
            engine,
            port,
            clock,
            online: AtomicBool::new(true),
            prepare_fails: AtomicBool::new(false),
            prepared_workspace: tempfile::tempdir().unwrap(),
            reject_arms: AtomicBool::new(false),
            runs_gate: Mutex::new(None),
            prepare_gate: Mutex::new(None),
            arm_gate: Mutex::new(None),
            before_limits: AtomicBool::new(false),
            manifest: Mutex::new("manifest-1".into()),
            prepares: AtomicUsize::new(0),
            launch_pushes: AtomicUsize::new(0),
        })
    }
    /// The machine's own clock moves to `at` and it runs a pass: whoever is
    /// connected or not.
    fn tick_at(&self, at: i64) {
        self.clock.0.store(at as u64, Ordering::SeqCst);
        self.engine.tick();
    }
    fn launches(&self) -> usize {
        self.port.launched.lock().unwrap().len()
    }
}

struct Wire(Arc<Machine>);

impl ArmHost for Wire {
    fn arms(&self) -> Result<ArmList, String> {
        Ok(self.0.engine.list())
    }
    fn arm(&self, arm: RoutineArm) -> Result<ArmAnswer, String> {
        if self.0.reject_arms.load(Ordering::SeqCst) {
            return Err("connection lost while arming".into());
        }
        if arm.launch.is_some() {
            self.0.launch_pushes.fetch_add(1, Ordering::SeqCst);
        }
        let gate = self.0.arm_gate.lock().unwrap().take();
        let wait = |gate: &(
            bool,
            std::sync::mpsc::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )| {
            gate.1.send(()).unwrap();
            gate.2
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        };
        if let Some(gate) = gate.as_ref().filter(|g| !g.0) {
            wait(gate);
        }
        let answer = self.0.engine.arm(arm, "app");
        if let Some(gate) = gate.as_ref().filter(|g| g.0) {
            wait(gate);
        }
        Ok(answer)
    }
    fn disarm(&self, key: &str, epoch: u64) -> Result<ArmAnswer, String> {
        Ok(self.0.engine.disarm(key, epoch))
    }
    fn runs(&self, since: u64) -> Result<ArmRunLog, String> {
        let log = self.0.engine.runs(since);
        let gate = self.0.runs_gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        Ok(log)
    }
    fn enforces_wake_limits(&self) -> bool {
        !self.0.before_limits.load(Ordering::SeqCst)
    }
}

struct Env(Arc<Machine>);

impl ArmEnv for Env {
    fn host(&self, machine: &str) -> ArmLookup {
        if machine != MACHINE {
            return ArmLookup::Unknown;
        }
        if !self.0.online.load(Ordering::SeqCst) {
            return ArmLookup::Offline;
        }
        ArmLookup::Ready(Arc::new(Wire(self.0.clone())))
    }
    fn place(&self, _: &DiscoveredRoutine, _: &str) -> Result<ArmPlace, String> {
        Ok(ArmPlace {
            workspace_id: "objects".into(),
            handle: "remote-workspace-x".into(),
            cwd: "/home/dev/work".into(),
        })
    }
}

impl ArmPrepare for Env {
    fn prepare(
        &self,
        routine: &DiscoveredRoutine,
        _: &str,
        _: &ArmPlace,
    ) -> Result<ArmLaunch, String> {
        self.0.prepares.fetch_add(1, Ordering::SeqCst);
        let prepared_digest = self.0.manifest.lock().unwrap().clone();
        let gate = self.0.prepare_gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        if self.0.prepare_fails.load(Ordering::SeqCst) {
            return Err("the workspace is not open on the machine".into());
        }
        // Managed instruction files are shared workspace side effects, even
        // when publication later discards this preparation's arm.
        std::fs::write(
            self.0.prepared_workspace.path().join("CLAUDE.md"),
            format!("Instructions for {}\n", routine.agent_key.as_ref().unwrap()),
        )
        .unwrap();
        Ok(ArmLaunch {
            launch: NativeLaunch::Cli(AgentCli::Claude),
            agent_startup: AgentStartup {
                args: vec![],
                settings: serde_json::json!({}),
                prompt: None,
                env: Default::default(),
            },
            prepared_paths: vec![],
            prepared_digest,
            workspace: crate::commands::pty_layout::workspace_key("acme/api:backend", "objects"),
            descriptor: None,
            tags: Default::default(),
        })
    }
}

struct Pair(Arc<Machine>, Arc<Machine>);
impl ArmEnv for Pair {
    fn host(&self, machine: &str) -> ArmLookup {
        match machine {
            MACHINE => ArmLookup::Ready(Arc::new(Wire(self.0.clone()))),
            "m-test-2" => ArmLookup::Ready(Arc::new(Wire(self.1.clone()))),
            _ => ArmLookup::Unknown,
        }
    }
    fn place(&self, r: &DiscoveredRoutine, m: &str) -> Result<ArmPlace, String> {
        Env(self.1.clone()).place(r, m)
    }
}
impl ArmPrepare for Pair {
    fn prepare(&self, r: &DiscoveredRoutine, m: &str, p: &ArmPlace) -> Result<ArmLaunch, String> {
        Env(self.1.clone()).prepare(r, m, p)
    }
}

/// One background sync of this Mac with the machine (the 30 s timer, or a
/// reconnect), as the app's would run it. It never prepares.
fn sync(temp: &TempHome, machine: &Arc<Machine>, now: i64) -> SyncReport {
    sync_with(temp, machine, now, &super::wake::NoNotify)
}

fn sync_with(
    temp: &TempHome,
    machine: &Arc<Machine>,
    now: i64,
    notify: &dyn super::wake::WakeNotify,
) -> SyncReport {
    let _sync = super::machine_arm_app::SYNCING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    with_input(temp, machine, now, notify, |input| {
        sync_machine(&Env(machine.clone()), input, MACHINE)
    })
}

/// SW-XS-25: the explicit refresh an arm, an edit or "Refresh on machine"
/// runs for routine `id`: it prepares and pushes the arm with its launch.
fn refresh(temp: &TempHome, machine: &Arc<Machine>, now: i64, id: &str) -> Result<String, String> {
    let _refresh = super::machine_arm_app::REFRESHING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let env = Env(machine.clone());
    let prepared = with_input(temp, machine, now, &super::wake::NoNotify, |input| {
        prepare_refresh(&env, &env, input, &key(id).to_string())
    })?;
    let _sync = super::machine_arm_app::SYNCING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    with_input(temp, machine, now, &super::wake::NoNotify, |input| {
        publish_refresh(&env, input, prepared)
    })
}

fn with_input<R>(
    temp: &TempHome,
    machine: &Arc<Machine>,
    now: i64,
    notify: &dyn super::wake::WakeNotify,
    body: impl FnOnce(&SyncInput<'_>) -> R,
) -> R {
    let home = temp.codesense_home();
    let principal = alice();
    let routines = discover_routines(&home, &principal).unwrap();
    let host = super::host::local_host_id(&home).unwrap();
    let store = store(&home);
    let mode_of =
        |r: &DiscoveredRoutine| service::effective_mode(&home, &principal, &r.key, &r.def);
    let author_of = |routine: &DiscoveredRoutine| {
        super::defs::read_revision(&routine.dir, routine.def.revision)
            .unwrap()
            .map(|record| record.author.name)
            .unwrap_or_default()
    };
    let undiscovered = |value: &str| {
        let key = super::model::RoutineKey::parse(value)?;
        let dir = service::routine_dir_of(&home, &principal, &key).ok()?;
        super::defs::read_def(&dir).ok().flatten()
    };
    let agent_of = |_: &DiscoveredRoutine| {
        Some(AgentFacts {
            manifest_hash: machine.manifest.lock().unwrap().clone(),
            allow_wake: false,
        })
    };
    let settled = |key: &str, from: &str, epoch: u64| {
        let key = super::model::RoutineKey::parse(key).unwrap();
        service::settle_handover(&home, &principal, &key, from, epoch).unwrap();
    };
    let input = SyncInput {
        store: &store,
        principal: &principal,
        host_id: &host,
        routines: &routines,
        mode_of: &mode_of,
        author_of: &author_of,
        undiscovered: &undiscovered,
        agent_of: &agent_of,
        settled: &settled,
        notify,
        now,
    };
    body(&input)
}

fn arm_on(temp: &TempHome, id: &str, placement: ArmPlacement) -> RoutineDef {
    service::arm_on(
        &temp.codesense_home(),
        &alice(),
        &key(id),
        placement,
        &Author::user("alice"),
    )
    .unwrap()
    .def
}

/// 09:00 UTC on 2026-10-06, and the days after.
fn nine(day: i64) -> i64 {
    ms("2026-10-06T09:00:00Z") + day * DAY
}

fn daily(id: &str, mode: &str) -> RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.mode = mode.into();
    def.task = "triage {{date}}".into();
    def
}

/// Pull only the shared definition; host identity and run state stay local.
fn pull_definition(temp: &TempHome, def: &RoutineDef) {
    let home = temp.codesense_home();
    let dir = service::routine_dir_of(&home, &alice(), &key(&def.id)).unwrap();
    super::defs::write_current(&dir, def).unwrap();
}

#[test]
fn every_mac_owner_change_takes_an_epoch_and_a_noop_keeps_the_definition() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    let initial = service::get_current(&temp.codesense_home(), &alice(), &key("nightly"))
        .unwrap()
        .def;
    temp.act_as(&other);
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    pull_definition(&temp, &initial);
    let moved = arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    assert_eq!(moved.arm_epoch, initial.arm_epoch + 1);
    assert_ne!(moved.armed_host, initial.armed_host);
    assert_eq!(arm_on(&temp, "nightly", ArmPlacement::ThisMac), moved);

    temp.act_as(temp.root());
    pull_definition(&temp, &moved);
    let here = service::arm_here(
        &temp.codesense_home(),
        &alice(),
        &key("nightly"),
        &Author::user("alice"),
    )
    .unwrap()
    .def;
    assert_eq!(here.arm_epoch, moved.arm_epoch + 1);
    assert_eq!(
        service::arm_here(
            &temp.codesense_home(),
            &alice(),
            &key("nightly"),
            &Author::user("alice"),
        )
        .unwrap()
        .def,
        here
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mac_takeover_fences_the_old_manager_and_only_its_owner_imports_the_run() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    let first_home = temp.codesense_home();
    save(&first_home, daily("nightly", "fresh"));
    let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();

    temp.act_as(&other);
    let second_home = temp.codesense_home();
    save(&second_home, daily("nightly", "fresh"));
    pull_definition(&temp, &first);
    let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    refresh(&temp, &machine, nine(0) - 30_000, "nightly").unwrap();
    assert_eq!(second.handover_from, None, "the worker did not move");
    assert_eq!(
        arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into())),
        second
    );

    temp.act_as(temp.root());
    let stale_disarm = machine
        .engine
        .disarm(&key("nightly").to_string(), first.arm_epoch);
    let stale_refresh = refresh(&temp, &machine, nine(0) - 20_000, "nightly");
    machine.tick_at(nine(0));
    let run_id = arm_run_id(
        &key("nightly").to_string(),
        second.arm_epoch,
        nine(0) as u64,
    );
    let old_import = sync(&temp, &machine, nine(0) + 1_000);
    temp.act_as(&other);
    let new_import = sync(&temp, &machine, nine(0) + 2_000);
    assert!(matches!(stale_disarm, ArmAnswer::Refused { .. })
        && stale_refresh.is_err() && old_import.recorded == 0 && new_import.recorded > 0,
        "epochs {} -> {}; old disarm: {stale_disarm:?}; old re-arm: {stale_refresh:?}; imports old/new: {}/{}",
        first.arm_epoch, second.arm_epoch, old_import.recorded, new_import.recorded);
    assert_eq!(second.arm_epoch, first.arm_epoch + 1);
    assert_eq!(machine.launches(), 1);
    assert!(store(&first_home).run(&run_id).unwrap().is_none());
    assert!(store(&second_home).run(&run_id).unwrap().is_some());
    assert_eq!(store(&second_home).runs(&key("nightly")).unwrap().len(), 1);
    assert_eq!(sync(&temp, &machine, nine(0) + 3_000).recorded, 0);

    // A tombstone must fence the stale manager too, while allowing the
    // current placement to resume at its own epoch.
    assert!(matches!(
        machine
            .engine
            .disarm(&key("nightly").to_string(), second.arm_epoch),
        ArmAnswer::Disarmed { .. }
    ));
    temp.act_as(temp.root());
    assert!(refresh(&temp, &machine, nine(0) + 4_000, "nightly").is_err());
    assert!(matches!(
        machine
            .engine
            .disarm(&key("nightly").to_string(), first.arm_epoch),
        ArmAnswer::Refused { .. }
    ));
    assert_eq!(sync(&temp, &machine, nine(0) + 5_000).recorded, 0);
    assert!(store(&first_home).runs(&key("nightly")).unwrap().is_empty());
    temp.act_as(&other);
    refresh(&temp, &machine, nine(0) + 6_000, "nightly").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_current_manager_imports_previous_epoch_history_once() {
    for (old_mac_pulled, tombstone) in [(false, false), (true, false), (false, true)] {
        let temp = TempHome::new();
        let other = temp.other_mac("other", 1);
        let first_home = temp.codesense_home();
        save(&first_home, daily("nightly", "fresh"));
        let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        let machine = Machine::new(nine(0) - 60_000);
        refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
        machine.tick_at(nine(0));
        let run_id = arm_run_id(&key("nightly").to_string(), first.arm_epoch, nine(0) as u64);

        temp.act_as(&other);
        let second_home = temp.codesense_home();
        save(&second_home, daily("nightly", "fresh"));
        pull_definition(&temp, &first);
        assert_eq!(sync(&temp, &machine, nine(0) + 500).recorded, 0);
        let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        refresh(&temp, &machine, nine(0) + 1_000, "nightly").unwrap();
        if tombstone {
            assert!(matches!(
                machine
                    .engine
                    .disarm(&key("nightly").to_string(), second.arm_epoch),
                ArmAnswer::Disarmed { .. }
            ));
        }
        temp.act_as(temp.root());
        if old_mac_pulled {
            pull_definition(&temp, &second);
        }
        let old_import = sync(&temp, &machine, nine(0) + 2_000);
        temp.act_as(&other);
        let new_import = sync(&temp, &machine, nine(0) + 3_000);
        assert_eq!(old_import.recorded, 0, "old Mac pulled: {old_mac_pulled}");
        assert!(
            new_import.recorded > 0,
            "the current owner keeps the previous epoch's history"
        );
        assert!(store(&first_home).runs(&key("nightly")).unwrap().is_empty());
        assert!(store(&second_home).run(&run_id).unwrap().is_some());
        assert_eq!(store(&second_home).runs(&key("nightly")).unwrap().len(), 1);
        assert_eq!(sync(&temp, &machine, nine(0) + 4_000).recorded, 0);

        // Return ownership and take it back: the second Mac now replays a
        // log containing a run it already stored before the rewind.
        temp.act_as(temp.root());
        pull_definition(&temp, &second);
        let third = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        refresh(&temp, &machine, nine(0) + 5_000, "nightly").unwrap();
        sync(&temp, &machine, nine(0) + 6_000);
        assert_eq!(store(&first_home).runs(&key("nightly")).unwrap().len(), 1);
        temp.act_as(&other);
        pull_definition(&temp, &third);
        let fourth = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        assert_eq!(fourth.arm_epoch, third.arm_epoch + 1);
        refresh(&temp, &machine, nine(0) + 7_000, "nightly").unwrap();
        sync(&temp, &machine, nine(0) + 8_000);
        assert!(store(&second_home).run(&run_id).unwrap().is_some());
        assert_eq!(store(&second_home).runs(&key("nightly")).unwrap().len(), 1);
        assert_eq!(
            arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into())),
            fourth
        );
        assert_eq!(sync(&temp, &machine, nine(0) + 9_000).recorded, 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_waits_for_an_in_flight_import_before_rewinding() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.tick_at(nine(0));
    temp.act_as(&other);
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    pull_definition(&temp, &first);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.runs_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let completed_early = std::thread::scope(|scope| {
        let importing = scope.spawn(|| sync(&temp, &machine, nine(0) + 1_000));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let takeover_home = &home;
        let taking_over = scope.spawn(move || {
            let result = service::arm_on(
                takeover_home,
                &alice(),
                &key("nightly"),
                ArmPlacement::Machine(MACHINE.into()),
                &Author::user("alice"),
            );
            done_tx.send(()).unwrap();
            result.unwrap()
        });
        let early = done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_ok();
        release_tx.send(()).unwrap();
        importing.join().unwrap();
        taking_over.join().unwrap();
        early
    });
    refresh(&temp, &machine, nine(0) + 2_000, "nightly").unwrap();
    sync(&temp, &machine, nine(0) + 3_000);
    assert!(
        !completed_early,
        "the ownership commit must wait until the old cursor save completes"
    );
    assert_eq!(store(&home).runs(&key("nightly")).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_waits_for_worker_epoch_acknowledgement_before_importing() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    let first_home = temp.codesense_home();
    save(&first_home, daily("nightly", "fresh"));
    let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.tick_at(nine(0));
    temp.act_as(&other);
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    pull_definition(&temp, &first);
    let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    machine.reject_arms.store(true, Ordering::SeqCst);
    assert!(refresh(&temp, &machine, nine(0) + 1_000, "nightly").is_err());
    let pending = sync(&temp, &machine, nine(0) + 2_000);
    temp.act_as(temp.root());
    pull_definition(&temp, &second);
    assert_eq!(sync(&temp, &machine, nine(0) + 3_000).recorded, 0);
    assert_eq!(
        pending.recorded, 0,
        "the new owner has not been acknowledged by the worker"
    );
    assert!(store(&home).runs(&key("nightly")).unwrap().is_empty());
    temp.act_as(&other);
    machine.reject_arms.store(false, Ordering::SeqCst);
    refresh(&temp, &machine, nine(0) + 4_000, "nightly").unwrap();
    sync(&temp, &machine, nine(0) + 5_000);
    assert_eq!(
        store(&home).runs(&key("nightly")).unwrap().len(),
        1,
        "pending history was retained"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replaying_an_earlier_epoch_wake_limit_does_not_pause_the_new_owner() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    let home = temp.codesense_home();
    let mut def = daily("triage", "wake");
    def.policy.max_wakes = Some(1);
    save(&home, def);
    let first = arm_on(&temp, "triage", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "triage").unwrap();
    machine.port.idle.lock().unwrap().push("barb-1".into());
    *machine.port.decide.lock().unwrap() = Some(DeliveryStatus::Undelivered {
        reason: codesense_pty_core::session_wake::UndeliveredReason::MaxWakes,
    });
    machine.tick_at(nine(0));
    sync(&temp, &machine, nine(0) + 1_000);
    assert!(!store(&home).switch_on(&key("triage").to_string()).unwrap());
    service::set_enabled(&home, &alice(), &key("triage"), true).unwrap();
    sync(&temp, &machine, nine(0) + 2_000);
    temp.act_as(&other);
    let second_home = temp.codesense_home();
    save(&second_home, daily("triage", "wake"));
    pull_definition(&temp, &first);
    arm_on(&temp, "triage", ArmPlacement::Machine(MACHINE.into()));
    refresh(&temp, &machine, nine(0) + 3_000, "triage").unwrap();
    let inbox = Inbox::default();
    sync_with(&temp, &machine, nine(0) + 4_000, &inbox);
    assert_eq!(store(&second_home).runs(&key("triage")).unwrap().len(), 1);
    assert!(
        store(&second_home)
            .switch_on(&key("triage").to_string())
            .unwrap(),
        "earlier-epoch outcomes are history"
    );
    assert!(inbox.0.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archived_routine_imports_pending_results_while_a_sibling_keeps_syncing() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    save(&home, daily("sibling", "fresh"));
    let def = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    arm_on(&temp, "sibling", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    refresh(&temp, &machine, nine(0) - 60_000, "sibling").unwrap();
    machine.online.store(false, Ordering::SeqCst);
    machine.tick_at(nine(0));
    service::archive(&home, &alice(), &key("nightly"), &Author::user("alice")).unwrap();
    let discovered = discover_routines(&home, &alice()).unwrap();
    assert!(!discovered.iter().any(|r| r.key == key("nightly")));
    assert!(super::machine_arm::machines_to_sync(&discovered, &[]).contains(&MACHINE.to_string()));
    machine.online.store(true, Ordering::SeqCst);
    sync(&temp, &machine, nine(0) + 1_000);
    let run_id = arm_run_id(&key("nightly").to_string(), def.arm_epoch, nine(0) as u64);
    assert!(
        store(&home).run(&run_id).unwrap().is_some(),
        "archiving retains pending outcomes"
    );
    assert_eq!(sync(&temp, &machine, nine(0) + 2_000).recorded, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_same_worker_takeover_keeps_the_launch_when_preparation_fails() {
    for mode in ["fresh", "wake"] {
        let temp = TempHome::new();
        let other = temp.other_mac("other", 1);
        let mut def = daily("nightly", mode);
        if mode == "wake" {
            def.policy.max_wakes = Some(3);
        }
        save(&temp.codesense_home(), def);
        let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        let machine = Machine::new(nine(0) - 60_000);
        refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
        temp.act_as(&other);
        save(&temp.codesense_home(), daily("nightly", "fresh"));
        pull_definition(&temp, &first);
        let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        machine.prepare_fails.store(true, Ordering::SeqCst);
        assert!(refresh(&temp, &machine, nine(0) - 30_000, "nightly").is_err());
        let held = machine.engine.list().armed.remove(0);
        assert_eq!(
            held.epoch, second.arm_epoch,
            "the new manager must fence the old one"
        );
        assert!(
            held.has_launch,
            "a failed refresh must retain the working preparation"
        );
        machine.tick_at(nine(0));
        assert_eq!(machine.launches(), 1, "{mode}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_does_not_reuse_a_launch_for_changed_content() {
    for changed in ["task", "manifest"] {
        let temp = TempHome::new();
        let other = temp.other_mac("other", 1);
        save(&temp.codesense_home(), daily("nightly", "fresh"));
        let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        let machine = Machine::new(nine(0) - 60_000);
        refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
        temp.act_as(&other);
        let home = temp.codesense_home();
        save(&home, daily("nightly", "fresh"));
        pull_definition(&temp, &first);
        if changed == "task" {
            let mut edited = first.clone();
            edited.task = "different task".into();
            service::save(
                &home,
                &alice(),
                service::SaveTarget::Update {
                    key: key("nightly"),
                    base_revision: first.revision,
                },
                edited,
                &Author::user("alice"),
            )
            .unwrap();
        } else {
            *machine.manifest.lock().unwrap() = "manifest-2".into();
        }
        let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
        machine.prepare_fails.store(true, Ordering::SeqCst);
        assert!(refresh(&temp, &machine, nine(0) - 30_000, "nightly").is_err());
        let held = machine.engine.list().armed.remove(0);
        assert_eq!(held.epoch, second.arm_epoch);
        assert!(
            !held.has_launch,
            "{changed} changed since the content was prepared"
        );
        machine.tick_at(nine(0));
        assert_eq!(machine.launches(), 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_worker_imports_a_late_completion_after_two_moves_once() {
    const SECOND: &str = "m-test-2";
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    save(&home, routine("sibling", "0 10 * * *"));
    let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    arm_on(&temp, "sibling", ArmPlacement::Machine(MACHINE.into()));
    let one = Machine::new(nine(0) - 60_000);
    let two = Machine::new(nine(0) + 1_000);
    refresh(&temp, &one, nine(0) - 60_000, "nightly").unwrap();
    refresh(&temp, &one, nine(0) - 60_000, "sibling").unwrap();
    one.tick_at(nine(0));
    let session = one.port.launched.lock().unwrap()[0].0.clone();
    let run_id = arm_run_id(&key("nightly").to_string(), first.arm_epoch, nine(0) as u64);
    let pair = Pair(one.clone(), two);
    let second = arm_on(&temp, "nightly", ArmPlacement::Machine(SECOND.into()));
    with_input(
        &temp,
        &one,
        nine(0) + 1_000,
        &super::wake::NoNotify,
        |input| sync_machine(&pair, input, MACHINE),
    );
    with_input(
        &temp,
        &one,
        nine(0) + 2_000,
        &super::wake::NoNotify,
        |input| {
            let prepared =
                prepare_refresh(&pair, &pair, input, &key("nightly").to_string()).unwrap();
            publish_refresh(&pair, input, prepared).unwrap()
        },
    );
    let third = arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    with_input(
        &temp,
        &one,
        nine(0) + 3_000,
        &super::wake::NoNotify,
        |input| sync_machine(&pair, input, SECOND),
    );
    assert_eq!(
        one.engine
            .list()
            .disarmed
            .iter()
            .find(|t| t.key == key("nightly").to_string())
            .unwrap()
            .epoch,
        second.arm_epoch
    );
    assert_eq!(third.arm_epoch, second.arm_epoch + 1);
    let discovered = discover_routines(&home, &alice()).unwrap();
    assert!(super::machine_arm::machines_to_sync(&discovered, &[]).contains(&MACHINE.to_string()));
    one.engine
        .complete(
            &session,
            Some(&run_id),
            "success",
            "finished on the retired worker",
        )
        .unwrap();
    sync(&temp, &one, nine(0) + 4_000);
    let run = store(&home).run(&run_id).unwrap().unwrap();
    assert_eq!(run.phase, "succeeded");
    assert_eq!(
        run.result_text.as_deref(),
        Some("finished on the retired worker")
    );
    assert_eq!(store(&home).runs(&key("nightly")).unwrap().len(), 1);
    assert_eq!(sync(&temp, &one, nine(0) + 5_000).recorded, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_author_change_during_takeover_keeps_the_working_launch() {
    let temp = TempHome::new();
    let other = temp.other_mac("other", 1);
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let first = service::arm_on(
        &home,
        &alice(),
        &key("nightly"),
        ArmPlacement::Machine(MACHINE.into()),
        &Author::agent("triage-agent"),
    )
    .unwrap()
    .def;
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    temp.act_as(&other);
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    pull_definition(&temp, &first);
    let second = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    machine.prepare_fails.store(true, Ordering::SeqCst);
    assert!(refresh(&temp, &machine, nine(0) - 30_000, "nightly").is_err());
    let held = machine.engine.list().armed.remove(0);
    assert_eq!(held.epoch, second.arm_epoch);
    assert!(
        held.has_launch,
        "revision attribution is not prepared content"
    );
    machine.tick_at(nine(0));
    assert_eq!(machine.launches(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unrelated_local_placement_finishes_while_remote_preparation_waits() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    save(&home, daily("local", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let mut local = service::get_current(&home, &alice(), &key("local"))
        .unwrap()
        .def;
    local.armed_host = "another-mac".into();
    pull_definition(&temp, &local);
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.prepare_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let completed_while_preparing = std::thread::scope(|scope| {
        let preparing = scope.spawn(|| refresh(&temp, &machine, nine(0) - 30_000, "nightly"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let place_home = &home;
        let placing = scope.spawn(move || {
            let result = service::arm_on(
                place_home,
                &alice(),
                &key("local"),
                ArmPlacement::ThisMac,
                &Author::user("alice"),
            );
            done_tx.send(()).unwrap();
            result.unwrap()
        });
        let completed = done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok();
        release_tx.send(()).unwrap();
        preparing.join().unwrap().unwrap();
        placing.join().unwrap();
        completed
    });
    assert!(
        completed_while_preparing,
        "an unrelated placement must not wait for remote preparation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_worker_fences_the_stale_mac_after_the_new_owner_acknowledges() {
    let temp = TempHome::new();
    let original = temp.codesense_home();
    let other = temp.other_mac("other", 1);
    save(&original, daily("nightly", "fresh"));
    save(&original, routine("sibling", "0 10 * * *"));
    let first = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    arm_on(&temp, "sibling", ArmPlacement::Machine(MACHINE.into()));
    let one = Machine::new(nine(0) - 60_000);
    let two = Machine::new(nine(0) + 1_000);
    let pair = Pair(one.clone(), two);
    refresh(&temp, &one, nine(0) - 60_000, "nightly").unwrap();
    one.tick_at(nine(0));
    let session = one.port.launched.lock().unwrap()[0].0.clone();
    let run_id = arm_run_id(&key("nightly").to_string(), first.arm_epoch, nine(0) as u64);
    let second = arm_on(&temp, "nightly", ArmPlacement::Machine("m-test-2".into()));
    with_input(
        &temp,
        &one,
        nine(0) + 1_000,
        &super::wake::NoNotify,
        |input| sync_machine(&pair, input, MACHINE),
    );
    temp.act_as(&other);
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    pull_definition(&temp, &second);
    let third = arm_on(&temp, "nightly", ArmPlacement::Machine("m-test-2".into()));
    // B's ordinary sync acknowledges the retired placement before its log.
    with_input(
        &temp,
        &one,
        nine(0) + 2_000,
        &super::wake::NoNotify,
        |input| sync_machine(&pair, input, MACHINE),
    );
    one.engine
        .complete(&session, Some(&run_id), "success", "late on M1")
        .unwrap();
    temp.act_as(temp.root());
    let stale = sync(&temp, &one, nine(0) + 3_000);
    temp.act_as(&other);
    let current = sync(&temp, &one, nine(0) + 4_000);
    assert_eq!(stale.recorded, 0, "A still has the older definition");
    assert!(current.recorded > 0);
    assert_eq!(
        store(&temp.codesense_home())
            .run(&run_id)
            .unwrap()
            .unwrap()
            .phase,
        "succeeded"
    );
    assert_eq!(
        store(&temp.codesense_home())
            .runs(&key("nightly"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(sync(&temp, &one, nine(0) + 5_000).recorded, 0);
    assert_eq!(
        one.engine
            .list()
            .disarmed
            .iter()
            .find(|t| t.key == key("nightly").to_string())
            .unwrap()
            .epoch,
        third.arm_epoch
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_refresh_response_cannot_replace_a_newer_prepared_receipt() {
    let temp = TempHome::new();
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.arm_gate.lock().unwrap() = Some((true, entered_tx, release_rx));
    std::thread::scope(|scope| {
        let old = scope.spawn(|| refresh(&temp, &machine, nine(0) - 50_000, "nightly"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        *machine.manifest.lock().unwrap() = "manifest-new".into();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let new = scope.spawn({
            let temp = &temp;
            let machine = &machine;
            move || {
                let result = refresh(temp, machine, nine(0) - 40_000, "nightly");
                done_tx.send(()).unwrap();
                result
            }
        });
        let _ = done_rx.recv_timeout(std::time::Duration::from_millis(300));
        release_tx.send(()).unwrap();
        old.join().unwrap().unwrap();
        new.join().unwrap().unwrap();
    });
    sync(&temp, &machine, nine(0) - 30_000);
    assert_eq!(
        machine.engine.list().armed[0].prepared_digest,
        "manifest-new"
    );
    assert_eq!(
        super::machine_arm::prepared(
            &store(&temp.codesense_home()),
            &key("nightly").to_string(),
            MACHINE
        )
        .unwrap()
        .unwrap()
        .manifest_hash,
        "manifest-new"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_sync_push_cannot_restore_an_older_preparation_at_the_same_revision() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    // A saved task edit needs a push, using the previous prepared recipe.
    let mut def = service::get_current(&home, &alice(), &key("nightly"))
        .unwrap()
        .def;
    def.task = "the edited task".into();
    def.revision += 1;
    pull_definition(&temp, &def);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.arm_gate.lock().unwrap() = Some((false, entered_tx, release_rx));
    std::thread::scope(|scope| {
        let old = scope.spawn(|| sync(&temp, &machine, nine(0) - 50_000));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        *machine.manifest.lock().unwrap() = "manifest-new".into();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let new = scope.spawn({
            let temp = &temp;
            let machine = &machine;
            move || {
                let result = refresh(temp, machine, nine(0) - 40_000, "nightly");
                done_tx.send(()).unwrap();
                result
            }
        });
        let _ = done_rx.recv_timeout(std::time::Duration::from_millis(300));
        release_tx.send(()).unwrap();
        assert!(old.join().unwrap().errors.is_empty());
        new.join().unwrap().unwrap();
    });
    assert_eq!(machine.engine.list().armed[0].revision, def.revision);
    assert_eq!(
        machine.engine.list().armed[0].prepared_digest,
        "manifest-new"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_discards_preparation_when_agent_content_changed_while_it_waited() {
    let temp = TempHome::new();
    save(&temp.codesense_home(), daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.prepare_gate.lock().unwrap() = Some((entered_tx, release_rx));
    std::thread::scope(|scope| {
        let old = scope.spawn(|| refresh(&temp, &machine, nine(0) - 50_000, "nightly"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        *machine.manifest.lock().unwrap() = "manifest-new".into();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let new = scope.spawn({
            let temp = &temp;
            let machine = &machine;
            move || {
                let result = refresh(temp, machine, nine(0) - 40_000, "nightly");
                done_tx.send(()).unwrap();
                result
            }
        });
        let _ = done_rx.recv_timeout(std::time::Duration::from_millis(300));
        release_tx.send(()).unwrap();
        let _ = old.join().unwrap();
        new.join().unwrap().unwrap();
    });
    sync(&temp, &machine, nine(0) - 30_000);
    assert_eq!(
        machine.engine.list().armed[0].prepared_digest,
        "manifest-new"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retargeted_refresh_keeps_the_new_agents_workspace_instructions() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let original = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *machine.prepare_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let retargeted = std::thread::scope(|scope| {
        // A has its original agent snapshot, before writing the destination.
        let old = scope.spawn(|| refresh(&temp, &machine, nine(0) - 50_000, "nightly"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut edited = original.clone();
        edited.agent = super::model::AgentRef::from("builtin/claude");
        let saved = service::save(
            &home,
            &alice(),
            service::SaveTarget::Update {
                key: key("nightly"),
                base_revision: original.revision,
            },
            edited,
            &Author::user("alice"),
        )
        .unwrap();
        *machine.manifest.lock().unwrap() = "manifest-B".into();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let new = scope.spawn({
            let temp = &temp;
            let machine = &machine;
            move || {
                let result = refresh(temp, machine, nine(0) - 40_000, "nightly");
                done_tx.send(()).unwrap();
                result
            }
        });
        // Before serialization, B finishes first and A overwrites its files.
        // With serialization B waits here, then writes last after A exits.
        let _ = done_rx.recv_timeout(std::time::Duration::from_millis(300));
        release_tx.send(()).unwrap();
        assert!(
            old.join().unwrap().is_err(),
            "the changed-input guard still rejects A"
        );
        new.join().unwrap().unwrap();
        saved.def
    });
    let routine = discover_routines(&home, &alice())
        .unwrap()
        .into_iter()
        .find(|r| r.key == key("nightly"))
        .unwrap();
    let agent = routine.agent_key.unwrap().to_string();
    let held = machine.engine.list().armed.remove(0);
    assert_eq!(held.revision, retargeted.revision);
    assert_eq!(held.prepared_digest, "manifest-B");
    let receipt = super::machine_arm::prepared(&store(&home), &key("nightly").to_string(), MACHINE)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.agent_key, agent);
    assert_eq!(receipt.manifest_hash, "manifest-B");
    assert_eq!(
        std::fs::read(machine.prepared_workspace.path().join("CLAUDE.md")).unwrap(),
        format!("Instructions for {agent}\n").as_bytes(),
        "discarding A's arm must not leave A's managed instructions behind"
    );
}

// AC1: armed on the machine with the desktop away, it fires on schedule on
// the machine's clock; the run appears on the desktop after it reconnects.
// The desktop itself never fires it.
#[tokio::test(flavor = "multi_thread")]
async fn an_armed_routine_fires_with_the_desktop_away_and_its_run_appears_after_reconnect() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let def = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    assert_eq!(def.armed_machine.as_deref(), Some(MACHINE));
    assert_eq!(def.arm_epoch, 1);
    assert_eq!(def.target.machine.as_deref(), Some(MACHINE));
    let machine = Machine::new(nine(0) - 3_600_000);
    // The arm (an explicit action) prepares it there (SW-XS-25).
    assert_eq!(
        refresh(&temp, &machine, nine(0) - 3_600_000, "nightly"),
        Ok(MACHINE.to_string())
    );
    let held = machine.engine.list();
    assert_eq!(held.armed.len(), 1);
    assert!(held.armed[0].has_launch, "prepared at arm time");
    assert_eq!(
        held.armed[0].originator.routine_host.as_deref(),
        Some(super::host::local_host_id(&home).unwrap().as_str())
    );

    // The desktop goes away. 09:00 comes on the machine.
    machine.online.store(false, Ordering::SeqCst);
    let clock = Arc::new(AtomicI64::new(nine(0) - 60_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    desk.tick();
    clock.store(nine(0) + 1_000, Ordering::SeqCst);
    let desk_pass = desk.tick().unwrap();
    assert_eq!(
        desk_pass.admitted, 0,
        "no Mac fires a machine-armed routine"
    );
    machine.tick_at(nine(0) + 2_000);
    assert_eq!(machine.launches(), 1, "the machine fired it on its own");
    let (_, prompt) = machine.port.launched.lock().unwrap()[0].clone();
    let run_id = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    assert!(prompt.starts_with(&format!(
        "[routine: Routine nightly | run {run_id}]\ntriage 2026-10-06"
    )));
    assert_eq!(
        sync(&temp, &machine, nine(0) + 3_000).recorded,
        0,
        "still away"
    );
    assert!(store(&home).run(&run_id).unwrap().is_none());

    // It comes back: the run is on the desktop, once.
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) + 60_000);
    assert!(report.recorded >= 1, "{report:?}");
    let run = store(&home).run(&run_id).unwrap().expect("recorded");
    assert_eq!(run.phase, "running");
    assert_eq!(run.source, "schedule");
    assert_eq!(run.launch_machine.as_deref(), Some(MACHINE));
    assert!(run.session_id.unwrap().starts_with("routine-"));
    assert_eq!(
        sync(&temp, &machine, nine(0) + 90_000).recorded,
        0,
        "read once"
    );
    assert_eq!(store(&home).runs(&key("nightly")).unwrap().len(), 1);
}

// AC1, wake: the machine delivers into its agent's idle session, and the
// desktop records it delivered, on that machine.
#[tokio::test(flavor = "multi_thread")]
async fn an_armed_wake_is_delivered_on_the_machine_and_recorded_on_reconnect() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("triage", "wake"));
    arm_on(&temp, "triage", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "triage").unwrap();
    machine.port.idle.lock().unwrap().push("barb-1".into());
    machine.online.store(false, Ordering::SeqCst);
    machine.tick_at(nine(0));
    assert_eq!(machine.port.delivered.lock().unwrap().len(), 1);
    assert_eq!(machine.launches(), 0, "woken, not duplicated");
    machine.online.store(true, Ordering::SeqCst);
    sync(&temp, &machine, nine(0) + 1_000);
    let run_id = arm_run_id(&key("triage").to_string(), 1, nine(0) as u64);
    let run = store(&home).run(&run_id).unwrap().expect("recorded");
    assert_eq!(run.phase, "running");
    assert_eq!(run.delivered_at, Some(42));
    assert_eq!(run.wake_machine.as_deref(), Some(MACHINE));
    assert_eq!(run.session_id.as_deref(), Some("barb-1"));
}

// r1 MUST 1: a wake routine with limits is never armed on a terminal server
// too old to enforce them (proto minor 12): it is reported with the "update
// the terminal server" note, and one already armed there is disarmed. Once
// the server is updated, the next sync arms it, limits included. A routine
// without limits is armed on the old server as before.
#[tokio::test(flavor = "multi_thread")]
async fn a_limited_wake_is_not_armed_on_a_server_before_limits_and_is_once_it_updates() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut limited = daily("heavy", "wake");
    limited.policy.min_gap = Some("4h".into());
    save(&home, limited);
    save(&home, daily("light", "wake"));
    arm_on(&temp, "heavy", ArmPlacement::Machine(MACHINE.into()));
    arm_on(&temp, "light", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    machine.before_limits.store(true, Ordering::SeqCst);
    let inbox = Inbox::default();
    let report = sync_with(&temp, &machine, nine(0) - 60_000, &inbox);
    let heavy = key("heavy").to_string();
    assert_eq!(report.outdated, vec![heavy.clone()], "{report:?}");
    // r2: it fires nowhere meanwhile, so the inbox is told, once a day.
    sync_with(&temp, &machine, nine(0) - 55_000, &inbox);
    let posts = inbox.0.lock().unwrap().clone();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, super::wake::NoticeKind::ServerOutdated);
    assert_eq!(
        posts[0].title(),
        "heavy isn't running: m-test-1's terminal server is too old to enforce its limits."
    );
    assert_eq!(
        posts[0].context(),
        "Update the terminal server, or remove the limits."
    );
    let armed: Vec<String> = machine
        .engine
        .list()
        .armed
        .into_iter()
        .map(|a| a.key)
        .collect();
    assert_eq!(armed, vec![key("light").to_string()]);
    // Updated: the next sync arms it, with its limits.
    machine.before_limits.store(false, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 50_000);
    assert_eq!(report.armed, vec![heavy.clone()], "{report:?}");
    assert!(report.outdated.is_empty());
    // A server that goes back to an old build (replaced) disarms it again.
    machine.before_limits.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 40_000);
    assert_eq!(report.disarmed, vec![heavy], "{report:?}");
}

// r1 MUST 1: the digest names whether the machine enforces the limits, so
// an arm built while it did not is pushed again once it does.
#[test]
fn the_arm_digest_names_whether_its_limits_are_enforced() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut limited = daily("heavy", "wake");
    limited.policy.max_wakes = Some(5);
    save(&home, limited);
    save(&home, daily("light", "wake"));
    let routines = discover_routines(&home, &alice()).unwrap();
    let digest = |id: &str, enforced: bool| {
        super::machine_arm::desired_arm(
            routines.iter().find(|r| r.key == key(id)).unwrap(),
            RoutineMode::Wake,
            &ArmPlace {
                workspace_id: "objects".into(),
                handle: String::new(),
                cwd: "/w".into(),
            },
            "1",
            "this-mac",
            "alice",
            &AgentFacts::default(),
            &Carried::default(),
            None,
            enforced,
        )
        .unwrap()
    };
    assert_ne!(
        digest("heavy", false).source_digest,
        digest("heavy", true).source_digest
    );
    assert_eq!(
        digest("light", false).source_digest,
        digest("light", true).source_digest
    );
    let heavy = digest("heavy", true);
    assert_eq!(
        heavy.limits.unwrap().budget,
        format!("routine:{}#this-mac:0", key("heavy"))
    );
}

/// The human inbox, recorded.
#[derive(Default)]
struct Inbox(Mutex<Vec<super::wake::WakeNotice>>);

impl super::wake::WakeNotify for Inbox {
    fn post(&self, notice: &super::wake::WakeNotice) {
        self.0.lock().unwrap().push(notice.clone());
    }
    fn redispatch(&self, _: super::admit::RoutinePointer, _: std::time::Duration) {}
}

// SW-AGR-13 AC1 + AC6 (machine-armed, desktop closed): the arm carries the
// routine's limits to the machine, whose queue enforces them with no
// desktop; the run that hit `max_wakes` pauses the routine on this Mac when
// the desktop comes back (once, one inbox note), which disarms it there; a
// resume re-arms it with a new budget.
#[tokio::test(flavor = "multi_thread")]
async fn an_armed_wake_at_its_max_wakes_pauses_the_routine_when_the_desktop_returns() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = daily("triage", "wake");
    def.policy.max_wakes = Some(3);
    def.policy.min_gap = Some("2h".into());
    save(&home, def);
    arm_on(&temp, "triage", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    sync(&temp, &machine, nine(0) - 60_000);
    let triage = key("triage").to_string();
    machine.port.idle.lock().unwrap().push("barb-1".into());
    machine.online.store(false, Ordering::SeqCst);
    *machine.port.decide.lock().unwrap() = Some(DeliveryStatus::Undelivered {
        reason: codesense_pty_core::session_wake::UndeliveredReason::MaxWakes,
    });
    machine.tick_at(nine(0));
    let sent = machine.port.delivered.lock().unwrap()[0]
        .limits
        .clone()
        .unwrap();
    let install = super::host::local_host_id(&home).unwrap();
    assert_eq!(sent.budget, format!("routine:{triage}#{install}:0"));
    assert_eq!(
        (sent.max_wakes, sent.min_gap_ms),
        (Some(3), Some(7_200_000))
    );
    machine.online.store(true, Ordering::SeqCst);
    let inbox = Inbox::default();
    sync_with(&temp, &machine, nine(0) + 1_000, &inbox);
    let run_id = arm_run_id(&triage, 1, nine(0) as u64);
    let run = store(&home).run(&run_id).unwrap().expect("recorded");
    assert_eq!(run.reason.as_deref(), Some("max_wakes"));
    assert!(!store(&home).switch_on(&triage).unwrap(), "paused");
    let posts = inbox.0.lock().unwrap().clone();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, super::wake::NoticeKind::MaxWakes);
    // Paused here: the next sync disarms it on the machine; nothing more is said.
    sync_with(&temp, &machine, nine(0) + 2_000, &inbox);
    assert!(machine.engine.list().armed.is_empty(), "disarmed");
    assert_eq!(inbox.0.lock().unwrap().len(), 1);
    // Resumed: armed again, under a new budget.
    service::set_enabled(&home, &alice(), &key("triage"), true).unwrap();
    sync(&temp, &machine, nine(0) + 3_000);
    *machine.port.decide.lock().unwrap() = None;
    machine.tick_at(nine(1));
    let budgets: Vec<String> = machine
        .port
        .delivered
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.limits.clone().unwrap().budget)
        .collect();
    assert_eq!(
        budgets.last().unwrap(),
        &format!("routine:{triage}#{install}:1")
    );
}

// AC2: a fresh fire into a workspace nothing was prepared in is
// `undelivered(needs_preparation)`, and the desktop shows it so.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_fire_into_an_unprepared_workspace_is_needs_preparation() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    machine.prepare_fails.store(true, Ordering::SeqCst);
    let why = refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap_err();
    // Armed anyway (its definition, no recipe): it fires and says why.
    assert_eq!(machine.engine.list().armed.len(), 1);
    assert!(!machine.engine.list().armed[0].has_launch);
    assert_eq!(
        stale_arms(&store(&home)).unwrap()[&key("nightly").to_string()].why,
        why
    );
    let report = sync(&temp, &machine, nine(0) - 60_000);
    assert!(report.armed.is_empty(), "{report:?}");
    assert_eq!(report.content_stale, vec![key("nightly").to_string()]);
    machine.tick_at(nine(0));
    assert_eq!(machine.launches(), 0);
    sync(&temp, &machine, nine(0) + 1_000);
    let run_id = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    let run = store(&home).run(&run_id).unwrap().expect("recorded");
    assert_eq!(run.phase, "undelivered");
    assert_eq!(run.reason.as_deref(), Some("needs_preparation"));
    // Once the desktop can prepare it, a refresh does, and the next fire
    // starts the agent.
    machine.prepare_fails.store(false, Ordering::SeqCst);
    sync(&temp, &machine, nine(0) + 2_000);
    assert!(
        !machine.engine.list().armed[0].has_launch,
        "never prepared in the background"
    );
    refresh(&temp, &machine, nine(0) + 2_000, "nightly").unwrap();
    assert!(machine.engine.list().armed[0].has_launch);
    machine.tick_at(nine(1));
    assert_eq!(machine.launches(), 1);
}

// AC3: re-armed to the desktop while the machine is connected, the machine
// stops at once and the desktop takes over from the next occurrence: one
// fire per occurrence, never two.
#[tokio::test(flavor = "multi_thread")]
async fn rearmed_to_the_desktop_the_machine_stops_and_nothing_fires_twice() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.tick_at(nine(0));
    assert_eq!(machine.launches(), 1);

    let clock = Arc::new(AtomicI64::new(nine(0) + 3_600_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    let def = arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    assert_eq!(def.armed_machine, None);
    assert_eq!(def.arm_epoch, 2, "a move is a new epoch");
    assert!(pending_handovers(&store(&home))
        .unwrap()
        .contains(&key("nightly").to_string()));
    let report = sync(&temp, &machine, nine(0) + 3_600_000);
    assert_eq!(
        report.disarmed,
        vec![key("nightly").to_string()],
        "{report:?}"
    );
    assert_eq!(report.handed_over, vec![key("nightly").to_string()]);
    assert!(handovers(&store(&home)).unwrap().is_empty());
    assert!(machine.engine.list().armed.is_empty());
    desk.tick(); // seeds the desktop's cursor from now
    clock.store(nine(1) + 1_000, Ordering::SeqCst);
    machine.tick_at(nine(1) + 1_000);
    let pass = desk.tick().unwrap();
    assert_eq!(machine.launches(), 1, "the machine fired nothing more");
    assert_eq!(pass.admitted, 1, "the desktop fired day 2, once");
}

// AC3 with the machine offline at the move (review r1: the offline-disarm
// handover). The machine is still armed, so it keeps firing; the desktop
// fires nothing until the machine acknowledges the disarm, then takes over
// from the next occurrence. Every occurrence fires exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn an_offline_disarm_hands_over_without_a_double_fire() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.online.store(false, Ordering::SeqCst);

    let clock = Arc::new(AtomicI64::new(nine(0) - 30_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    assert!(sync(&temp, &machine, nine(0) - 20_000)
        .handed_over
        .is_empty());
    desk.tick();
    // 09:00 day 1: the machine (still armed there) fires; the desktop holds.
    clock.store(nine(0) + 1_000, Ordering::SeqCst);
    machine.tick_at(nine(0) + 1_000);
    assert_eq!(desk.tick().unwrap().admitted, 0, "held for the handover");
    assert_eq!(machine.launches(), 1);
    // The machine comes back: disarmed, its run recorded, handover done.
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) + 7_200_000);
    assert_eq!(
        report.disarmed,
        vec![key("nightly").to_string()],
        "{report:?}"
    );
    assert_eq!(report.handed_over, vec![key("nightly").to_string()]);
    assert!(report.recorded >= 1);
    let day1 = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    assert!(store(&home).run(&day1).unwrap().is_some());
    // From now on the desktop fires it, and only the desktop.
    clock.store(nine(0) + 7_200_000, Ordering::SeqCst);
    desk.tick();
    clock.store(nine(1) + 1_000, Ordering::SeqCst);
    machine.tick_at(nine(1) + 1_000);
    assert_eq!(desk.tick().unwrap().admitted, 1);
    assert_eq!(machine.launches(), 1);
    // Day 1 on the machine, day 2 on the desktop: one run each.
    assert_eq!(store(&home).runs(&key("nightly")).unwrap().len(), 2);
}

// Every edit is pushed as the new revision under the same epoch; the
// machine keeps the schedule's cursor. A stale definition (an older epoch)
// is refused by the machine and reported, never applied.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_is_pushed_and_a_stale_arm_is_refused() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let def = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    let next = machine.engine.list().armed[0].next_at_ms;
    let mut edited = def.clone();
    edited.task = "triage again {{date}}".into();
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("nightly"),
            base_revision: def.revision,
        },
        edited,
        &Author::user("alice"),
    )
    .unwrap();
    // SW-XS-25 MUST 1: the edit's definition is pushed by the sync, on the
    // recipe prepared at arm time: no preparation.
    let prepares = machine.prepares.load(Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 30_000);
    assert_eq!(report.armed.len(), 1, "{report:?}");
    assert!(report.content_stale.is_empty(), "{report:?}");
    assert_eq!(machine.prepares.load(Ordering::SeqCst), prepares);
    let held = &machine.engine.list().armed[0];
    assert_eq!(held.revision, def.revision + 1);
    assert_eq!(held.epoch, 1, "an edit keeps the epoch");
    assert_eq!(held.next_at_ms, next, "and the cursor");
    assert!(held.has_launch, "still starts the agent it prepared");
    // Someone else moved it on (epoch 3 on the machine): this Mac's copy at
    // epoch 1 is stale and changes nothing there.
    let mut newer = super::machine_arm::desired_arm(
        discover_routines(&home, &alice())
            .unwrap()
            .iter()
            .find(|r| r.key == key("nightly"))
            .unwrap(),
        RoutineMode::Fresh,
        &ArmPlace {
            workspace_id: "objects".into(),
            handle: String::new(),
            cwd: "/home/dev/work".into(),
        },
        "1",
        "another-mac",
        "bob",
        &AgentFacts::default(),
        &Carried::default(),
        None,
        true,
    )
    .unwrap();
    newer.epoch = 3;
    assert!(matches!(
        machine.engine.arm(newer, "app"),
        ArmAnswer::Armed { .. }
    ));
    let mut again = def;
    again.task = "a third edit".into();
    let current = service::get_current(&home, &alice(), &key("nightly"))
        .unwrap()
        .def;
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("nightly"),
            base_revision: current.revision,
        },
        again,
        &Author::user("alice"),
    )
    .unwrap();
    let refused = refresh(&temp, &machine, nine(0) - 10_000, "nightly").unwrap_err();
    assert!(refused.contains("newer arm"), "{refused}");
    let report = sync(&temp, &machine, nine(0) - 10_000);
    assert_eq!(report.stale, vec![key("nightly").to_string()], "{report:?}");
    assert_eq!(machine.engine.list().armed[0].epoch, 3);
}

// A routine paused on its managing Mac is disarmed on the machine (it fires
// nothing), and resuming arms it again at the same epoch.
#[tokio::test(flavor = "multi_thread")]
async fn pausing_disarms_and_resuming_rearms() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    service::set_enabled(&home, &alice(), &key("nightly"), false).unwrap();
    let report = sync(&temp, &machine, nine(0) - 50_000);
    assert_eq!(report.disarmed.len(), 1, "{report:?}");
    machine.tick_at(nine(0));
    assert_eq!(machine.launches(), 0);
    // Resuming (an explicit action) prepares it again (SW-XS-25).
    service::set_enabled(&home, &alice(), &key("nightly"), true).unwrap();
    refresh(&temp, &machine, nine(0) + 1_000, "nightly").unwrap();
    assert_eq!(machine.engine.list().armed.len(), 1);
    machine.tick_at(nine(1));
    assert_eq!(machine.launches(), 1);
}

// §9.2 / §6: what a routine armed on a machine can be.
#[test]
fn a_machine_armed_routine_runs_once_on_its_schedule_with_its_agent_there() {
    let mut def = daily("nightly", "fresh");
    def.revision = 1;
    def.owner = "1".into();
    def.armed_machine = Some(MACHINE.into());
    def.arm_epoch = 1;
    assert_eq!(def.validate(), Ok(()));
    let mut each = def.clone();
    each.runs_on = "each_mac".into();
    assert!(each.validate().is_err());
    let mut events = def.clone();
    events
        .triggers
        .push(RoutineTrigger::Event(super::model::EventSpec {
            kind: "agent_done".into(),
            from_agent: None,
            role: None,
            metadata: None,
        }));
    assert!(events.validate().is_err());
    let mut elsewhere = def.clone();
    elsewhere.target.machine = Some("m-other".into());
    assert!(elsewhere.validate().is_err());
    let mut no_epoch = def.clone();
    no_epoch.arm_epoch = 0;
    assert!(no_epoch.validate().is_err());
    let mut local = def;
    local.armed_machine = Some("local".into());
    assert!(local.validate().is_err());
}

// MUST 4 (review r1): a machine missing from the roster may still be
// running and firing: its handovers are never released for its absence.
// They wait, shown as waiting, until it acknowledges or a person releases
// them because it is gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_machine_missing_from_the_roster_keeps_its_handovers_until_released() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine("m-gone".into()));
    arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    let machine = Machine::new(nine(0));
    let principal = alice();
    let routines = discover_routines(&home, &principal).unwrap();
    let host = super::host::local_host_id(&home).unwrap();
    let store = store(&home);
    let mode_of =
        |r: &DiscoveredRoutine| service::effective_mode(&home, &principal, &r.key, &r.def);
    let author_of = |_: &DiscoveredRoutine| String::new();
    let undiscovered = |_: &str| None;
    let agent_of = |_: &DiscoveredRoutine| Some(AgentFacts::default());
    let settled = |_: &str, _: &str, _: u64| panic!("never acknowledged");
    let input = SyncInput {
        store: &store,
        principal: &principal,
        host_id: &host,
        routines: &routines,
        mode_of: &mode_of,
        author_of: &author_of,
        undiscovered: &undiscovered,
        agent_of: &agent_of,
        settled: &settled,
        notify: &super::wake::NoNotify,
        now: nine(0),
    };
    let report = sync_machine(&Env(machine), &input, "m-gone");
    assert!(report.handed_over.is_empty(), "{report:?}");
    assert_eq!(report.waiting, vec![key("nightly").to_string()]);
    assert!(pending_handovers(&store)
        .unwrap()
        .contains(&key("nightly").to_string()));
    // The UI shows what it waits on.
    let view = service::get(&home, &alice(), &key("nightly"))
        .unwrap()
        .machine_arm
        .expect("a machine arm view");
    assert_eq!(view.waiting_on.as_deref(), Some("m-gone"));
    assert_eq!(
        view.waiting.as_deref(),
        Some("waiting for m-gone to confirm it stopped firing it")
    );
    // Removing it is refused while it is waited on.
    let refused =
        super::machine_arm_app::check_machine_removal(&home, &alice(), "m-gone").unwrap_err();
    assert!(refused.contains("routine_handover_release"), "{refused}");
    // A person says it is gone: released, here and in the file.
    service::release_handover(&home, &alice(), &key("nightly"), "m-gone").unwrap();
    assert!(pending_handovers(&store).unwrap().is_empty());
    let def = service::get_current(&home, &alice(), &key("nightly"))
        .unwrap()
        .def;
    assert_eq!(def.handover_from, None);
    assert!(super::machine_arm_app::check_machine_removal(&home, &alice(), "m-gone").is_ok());
}

// Coordinator scope (SW-AGR-8): the desktop is disconnected, the machine
// fires the routine and starts its agent, the agent calls routine_done (the
// machine answers it: no desktop relays its CodeSense MCP), and after the
// desktop reconnects the run shows done, with the agent's summary. Another
// desktop-relayed tool meanwhile is a typed `desktop_offline`.
#[tokio::test(flavor = "multi_thread")]
async fn routine_done_with_the_desktop_away_shows_the_run_done_after_reconnect() {
    use codesense_session_server::routine_arm::local_mcp::{respond, Offline, DESKTOP_OFFLINE};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.online.store(false, Ordering::SeqCst);
    machine.tick_at(nine(0));
    let (session, _) = machine.port.launched.lock().unwrap()[0].clone();
    let run_id = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    let call = |id: u64, tool: &str, args: serde_json::Value| -> serde_json::Value {
        let line = serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}}).to_string();
        serde_json::from_str(
            &respond(
                &line,
                &session,
                Some(&machine.engine),
                true,
                Offline::StartedByMachine,
            )
            .unwrap(),
        )
        .unwrap()
    };
    let offline = call(1, "send_message", serde_json::json!({"to": "bob"}));
    assert_eq!(offline["error"]["data"]["type"], DESKTOP_OFFLINE);
    let done = call(
        2,
        "routine_done",
        serde_json::json!({"runId": run_id, "outcome": "success", "summary": "nightly done on the machine"}),
    );
    assert!(done.get("result").is_some(), "{done}");
    assert!(
        store(&home).run(&run_id).unwrap().is_none(),
        "nothing yet: away"
    );
    machine.online.store(true, Ordering::SeqCst);
    sync(&temp, &machine, nine(0) + 600_000);
    let run = store(&home).run(&run_id).unwrap().expect("recorded");
    assert_eq!(run.phase, "succeeded", "{run:?}");
    assert_eq!(
        run.result_text.as_deref(),
        Some("nightly done on the machine")
    );
    assert!(run.ended_at.is_some());
    assert_eq!(run.launch_machine.as_deref(), Some(MACHINE));
}

// A re-read never moves a settled row back, but the agent's own verdict
// replaces an ending without one (this Mac saw the session end first).
#[test]
fn a_run_row_only_moves_forward_and_a_verdict_replaces_a_bare_ending() {
    use super::machine_arm::record_run;
    use codesense_pty_core::routine_arm::{ArmMode, ArmRun, ArmRunStatus};
    let temp = TempHome::new();
    let store = store(&temp.codesense_home());
    let entry = |seq: u64, status: ArmRunStatus| ArmRun {
        seq,
        run_id: "run-1".into(),
        key: key("nightly").to_string(),
        epoch: 1,
        revision: 1,
        mode: ArmMode::Fresh,
        source: "schedule".into(),
        scheduled_at_ms: 1,
        fired_at_ms: 1,
        status,
        originator: Default::default(),
        host_boot_id: "b".into(),
        at_ms: 2,
    };
    let launched = entry(
        2,
        ArmRunStatus::Launched {
            session: PtySessionId::new("routine-1"),
            at_ms: 2,
        },
    );
    assert!(record_run(&store, MACHINE, &launched, 10).unwrap());
    store
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET phase = 'ended' WHERE run_id = 'run-1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        !record_run(&store, MACHINE, &launched, 11).unwrap(),
        "never back to running"
    );
    let done = entry(
        3,
        ArmRunStatus::Done {
            outcome: "failed".into(),
            summary: "tests red".into(),
            at_ms: 3,
        },
    );
    assert!(record_run(&store, MACHINE, &done, 12).unwrap());
    let run = store.run("run-1").unwrap().unwrap();
    assert_eq!(run.phase, "failed");
    assert_eq!(run.result_text.as_deref(), Some("tests red"));
    assert!(!record_run(&store, MACHINE, &launched, 13).unwrap());
    assert_eq!(store.run("run-1").unwrap().unwrap().phase, "failed");
}

// ── review r1 ──────────────────────────────────────────────────────────────

// SHOULD 4: a move off a machine is written into the routine's file, so a
// Mac that never saw the move (no local handover record) holds it too; the
// managing Mac clears it once the machine acknowledges.
#[tokio::test(flavor = "multi_thread")]
async fn a_routine_moved_off_a_machine_is_held_on_every_mac_until_it_confirms() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.online.store(false, Ordering::SeqCst);
    let def = arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    assert_eq!(
        def.handover_from,
        Some(super::model::HandoverFrom {
            machine: MACHINE.into(),
            epoch: 2
        })
    );
    // Another Mac: the file alone holds it (no handover recorded there).
    super::machine_arm::release_handover(&store(&home), &key("nightly").to_string(), MACHINE)
        .unwrap();
    let clock = Arc::new(AtomicI64::new(nine(0) - 30_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    desk.tick();
    clock.store(nine(0) + 1_000, Ordering::SeqCst);
    assert_eq!(desk.tick().unwrap().admitted, 0, "held by its file");
    // A run by hand is refused too, with what to do.
    let refused = service::run_now(&home, &alice(), &key("nightly"), Default::default());
    assert!(
        matches!(&refused, Err(service::RoutineError::HandingOver { machine, .. }) if machine == MACHINE),
        "{refused:?}"
    );
    assert!(refused
        .unwrap_err()
        .to_string()
        .contains("routine_handover_release"));
    // The managing Mac's sync settles it with the machine: the file clears.
    super::machine_arm::begin_handover(&store(&home), &key("nightly").to_string(), MACHINE, 2, 0)
        .unwrap();
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) + 7_200_000);
    assert_eq!(report.handed_over, vec![key("nightly").to_string()]);
    let def = service::get_current(&home, &alice(), &key("nightly"))
        .unwrap()
        .def;
    assert_eq!(def.handover_from, None);
}

// SHOULD 5: handovers that cannot be read hold every routine that was ever
// on a machine (fails closed), never release them.
#[tokio::test(flavor = "multi_thread")]
async fn unreadable_handovers_hold_routines_that_were_on_a_machine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    sync(&temp, &machine, nine(0) - 50_000);
    assert_eq!(
        service::get_current(&home, &alice(), &key("nightly"))
            .unwrap()
            .def
            .handover_from,
        None,
        "acknowledged"
    );
    store(&home)
        .transaction(|tx| {
            tx.execute("DROP TABLE routine_arm_handover", [])?;
            Ok(())
        })
        .unwrap();
    let clock = Arc::new(AtomicI64::new(nine(0) - 30_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    desk.tick();
    clock.store(nine(0) + 1_000, Ordering::SeqCst);
    assert_eq!(desk.tick().unwrap().admitted, 0, "held: fails closed");
    // The sync fails closed too: it arms nothing it cannot check against
    // the handovers, and says why.
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let before = machine.engine.list().armed.len();
    let report = sync(&temp, &machine, nine(0) + 2_000);
    assert!(report.armed.is_empty(), "{report:?}");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("handovers unreadable")),
        "{report:?}"
    );
    assert_eq!(machine.engine.list().armed.len(), before);
}

// SHOULD 3: the last occurrence the machine fired comes back with its
// acknowledgement; back on this Mac the schedule starts after it, even with
// this Mac's clock behind the machine's.
#[tokio::test(flavor = "multi_thread")]
async fn back_from_a_machine_whose_clock_ran_ahead_nothing_fires_twice() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    // The machine's clock is ahead: it fires 09:00 while this Mac reads 08:59.
    machine.tick_at(nine(0) + 1_000);
    assert_eq!(machine.launches(), 1);
    // Its agent finished the run there: nothing holds the next fire here.
    let (session, _) = machine.port.launched.lock().unwrap()[0].clone();
    let fired = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    machine
        .engine
        .complete(&session, Some(&fired), "success", "done")
        .unwrap();
    arm_on(&temp, "nightly", ArmPlacement::ThisMac);
    let report = sync(&temp, &machine, nine(0) - 50_000);
    assert_eq!(
        store(&home).run(&fired).unwrap().unwrap().phase,
        "succeeded"
    );
    assert_eq!(report.handed_over.len(), 1, "{report:?}");
    assert_eq!(
        super::machine_arm::fired_floor(&store(&home), &key("nightly").to_string()).unwrap(),
        Some(nine(0))
    );
    let clock = Arc::new(AtomicI64::new(nine(0) - 40_000));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let desk = ticker(&home, &clock, launcher.clone());
    desk.tick(); // seeds: after 09:00 (the machine fired it), not at it
    clock.store(nine(0) + 1_000, Ordering::SeqCst);
    assert_eq!(
        desk.tick().unwrap().admitted,
        0,
        "09:00 already fired there"
    );
    clock.store(nine(1) + 1_000, Ordering::SeqCst);
    assert_eq!(desk.tick().unwrap().admitted, 1);
}

// SW-XS-25 (supersedes SHOULD 7): an arm whose source changed (its agent
// edited) is never prepared by the background sync, on the timer or on a
// reconnect: it is marked `content_stale` and the machine keeps what it was
// last prepared with. Only "Refresh on machine" prepares it, and clears it.
#[tokio::test(flavor = "multi_thread")]
async fn a_drifted_arm_is_marked_stale_and_only_a_refresh_prepares_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    assert_eq!(machine.prepares.load(Ordering::SeqCst), 1);
    let report = sync(&temp, &machine, nine(0) - 55_000);
    assert!(report.armed.is_empty(), "nothing changed: {report:?}");
    assert!(report.content_stale.is_empty(), "{report:?}");
    let digest = machine.engine.list().armed[0].source_digest.clone();
    let (prepares, pushes) = (
        machine.prepares.load(Ordering::SeqCst),
        machine.launch_pushes.load(Ordering::SeqCst),
    );
    let nightly = key("nightly").to_string();

    // Team content changes its agent. The timer's sync: stale, nothing sent.
    *machine.manifest.lock().unwrap() = "manifest-2".into();
    let report = sync(&temp, &machine, nine(0) - 50_000);
    assert_eq!(report.content_stale, vec![nightly.clone()], "{report:?}");
    assert!(report.armed.is_empty(), "{report:?}");
    // A reconnect's sync: the same.
    machine.online.store(false, Ordering::SeqCst);
    assert!(sync(&temp, &machine, nine(0) - 45_000)
        .content_stale
        .is_empty());
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 40_000);
    assert_eq!(report.content_stale, vec![nightly.clone()], "{report:?}");
    assert!(report.armed.is_empty(), "{report:?}");
    assert_eq!(
        machine.prepares.load(Ordering::SeqCst),
        prepares,
        "no preparation on drift"
    );
    assert_eq!(
        machine.launch_pushes.load(Ordering::SeqCst),
        pushes,
        "nothing applied there"
    );
    let held = &machine.engine.list().armed[0];
    assert_eq!(held.source_digest, digest, "the machine keeps what it had");
    assert!(held.has_launch, "and still fires with it");
    let stale = stale_arms(&store(&home)).unwrap();
    assert_eq!(stale[&nightly].machine, MACHINE);
    assert_eq!(stale[&nightly].why, super::machine_arm::STALE_DRIFTED);
    // The routine row shows it.
    let caller = crate::commands::routines::Caller {
        home: home.clone(),
        principal: alice(),
    };
    let rows = crate::commands::routines::list(&caller).unwrap();
    let row = rows
        .iter()
        .find(|r| r.summary.key.to_string() == nightly)
        .unwrap();
    assert_eq!(row.content_stale.as_ref().unwrap().machine, MACHINE);

    // Refresh on machine: prepared from now, pushed, and no longer stale.
    assert_eq!(
        refresh(&temp, &machine, nine(0) - 30_000, "nightly"),
        Ok(MACHINE.to_string())
    );
    assert_eq!(machine.prepares.load(Ordering::SeqCst), prepares + 1);
    assert_ne!(machine.engine.list().armed[0].source_digest, digest);
    assert!(stale_arms(&store(&home)).unwrap().is_empty());
    let rows = crate::commands::routines::list(&caller).unwrap();
    assert!(rows.iter().all(|r| r.content_stale.is_none()));
    let report = sync(&temp, &machine, nine(0) - 20_000);
    assert!(
        report.content_stale.is_empty() && report.armed.is_empty(),
        "{report:?}"
    );
}

// SW-XS-25: a refresh that cannot run (the machine away) leaves the arm
// stale with why; a routine not armed on a machine has nothing to refresh.
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_that_cannot_run_says_why_and_stays_stale() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    save(&home, daily("local", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    machine.online.store(false, Ordering::SeqCst);
    let why = refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap_err();
    assert!(why.contains("not connected"), "{why}");
    assert_eq!(machine.prepares.load(Ordering::SeqCst), 0);
    let stale = stale_arms(&store(&home)).unwrap();
    assert!(stale[&key("nightly").to_string()]
        .why
        .contains("not connected"));
    let why = refresh(&temp, &machine, nine(0) - 60_000, "local").unwrap_err();
    assert!(why.contains("not armed on a machine"), "{why}");
}

// MUST 5: the desktop renders the declared variables (from their defaults)
// when it pushes the arm; the machine renders the built-ins, with
// {{last_run_summary}} from the machine's own last successful run, seeded
// by the desktop's. What cannot render there cannot be armed there.
#[tokio::test(flavor = "multi_thread")]
async fn variables_render_at_push_and_the_last_summary_on_the_machine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = daily("nightly", "fresh");
    def.task = "triage {{area}} on {{date}}; last: {{last_run_summary}}".into();
    def.variables.insert(
        "area".into(),
        super::model::Variable {
            var_type: super::model::VariableType::Text,
            default: Some("billing".into()),
            extra: Default::default(),
        },
    );
    save(&home, def);
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    machine.tick_at(nine(0));
    let (session, prompt) = machine.port.launched.lock().unwrap()[0].clone();
    assert!(
        prompt.ends_with("triage billing on 2026-10-06; last: "),
        "{prompt}"
    );
    let run_id = arm_run_id(&key("nightly").to_string(), 1, nine(0) as u64);
    machine
        .engine
        .complete(&session, Some(&run_id), "success", "4 invoices fixed")
        .unwrap();
    machine.tick_at(nine(1));
    let (_, second) = machine.port.launched.lock().unwrap()[1].clone();
    assert!(second.ends_with("last: 4 invoices fixed"), "{second}");

    let mut schema = daily("other", "fresh");
    schema.revision = 1;
    schema.owner = "1".into();
    schema.armed_machine = Some(MACHINE.into());
    schema.arm_epoch = 1;
    assert_eq!(schema.validate(), Ok(()));
    let mut with_schema = schema.clone();
    with_schema.result_schema = Some(serde_json::json!({"type": "object"}));
    assert!(with_schema.validate().is_err());
}

// SHOULD 2: a routine_done this Mac relays for a run the machine fired is
// finished on the machine too: its open run there ends, so the next fire
// starts the agent again although its session idles on.
#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_routine_done_finishes_the_run_on_the_machine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("hourly", "*/5 * * * *");
    def.mode = "fresh".into();
    save(&home, def);
    arm_on(&temp, "hourly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "hourly").unwrap();
    machine.tick_at(nine(0));
    sync(&temp, &machine, nine(0) + 1_000);
    let (session, _) = machine.port.launched.lock().unwrap()[0].clone();
    let run_id = arm_run_id(&key("hourly").to_string(), 1, nine(0) as u64);
    let params: crate::mcp::routine_tools::RoutineDoneParams = serde_json::from_value(
        serde_json::json!({"sessionId": session, "runId": run_id, "outcome": "success", "summary": "ok"}),
    )
    .unwrap();
    let engine = machine.engine.clone();
    let relayed = crate::mcp::routine_tools::done_with(
        &store(&home),
        &session,
        params,
        nine(0) + 2_000,
        &|run, outcome, summary| {
            let entry = engine
                .complete(&session, Some(run), outcome, summary)
                .map_err(|e| e.to_string())?;
            super::machine_arm::record_run(&store(&home), MACHINE, &entry, nine(0) + 2_000)
                .map_err(|e| e.to_string())?;
            Ok(Some(entry))
        },
    )
    .unwrap();
    assert!(format!("{relayed:?}").contains("succeeded"), "{relayed:?}");
    assert_eq!(
        store(&home).run(&run_id).unwrap().unwrap().phase,
        "succeeded"
    );
    machine.tick_at(nine(0) + 5 * 60_000);
    assert_eq!(machine.launches(), 2, "not coalesced into the finished run");
}

// MUST 2: a run by hand of a machine-armed routine never fires on this Mac;
// fired on the machine, it goes through its policy (a second coalesces).
#[tokio::test(flavor = "multi_thread")]
async fn a_run_by_hand_of_a_machine_armed_routine_fires_on_the_machine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    assert!(matches!(
        service::run_now(&home, &alice(), &key("nightly"), Default::default()),
        Err(service::RoutineError::OnMachine { .. })
    ));
    let engine = machine.engine.clone();
    let fire = |k: &super::model::RoutineKey, _vars: std::collections::BTreeMap<String, String>| {
        use codesense_pty_core::routine_arm::{ArmFireAnswer, ArmRunStatus};
        let ArmFireAnswer::Fired { run } = engine.fire_now(&k.to_string(), 1, None) else {
            return Err("refused".to_string());
        };
        super::machine_arm::record_run(&store(&home), MACHINE, &run, nine(0))
            .map_err(|e| e.to_string())?;
        Ok(super::admit::Admission {
            run_id: run.run_id.clone(),
            outcome: match run.status {
                ArmRunStatus::Coalesced { into } => super::admit::Outcome::Coalesced {
                    active_run_id: into,
                },
                _ => super::admit::Outcome::Queued,
            },
        })
    };
    let params = |id: &str| {
        serde_json::from_value::<crate::mcp::routine_tools::RoutineRunNowParams>(
            serde_json::json!({ "id": key(id).to_string() }),
        )
        .unwrap()
    };
    let first =
        crate::mcp::routine_tools::run_now_with(&home, &alice(), params("nightly"), &fire).unwrap();
    assert!(format!("{first:?}").contains("queued"), "{first:?}");
    assert_eq!(machine.launches(), 1);
    let second =
        crate::mcp::routine_tools::run_now_with(&home, &alice(), params("nightly"), &fire).unwrap();
    assert!(format!("{second:?}").contains("coalesced"), "{second:?}");
    assert_eq!(machine.launches(), 1);
}

// MUST 4: a machine routines are armed on is not removed until they move.
#[test]
fn a_machine_with_armed_routines_is_not_removed() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let refused =
        super::machine_arm_app::check_machine_removal(&home, &alice(), MACHINE).unwrap_err();
    assert!(refused.contains("routine_arm_on"), "{refused}");
    assert!(super::machine_arm_app::check_machine_removal(&home, &alice(), "m-other").is_ok());
}

// SW-XS-25 review r1, MUST 1: an edit made while the machine is away (so its
// explicit refresh could not run) still reaches the machine when it
// reconnects: the sync pushes the new definition on the recipe prepared at
// arm time, with no preparation, and the machine fires the new schedule.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_while_the_machine_is_away_reaches_it_on_reconnect_without_a_prepare() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let def = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    let prepares = machine.prepares.load(Ordering::SeqCst);
    machine.online.store(false, Ordering::SeqCst);
    // Edited away: 10:00 instead of 09:00, and a new task.
    let mut edited = def.clone();
    edited.triggers = routine("nightly", "0 10 * * *").triggers;
    edited.task = "triage at ten {{date}}".into();
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("nightly"),
            base_revision: def.revision,
        },
        edited,
        &Author::user("alice"),
    )
    .unwrap();
    // Its refresh (the edit's explicit action) cannot reach the machine.
    let why = refresh(&temp, &machine, nine(0) - 50_000, "nightly").unwrap_err();
    assert!(why.contains("not connected"), "{why}");
    assert_eq!(machine.engine.list().armed[0].revision, def.revision);
    // Back: the timer's sync pushes the new definition, prepares nothing.
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 40_000);
    assert_eq!(report.armed, vec![key("nightly").to_string()], "{report:?}");
    assert_eq!(machine.prepares.load(Ordering::SeqCst), prepares);
    let held = machine.engine.list().armed[0].clone();
    assert_eq!(held.revision, def.revision + 1);
    assert!(held.has_launch, "it still starts the agent it prepared");
    // Its content is what was prepared: nothing is stale once synced.
    assert!(report.content_stale.is_empty(), "{report:?}");
    assert!(stale_arms(&store(&home)).unwrap().is_empty());
    // 09:00 no longer fires there; 10:00 does, with the new task.
    machine.tick_at(nine(0) + 1_000);
    assert_eq!(machine.launches(), 0);
    machine.tick_at(nine(0) + 3_600_000 + 1_000);
    assert_eq!(machine.launches(), 1);
    let (_, prompt) = machine.port.launched.lock().unwrap()[0].clone();
    assert!(prompt.contains("triage at ten"), "{prompt}");
}

// SW-XS-25 review r1, SHOULD 4: armed on a machine while the machine it left
// has not confirmed it stopped, the arm's refresh cannot run yet: it says so,
// and the arm is marked stale with why, so the row offers Refresh.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_waiting_on_a_handover_is_stale_with_why() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine("m-gone".into()));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    let why = refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap_err();
    assert_eq!(why, super::machine_arm::waiting_for("m-gone"));
    assert_eq!(machine.prepares.load(Ordering::SeqCst), 0);
    let stale = stale_arms(&store(&home)).unwrap();
    assert_eq!(stale[&key("nightly").to_string()].machine, MACHINE);
    assert_eq!(stale[&key("nightly").to_string()].why, why);
    // Released (the machine is gone): the release's refresh prepares it.
    service::release_handover(&home, &alice(), &key("nightly"), "m-gone").unwrap();
    refresh(&temp, &machine, nine(0) - 50_000, "nightly").unwrap();
    assert!(machine.engine.list().armed[0].has_launch);
    assert!(stale_arms(&store(&home)).unwrap().is_empty());
}

// SW-XS-25 review r1, LOW 8: archiving a routine forgets its stale mark and
// what was prepared for it; a new reason restarts `since_ms`.
#[tokio::test(flavor = "multi_thread")]
async fn archiving_forgets_the_arm_state_and_a_new_reason_restarts_since() {
    use super::machine_arm::{mark_stale, prepared};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    let nightly = key("nightly").to_string();
    let st = store(&home);
    assert!(prepared(&st, &nightly, MACHINE).unwrap().is_some());
    mark_stale(&st, &nightly, MACHINE, "one", 10).unwrap();
    mark_stale(&st, &nightly, MACHINE, "one", 20).unwrap();
    assert_eq!(
        stale_arms(&st).unwrap()[&nightly].since_ms,
        10,
        "same reason"
    );
    mark_stale(&st, &nightly, MACHINE, "two", 30).unwrap();
    assert_eq!(
        stale_arms(&st).unwrap()[&nightly].since_ms,
        30,
        "new reason"
    );
    service::archive(&home, &alice(), &key("nightly"), &Author::user("alice")).unwrap();
    assert!(stale_arms(&st).unwrap().is_empty());
    assert!(prepared(&st, &nightly, MACHINE).unwrap().is_none());
}

// SW-XS-25 review r1, SHOULD 3: an explicit action waits for its refresh at
// most so long; past it the arm is marked "still preparing" and the action
// returns, and the refresh, finishing on its own, clears the mark.
#[test]
fn a_slow_refresh_returns_on_time_and_clears_its_mark_when_it_lands() {
    use super::machine_arm_app::{refresh_within, STALE_STILL_PREPARING};
    use std::sync::mpsc;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let nightly = key("nightly").to_string();
    let (go, wait) = mpsc::channel::<()>();
    let (landed, has_landed) = mpsc::channel::<()>();
    let started = std::time::Instant::now();
    let why = refresh_within(
        std::time::Duration::from_millis(100),
        &home,
        &nightly,
        MACHINE,
        move || {
            wait.recv().unwrap();
            let _ = landed.send(());
            Ok(MACHINE.to_string())
        },
    )
    .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(why.contains(STALE_STILL_PREPARING), "{why}");
    assert_eq!(
        stale_arms(&store(&home)).unwrap()[&nightly].why,
        STALE_STILL_PREPARING
    );
    go.send(()).unwrap();
    has_landed.recv().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !stale_arms(&store(&home)).unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the mark stays");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // A quick one just answers.
    assert_eq!(
        refresh_within(
            std::time::Duration::from_secs(5),
            &home,
            &nightly,
            MACHINE,
            || Ok(MACHINE.to_string())
        ),
        Ok(MACHINE.to_string())
    );
}

// SW-XS-25 review r2, SHOULD 1: the kept recipe is the agent's it was
// prepared for. Reassigned to another agent while the machine is away, the
// sync never pushes the old agent's launch under the new one: the definition
// goes without a launch (a fresh fire is `needs_preparation`), the old recipe
// is dropped, and the arm is stale saying why. A recipe kept before the agent
// was recorded is treated the same way.
#[tokio::test(flavor = "multi_thread")]
async fn a_reassigned_agent_never_fires_with_the_old_agents_recipe() {
    use super::machine_arm::{prepared, STALE_AGENT_UNKNOWN, STALE_OTHER_AGENT};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, daily("nightly", "fresh"));
    let def = arm_on(&temp, "nightly", ArmPlacement::Machine(MACHINE.into()));
    let machine = Machine::new(nine(0) - 60_000);
    refresh(&temp, &machine, nine(0) - 60_000, "nightly").unwrap();
    let nightly = key("nightly").to_string();
    let pushes = machine.launch_pushes.load(Ordering::SeqCst);
    let old_agent = prepared(&store(&home), &nightly, MACHINE)
        .unwrap()
        .unwrap()
        .agent_key;
    assert!(!old_agent.is_empty());
    machine.online.store(false, Ordering::SeqCst);
    let mut edited = def.clone();
    edited.agent = super::model::AgentRef::from("builtin/claude");
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("nightly"),
            base_revision: def.revision,
        },
        edited,
        &Author::user("alice"),
    )
    .unwrap();
    assert!(refresh(&temp, &machine, nine(0) - 50_000, "nightly").is_err());
    machine.online.store(true, Ordering::SeqCst);
    let report = sync(&temp, &machine, nine(0) - 40_000);
    assert_eq!(report.armed, vec![nightly.clone()], "{report:?}");
    assert_eq!(
        machine.launch_pushes.load(Ordering::SeqCst),
        pushes,
        "no launch pushed with the old agent's recipe"
    );
    let held = machine.engine.list().armed[0].clone();
    assert_eq!(held.revision, def.revision + 1);
    assert!(!held.has_launch, "fresh fires need preparation");
    assert!(prepared(&store(&home), &nightly, MACHINE)
        .unwrap()
        .is_none());
    assert_eq!(
        stale_arms(&store(&home)).unwrap()[&nightly].why,
        STALE_OTHER_AGENT
    );
    machine.tick_at(nine(0) + 1_000);
    assert_eq!(machine.launches(), 0);
    // Refresh prepares the new agent there.
    refresh(&temp, &machine, nine(0) + 2_000, "nightly").unwrap();
    assert!(machine.engine.list().armed[0].has_launch);
    assert!(stale_arms(&store(&home)).unwrap().is_empty());

    // A recipe kept before its agent was recorded: never used.
    store(&home)
        .transaction(|tx| {
            tx.execute("UPDATE routine_arm_prepared SET agent_key = ''", [])?;
            Ok(())
        })
        .unwrap();
    let pushes = machine.launch_pushes.load(Ordering::SeqCst);
    let mut again = service::get_current(&home, &alice(), &key("nightly"))
        .unwrap()
        .def;
    let base = again.revision;
    again.task = "triage once more {{date}}".into();
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("nightly"),
            base_revision: base,
        },
        again,
        &Author::user("alice"),
    )
    .unwrap();
    sync(&temp, &machine, nine(0) + 3_000);
    assert_eq!(machine.launch_pushes.load(Ordering::SeqCst), pushes);
    assert!(!machine.engine.list().armed[0].has_launch);
    assert_eq!(
        stale_arms(&store(&home)).unwrap()[&nightly].why,
        STALE_AGENT_UNKNOWN
    );
}

// SW-XS-25 review r2, LOW 2: a slow refresh that lands clears only its own
// "still preparing" mark, never a later one.
#[test]
fn a_slow_refresh_leaves_a_later_mark_alone() {
    use super::machine_arm::mark_stale;
    use super::machine_arm_app::refresh_within;
    use std::sync::mpsc;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let nightly = key("nightly").to_string();
    let (go, wait) = mpsc::channel::<()>();
    let (landed, has_landed) = mpsc::channel::<()>();
    let _ = refresh_within(
        std::time::Duration::from_millis(50),
        &home,
        &nightly,
        MACHINE,
        move || {
            wait.recv().unwrap();
            let _ = landed.send(());
            Ok(MACHINE.to_string())
        },
    );
    // Meanwhile something newer marks it, for another reason.
    mark_stale(
        &store(&home),
        &nightly,
        MACHINE,
        "m-test-1 is not connected",
        1,
    )
    .unwrap();
    go.send(()).unwrap();
    has_landed.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        stale_arms(&store(&home)).unwrap()[&nightly].why,
        "m-test-1 is not connected"
    );
}
