// SPDX-License-Identifier: MIT
// SW-AGR-7: real-SSH proof of the routines non-negotiables on a Docker Linux
// machine (SPEC §2 R1–R6, §9.11), through the real claim loop, dispatch, run
// store and desktop delivery core, and the machine's real terminal server
// (`SessionWakeV1`: idle gate, bracketed paste, submit confirmation, timers).
//
// The agent is a fake `claude` on the machine: it reports its activity through
// the hooks its saved-agent launch was given (the session server's own
// `--activity-hook` helper, so idle/busy is observed exactly as a real Claude's
// is), turns bracketed paste on, and prints each prompt it is given. It also
// appends every prompt to a file on the machine, so what happened while the
// desktop was away is read from the machine, not from the desktop.
//
// R2 cells covered here (remote): wake × {absent, idle, busy} for a saved
// agent; timers × {saved, unnamed} × idle, and saved × busy, firing with the
// desktop gone, and ending with their session.
//
// The roster is the app's own `wake_app::roster_from` (its tab filter, the
// real agent probe on the machine through `pty::agent_present`, the quit
// watch), fed the machine's tabs. Limit: this test starts its tabs with a
// bare `spawn` (as SW-AGR-2b's launch proof does), not `open_tab`, so they
// carry no layout entry; each is given the `remoteAgentKey` tag its layout
// entry would carry. The layout read itself (`AppWakeEnv::roster`) needs a
// running app and is covered by the real-app suite (local) and the
// `candidates_come_from_every_machines_layout…` seam test.

#[test]
#[ignore = "owned real SSH: wake never duplicates; remote timers survive the desktop and die with their session (SW-AGR-7)"]
fn remote_routine_wake_over_real_ssh() {
    remote_hydration_fixture_with(true, false, true, true, true);
}

/// The fake agent. Records each launch in `routine-launches.jsonl` and each
/// prompt it is given in `routine-received.jsonl` (both in its cwd).
const FAKE_AGENT: &str = r#"#!/usr/bin/python3
import sys, os, json, subprocess, time, tty, re
argv = sys.argv
if '--help' in argv:
    print('--mcp-config --strict-mcp-config --settings'); sys.exit(0)
def log(name, row):
    row['pid'] = os.getpid(); row['at'] = time.time()
    with open(name, 'a') as out: out.write(json.dumps(row) + '\n')
log('routine-launches.jsonl', {'cwd': os.getcwd(), 'argv': argv})
hooks = {}
if '--settings' in argv:
    raw = argv[argv.index('--settings') + 1]
    try:
        settings = json.load(open(raw))
    except Exception:
        settings = json.loads(raw)
    hooks = settings.get('hooks', {})
SID = 'fake-%d' % os.getpid()
def hook(event):
    for group in hooks.get(event, []):
        for h in group.get('hooks', []):
            command = h.get('command', '')
            if '--activity-hook' in command:
                done = subprocess.run(command, shell=True, input=json.dumps({'hook_event_name': event, 'session_id': SID}), text=True, timeout=10, capture_output=True)
                log('routine-hooks.jsonl', {'event': event, 'code': done.returncode, 'err': done.stderr[-400:], 'env': {k: v for k, v in os.environ.items() if 'ACTIVITY' in k or 'CODESENSE' in k}})
def out(text):
    sys.stdout.write(text); sys.stdout.flush()
def prompt():
    out('\x1b[?2004h\r\n> ')
def turn(text, via):
    hook('UserPromptSubmit')
    lines = text.split('\n')
    log('routine-received.jsonl', {'header': lines[0], 'text': text, 'via': via, 'pasted': via == 'paste'})
    out('\r\n' + '\r\n'.join('| ' + line for line in lines) + '\r\n')
    busy = re.search(r'BUSY:(\d+)', text)
    if busy:
        time.sleep(int(busy.group(1)))
    out('DONE ' + lines[0] + '\r\n')
    hook('Stop')
    prompt()
hook('SessionStart')
if len(argv) >= 2 and argv[-2] == '--':
    turn(argv[-1], 'argv')
else:
    prompt()
fd = sys.stdin.fileno()
tty.setraw(fd)
buf = b''
while True:
    chunk = os.read(fd, 4096)
    if not chunk:
        break
    buf += chunk
    while True:
        start = buf.find(b'\x1b[200~')
        end = buf.find(b'\x1b[201~')
        enter = buf.find(b'\r', end + 6) if end >= 0 else -1
        if start < 0 or end < 0 or enter < 0:
            break
        text = buf[start + 6:end].decode('utf-8', 'replace')
        buf = buf[enter + 1:]
        turn(text, 'paste')
"#;

/// One machine's terminal server as the desktop reaches it: the live client
/// while connected, offline while not (an app quit, a dropped link).
struct SshHost {
    machine: String,
    client: std::sync::Mutex<Option<Arc<crate::terminal::session_server::SessionServerClient>>>,
}

impl crate::delivery::host::HostResolver for SshHost {
    fn resolve(&self, machine: &str) -> crate::delivery::host::HostLookup {
        use crate::delivery::host::HostLookup;
        if machine != self.machine {
            return HostLookup::Unknown;
        }
        match self.client.lock().unwrap().clone() {
            Some(client) => HostLookup::Ready(client),
            None => HostLookup::Offline {
                detail: "the desktop is away".into(),
            },
        }
    }
}

#[derive(Default)]
struct SshWindow {
    delivered: std::sync::Mutex<Vec<crate::delivery::events::WakeDeliveredEvent>>,
}

impl crate::delivery::events::WakeEventSink for SshWindow {
    fn delivered(&self, event: &crate::delivery::events::WakeDeliveredEvent) {
        self.delivered.lock().unwrap().push(event.clone());
    }
    fn outcome(&self, _: &crate::delivery::events::WakeOutcomeEvent) {}
    fn sync_health(&self, _: &crate::delivery::events::SyncHealth) {}
}

/// The wake environment over the real machine: launches go through the
/// SW-AGR-2b remote launch ([`SshRoutineMachine`]); the roster is every
/// session this test started there that is still running (the session
/// server judges idle and busy itself, so each is `Unknown` here); delivery
/// goes through the real delivery core to the machine's terminal server.
struct SshWakeMachine {
    launch: Arc<SshRoutineMachine>,
    host: Arc<SshHost>,
    service: Arc<crate::delivery::DeliveryService>,
    /// `(session, agent key)` of every tab started on the machine.
    started: std::sync::Mutex<Vec<(String, String)>>,
    quits: crate::routines::wake::QuitWatch,
}

impl SshWakeMachine {
    fn client(&self) -> Arc<crate::terminal::session_server::SessionServerClient> {
        self.host.client.lock().unwrap().clone().expect("connected")
    }
}

impl crate::routines::wake::WakeNotify for SshWakeMachine {
    fn post(&self, notice: &crate::routines::wake::WakeNotice) {
        self.launch.posts.lock().unwrap().push(notice.clone());
    }
    fn redispatch(&self, _: crate::routines::admit::RoutinePointer, _: std::time::Duration) {}
}

impl crate::routines::wake::WakeEnv for SshWakeMachine {
    fn restore(&self) -> crate::routines::wake::Restore {
        crate::routines::wake::Restore::Settled
    }
    fn roster(&self) -> crate::routines::wake::RosterFuture<'_> {
        use crate::routines::wake_app::{roster_from, HostTabs, RosterRules};
        use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
        let sessions = self.started.lock().unwrap().clone();
        let client = self.client();
        let ids: Vec<PtySessionId> = sessions.iter().map(|(s, _)| PtySessionId::new(s)).collect();
        let alive = client.alive_session_ids(&ids).map_err(|e| e.to_string());
        let machine = self.host.machine.clone();
        let quits = &self.quits;
        Box::pin(async move {
            let alive = alive?;
            let tabs = sessions
                .iter()
                .map(|(session, agent)| codesense_pty_core::layout::TabEntry {
                    session: PtySessionId::new(session),
                    name: session.clone(),
                    role: None,
                    placement: codesense_pty_core::layout::Placement::Tab,
                    state: if alive.contains(&PtySessionId::new(session)) {
                        codesense_pty_core::layout::EntryState::Live
                    } else {
                        codesense_pty_core::layout::EntryState::Exited {
                            exit_code: Some(0),
                            signal: None,
                        }
                    },
                    tags: [
                        ("remoteAgentKey".to_string(), agent.clone()),
                        ("remoteHandle".to_string(), format!("h-{session}")),
                    ]
                    .into_iter()
                    .collect(),
                })
                .collect();
            let hosts = [HostTabs {
                machine: machine.clone(),
                online: true,
                tabs,
            }];
            let probe = |_: &str, session: &str| {
                crate::commands::pty::agent_present(
                    client.as_ref(),
                    &PtySessionId::new(session),
                    false,
                )
            };
            let now = crate::routines::finish::now_ms();
            let quit = |m: &str, s: &str, absent: bool| quits.observe(m, s, absent, now);
            Ok(crate::routines::wake::Roster::from(roster_from(
                &hosts,
                &probe,
                &|_| None,
                &|_| None,
                &RosterRules::of(None, &quit),
            )))
        })
    }
    fn machine_known(&self, machine: &str) -> bool {
        machine == self.host.machine
    }
    fn posture(
        &self,
        _: &crate::routines::wake::Candidate,
        _: &Principal,
    ) -> crate::delivery::authority::SessionPosture {
        Default::default()
    }
    fn deliver(
        &self,
        delivery: crate::delivery::WakeDelivery,
    ) -> Result<crate::delivery::DeliveryReceipt, crate::delivery::DeliveryError> {
        self.service
            .deliver(&crate::delivery::authority::Caller::RoutineHost, delivery)
    }
    fn cancel(
        &self,
        target: &crate::delivery::authority::SessionRef,
        key: &str,
    ) -> Result<crate::delivery::run_status::WakeRunStatus, crate::delivery::DeliveryError> {
        self.service
            .cancel(&crate::delivery::authority::Caller::RoutineHost, target, key)
    }
    fn remote_tab<'a>(
        &'a self,
        launch: &'a crate::routines::wake::RemoteLaunch,
        op: crate::routines::wake::TabOp,
    ) -> crate::routines::wake::TabFuture<'a> {
        Box::pin(async move {
            let reply =
                crate::routines::wake::WakeEnv::remote_tab(self.launch.as_ref(), launch, op).await;
            if let crate::routines::wake::TabReply::Opened { session, .. } = &reply {
                let mut sessions = self.started.lock().unwrap();
                if !sessions.iter().any(|(s, _)| s == session) {
                    sessions.push((session.clone(), launch.agent.clone()));
                }
            }
            reply
        })
    }
    fn flights(&self) -> &crate::routines::wake::LaunchFlights {
        &self.launch.flights
    }
}

struct SshWakeLauncher(Arc<SshWakeMachine>);

impl crate::routines::dispatch::Launcher for SshWakeLauncher {
    fn launch(
        &self,
        _: crate::routines::dispatch::LaunchRequest,
    ) -> crate::routines::dispatch::LaunchFuture<'_> {
        panic!("a routine aimed at the machine never launches on this Mac")
    }
    fn kill_run(&self, _: &str) {}
    fn wake_env(&self) -> Option<Arc<dyn crate::routines::wake::WakeEnv>> {
        Some(self.0.clone())
    }
}

/// Wait up to `secs` for `cond`, checking every 250 ms.
fn wait_secs(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    cond()
}

/// The prompts the fake agent in `cwd` was given, in order.
fn received(cwd: &str) -> Vec<serde_json::Value> {
    container_file(&format!("{cwd}/routine-received.jsonl"))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn received_headers(cwd: &str) -> Vec<String> {
    received(cwd)
        .iter()
        .map(|row| row["header"].as_str().unwrap().to_string())
        .collect()
}

/// Running fake agents on the machine.
fn running_agents() -> usize {
    let output = Command::new("docker")
        .args([
            "exec",
            &var("CODESENSE_REMOTE_HOST_CONTAINER"),
            "sh",
            "-c",
            "pgrep -fc '[.]local/bin/claude' || true",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0)
}

/// The machine observes `session`'s agent idle at its prompt (§9.3).
fn observed_idle(
    client: &crate::terminal::session_server::SessionServerClient,
    session: &str,
) -> bool {
    use codesense_pty_core::domain::TerminalDomain;
    client.activity_snapshot().is_ok_and(|snapshots| {
        snapshots.iter().any(|s| {
            s.session == session
                && s.observed
                && s.exit.is_none()
                && s.asks.is_empty()
                && matches!(
                    s.state,
                    codesense_pty_core::activity::State::Idle
                        | codesense_pty_core::activity::State::Done
                )
        })
    })
}

/// `session`'s screen on the machine, as `read_terminal_tab` reads it.
fn screen(
    client: &crate::terminal::session_server::SessionServerClient,
    session: &str,
) -> String {
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    client
        .read_terminal_text(
            &PtySessionId::new(session),
            2_000,
            codesense_pty_core::emulator::TextMode::Normal,
        )
        .map(|text| text.text)
        .unwrap_or_default()
}

/// What the wake proof leaves for the part after the desktop comes back.
struct WakeProofState {
    barb_session: String,
    unnamed_session: String,
    unnamed_cwd: String,
    /// `(session, timer id, header)` of the timers set before the desktop left.
    away_timers: Vec<(String, String, String)>,
    left_at: std::time::SystemTime,
}

/// Part one, connected: wake × {absent, idle (five fires), busy}; an unnamed
/// session never woken; timers set on both sessions just before the desktop
/// goes away.
#[allow(clippy::too_many_arguments)]
async fn remote_routine_wake_proof(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: Arc<dyn crate::content_engine::destination::DestinationIo>,
    client: Arc<crate::terminal::session_server::SessionServerClient>,
    runtime: &crate::commands::machines::RuntimeMemberTarget,
    prepared_cwd: &str,
    prepared_handle: &str,
    content: &std::path::Path,
    host: Arc<SshHost>,
    service: Arc<crate::delivery::DeliveryService>,
) -> (WakeProofState, Arc<SshWakeMachine>) {
    use crate::agents::{service as agents, Placement};
    use crate::delivery::authority::{Caller, SessionRef};
    use crate::delivery::wakeup::{schedule, WakeupAsk};
    use crate::routines::model::RoutineDef;
    use codesense_pty_core::domain::PtySessionId;
    let home = ctx.codesense_home.clone();
    let principal = ctx.principal();
    let org_root = crate::context_repo::path_to_org_root(&home, &principal, &ctx.org_slug).unwrap();
    let team = ctx.team_name.clone().unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!(
            "slug: {}\ndisplay_name: SSH fixture\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
            ctx.org_slug
        ),
    )
    .unwrap();
    std::fs::write(org_root.join("teams.yml"), format!("teams:\n  - name: {team}\n")).unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 41\n    github_username: fixture\n",
    )
    .unwrap();
    std::fs::create_dir_all(content.join(".claude/hooks")).unwrap();
    std::fs::write(content.join(".claude/hooks/codesense-notify.sh"), "#!/bin/sh\n").unwrap();
    let mut barb = crate::agents::resolve::tests::draft("barb", codesense_pty_core::agent_cli::AgentCli::Claude);
    barb.startup = vec!["docs/current.md".into()];
    barb.mcp.servers.clear();
    let barb = agents::save(
        &home,
        &principal,
        agents::SaveTarget::Create(Placement::Team {
            org: ctx.org_slug.clone(),
            team: team.clone(),
        }),
        barb,
        &crate::routines::model::Author::user(principal.as_str()),
    )
    .unwrap()
    .key;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(FAKE_AGENT);
    docker_as_dev(&format!("mkdir -p /home/dev/.local/bin && printf '%s' '{encoded}' | base64 -d > /home/dev/.local/bin/claude && chmod 700 /home/dev/.local/bin/claude"));

    let machine_id = runtime.member.machine_id.clone();
    let launch = Arc::new(SshRoutineMachine {
        ctx: ctx.clone(),
        slug: slug.clone(),
        io,
        client: client.clone(),
        runtime: runtime.clone(),
        prepared: (prepared_handle.to_string(), prepared_cwd.to_string()),
        state: crate::commands::dashboard_launch::DashboardLaunchState::new(41),
        receipts: home.join("routine-launch-intents"),
        lose_next_create: std::sync::atomic::AtomicBool::new(false),
        calls: Default::default(),
        spawns: Default::default(),
        step_two: Default::default(),
        posts: Default::default(),
        flights: Default::default(),
    });
    let machine = Arc::new(SshWakeMachine {
        launch: launch.clone(),
        host: host.clone(),
        service: service.clone(),
        started: Default::default(),
        quits: Default::default(),
    });
    crate::routines::wake::install(&home, machine.clone());
    let launcher: Arc<dyn crate::routines::dispatch::Launcher> =
        Arc::new(SshWakeLauncher(machine.clone()));
    let routine = |id: &str, task: &str| -> RoutineDef {
        let mut target = crate::routines::model::Where::team_workspace();
        target.kind = format!("repo:{slug}");
        target.machine = Some(machine_id.clone());
        RoutineDef {
            action: Default::default(),
            command: Vec::new(),
            after: None,
            id: id.into(),
            name: format!("Routine {id}"),
            owner: principal.as_str().to_string(),
            agent: crate::routines::model::AgentRef::from(barb.to_string().as_str()),
            mode: "wake".into(),
            task: task.into(),
            target,
            triggers: Vec::new(),
            policy: crate::routines::model::Policy {
                max_runtime: Some("30m".into()),
                visible: true,
                ..Default::default()
            },
            variables: Default::default(),
            revision: 0,
            armed_host: String::new(),
            result_schema: None,
            runs_on: String::new(),
            archived_at: None,
            armed_machine: None,
            arm_epoch: 0,
            handover_from: None,
            archived_with_agent: false,
            migrated: None,
            extra: Default::default(),
        }
    };
    let key_of = |id: &str| crate::routines::model::RoutineKey::new(&ctx.org_slug, &team, id);
    for def in [
        routine("triage", "TRIAGE for {{date}}"),
        routine("long-task", "LONG BUSY:12 for {{date}}"),
        routine("follow-up", "FOLLOW_UP for {{date}}"),
    ] {
        crate::routines::service::save(
            &home,
            &principal,
            crate::routines::service::SaveTarget::Create(crate::routines::service::Placement::Team {
                org: ctx.org_slug.clone(),
                team: team.clone(),
            }),
            def,
            &crate::routines::model::Author::user(principal.as_str()),
        )
        .unwrap();
    }
    let store = crate::routines::state::RoutineStateStore::open(&home).unwrap();
    let probe = Command::new("docker")
        .args([
            "exec",
            &var("CODESENSE_REMOTE_HOST_CONTAINER"),
            "sh",
            "-c",
            "ls -ld /home/dev/.codesense/remote-private/v1 /home/dev/.codesense/remote-private/v1/*.sock 2>&1; grep -rh 'activity' /home/dev/.codesense/remote-private/v1/logs 2>/dev/null | tail -5",
        ])
        .output()
        .unwrap();
    println!(
        "ACTIVITY_PROBE supports_activity={} supports_wake={}\n{}",
        client.supports_activity(),
        client.supports_session_wake(),
        String::from_utf8_lossy(&probe.stdout)
    );
    // Found by this proof: a private-account machine's activity service refused
    // its `/proc/self/fd` root, so no agent there was ever observed.
    assert!(client.supports_activity(), "the machine advertises ActivityV2: its agents are observed, never upgrade-required");
    let fire = |id: &str| {
        crate::routines::service::run_now(&home, &principal, &key_of(id), Default::default())
            .unwrap()
    };
    // Settle a wake run: catch up on the machine's outcomes until it leaves
    // `queued` (or `secs` pass).
    let settle = |run_id: &str, secs: u64| -> crate::routines::runs::RoutineRun {
        wait_secs(secs, || {
            let _ = service.sync(&machine_id);
            store.run(run_id).unwrap().unwrap().phase != "queued"
        });
        store.run(run_id).unwrap().unwrap()
    };
    let header = |id: &str, run_id: &str| format!("[routine: Routine {id} | run {run_id}]");

    // ── wake × absent: no Barb anywhere → exactly one starts, the task first.
    let first = fire("triage");
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    let run = store.run(&first.run_id).unwrap().unwrap();
    assert_eq!(run.phase, "delivered", "absent: started with its task: {run:?}");
    let barb_session = run.session_id.clone().unwrap();
    assert!(wait_secs(30, || received_headers(prepared_cwd).len() == 1));
    assert_eq!(received_headers(prepared_cwd), [header("triage", &first.run_id)]);
    assert_eq!(launch.spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Barb settles at her prompt (the Stop hook): observed idle.
    assert!(wait_secs(30, || screen(&client, &barb_session).contains(&format!("DONE {}", header("triage", &first.run_id)))));

    // ── wake × idle, five fires: every one goes into that one Barb, once.
    let mut idle_runs = Vec::new();
    for _ in 0..5 {
        // §9.11: the machine observes Barb idle at her prompt before the wake.
        assert!(
            wait_secs(30, || observed_idle(&client, &barb_session)),
            "Barb observed && idle before the wake"
        );
        let admitted = fire("triage");
        crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
        let run = settle(&admitted.run_id, 60);
        assert_eq!(run.phase, "delivered", "idle: typed into Barb: {run:?}");
        assert_eq!(run.session_id.as_deref(), Some(barb_session.as_str()));
        assert!(run.delivered_at.is_some());
        // Barb finishes the turn before the next fire.
        let done = format!("DONE {}", header("triage", &admitted.run_id));
        assert!(wait_secs(30, || screen(&client, &barb_session).contains(&done)), "{done}");
        idle_runs.push(admitted.run_id);
    }
    assert_eq!(
        launch.spawns.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "five fires against a live Barb start nothing"
    );
    assert_eq!(running_agents(), 1, "one Barb process on the machine");
    let launches = recorded_launches(prepared_cwd);
    assert_eq!(launches.len(), 1, "one Barb launch: {launches:?}");
    let text = screen(&client, &barb_session);
    for run_id in &idle_runs {
        let done = format!("DONE {}", header("triage", run_id));
        assert_eq!(text.matches(&done).count(), 1, "exactly once on screen: {done}\n{text}");
    }

    // ── wake × busy: Barb is mid-turn → queued, typed once she is idle.
    let long = fire("long-task");
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(settle(&long.run_id, 60).phase, "delivered");
    // She is working on it (12 s): the next wake waits.
    let follow = fire("follow-up");
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    let queued = store.run(&follow.run_id).unwrap().unwrap();
    assert_eq!(queued.phase, "queued", "busy: queued, not typed: {queued:?}");
    assert_eq!(queued.session_id.as_deref(), Some(barb_session.as_str()));
    assert!(
        !received_headers(prepared_cwd).contains(&header("follow-up", &follow.run_id)),
        "never typed while busy"
    );
    let run = settle(&follow.run_id, 90);
    assert_eq!(run.phase, "delivered", "delivered once idle: {run:?}");
    let headers = received_headers(prepared_cwd);
    let long_at = headers.iter().position(|h| *h == header("long-task", &long.run_id)).unwrap();
    let follow_at = headers.iter().position(|h| *h == header("follow-up", &follow.run_id)).unwrap();
    assert!(long_at < follow_at);
    let busy_rows = received(prepared_cwd);
    let long_done_by = busy_rows[long_at]["at"].as_f64().unwrap() + 12.0;
    assert!(
        busy_rows[follow_at]["at"].as_f64().unwrap() >= long_done_by - 0.5,
        "typed only after Barb finished her turn"
    );
    assert!(wait_secs(30, || screen(&client, &barb_session).contains(&format!("DONE {}", header("follow-up", &follow.run_id)))));
    assert_eq!(launch.spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(running_agents(), 1);

    // ── timer × saved × busy: a timer due while Barb is mid-turn waits for her.
    assert!(wait_secs(30, || observed_idle(&client, &barb_session)));
    let long_again = fire("long-task");
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(settle(&long_again.run_id, 60).phase, "delivered");
    {
        use crate::delivery::authority::{Caller, SessionRef};
        let own = SessionRef::new(&machine_id, barb_session.as_str());
        crate::delivery::wakeup::schedule(
            &service,
            &Caller::Session(own.clone()),
            &own,
            &crate::delivery::wakeup::WakeupAsk {
                prompt: "TIMER_PING busy".into(),
                in_: Some("3s".into()),
                name: Some("busy check".into()),
                ..Default::default()
            },
            crate::routines::finish::now_ms() as u64,
            "Barb",
        )
        .unwrap();
    }
    let busy_header = "[timer: busy check | fire 1]".to_string();
    assert!(
        wait_secs(60, || received_headers(prepared_cwd).contains(&busy_header)),
        "the busy timer is delivered once Barb is idle"
    );
    let rows = received(prepared_cwd);
    let long_row = rows
        .iter()
        .find(|r| r["header"] == header("long-task", &long_again.run_id))
        .unwrap();
    let timer_rows: Vec<_> = rows.iter().filter(|r| r["header"] == busy_header).collect();
    assert_eq!(timer_rows.len(), 1, "one delivery: {timer_rows:?}");
    assert!(
        timer_rows[0]["at"].as_f64().unwrap() >= long_row["at"].as_f64().unwrap() + 12.0 - 0.5,
        "the busy timer waited for Barb's turn to end"
    );

    // Every wake and timer went in as one bracketed paste (§9.4); only the
    // launch's own first prompt came on the command line.
    for row in received(prepared_cwd) {
        let first = row["header"] == header("triage", &first.run_id);
        assert_eq!(
            row["pasted"].as_bool(),
            Some(!first),
            "{row}: a wake or timer is pasted, the launch prompt is not"
        );
    }

    // ── an unnamed Claude session on the machine: never a wake target.
    let unnamed_cwd = format!("{prepared_cwd}/unnamed-tab");
    docker_as_dev(&format!("mkdir -p '{unnamed_cwd}'"));
    let unnamed_session = format!("unnamed-{}", uuid::Uuid::new_v4().simple());
    {
        use crate::terminal::session_server::NativeMcpSelection;
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::testing::registration;
        let session = PtySessionId::new(&unnamed_session);
        client
            .prepare_native_remote(
                &session,
                format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
                NativeMcpSelection {
                    server_ids: vec![],
                    remote_entries: vec![],
                    agent_startup: None,
                },
                NativeLaunch::Cli(AgentCli::Claude),
                Arc::new(|_, _| {}),
            )
            .unwrap();
        client
            .preflight_native_remote(&session, &unnamed_cwd, NativeLaunch::Cli(AgentCli::Claude))
            .unwrap();
        client
            .spawn(
                SpawnIntent {
                    remote_launch: Some(NativeLaunch::Cli(AgentCli::Claude)),
                    command: None,
                    argv: None,
                    shell: None,
                    cwd: unnamed_cwd.clone(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration(&unnamed_session, None, false),
            )
            .unwrap();
    }
    machine
        .started
        .lock()
        .unwrap()
        .push((unnamed_session.clone(), "builtin/claude".into()));
    assert!(wait_secs(30, || recorded_launches(&unnamed_cwd).len() == 1));
    let again = fire("triage");
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    let run = settle(&again.run_id, 60);
    assert_eq!(run.session_id.as_deref(), Some(barb_session.as_str()), "{run:?}");
    assert!(received_headers(&unnamed_cwd).is_empty(), "an unnamed tab is never woken");

    // A timer on a session that ends: it dies with it, never fires.
    let doomed_session = format!("doomed-{}", uuid::Uuid::new_v4().simple());
    let doomed_cwd = format!("{prepared_cwd}/doomed-tab");
    docker_as_dev(&format!("mkdir -p '{doomed_cwd}'"));
    {
        use crate::terminal::session_server::NativeMcpSelection;
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        use codesense_pty_core::domain::TerminalDomain;
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::testing::registration;
        let session = PtySessionId::new(&doomed_session);
        client
            .prepare_native_remote(
                &session,
                format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
                NativeMcpSelection {
                    server_ids: vec![],
                    remote_entries: vec![],
                    agent_startup: None,
                },
                NativeLaunch::Cli(AgentCli::Claude),
                Arc::new(|_, _| {}),
            )
            .unwrap();
        client
            .spawn(
                SpawnIntent {
                    remote_launch: Some(NativeLaunch::Cli(AgentCli::Claude)),
                    command: None,
                    argv: None,
                    shell: None,
                    cwd: doomed_cwd.clone(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration(&doomed_session, None, false),
            )
            .unwrap();
        assert!(wait_secs(30, || recorded_launches(&doomed_cwd).len() == 1));
        assert!(wait_secs(10, || screen(&client, &doomed_session).contains("> ")));
        let own = SessionRef::new(&machine_id, doomed_session.as_str());
        schedule(
            &service,
            &Caller::Session(own.clone()),
            &own,
            &WakeupAsk {
                prompt: "TIMER_PING doomed".into(),
                in_: Some("15s".into()),
                every: Some("1m".into()),
                name: Some("doomed check".into()),
                ..WakeupAsk::default()
            },
            crate::routines::finish::now_ms() as u64,
            "Barb",
        )
        .unwrap();
        assert_eq!(client.wake_timer_list(Some(&session)).unwrap().len(), 1);
        // The tab is closed: its timers end with it, on the machine.
        client.kill(&session).unwrap();
        assert!(wait_secs(15, || client.wake_timer_list(Some(&session)).unwrap().is_empty()), "the timer died with its session");
    }
    // ── timers × {saved, unnamed}: set through the MCP core (owner = the
    // calling session), just before the desktop goes away.
    let mut away_timers = Vec::new();
    for (session, name) in [(&barb_session, "barb check"), (&unnamed_session, "unnamed check")] {
        let own = SessionRef::new(&machine_id, session.as_str());
        let set = schedule(
            &service,
            &Caller::Session(own.clone()),
            &own,
            &WakeupAsk {
                prompt: format!("TIMER_PING {name}"),
                in_: Some("20s".into()),
                name: Some(name.into()),
                ..WakeupAsk::default()
            },
            crate::routines::finish::now_ms() as u64,
            "Barb",
        )
        .unwrap();
        away_timers.push((session.clone(), set.timer_id, format!("[timer: {name} | fire 1]")));
    }
    let state = WakeProofState {
        barb_session,
        unnamed_session,
        unnamed_cwd,
        away_timers,
        left_at: std::time::SystemTime::now(),
    };
    let _ = doomed_cwd;
    (state, machine)
}

/// Part two, after the desktop was away past the timers' time and came back:
/// both timers fired on the machine while it was gone, each typed once into
/// its own session, and the outcomes are caught up on reconnect.
async fn remote_routine_wake_after_reconnect(
    client: Arc<crate::terminal::session_server::SessionServerClient>,
    prepared_cwd: &str,
    machine_id: &str,
    state: &WakeProofState,
    service: &crate::delivery::DeliveryService,
    window: &SshWindow,
    machine: &SshWakeMachine,
) {
    use codesense_pty_core::domain::PtySessionId;
    let back = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    for (session, timer_id, header) in &state.away_timers {
        let cwd = if *session == state.unnamed_session {
            state.unnamed_cwd.clone()
        } else {
            prepared_cwd.to_string()
        };
        let rows: Vec<serde_json::Value> = received(&cwd)
            .into_iter()
            .filter(|row| row["header"].as_str() == Some(header.as_str()))
            .collect();
        assert_eq!(rows.len(), 1, "{header}: fired once, on the machine: {rows:?}");
        assert_eq!(rows[0]["pasted"].as_bool(), Some(true), "{header}: pasted");
        let left = state
            .left_at
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let at = rows[0]["at"].as_f64().unwrap();
        assert!(
            left < at && at < back,
            "{header}: fired while the desktop was away ({left} < {at} < {back})"
        );
        let text = screen(&client, session);
        assert_eq!(
            text.matches(&format!("DONE {header}")).count(),
            1,
            "{header}: exactly once on screen\n{text}"
        );
        // One-shot: done, so no longer listed.
        assert!(client
            .wake_timer_list(Some(&PtySessionId::new(session)))
            .unwrap()
            .iter()
            .all(|t| t.timer_id != *timer_id));
    }
    // The desktop catches up: the deliveries are in each tab's history.
    service.sync(machine_id).unwrap();
    for (session, _, _) in &state.away_timers {
        let history = service.history(machine_id, session);
        assert!(
            history.iter().any(|entry| matches!(
                entry.status,
                crate::delivery::run_status::WakeRunStatus::Delivered { .. }
            )),
            "{session}: {history:?}"
        );
    }
    let delivered = window.delivered.lock().unwrap().len();
    assert!(delivered >= 2, "the window was told: {delivered}");
    assert_eq!(running_agents(), 2, "Barb and the unnamed tab, nothing more");
    assert!(machine.launch.posts.lock().unwrap().is_empty());
    let _ = state.barb_session.as_str();
    println!(
        "REMOTE_ROUTINE_WAKE_PASS machine={machine_id} absent=1_launch idle=5_fires_1_session busy=queued_then_delivered unnamed=never_woken timers_saved_unnamed=fired_while_desktop_away timer_died_with_session=true"
    );
}
