// SPDX-License-Identifier: MIT
// SW-AGR-2b: real-SSH proof that a routine starts its agent on a machine.
//
// Over the owned Docker machine: a `wake` routine whose agent is absent (AC1)
// and a `fresh` routine (AC4) each start exactly one agent there, through the
// real claim loop, dispatch and run store. Each launch prepares the routine's
// workspace with the SW-XS-7 engine (one manifest, one diff, one batch), runs
// in the workspace the machine prepared (its own reported path, never one on
// this Mac), is keyed by `uuid_v5(run_id)`, and starts the agent with the
// routine's task as its first prompt. The wake's Create answer is dropped
// after the spawn: the Attach on the same request id finds the tab through the
// durable launch intent, and the agent is spawned exactly once.
//
// SW-AGR-12: the workspace is closed in the UI (no viewer state at all): each
// launch chooses it from the durable preparation receipt, as the app's
// `durable_place` does, and carries the durable mark through the receipt gate
// (cwd, workspace, team and server exactly as the receipt records them).
//
// The machine side is real (SSH, the private terminal server, Step 2 batch,
// native agent startup, the spawn); `remote_shell_tab_bound` itself needs a
// running app, so the test's `remote_tab` drives the same parts it does, in
// the same order (as `remote_saved_agent_startup_proof` does).

#[test]
#[ignore = "owned real SSH: routine launch-if-absent and fresh on a machine (SW-AGR-2b)"]
fn remote_routine_launch_over_real_ssh() {
    remote_hydration_fixture(true, false, true, true);
}

struct SshRoutineMachine {
    ctx: crate::content_engine::ResolverContext,
    slug: crate::domain::SourceRepoSlug,
    io: Arc<dyn crate::content_engine::destination::DestinationIo>,
    client: Arc<crate::terminal::session_server::SessionServerClient>,
    runtime: crate::commands::machines::RuntimeMemberTarget,
    /// The handle and worktree the machine reported when it prepared the
    /// workspace (its receipt must lead back to them).
    prepared: (String, String),
    state: crate::commands::dashboard_launch::DashboardLaunchState,
    receipts: std::path::PathBuf,
    /// Drop the next Create's answer after its spawn.
    lose_next_create: std::sync::atomic::AtomicBool,
    calls: std::sync::Mutex<Vec<(crate::routines::wake::TabOp, uuid::Uuid)>>,
    spawns: std::sync::atomic::AtomicUsize,
    step_two: std::sync::Mutex<Vec<crate::agents::prepare::PrepareReport>>,
    posts: std::sync::Mutex<Vec<crate::routines::wake::WakeNotice>>,
    flights: crate::routines::wake::LaunchFlights,
}

impl SshRoutineMachine {
    fn team_key(&self) -> String {
        format!(
            "{}:{}",
            self.ctx.org_slug,
            self.ctx.team_name.as_deref().unwrap()
        )
    }

    async fn create(
        &self,
        launch: &crate::routines::wake::RemoteLaunch,
    ) -> crate::routines::wake::TabReply {
        use crate::agents::remote_launch;
        use crate::routines::wake::TabReply;
        use crate::terminal::session_server::NativeMcpSelection;
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        use codesense_pty_core::domain::PtySessionId;
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::testing::registration;
        // SW-AGR-12: from the receipts on disk; nothing open in any viewer.
        let places = crate::mcp::remote_workspace_target::prepared_places(
            &self.ctx.codesense_home,
            &self.ctx.principal(),
            41,
            &launch.machine,
        )
        .unwrap();
        let server = self.client.namespace().server_id.clone();
        // The app's choice (`durable_place` past its app lookups): each
        // candidate in order, confirmed by the real machine as the receipt
        // gate confirms it. A stale receipt (its worktree gone) sorts first.
        let asked = std::sync::Mutex::new(Vec::new());
        let place = crate::routines::wake_app::choose_place(
            &places,
            &self.team_key(),
            &launch.machine,
            Some(&server),
            &|_| true,
            &launch.target,
            |candidate| {
                asked.lock().unwrap().push(candidate.workspace_id.clone());
                let request = places
                    .iter()
                    .find(|p| p.handle == candidate.handle)
                    .unwrap()
                    .request
                    .clone();
                let confirmed = crate::mcp::remote_workspace_target::confirm_prepared(
                    &self.client,
                    request,
                    &candidate.handle,
                    &candidate.cwd,
                )
                .map(|_| ());
                async move { confirmed }
            },
        )
        .await
        .expect("the routine's workspace on the machine, from its receipt");
        assert_eq!(
            asked.lock().unwrap().first().map(String::as_str),
            Some("aaa-gone"),
            "the stale receipt was asked first, and passed over"
        );
        assert_eq!(
            (place.handle.clone(), place.cwd.clone()),
            self.prepared,
            "the receipt leads to the workspace the machine prepared"
        );
        let key = crate::agents::AgentKey::parse(&launch.agent).unwrap();
        let mut agent = remote_launch::prepare(
            &self.ctx.codesense_home,
            &self.ctx.principal(),
            &key,
            launch.org.clone(),
            launch.team.clone(),
            place.handle.clone(),
        )
        .unwrap();
        remote_launch::start_with_prompt(&mut agent, Some(launch.prompt.clone())).unwrap();
        let session = PtySessionId::new(format!("routine-{}", launch.request_id.simple()));
        let mut record = crate::commands::pty_layout::RemoteShellRecord {
        workspace_story_id: None,
            agent: Some(agent.clone()),
            phase: serde_json::from_value(serde_json::json!("preparing")).unwrap(),
            handle: format!("remote-tab-{}", launch.request_id),
            owner_id: 41,
            request_id: launch.request_id,
            machine_id: launch.machine.clone(),
            team_key: self.team_key(),
            workspace_id: place.workspace_id.clone(),
            workspace: crate::commands::pty_layout::workspace_key(
                &self.team_key(),
                &place.workspace_id,
            ),
            cwd: place.cwd.clone(),
            name: None,
            mcp_profile_revision: None,
            mcp_inventory: vec![],
            hooks: false,
            launch: NativeLaunch::Cli(AgentCli::Claude),
            permission_mode: Default::default(),
            target: RuntimeSessionTarget {
                runtime: self.runtime.clone(),
                session: session.clone(),
            },
            status: "pending",
            identity: crate::commands::pty_layout::RemoteLaunchIdentity {
                launched_by: Some(format!("routine {}", launch.routine_name)),
                if_absent: true,
                durable_workspace: true,
                ..Default::default()
            },
        };
        // The launched agent's reverse-MCP authority rests on this group
        // check: it holds with nothing open in the viewer.
        crate::commands::pty_layout::remote_shell_group_in(&serde_json::json!({}), &record)
            .expect("a durable launch's authority holds with its workspace closed");
        // The receipt gate, exactly as the launch checks it.
        let receipt = places
            .iter()
            .find(|p| p.handle == place.handle)
            .unwrap();
        assert_eq!(
            (&receipt.cwd, &receipt.workspace_id, &receipt.team_key, &receipt.server_id),
            (&record.cwd, &record.workspace_id, &record.team_key, &record.target.runtime.namespace.server_id),
        );
        // The launch intent, durable under its request id before anything runs.
        self.state
            .reserve_remote_shell_durable(&self.receipts, record.clone())
            .unwrap();
        self.client
            .prepare_native_remote(
                &session,
                format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                ),
                NativeMcpSelection {
                    server_ids: vec![],
                    remote_entries: vec![],
                    agent_startup: Some(agent.startup.clone()),
                },
                NativeLaunch::Cli(AgentCli::Claude),
                Arc::new(|_, _| {}),
            )
            .unwrap();
        let cwd = place.cwd.clone();
        remote_launch::drive_prepared(
            record.preparing(),
            async {
                // SW-XS-7 Step 2: one manifest, one diff, at most one batch.
                let report = remote_launch::prepare_workspace(
                    &self.ctx,
                    &self.slug,
                    self.io.as_ref(),
                    NativeLaunch::Cli(AgentCli::Claude),
                    Some((&agent.manifest, "routine")),
                    &agent.workspace_files,
                    &|_| {},
                )?;
                self.step_two.lock().unwrap().push(report);
                self.client
                    .preflight_native_remote(&session, &cwd, NativeLaunch::Cli(AgentCli::Claude))
                    .map_err(|e| e.to_string())
            },
            |()| async {
                assert!(self
                    .state
                    .begin_remote_spawn(&self.receipts, &mut record)
                    .unwrap());
                self.client
                    .spawn(
                        SpawnIntent {
                            remote_launch: Some(NativeLaunch::Cli(AgentCli::Claude)),
                            command: None,
                            argv: None,
                            shell: None,
                            cwd: cwd.clone(),
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
                        registration(session.as_str(), None, false),
                    )
                    .map_err(|e| e.message)?;
                self.spawns
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();
        if self
            .lose_next_create
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return TabReply::Unknown("the machine's answer was lost".into());
        }
        TabReply::Opened {
            session: session.as_str().to_string(),
            handle: record.handle,
        }
    }

    async fn attach(
        &self,
        launch: &crate::routines::wake::RemoteLaunch,
    ) -> crate::routines::wake::TabReply {
        use crate::routines::wake::TabReply;
        let Some(mut record) = self
            .state
            .restore_remote_shell(
                &self.receipts,
                41,
                &self.team_key(),
                &launch.machine,
                &crate::commands::pty_layout::RemoteTabReference::RequestId(launch.request_id),
            )
            .unwrap()
        else {
            return TabReply::NotStarted {
                reason: codesense_pty_core::session_wake::UndeliveredReason::Unavailable,
                detail: "no launch under that request".into(),
            };
        };
        self.state
            .refresh_remote_spawn_phase(&self.receipts, &mut record)
            .unwrap();
        // An attempted spawn is never replayed: Attach finds, never starts.
        let replay = crate::agents::remote_launch::drive_prepared(
            record.preparing(),
            async { Err::<(), String>("an Attach never prepares".into()) },
            |()| async { Err("an Attach never spawns".into()) },
        )
        .await;
        assert!(replay.unwrap_err().contains("already attempted"));
        use codesense_pty_core::domain::TerminalDomain;
        let alive = self.client.read_terminal_text(
            &record.target.session,
            1,
            codesense_pty_core::emulator::TextMode::Normal,
        );
        assert!(alive.is_ok(), "the tab the Create made is on the machine");
        TabReply::Opened {
            session: record.target.session.as_str().to_string(),
            handle: record.handle,
        }
    }
}

/// The run lifecycle's view of sessions, as the app's: a session on the
/// machine is asked of the machine's real terminal server; this Mac knows
/// none of them (its own answer would be "gone").
impl crate::routines::finish::Sessions for SshRoutineMachine {
    fn kill_run(&self, _: &str, _: Option<&str>) {}
    fn liveness(&self, _: &str) -> crate::commands::pty_backend::Liveness<bool> {
        crate::commands::pty_backend::Liveness::Known(false)
    }
    fn liveness_on(
        &self,
        machine: Option<&str>,
        session_id: &str,
    ) -> crate::commands::pty_backend::Liveness<bool> {
        use codesense_pty_core::domain::TerminalDomain;
        match machine {
            None => self.liveness(session_id),
            Some(machine) => {
                assert_eq!(machine, self.runtime.member.machine_id);
                crate::routines::finish::remote_liveness(
                    self.client
                        .alive_session_ids(&[codesense_pty_core::domain::PtySessionId::new(
                            session_id,
                        )])
                        .map_err(|e| e.to_string()),
                    session_id,
                )
            }
        }
    }
}

impl crate::routines::wake::WakeNotify for SshRoutineMachine {
    fn post(&self, notice: &crate::routines::wake::WakeNotice) {
        self.posts.lock().unwrap().push(notice.clone());
    }
    fn redispatch(&self, _: crate::routines::admit::RoutinePointer, _: std::time::Duration) {}
}

impl crate::routines::wake::WakeEnv for SshRoutineMachine {
    fn restore(&self) -> crate::routines::wake::Restore {
        crate::routines::wake::Restore::Settled
    }
    fn roster(&self) -> crate::routines::wake::RosterFuture<'_> {
        // Barb runs nowhere yet.
        Box::pin(async { Ok(crate::routines::wake::Roster::default()) })
    }
    fn machine_known(&self, machine: &str) -> bool {
        machine == self.runtime.member.machine_id
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
        _: crate::delivery::WakeDelivery,
    ) -> Result<crate::delivery::DeliveryReceipt, crate::delivery::DeliveryError> {
        panic!("nothing runs yet: nothing is delivered")
    }
    fn cancel(
        &self,
        _: &crate::delivery::authority::SessionRef,
        _: &str,
    ) -> Result<crate::delivery::run_status::WakeRunStatus, crate::delivery::DeliveryError> {
        panic!("nothing is cancelled")
    }
    fn remote_tab<'a>(
        &'a self,
        launch: &'a crate::routines::wake::RemoteLaunch,
        op: crate::routines::wake::TabOp,
    ) -> crate::routines::wake::TabFuture<'a> {
        self.calls.lock().unwrap().push((op, launch.request_id));
        Box::pin(async move {
            match op {
                crate::routines::wake::TabOp::Create => self.create(launch).await,
                crate::routines::wake::TabOp::Attach => self.attach(launch).await,
            }
        })
    }
    fn flights(&self) -> &crate::routines::wake::LaunchFlights {
        &self.flights
    }
}

struct SshRoutineLauncher(Arc<SshRoutineMachine>);

impl crate::routines::dispatch::Launcher for SshRoutineLauncher {
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

fn container_file(path: &str) -> String {
    let output = Command::new("docker")
        .args(["exec", &var("CODESENSE_REMOTE_HOST_CONTAINER"), "cat", path])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The agent CLI's launches the fake recorded, one JSON line each.
fn recorded_launches(cwd: &str) -> Vec<serde_json::Value> {
    container_file(&format!("{cwd}/routine-launches.jsonl"))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn remote_routine_launch_proof(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: Arc<dyn crate::content_engine::destination::DestinationIo>,
    client: Arc<crate::terminal::session_server::SessionServerClient>,
    runtime: &crate::commands::machines::RuntimeMemberTarget,
    prepared_cwd: &str,
    prepared_handle: &str,
    content: &std::path::Path,
) {
    use crate::agents::{service, Placement};
    use crate::routines::model::RoutineDef;
    use crate::routines::wake::TabOp;
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
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {team}\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 41\n    github_username: fixture\n",
    )
    .unwrap();
    std::fs::create_dir_all(content.join(".claude/hooks")).unwrap();
    std::fs::write(
        content.join(".claude/hooks/codesense-notify.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let mut barb =
        crate::agents::resolve::tests::draft("barb", codesense_pty_core::agent_cli::AgentCli::Claude);
    barb.startup = vec!["docs/current.md".into()];
    barb.mcp.servers.clear();
    let barb = service::save(
        &home,
        &principal,
        service::SaveTarget::Create(Placement::Team {
            org: ctx.org_slug.clone(),
            team: team.clone(),
        }),
        barb,
        &crate::routines::model::Author::user(principal.as_str()),
    )
    .unwrap()
    .key;

    // A fake agent CLI: it records every launch (cwd, argv) and proves the
    // workspace was prepared before it started. No provider, no inference.
    let script = r#"#!/usr/bin/python3
import sys,os,json
if '--help' in sys.argv:
 print('--mcp-config --strict-mcp-config --settings');sys.exit(0)
assert os.path.exists('docs/current.md'), 'Step 2 ran before the agent'
with open('routine-launches.jsonl','a') as out: out.write(json.dumps({'cwd':os.getcwd(),'argv':sys.argv})+'\n')
print('ROUTINE_AGENT_STARTED',flush=True)
import time; time.sleep(600)
"#;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    docker_as_dev(&format!("mkdir -p /home/dev/.local/bin && printf '%s' '{encoded}' | base64 -d > /home/dev/.local/bin/claude && chmod 700 /home/dev/.local/bin/claude"));

    let machine_id = runtime.member.machine_id.clone();
    let machine = Arc::new(SshRoutineMachine {
        ctx: ctx.clone(),
        slug: slug.clone(),
        io,
        client,
        runtime: runtime.clone(),
        prepared: (prepared_handle.to_string(), prepared_cwd.to_string()),
        state: crate::commands::dashboard_launch::DashboardLaunchState::new(41),
        receipts: home.join("routine-launch-intents"),
        lose_next_create: std::sync::atomic::AtomicBool::new(true),
        calls: Default::default(),
        spawns: Default::default(),
        step_two: Default::default(),
        posts: Default::default(),
        flights: Default::default(),
    });
    let launcher: Arc<dyn crate::routines::dispatch::Launcher> =
        Arc::new(SshRoutineLauncher(machine.clone()));
    let routine = |id: &str, mode: &str, task: &str| -> RoutineDef {
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
            mode: mode.into(),
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
        routine("wake-barb", "wake", "WAKE_TASK triage for {{date}}"),
        routine("fresh-barb", "fresh", "FRESH_TASK nightly for {{date}}"),
    ] {
        crate::routines::service::save(
            &home,
            &principal,
            crate::routines::service::SaveTarget::Create(
                crate::routines::service::Placement::Team {
                    org: ctx.org_slug.clone(),
                    team: team.clone(),
                },
            ),
            def,
            &crate::routines::model::Author::user(principal.as_str()),
        )
        .unwrap();
    }
    let store = crate::routines::state::RoutineStateStore::open(&home).unwrap();

    // AC1 + AC2: Barb is absent; the Create's answer is lost after the spawn.
    let wake = crate::routines::service::run_now(
        &home,
        &principal,
        &key_of("wake-barb"),
        Default::default(),
    )
    .unwrap();
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    let request = crate::routines::wake::launch_request_id(&wake.run_id);
    assert_eq!(
        *machine.calls.lock().unwrap(),
        vec![(TabOp::Create, request), (TabOp::Attach, request)],
        "one Create, then an Attach on the same request id"
    );
    assert_eq!(machine.spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    let run = store.run(&wake.run_id).unwrap().unwrap();
    assert_eq!(run.phase, "delivered", "started with its task: {run:?}");
    assert!(run.delivered_at.is_some());
    assert_eq!(run.wake_machine.as_deref(), Some(machine_id.as_str()));
    assert_eq!(run.launch_machine.as_deref(), Some(machine_id.as_str()));
    let session = run.session_id.clone().unwrap();
    assert_eq!(session, format!("routine-{}", request.simple()));
    let started = codesense_session_server::testing::wait_until(|| {
        recorded_launches(prepared_cwd).len() == 1
    });
    assert!(started, "the agent started on the machine");
    let launches = recorded_launches(prepared_cwd);
    let argv: Vec<String> = serde_json::from_value(launches[0]["argv"].clone()).unwrap();
    assert_eq!(
        launches[0]["cwd"], prepared_cwd,
        "in the workspace the machine prepared"
    );
    assert_eq!(argv[argv.len() - 2], "--");
    let prompt = &argv[argv.len() - 1];
    assert!(
        prompt.starts_with(&format!(
            "[routine: Routine wake-barb | run {}]",
            wake.run_id
        )) && prompt.contains("WAKE_TASK triage for 20"),
        "the task is the first prompt: {prompt:?}"
    );
    assert!(
        machine.posts.lock().unwrap().is_empty(),
        "confirmed by the Attach"
    );

    // AC4: a fresh routine aimed at the machine starts there too.
    let fresh = crate::routines::service::run_now(
        &home,
        &principal,
        &key_of("fresh-barb"),
        Default::default(),
    )
    .unwrap();
    crate::routines::test_support::drain_claim_loop(&home, launcher.clone()).await;
    let fresh_request = crate::routines::wake::launch_request_id(&fresh.run_id);
    assert_eq!(
        machine.calls.lock().unwrap()[2..],
        [(TabOp::Create, fresh_request)],
        "one Create under its own run's id"
    );
    let run = store.run(&fresh.run_id).unwrap().unwrap();
    assert_eq!(run.phase, "running", "{run:?}");
    assert_eq!(
        run.session_id.as_deref(),
        Some(format!("routine-{}", fresh_request.simple()).as_str())
    );
    assert!(codesense_session_server::testing::wait_until(
        || recorded_launches(prepared_cwd).len() == 2
    ));
    let launches = recorded_launches(prepared_cwd);
    let argv: Vec<String> = serde_json::from_value(launches[1]["argv"].clone()).unwrap();
    assert!(
        argv[argv.len() - 1].starts_with("FRESH_TASK nightly for 20"),
        "{argv:?}"
    );
    assert_eq!(machine.spawns.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(run.launch_machine.as_deref(), Some(machine_id.as_str()));

    // MUST 1 (review r1): routine maintenance asks the machine about the
    // remote run, never this Mac, so a running agent there is not swept.
    let clock_at = Arc::new(std::sync::atomic::AtomicI64::new(
        crate::routines::finish::now_ms() - 1_000,
    ));
    let clock = clock_at.clone();
    let ambient_home = home.clone();
    let ambient_principal = principal.clone();
    let ticker = crate::routines::tick::RoutineTicker::new(
        Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        Arc::new(crate::routines::tick::RoutineAdmitter),
        Arc::new(move || Some((ambient_home.clone(), ambient_principal.clone()))),
        machine.clone(),
    );
    clock_at.store(
        crate::routines::finish::now_ms(),
        std::sync::atomic::Ordering::SeqCst,
    );
    ticker.tick();
    let fresh_session = run.session_id.clone().unwrap();
    assert_eq!(
        store.run(&fresh.run_id).unwrap().unwrap().phase,
        "running",
        "its agent still runs on the machine"
    );
    // Its agent ends on the machine: the next tick ends the run.
    {
        use codesense_pty_core::domain::TerminalDomain;
        machine
            .client
            .kill(&codesense_pty_core::domain::PtySessionId::new(
                &fresh_session,
            ))
            .unwrap();
    }
    let ended = codesense_session_server::testing::wait_until(|| {
        ticker.tick();
        store.run(&fresh.run_id).unwrap().unwrap().phase == "ended"
    });
    assert!(ended, "{:?}", store.run(&fresh.run_id).unwrap());
    let reports = machine.step_two.lock().unwrap();
    assert_eq!(reports.len(), 2, "Step 2 once per launch");
    assert!(reports
        .iter()
        .all(|report| report.status == crate::agents::prepare::PrepareStatus::Ready));
    println!(
        "REMOTE_ROUTINE_LAUNCH_PASS machine={machine_id} cwd={prepared_cwd} wake_run={} wake_request={request} wake_phase=delivered calls=Create,Attach spawns=1 fresh_run={} fresh_request={fresh_request} maintenance=kept_running_then_ended step2_written={:?}",
        wake.run_id,
        fresh.run_id,
        reports.iter().map(|r| r.written.len()).collect::<Vec<_>>()
    );
}
