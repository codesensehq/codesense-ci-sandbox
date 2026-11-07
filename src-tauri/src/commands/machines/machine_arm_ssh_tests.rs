// SPDX-License-Identifier: MIT
// SW-AGR-8: real-SSH proof of routines armed on a machine.
//
// Over the owned Docker machine, with the real private terminal server: this
// Mac arms three routines on the machine through the real sync (two prepared
// there with the SW-XS-7 engine, one it cannot prepare).
//
// 1. Connected: the first fires on the machine's clock while this Mac is
//    connected. Its agent's CodeSense MCP is relayed to this Mac (review r1,
//    SHOULD 1: a fake desktop MCP answers `list_machines`). While it runs, a
//    desktop launch-if-absent of the same agent there is refused
//    `agentRunning` by the machine's single flight (MUST 2).
// 2. Away: this Mac disconnects. The second fires twice, a minute apart. Each
//    time a fake `claude` opens its CodeSense MCP exactly as the CLI would
//    (the bridge from its `--mcp-config`), gets `desktop_offline` for a
//    desktop tool and finishes its run with `routine_done`, answered by the
//    machine; it then idles at its prompt. The second fire still starts a
//    new agent (MUST 1: overlap is on the open run, not the session). The
//    unprepared one is `undelivered(needs_preparation)`.
// 3. Back: this Mac reconnects; the runs appear, finished with the agents'
//    summaries. Moved back to this Mac, the machine disarms it and refuses
//    the older epoch.

#[test]
#[ignore = "owned real SSH: routines armed on a machine fire with the desktop away (SW-AGR-8)"]
fn remote_machine_armed_routine_over_real_ssh() {
    std::env::set_var("CODESENSE_MACHINE_ARMED_PROOF", "1");
    remote_hydration_fixture(true, false, true, true);
}

/// The machine as the sync sees it: reachable through `client` (or away),
/// its workspace from its durable preparation receipt (SW-AGR-12: closed in
/// the UI, no viewer state), and preparation through the real
/// SW-XS-7 engine.
struct SshArmEnv {
    ctx: crate::content_engine::ResolverContext,
    slug: crate::domain::SourceRepoSlug,
    io: Arc<dyn crate::content_engine::destination::DestinationIo>,
    client: std::sync::Mutex<Option<Arc<crate::terminal::session_server::SessionServerClient>>>,
    machine: String,
    /// Routine ids whose agent cannot be prepared on the machine.
    unpreparable: Vec<String>,
}

impl crate::routines::machine_arm::ArmEnv for SshArmEnv {
    fn host(&self, machine: &str) -> crate::routines::machine_arm::ArmLookup {
        use crate::routines::machine_arm::ArmLookup;
        if machine != self.machine {
            return ArmLookup::Unknown;
        }
        match self.client.lock().unwrap().clone() {
            Some(client) if client.supports_routine_arm() => ArmLookup::Ready(client),
            Some(_) => ArmLookup::Unsupported("RoutineArmV1 missing".into()),
            None => ArmLookup::Offline,
        }
    }

    fn place(
        &self,
        routine: &crate::routines::DiscoveredRoutine,
        machine: &str,
    ) -> Result<crate::routines::machine_arm::ArmPlace, String> {
        let team_key = format!("{}:{}", routine.key.org, routine.key.team);
        let target = routine.def.target.resolve().map_err(|e| e.to_string())?;
        let places = crate::mcp::remote_workspace_target::prepared_places(
            &self.ctx.codesense_home,
            &self.ctx.principal(),
            41,
            machine,
        )?;
        let server = self
            .client
            .lock()
            .unwrap()
            .as_ref()
            .map(|client| client.namespace().server_id.clone());
        // The app's choice (`durable_place` past its app lookups), each
        // candidate confirmed by the real machine; the stale one sorts first.
        use futures_util::FutureExt;
        let client = self.client.lock().unwrap().clone();
        let place = crate::routines::wake_app::choose_place(
            &places,
            &team_key,
            machine,
            server.as_deref(),
            &|_| true,
            &target,
            |candidate| {
                let request = places
                    .iter()
                    .find(|p| p.handle == candidate.handle)
                    .unwrap()
                    .request
                    .clone();
                let confirmed = client
                    .as_ref()
                    .ok_or_else(|| "offline".to_string())
                    .and_then(|client| {
                        crate::mcp::remote_workspace_target::confirm_prepared(
                            client,
                            request,
                            &candidate.handle,
                            &candidate.cwd,
                        )
                    })
                    .map(|_| ());
                async move { confirmed }
            },
        )
        .now_or_never()
        .expect("the choice asks the machine synchronously")
        .map_err(|error| error.detail().to_string())?;
        Ok(crate::routines::machine_arm::ArmPlace {
            workspace_id: place.workspace_id,
            handle: place.handle,
            cwd: place.cwd,
        })
    }
}

impl crate::routines::machine_arm::ArmPrepare for SshArmEnv {
    fn prepare(
        &self,
        routine: &crate::routines::DiscoveredRoutine,
        _machine: &str,
        place: &crate::routines::machine_arm::ArmPlace,
    ) -> Result<codesense_pty_core::routine_arm::ArmLaunch, String> {
        use crate::agents::remote_launch;
        if self.unpreparable.contains(&routine.key.id) {
            return Err("the workspace is not prepared for this agent".into());
        }
        let key = routine.agent_key.clone().ok_or("no agent")?;
        let agent = remote_launch::prepare(
            &self.ctx.codesense_home,
            &self.ctx.principal(),
            &key,
            routine.key.org.clone(),
            routine.key.team.clone(),
            place.handle.clone(),
        )?;
        remote_launch::prepare_workspace(
            &self.ctx,
            &self.slug,
            self.io.as_ref(),
            codesense_host_proto::agent_cli::NativeLaunch::Cli(agent.runtime),
            Some((&agent.manifest, agent.name.as_str())),
            &agent.workspace_files,
            &|_| {},
        )?;
        let team_key = format!("{}:{}", routine.key.org, routine.key.team);
        let descriptor = codesense_pty_core::layout::WorkspaceDescriptor {
            display_name: place.workspace_id.clone(),
            kind: "generic".into(),
            org: Some(routine.key.org.clone()),
            team: Some(routine.key.team.clone()),
            story_id: None,
            repo: Some(self.slug.to_string()),
            worktree_path: Some(std::path::PathBuf::from(&place.cwd)),
            branch: None,
        };
        Ok(crate::routines::machine_arm::launch_recipe(
            &agent,
            place,
            "41",
            &team_key,
            crate::commands::pty_layout::workspace_key(&team_key, &place.workspace_id),
            Some(descriptor),
        ))
    }
}

/// The armed agent's launches the fake recorded, one JSON line each.
fn armed_launches(cwd: &str) -> Vec<serde_json::Value> {
    container_file(&format!("{cwd}/armed-launches.jsonl"))
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn sync_with(
    env: &SshArmEnv,
    home: &std::path::Path,
    principal: &Principal,
) -> crate::routines::machine_arm::SyncReport {
    let _sync = crate::routines::machine_arm_app::SYNCING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    with_input(home, principal, |input| {
        crate::routines::machine_arm::sync_machine(env, input, &env.machine)
    })
}

/// SW-XS-25: the explicit refresh an arm makes: it prepares the agent there.
fn refresh_with(
    env: &SshArmEnv,
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
) -> Result<String, String> {
    let _refresh = crate::routines::machine_arm_app::REFRESHING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prepared = with_input(home, principal, |input| {
        crate::routines::machine_arm::prepare_refresh(env, env, input, key)
    })?;
    let _sync = crate::routines::machine_arm_app::SYNCING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    with_input(home, principal, |input| {
        crate::routines::machine_arm::publish_refresh(env, input, prepared)
    })
}

fn with_input<R>(
    home: &std::path::Path,
    principal: &Principal,
    body: impl FnOnce(&crate::routines::machine_arm::SyncInput<'_>) -> R,
) -> R {
    use crate::routines::machine_arm::SyncInput;
    let routines = crate::routines::discover_routines(home, principal).unwrap();
    let host = crate::routines::host::local_host_id(home).unwrap();
    let store = crate::routines::state::RoutineStateStore::open(home).unwrap();
    let mode_of = |r: &crate::routines::DiscoveredRoutine| {
        crate::routines::service::effective_mode(home, principal, &r.key, &r.def)
    };
    let author_of = |_: &crate::routines::DiscoveredRoutine| "fixture".to_string();
    let undiscovered = |_: &str| None;
    let agent_of = |r: &crate::routines::DiscoveredRoutine| {
        Some(crate::routines::machine_arm::AgentFacts {
            manifest_hash: r.def.revision.to_string(),
            allow_wake: false,
        })
    };
    let settled = |key: &str, machine: &str, epoch: u64| {
        let key = crate::routines::model::RoutineKey::parse(key).unwrap();
        crate::routines::service::settle_handover(home, principal, &key, machine, epoch).unwrap();
    };
    let input = SyncInput {
        store: &store,
        principal,
        host_id: &host,
        routines: &routines,
        mode_of: &mode_of,
        author_of: &author_of,
        undiscovered: &undiscovered,
        agent_of: &agent_of,
        settled: &settled,
        notify: &crate::routines::wake::NoNotify,
        now: crate::routines::finish::now_ms(),
    };
    body(&input)
}

#[allow(clippy::too_many_arguments)]
async fn machine_armed_proof<D, DF, R, RF>(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: Arc<dyn crate::content_engine::destination::DestinationIo>,
    client: Arc<crate::terminal::session_server::SessionServerClient>,
    machine_id: &str,
    prepared_cwd: &str,
    prepared_handle: &str,
    content: &std::path::Path,
    disconnect: D,
    reconnect: R,
) where
    D: FnOnce() -> DF,
    DF: std::future::Future<Output = ()>,
    R: FnOnce() -> RF,
    RF: std::future::Future<Output = Arc<crate::terminal::session_server::SessionServerClient>>,
{
    use crate::agents::{service, Placement};
    use crate::routines::model::{RoutineTrigger, ScheduleSpec};
    use crate::routines::service::ArmPlacement;
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
    let mut barb = crate::agents::resolve::tests::draft(
        "barb",
        codesense_pty_core::agent_cli::AgentCli::Claude,
    );
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

    // A fake agent CLI that, like the real one, starts the CodeSense MCP
    // server its `--mcp-config` names, then calls a desktop tool and
    // `routine_done` for the run in its first prompt, and idles.
    let script = r#"#!/usr/bin/python3
import sys,os,json,subprocess,re,time
if '--help' in sys.argv:
 print('--mcp-config --strict-mcp-config --settings');sys.exit(0)
assert os.path.exists('docs/current.md'), 'Step 2 ran before the agent'
prompt = sys.argv[-1] if '--' in sys.argv else ''
rec = {'cwd':os.getcwd(),'argv':sys.argv}
with open('armed-starts.jsonl','a') as out: out.write(json.dumps({'argv':sys.argv})+'\n')
cfg = json.load(open(sys.argv[sys.argv.index('--mcp-config')+1]))
srv = cfg.get('mcpServers',{}).get('codesense')
run = re.search(r'\| run ([0-9a-f-]+)\]', prompt)
if srv and run:
 p = subprocess.Popen([srv['command']]+srv['args'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
 def call(i, method, params):
  p.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':method,'params':params})+'\n'); p.stdin.flush()
  return json.loads(p.stdout.readline())
 def step(name, f):
  try:
   rec[name] = f()
  except Exception as e:
   rec[name+'_error'] = repr(e)
   with open('armed-errors.txt','a') as out: out.write(name+': '+repr(e)+'\n')
 step('init', lambda: call(1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'fake','version':'1'}}))
 p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n'); p.stdin.flush()
 with open('armed-errors.txt','a') as out: out.write('initialized '+json.dumps(rec.get('init'))+'\n')
 step('other', lambda: call(2,'tools/call',{'name':'list_machines','arguments':{}}))
 step('done', lambda: call(3,'tools/call',{'name':'routine_done','arguments':{'runId':run.group(1),'outcome':'success','summary':'ARMED_DONE on the machine'}}))
with open('armed-launches.jsonl','a') as out: out.write(json.dumps(rec)+'\n')
print('ARMED_AGENT_STARTED',flush=True)
time.sleep(600)
"#;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    docker_as_dev(&format!("mkdir -p /home/dev/.local/bin && printf '%s' '{encoded}' | base64 -d > /home/dev/.local/bin/claude && chmod 700 /home/dev/.local/bin/claude"));

    // The machine's clock decides, in the routine's own zone (UTC here): the
    // connected one at a minute about two from now, the others two and three
    // minutes after it.
    use chrono::Timelike;
    let at = chrono::Utc::now() + chrono::Duration::seconds(150);
    let minute = |offset: i64| {
        let t = at + chrono::Duration::minutes(offset);
        format!("{} {} * * *", t.minute(), t.hour())
    };
    let team_key = format!("{}:{team}", ctx.org_slug);
    let schedules = |crons: &[String]| -> Vec<RoutineTrigger> {
        crons
            .iter()
            .map(|cron| {
                RoutineTrigger::Schedule(ScheduleSpec {
                    cron: cron.clone(),
                    tz: "UTC".into(),
                })
            })
            .collect()
    };
    for (id, triggers) in [
        ("armed-connected", schedules(&[minute(0)])),
        ("armed-nightly", schedules(&[minute(2), minute(3)])),
        ("armed-unprepared", schedules(&[minute(2)])),
    ] {
        let mut target = crate::routines::model::Where::team_workspace();
        target.kind = format!("repo:{slug}");
        crate::routines::service::save(
            &home,
            &principal,
            crate::routines::service::SaveTarget::Create(
                crate::routines::service::Placement::Team {
                    org: ctx.org_slug.clone(),
                    team: team.clone(),
                },
            ),
            crate::routines::model::RoutineDef {
                action: Default::default(),
                command: Vec::new(),
                after: None,
                id: id.into(),
                name: format!("Routine {id}"),
                owner: principal.as_str().to_string(),
                agent: crate::routines::model::AgentRef::from(barb.to_string().as_str()),
                mode: "fresh".into(),
                task: "ARMED_TASK nightly for {{date}}".into(),
                target,
                triggers,
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
            },
            &crate::routines::model::Author::user(principal.as_str()),
        )
        .unwrap();
        crate::routines::service::arm_on(
            &home,
            &principal,
            &crate::routines::model::RoutineKey::new(&ctx.org_slug, &team, id),
            ArmPlacement::Machine(machine_id.to_string()),
            &crate::routines::model::Author::user(principal.as_str()),
        )
        .unwrap();
    }
    // This Mac relays the CodeSense MCP of the agents the machine starts
    // while it is connected (a fake desktop MCP stands in for the app's).
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    client.accept_routine_bindings(
        secret.clone(),
        fake_desktop_mcp(),
        tokio::runtime::Handle::current(),
    );
    client.routine_arm_bind_secret(&secret, None).unwrap();
    let key_of = |id: &str| crate::routines::model::RoutineKey::new(&ctx.org_slug, &team, id);
    let env = SshArmEnv {
        ctx: ctx.clone(),
        slug: slug.clone(),
        io,
        client: std::sync::Mutex::new(Some(client.clone())),
        machine: machine_id.to_string(),
        unpreparable: vec!["armed-unprepared".into()],
    };
    // SW-AGR-12: the workspace is found from its receipt, nothing open.
    let places =
        crate::mcp::remote_workspace_target::prepared_places(&home, &principal, 41, machine_id)
            .unwrap();
    assert!(
        places
            .iter()
            .any(|p| p.handle == prepared_handle && p.cwd == prepared_cwd),
        "{places:?}"
    );
    // Arming them is explicit (SW-XS-25): each is prepared there now. The
    // one that cannot be is still armed by its refresh, with its definition
    // and no launch recipe (review r1 MUST 1), and stays content_stale.
    for id in ["armed-nightly", "armed-connected"] {
        refresh_with(&env, &home, &principal, &key_of(id).to_string()).unwrap();
    }
    assert!(refresh_with(
        &env,
        &home,
        &principal,
        &key_of("armed-unprepared").to_string()
    )
    .is_err());
    let list = client.routine_arms().unwrap();
    assert_eq!(list.armed.len(), 3, "{list:?}");
    // The sync that follows finds every arm current: nothing to push.
    let armed = sync_with(&env, &home, &principal);
    assert!(armed.armed.is_empty(), "{armed:?}");
    assert!(armed.errors.is_empty(), "{armed:?}");
    assert_eq!(
        armed.content_stale,
        vec![key_of("armed-unprepared").to_string()],
        "{armed:?}"
    );
    let info_of = |id: &str| {
        list.armed
            .iter()
            .find(|info| info.key == key_of(id).to_string())
            .unwrap()
            .clone()
    };
    let nightly_info = info_of("armed-nightly");
    assert!(nightly_info.has_launch, "prepared at arm time");
    assert!(
        info_of("armed-connected").has_launch,
        "prepared at arm time"
    );
    assert!(
        !info_of("armed-unprepared").has_launch,
        "its preparation failed: fires are needs_preparation"
    );
    let fire_at = nightly_info.next_at_ms.expect("scheduled");
    let connected_at = info_of("armed-connected").next_at_ms.expect("scheduled");
    let launches_of = |name: &str| -> Vec<serde_json::Value> {
        armed_launches(prepared_cwd)
            .into_iter()
            .filter(|l| {
                l["argv"]
                    .as_array()
                    .and_then(|argv| argv.last())
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.contains(&format!("[routine: Routine {name} |")))
            })
            .collect()
    };
    let wait_for = |name: &'static str, count: usize, secs: u64| {
        let launches_of = &launches_of;
        async move {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            loop {
                let found = launches_of(name);
                if found.iter().filter(|l| l.get("done").is_some()).count() >= count
                    || std::time::Instant::now() >= deadline
                {
                    return found;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // 1. Connected: its agent's CodeSense MCP is this Mac's (relayed).
    let connected = wait_for("armed-connected", 1, 300).await;
    assert_eq!(
        connected.len(),
        1,
        "fired once while connected (at {connected_at}); machine runs {:?}; agent starts {:?}; agent errors {:?}; server logs {}",
        client.routine_arm_runs(0).map(|log| log.runs),
        container_file(&format!("{prepared_cwd}/armed-starts.jsonl")),
        container_file(&format!("{prepared_cwd}/armed-errors.txt")),
        container_logs(),
    );
    assert_eq!(
        connected[0]["other"]["result"]["content"][0]["text"], "DESKTOP_ANSWERED",
        "relayed to this Mac: {}",
        connected[0]
    );
    // MUST 2: while that agent runs there, a desktop launch-if-absent of the
    // same agent in the same workspace is refused by the machine's flight.
    let mut tags = codesense_pty_core::layout::Tags::new();
    tags.insert("remoteAgentKey".into(), barb.to_string());
    tags.insert("remoteWorkspaceId".into(), "objects".into());
    tags.insert(
        codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT.into(),
        "1".into(),
    );
    let refused = client
        .open_remote_tab(
            codesense_pty_core::layout::OpenTab {
                viewer: crate::commands::pty_layout::app_viewer(),
                workspace: crate::commands::pty_layout::workspace_key(&team_key, "objects"),
                session: codesense_pty_core::domain::PtySessionId::new("desk-if-absent"),
                descriptor: Some(Default::default()),
                name: None,
                role: None,
                tags,
                placement: codesense_pty_core::layout::Placement::Tab,
                index: None,
                activate: false,
            },
            || panic!("the machine's flight refuses before any spawn"),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.starts_with(codesense_pty_core::routine_arm::AGENT_RUNNING),
        "{refused}"
    );

    // 2. This Mac goes away. The machine fires on its own clock, twice.
    drop(client);
    *env.client.lock().unwrap() = None;
    disconnect().await;
    let nightly = wait_for("armed-nightly", 2, 420).await;
    assert_eq!(
        nightly.iter().filter(|l| l.get("done").is_some()).count(),
        2,
        "MUST 1: its agent finished the first run and idles; the next fire starts the agent again: {nightly:?}"
    );
    let launch = nightly[0].clone();
    assert_eq!(
        launch["cwd"], prepared_cwd,
        "in the workspace the machine prepared"
    );
    let run_id = codesense_pty_core::routine_arm::arm_run_id(
        &key_of("armed-nightly").to_string(),
        nightly_info.epoch,
        fire_at,
    );
    let second_run = codesense_pty_core::routine_arm::arm_run_id(
        &key_of("armed-nightly").to_string(),
        nightly_info.epoch,
        fire_at + 60_000,
    );
    let argv: Vec<String> = serde_json::from_value(launch["argv"].clone()).unwrap();
    assert!(
        argv[argv.len() - 1].starts_with(&format!(
            "[routine: Routine armed-nightly | run {run_id}]\nARMED_TASK nightly for 20"
        )),
        "{argv:?}"
    );
    assert_eq!(
        launch["other"]["error"]["data"]["type"], "desktop_offline",
        "a desktop tool says why: {launch}"
    );
    assert!(
        launch["done"].get("result").is_some(),
        "routine_done answered on the machine: {launch}"
    );
    // The session a machine-armed fire starts is named after its run.
    let first_session = format!("routine-{}", run_id.replace('-', ""));

    // 3. This Mac comes back: the runs are here.
    let client = reconnect().await;
    *env.client.lock().unwrap() = Some(client.clone());
    let back = sync_with(&env, &home, &principal);
    assert!(back.recorded >= 2, "{back:?}");
    let store = crate::routines::state::RoutineStateStore::open(&home).unwrap();
    let nightly = store.run(&run_id).unwrap().expect("its run is on this Mac");
    assert_eq!(nightly.phase, "succeeded", "{nightly:?}");
    assert_eq!(
        nightly.result_text.as_deref(),
        Some("ARMED_DONE on the machine")
    );
    assert_eq!(nightly.launch_machine.as_deref(), Some(machine_id));
    let second = store.run(&second_run).unwrap().expect("its second run");
    assert_eq!(
        second.phase, "succeeded",
        "launched again, not coalesced: {second:?}"
    );
    // The first agent still idles at its prompt.
    let first_live = {
        use codesense_pty_core::domain::TerminalDomain;
        client
            .live_child_pids(&[codesense_pty_core::domain::PtySessionId::new(
                &first_session,
            )])
            .unwrap()
            .first()
            .copied()
            .flatten()
            .is_some()
    };
    assert!(first_live, "its session lived on through the second fire");
    let unprepared_id = codesense_pty_core::routine_arm::arm_run_id(
        &key_of("armed-unprepared").to_string(),
        1,
        fire_at,
    );
    let unprepared = store.run(&unprepared_id).unwrap().expect("its run too");
    assert_eq!(unprepared.phase, "undelivered");
    assert_eq!(unprepared.reason.as_deref(), Some("needs_preparation"));

    // Moved back to this Mac: disarmed there, and the older epoch refused.
    crate::routines::service::arm_on(
        &home,
        &principal,
        &key_of("armed-nightly"),
        ArmPlacement::ThisMac,
        &crate::routines::model::Author::user(principal.as_str()),
    )
    .unwrap();
    let handed = sync_with(&env, &home, &principal);
    assert_eq!(
        handed.disarmed,
        vec![key_of("armed-nightly").to_string()],
        "{handed:?}"
    );
    assert_eq!(
        handed.handed_over,
        vec![key_of("armed-nightly").to_string()]
    );
    let list = client.routine_arms().unwrap();
    assert!(list
        .disarmed
        .iter()
        .any(|t| t.key == key_of("armed-nightly").to_string() && t.epoch == 2));
    let stale = codesense_pty_core::routine_arm::RoutineArm {
        limits: None,
        key: key_of("armed-nightly").to_string(),
        epoch: 1,
        revision: 99,
        name: "stale".into(),
        mode: codesense_pty_core::routine_arm::ArmMode::Fresh,
        schedules: vec![codesense_pty_core::routine_arm::ArmSchedule {
            cron: "0 9 * * *".into(),
            tz: "UTC".into(),
        }],
        task: "x".into(),
        target: codesense_pty_core::routine_arm::ArmTarget {
            agent: barb.to_string(),
            workspace_id: "objects".into(),
            cwd: prepared_cwd.into(),
            owner: None,
            allow_wake: false,
        },
        max_wait_ms: 60_000,
        overlap: Default::default(),
        missed: Default::default(),
        launch: None,
        originator: Default::default(),
        author: String::new(),
        max_runtime_ms: 0,
        last_fired_ms: None,
        last_run_summary: None,
        last_run_at_ms: None,
        source_digest: String::new(),
    };
    assert!(matches!(
        client.routine_arm(stale).unwrap(),
        codesense_pty_core::routine_arm::ArmAnswer::Refused {
            refusal: codesense_pty_core::routine_arm::ArmRefusal::Stale { held_epoch: 2, .. }
        }
    ));
    println!(
        "REMOTE_MACHINE_ARMED_PASS machine={machine_id} cwd={prepared_cwd} connected_tool={} desktop_if_absent={} fired_at={fire_at} run={run_id} phase={} summary={:?} second_run={second_run} second_phase={} first_session_live={first_live} other_tool={} unprepared={}({}) disarmed_epoch=2 stale_epoch_1=refused",
        connected[0]["other"]["result"]["content"][0]["text"],
        codesense_pty_core::routine_arm::AGENT_RUNNING,
        nightly.phase,
        nightly.result_text.unwrap_or_default(),
        second.phase,
        launch["other"]["error"]["data"]["type"],
        unprepared.phase,
        unprepared.reason.unwrap_or_default(),
    );
}

/// A desktop's CodeSense MCP, as far as this proof needs one: every request
/// is answered `DESKTOP_ANSWERED`, so a relayed call is told from the
/// machine's own answers.
fn fake_desktop_mcp() -> crate::terminal::session_server::RoutineBinder {
    Arc::new(|_session: &str| {
        let serve: Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync> = Arc::new(
            |_server: String, stream: tokio::io::DuplexStream| {
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
                    let (read, mut write) = tokio::io::split(stream);
                    let mut lines = tokio::io::BufReader::new(read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                            continue;
                        };
                        let Some(id) = message.get("id").filter(|id| !id.is_null()) else {
                            continue;
                        };
                        let answer = serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                            "protocolVersion":"2024-11-05","capabilities":{"tools":{}},
                            "serverInfo":{"name":"fake desktop","version":"1"},
                            "content":[{"type":"text","text":"DESKTOP_ANSWERED"}],"isError":false}});
                        if write
                            .write_all(format!("{answer}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            },
        );
        Some(serve)
    })
}

/// The tails of the machine's CodeSense logs, for a failure's message.
fn container_logs() -> String {
    let output = std::process::Command::new("docker")
        .args([
            "exec",
            &std::env::var("CODESENSE_REMOTE_HOST_CONTAINER").unwrap_or_default(),
            "sh",
            "-c",
            "for f in $(find /home/dev/.codesense /tmp -name '*.log' 2>/dev/null); do echo \"== $f\"; tail -n 60 \"$f\"; done",
        ])
        .output();
    output
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}
