// SPDX-License-Identifier: MIT
//! Seam tests for routines running agents: agents saved through the real
//! agent store, routines through the real routine service, fired by the real
//! ticker and claim loop into the stub launcher, and the agent's generated
//! launch entries written by the same helper the launch spines call.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::agents::adapter::LaunchMode;
use crate::agents::launch_check::LaunchWarningCode;
use crate::agents::model::LaunchMode as AgentLaunchMode;
use crate::agents::{binding, AgentKey};
use crate::context_repo::inheritance::team_view;
use crate::domain::Principal;
use crate::install::agent_account_store::AgentProvider;
use crate::routines::model::RoutineKey;
use codesense_pty_core::agent_cli::AgentCli;

use super::agents::RoutineReferrers;
use super::model::{AgentRef, Author};
use super::run_seams::{key, manual, ms, routine, run, save, ticker};
use super::service::{self, PersonalOwner, Placement, SaveTarget};
use super::state::RoutineStateStore;
use super::test_support::{
    agent, alice, drain_claim_loop, save_team_agent, stub_agent, write_org, StubLauncher, TempHome,
    ORG, TEAM,
};

#[tokio::test]
async fn dispatch_prepares_merge_marked_startup_files() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let org = crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap();
    let view = team_view(&org, ORG, TEAM);
    let content = view.own_content().own_path().unwrap().to_path_buf();
    std::fs::create_dir_all(content.join(".claude/skills/judge")).unwrap();
    let settings = serde_json::json!({"hooks": {"Stop": [{"command": "team-hook"}]}});
    std::fs::write(
        content.join(".claude/settings.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    std::fs::write(
        content.join(".claude/skills/judge/SKILL.md"),
        "Judge agents\n",
    )
    .unwrap();
    crate::install::merge_marks::set_merge_mark(
        &crate::context_repo::path_to_team_config_dir(&org, TEAM),
        ".claude/settings.json",
        BTreeMap::from([("hooks.Stop".into(), "command".into())]),
    )
    .unwrap();
    std::fs::create_dir_all(content.join(".codex")).unwrap();
    std::fs::write(
        content.join(".codex/config.toml"),
        "model = \"project-model\"\n",
    )
    .unwrap();
    crate::install::merge_marks::set_merge_mark(
        &crate::context_repo::path_to_team_config_dir(&org, TEAM),
        ".codex/config.toml",
        BTreeMap::new(),
    )
    .unwrap();
    let mut judge = agent("judge", AgentCli::Claude);
    judge.startup = vec![".claude".into(), ".codex".into()];
    save_team_agent(&home, &alice(), judge);
    let mut def = routine("judge", "0 9 * * *");
    def.agent = AgentRef::from("judge");
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    manual(&home, "judge");
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    let cwd = &launched.request.cwd;
    assert!(cwd.join(".claude/skills/judge/SKILL.md").exists());
    let actual: serde_json::Value = serde_json::from_slice(
        &std::fs::read(cwd.join(".claude/settings.json"))
            .expect("merged startup settings must be prepared"),
    )
    .unwrap();
    assert_eq!(actual, settings);
    let codex: toml::Value =
        toml::from_str(&std::fs::read_to_string(cwd.join(".codex/config.toml")).unwrap()).unwrap();
    assert_eq!(codex["model"].as_str(), Some("project-model"));
    assert!(codex["mcp_servers"]["codesense"]["command"]
        .as_str()
        .is_some());
    launcher.exit(&launched.session_id).await;
}

#[tokio::test]
async fn fresh_team_context_matches_preview_and_resets_only_its_workspace() {
    use crate::mcp::agent_tools::{self, AgentCaller, AgentReply};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let org = crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap();
    let view = team_view(&org, ORG, TEAM);
    let content = view.own_content().own_path().unwrap().to_path_buf();
    let skills = [
        "adt-evaluate",
        "adt-lint",
        "agent-design",
        "cited-retrieval",
    ];
    let mut evaluator = agent("adt-evaluator", AgentCli::Claude);
    evaluator.context = Some(
        skills
            .iter()
            .map(|name| {
                let source = content.join(format!("skills/{name}/SKILL.md"));
                std::fs::create_dir_all(source.parent().unwrap()).unwrap();
                std::fs::write(
                    source,
                    format!("---\nname: {name}\ndescription: Evaluate agents\n---\n{name}\n"),
                )
                .unwrap();
                serde_yaml::from_str(&format!(
                    "path: skills/{name}\ndest: .claude/skills/{name}\n"
                ))
                .unwrap()
            })
            .collect(),
    );
    let agent_key = save_team_agent(&home, &alice(), evaluator.clone());
    let preview = || {
        let reply = agent_tools::preview(
            &home,
            &alice(),
            crate::agents::Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            },
            &AgentCaller::new(&alice(), None),
            serde_json::from_value(
                serde_json::json!({"agent": agent_key.to_string(), "teamWorkspace": true}),
            )
            .unwrap(),
        )
        .unwrap();
        let AgentReply::Ok(reply) = reply else {
            panic!("preview refused")
        };
        assert!(reply.context.warnings.is_empty());
        reply.context.files
    };
    let snapshot = |root: &std::path::Path| -> BTreeMap<std::path::PathBuf, Vec<u8>> {
        walkdir::WalkDir::new(root)
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (
                    e.path().strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect()
    };
    let before = snapshot(&content);
    let mut def = routine("adt-evaluate", "0 9 * * *");
    def.agent = AgentRef::from("adt-evaluator");
    def.mode = "fresh".into();
    save(&home, def.clone());
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let mut previous_cwd = None;
    let mut previous_directory = None;
    for visible in [false, true] {
        if visible {
            def.policy.visible = true;
            service::save(
                &home,
                &alice(),
                SaveTarget::Update {
                    key: key("adt-evaluate"),
                    base_revision: 1,
                },
                def.clone(),
                &Author::user("alice"),
            )
            .unwrap();
            std::fs::write(
                content.join("skills/adt-evaluate/SKILL.md"),
                "---\nname: adt-evaluate\ndescription: Revised evaluator\n---\nRevised\n",
            )
            .unwrap();
            evaluator
                .context
                .as_mut()
                .unwrap()
                .retain(|entry| entry.path != "skills/adt-lint");
            crate::agents::service::save(
                &home,
                &alice(),
                crate::agents::SaveTarget::Update {
                    key: agent_key.clone(),
                    base_revision: 1,
                },
                evaluator.clone(),
                &Author::user("alice"),
            )
            .unwrap();
        }
        let expected = preview();
        manual(&home, "adt-evaluate");
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        let launched = launcher.last();
        let cwd = &launched.request.cwd;
        assert!(
            !cwd.starts_with(&content),
            "projected files must not pollute team content"
        );
        assert_eq!(launched.request.visible, visible);
        if let Some(previous) = &previous_cwd {
            assert_eq!(cwd, previous);
        }
        if let Some(previous) = &previous_directory {
            use std::os::unix::fs::MetadataExt;
            let previous: &std::fs::File = previous;
            assert_eq!(
                previous.metadata().unwrap().ino(),
                std::fs::metadata(cwd).unwrap().ino(),
                "a completed run's visible tab must keep a valid cwd"
            );
        }
        let actual = snapshot(cwd)
            .into_iter()
            .filter(|(p, _)| p.starts_with(".claude/skills"))
            .collect::<BTreeMap<_, _>>();
        let expected = expected
            .iter()
            .map(|f| {
                (
                    std::path::PathBuf::from(&f.destination),
                    std::fs::read(&f.source).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(actual, expected);
        assert!(
            actual.contains_key(&std::path::PathBuf::from(
                ".claude/skills/adt-evaluate/SKILL.md"
            )),
            "/adt-evaluate is discoverable"
        );
        assert!(!cwd.join("previous-run.txt").exists());
        assert!(!cwd.join("outside-link").exists());
        if !visible {
            assert_eq!(actual.len(), 4);
            assert_eq!(snapshot(&content), before);
        }
        std::fs::write(cwd.join("previous-run.txt"), "scratch").unwrap();
        std::os::unix::fs::symlink(&content, cwd.join("outside-link")).unwrap();
        previous_cwd = Some(cwd.clone());
        previous_directory = Some(std::fs::File::open(cwd).unwrap());
        launcher.exit(&launched.session_id).await;
    }
    assert!(
        content.join("skills/adt-evaluate/SKILL.md").exists(),
        "reset must not follow scratch symlinks"
    );
    let cwd = previous_cwd.unwrap();
    let source_before = snapshot(&content);
    std::fs::remove_dir_all(&cwd).unwrap();
    std::os::unix::fs::symlink(&content, &cwd).unwrap();
    let refused = manual(&home, "adt-evaluate").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(
        launcher.calls(),
        2,
        "a workspace symlink must never launch or clear its target"
    );
    assert_eq!(run(&home, &refused).phase, "failed");
    assert_eq!(snapshot(&content), source_before);
}

fn bob() -> Principal {
    Principal::from(2u64)
}

/// The codex `triage` team agent, with an MCP server and an arg of its own,
/// and one Codex account bound to it for Alice.
fn codex_triage(home: &std::path::Path) -> (AgentKey, String) {
    // The team's Codex MCP config, which has the agent's server.
    let content = crate::context_repo::path_to_team_content_dir(
        &crate::context_repo::path_to_org_root(home, &alice(), ORG).unwrap(),
        TEAM,
    );
    std::fs::create_dir_all(content.join(".codex")).unwrap();
    std::fs::write(
        content.join(".codex/config.toml"),
        "[mcp_servers.slack]\ncommand = \"slack-mcp\"\n",
    )
    .unwrap();
    let key = save_team_agent(
        home,
        &alice(),
        crate::agents::model::Agent {
            args: vec!["--model".into(), "gpt-5".into()],
            mcp: crate::agents::model::McpAccess {
                bridge_tools: None,
                servers: vec!["slack".into()],
                extra: Default::default(),
            },
            ..agent("triage", AgentCli::Codex)
        },
    );
    let account =
        crate::agents::resolve::tests::ready_account(home, &alice(), AgentProvider::Codex);
    binding::set(home, &alice(), &key, &account.id).unwrap();
    (key, account.id)
}

/// A team routine, fired by its schedule through the real ticker and claim
/// loop, launches its agent with that agent's args, servers, runtime and this
/// user's bound account, and records `agent@revision`. Archiving the agent
/// keeps the routine running at the archived revision, and a headless run
/// gets the documented Codex default policy and never a bypass flag.
#[tokio::test]
async fn a_routine_runs_its_agent_with_the_users_account_and_records_its_revision() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let (agent_key, account_id) = codex_triage(&home);
    let mut def = routine("sweep", "*/5 * * * *");
    def.agent = AgentRef::from("triage");
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())).writing_generated());
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let ticker = ticker(&home, &clock, launcher.clone());

    ticker.tick().unwrap();
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    let run_id = ticker.tick().unwrap().changed[0].1.clone();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    let agent = &launched.request.agent;
    assert_eq!(agent.launch.binary, "codex");
    assert_eq!(launched.request.agent.runtime, AgentCli::Codex);
    assert!(
        agent
            .launch
            .args
            .ends_with(&["--model".into(), "gpt-5".into()]),
        "{:?}",
        agent.launch.args
    );
    assert_eq!(agent.launch.mcp_allow, Some(vec!["slack".to_string()]));
    assert_eq!(agent.launch.mode, LaunchMode::Headless, "not visible");
    // The headless launch applies this user's binding for the agent.
    assert_eq!(agent.account_override.as_deref(), Some(account_id.as_str()));
    assert_eq!(
        agent.account_input().override_account_id.as_deref(),
        Some(account_id.as_str())
    );
    assert_eq!(
        launched.request.prompt,
        "Summarize what changed. Last time: "
    );
    assert_eq!(
        launched.request.provenance.agent.as_deref(),
        Some("acme/api/backend/triage@1")
    );
    assert_eq!(
        launched.request.provenance.manifest_hash.as_deref(),
        Some(agent.manifest_hash.as_str())
    );
    assert_eq!(
        run(&home, &run_id).agent.as_deref(),
        Some("acme/api/backend/triage@1")
    );
    // The session is stamped with the agent, as every agent launch is.
    assert_eq!(
        launcher
            .registry
            .agent_revision_for_session(&launched.session_id)
            .as_deref(),
        Some("acme/api/backend/triage@1")
    );

    // A headless run's generated entries, written by the helper the launch
    // spines call: the default Codex policy, and no bypass flag in the argv.
    // Codex keeps the user's own home; the policy rides the generated argv.
    for entry in [
        "approval_policy=\"never\"",
        "sandbox_mode=\"workspace-write\"",
    ] {
        assert!(
            launched.generated_argv.iter().any(|a| a == entry),
            "{entry} missing from {:?}",
            launched.generated_argv
        );
    }
    for argv in [&agent.launch.args, &launched.generated_argv] {
        assert!(
            !argv
                .iter()
                .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"),
            "{argv:?}"
        );
    }

    launcher
        .signal_done(&launched.session_id, "success", "swept")
        .await;
    assert_eq!(run(&home, &run_id).phase, "succeeded");
    launcher.exit(&launched.session_id).await;

    // Archived, it still resolves and runs; the run records the archived
    // revision.
    let archived =
        crate::agents::service::archive(&home, &alice(), &agent_key, &Author::user("alice"))
            .unwrap();
    assert_eq!(archived.agent.revision, 2);
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    let second = ticker.tick().unwrap().changed[0].1.clone();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let row = run(&home, &second);
    assert_eq!(row.phase, "running");
    assert_eq!(row.agent.as_deref(), Some("acme/api/backend/triage@2"));
    launcher.exit(&launcher.last().session_id).await;
}

/// The settings file the launch built for a Claude agent's routine run.
fn settings_file(argv: &[String]) -> std::path::PathBuf {
    let at = argv
        .iter()
        .position(|a| a == "--settings")
        .unwrap_or_else(|| panic!("no --settings in {argv:?}"));
    std::path::PathBuf::from(&argv[at + 1])
}

/// A Claude agent's routine run gets the agent's own permissions in a
/// per-launch `--settings` file, plus only what CodeSense needs, and no
/// bypass flag. The file lives as long as the run's session: it is gone once
/// the run ends, whether the agent exits or the run is stopped.
#[tokio::test]
async fn a_claude_routine_run_gets_its_own_settings_file_until_the_run_ends() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save_team_agent(
        &home,
        &alice(),
        crate::agents::model::Agent {
            permissions: Some(crate::agents::model::Permissions {
                claude: Some(crate::agents::model::ClaudePermissions {
                    allow: vec!["Bash(npm test:*)".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..agent("tester", AgentCli::Claude)
        },
    );
    // The workspace's MCP config, which every Claude launch scopes.
    let content = crate::context_repo::path_to_team_content_dir(
        &crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap(),
        TEAM,
    );
    std::fs::write(
        content.join(".mcp.json"),
        r#"{"mcpServers": {"codesense": {"command": "bridge"}}}"#,
    )
    .unwrap();
    let mut def = routine("test", "0 9 * * *");
    def.agent = AgentRef::from("tester");
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())).writing_generated());

    let run_id = manual(&home, "test").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    assert_eq!(launched.request.agent.launch.mode, LaunchMode::Headless);
    let file = settings_file(&launched.generated_argv);
    assert!(
        !file.starts_with(&launched.request.cwd),
        "never in the workspace"
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let allow: Vec<&str> = settings["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(allow.contains(&"Bash(npm test:*)"), "{settings}");
    for required in crate::agents::permissions::CLAUDE_REQUIRED_ALLOW {
        assert!(allow.contains(required), "{required} missing: {settings}");
    }
    assert_eq!(
        settings["permissions"]["defaultMode"],
        crate::agents::permissions::CLAUDE_HEADLESS_DEFAULT_MODE
    );
    for argv in [
        &launched.request.agent.launch.args,
        &launched.generated_argv,
    ] {
        assert!(!argv.iter().any(|a| a.contains("dangerously")), "{argv:?}");
    }

    launcher
        .signal_done(&launched.session_id, "success", "tested")
        .await;
    assert_eq!(run(&home, &run_id).phase, "succeeded");
    launcher.exit(&launched.session_id).await;
    assert!(!file.exists(), "removed when the run's session ended");

    // A stopped run's session goes too, and its settings with it.
    let stopped = manual(&home, "test").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let file = settings_file(&launcher.last().generated_argv);
    assert!(file.exists());
    assert!(service::stop_run(&home, &alice(), &stopped, launcher.as_ref()).unwrap());
    assert!(!file.exists(), "removed when the run was stopped");
}

/// A visible routine runs its agent interactive, whatever the agent declares,
/// and an edit to the agent applies to the next run with no routine change.
#[tokio::test]
async fn visible_wins_over_the_agents_mode_and_agent_edits_apply_to_the_next_run() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key_ = save_team_agent(
        &home,
        &alice(),
        crate::agents::model::Agent {
            launch: AgentLaunchMode::Headless,
            ..agent("builder", AgentCli::Claude)
        },
    );
    let mut def = routine("build", "*/5 * * * *");
    def.agent = AgentRef::from("builder");
    def.policy.visible = true;
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    manual(&home, "build");
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let first = launcher.last();
    assert!(first.request.visible);
    assert_eq!(first.request.agent.launch.mode, LaunchMode::Interactive);
    assert!(!first
        .request
        .agent
        .launch
        .args
        .contains(&"--verbose".into()));
    launcher.exit(&first.session_id).await;

    let current = crate::agents::service::get(&home, &alice(), &key_).unwrap();
    crate::agents::service::save(
        &home,
        &alice(),
        crate::agents::SaveTarget::Update {
            key: key_.clone(),
            base_revision: current.agent.revision,
        },
        crate::agents::model::Agent {
            args: vec!["--verbose".into()],
            ..current.agent
        },
        &Author::user("alice"),
    )
    .unwrap();
    let run_id = manual(&home, "build").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert!(launcher
        .last()
        .request
        .agent
        .launch
        .args
        .contains(&"--verbose".into()));
    assert_eq!(
        run(&home, &run_id).agent.as_deref(),
        Some("acme/api/backend/builder@2")
    );
    assert_eq!(
        service::get(&home, &alice(), &key("build"))
            .unwrap()
            .def
            .revision,
        1,
        "the routine was not touched"
    );
    launcher.exit(&launcher.last().session_id).await;
}

/// A routine is a referrer of its agent: a hard delete is refused while it
/// names it, and `used_by` lists it, including a teammate's team routine.
#[test]
fn a_routines_agent_cannot_be_deleted_out_from_under_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let (agent_key, _) = codex_triage(&home);
    let mut def = routine("sweep", "0 9 * * *");
    def.agent = AgentRef::from("triage");
    save(&home, def);
    // Another routine waits for it to finish.
    let mut follow = routine("follow", "0 9 * * *");
    follow.triggers = vec![super::model::RoutineTrigger::Event(
        super::model::EventSpec {
            kind: "agent_done".into(),
            from_agent: Some(AgentRef::from("acme/api/backend/triage")),
            role: None,
            metadata: None,
        },
    )];
    save(&home, follow);

    let refused =
        crate::agents::service::delete(&home, &alice(), &agent_key, None, &RoutineReferrers)
            .unwrap_err();
    assert_eq!(
        refused,
        crate::agents::AgentError::InUse {
            referrers: vec![
                "acme/api/backend/follow".to_string(),
                "acme/api/backend/sweep".to_string()
            ],
            owned: vec!["acme/api/backend/sweep".to_string()],
        }
    );
    assert_eq!(
        crate::agents::service::used_by(&home, &alice(), &agent_key, &RoutineReferrers).unwrap(),
        ["acme/api/backend/follow", "acme/api/backend/sweep"]
    );
    // Archiving the routines frees it.
    for id in ["sweep", "follow"] {
        service::archive(&home, &alice(), &key(id), &Author::user("alice")).unwrap();
    }
    crate::agents::service::delete(&home, &alice(), &agent_key, None, &RoutineReferrers).unwrap();
}

/// A teammate's personal routine naming a team agent is invisible to whoever
/// deletes that agent. It is stranded, not hidden: its list row says the agent
/// does not resolve and it is Unassigned, so nothing fires it (SW-AGR-1).
#[tokio::test]
async fn a_stranded_personal_routine_fails_each_run_with_agent_unavailable() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let (agent_key, _) = codex_triage(&home);
    // Bob's clone of the team, with the agent his git pull brought.
    write_org(&home, 2);
    let alice_agents = crate::agents::roots::team_agents_dir(&home, &alice(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join("triage");
    let bob_agents = crate::agents::roots::team_agents_dir(&home, &bob(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join("triage");
    super::test_support::copy_dir(&alice_agents, &bob_agents);
    let bobs = service::save(
        &home,
        &bob(),
        SaveTarget::Create(Placement::Personal(PersonalOwner {
            github_user_id: 2,
            username: "bob".into(),
            display_name: "Bob".into(),
            avatar_url: None,
            org: "acme/api".into(),
        })),
        crate::routines::model::RoutineDef {
            agent: AgentRef::from(agent_key.to_string().as_str()),
            ..routine("bobs-sweep", "0 9 * * *")
        },
        &Author::user("bob"),
    )
    .unwrap()
    .key;
    assert!(bobs.is_personal());
    assert_eq!(bobs.org, "acme/api");
    let row = |who: &Principal, key: &RoutineKey| {
        service::list_summaries(&home, who)
            .unwrap()
            .into_iter()
            .find(|r| r.key == *key)
            .unwrap()
    };
    assert!(row(&bob(), &bobs).agent_display.resolvable);
    assert_eq!(row(&bob(), &bobs).agent_display.name, "Agent triage");

    // Alice sees no referrer and deletes it; the delete reaches Bob's clone.
    assert!(
        crate::agents::service::used_by(&home, &alice(), &agent_key, &RoutineReferrers)
            .unwrap()
            .is_empty()
    );
    crate::agents::service::delete(&home, &alice(), &agent_key, None, &RoutineReferrers).unwrap();
    std::fs::remove_dir_all(&bob_agents).unwrap();

    let display = row(&bob(), &bobs).agent_display;
    assert!(!display.resolvable);
    assert_eq!(display.name, "acme/api/backend/triage");
    // SW-AGR-1: it is Unassigned and never fires, by hand or on schedule.
    let stranded = service::list_summaries(&home, &bob())
        .unwrap()
        .into_iter()
        .find(|r| r.key == bobs)
        .unwrap();
    assert!(stranded.unassigned);
    assert_eq!(stranded.next_run_at, None);
    assert!(matches!(
        service::run_now(&home, &bob(), &bobs, BTreeMap::new()),
        Err(super::RoutineError::Unassigned(_))
    ));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 0);
    assert_eq!(launcher.calls(), 0);
}

/// A save is checked for where its agent may live, and the task's leading
/// command is checked against the agent, warning only.
#[test]
fn a_save_checks_where_its_agent_lives_and_warns_about_a_missing_command() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let personal_agent =
        super::test_support::save_personal_agent(&home, &alice(), agent("mine", AgentCli::Claude));
    let team = Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let refused = service::save(
        &home,
        &alice(),
        SaveTarget::Create(team.clone()),
        crate::routines::model::RoutineDef {
            agent: AgentRef::from(personal_agent.to_string().as_str()),
            ..routine("personal-runner", "0 9 * * *")
        },
        &Author::user("alice"),
    )
    .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("a team routine needs a team agent; any teammate may run it"),
        "{refused}"
    );

    let saved = service::save(
        &home,
        &alice(),
        SaveTarget::Create(team),
        crate::routines::model::RoutineDef {
            task: "/triage-slack today".into(),
            ..routine("slack", "0 9 * * *")
        },
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(
        saved
            .warnings
            .iter()
            .map(|w| w.message.as_str())
            .collect::<Vec<_>>(),
        ["this agent doesn't start with /triage-slack — add it to the agent or the run won't find it"]
    );
}

#[test]
fn an_older_coordinator_file_gains_the_runs_agent_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coordinator.sqlite3");
    RoutineStateStore::open_at(&path).unwrap();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE routine_result_runs; DROP TABLE routine_runs;
             CREATE TABLE routine_runs (
                 run_id TEXT PRIMARY KEY, routine_key TEXT NOT NULL, revision INTEGER NOT NULL,
                 source TEXT NOT NULL, occurrence_key TEXT UNIQUE, phase TEXT NOT NULL,
                 reason TEXT, linked_run_id TEXT, session_id TEXT, created_at INTEGER NOT NULL,
                 started_at INTEGER, ended_at INTEGER, result_text TEXT, request TEXT,
                 max_runtime_ms INTEGER, causation TEXT);
             INSERT INTO routine_runs (run_id, routine_key, revision, source, phase, created_at)
                 VALUES ('before', 'acme/api/backend/sweep', 1, 'manual', 'succeeded', 1);",
        )
        .unwrap();
    }
    crate::coordinator::SqliteOrchestrationStore::open(&path).unwrap();
    let store = RoutineStateStore::open_at(&path).unwrap();
    let before = store.run("before").unwrap().unwrap();
    assert_eq!(before.agent, None);
    assert_eq!(before.detail, None, "a run from before the column reads no detail");
    assert_eq!(before.phase, "succeeded");
}

/// A personal routine may run a team agent; on a Mac without that team the
/// agent does not resolve, which the save warns about, saying why, and the
/// routine is Unassigned there, so it never runs (SW-AGR-1). Copied into a team, it runs that team's agent of the
/// same id; when the team has none, the copy still saves, with the warning.
#[tokio::test]
async fn a_team_agent_this_mac_lacks_is_explained_and_a_copy_names_the_same_id() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mine = service::save(
        &home,
        &alice(),
        SaveTarget::Create(Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        })),
        crate::routines::model::RoutineDef {
            agent: AgentRef::from("acme/api/frontend/triage"),
            ..routine("sweep", "0 9 * * *")
        },
        &Author::user("alice"),
    )
    .unwrap();
    let warning = &mine.warnings[0];
    assert_eq!(warning.code, LaunchWarningCode::AgentUnavailable);
    assert!(
        warning
            .message
            .contains("this agent lives in team acme/api/frontend, which this Mac may not have"),
        "{}",
        warning.message
    );

    // SW-AGR-1: with no such agent here it is Unassigned and never runs.
    assert!(matches!(
        service::run_now(&home, &alice(), &mine.key, BTreeMap::new()),
        Err(super::RoutineError::Unassigned(_))
    ));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 0);
    assert_eq!(launcher.calls(), 0);

    let copy = service::copy_to_mine(
        &home,
        &alice(),
        &mine.key,
        Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(copy.def.agent.as_str(), "triage");
    assert_eq!(
        copy.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
        [LaunchWarningCode::AgentUnavailable]
    );
}

/// A team routine may run a starter, which every Mac has: it launches the
/// built-in Claude with the runtime's defaults and the team's CLAUDE.md, and
/// records the starter's fixed revision. The starter counts the routine as a
/// user and can never be deleted out from under it.
#[tokio::test]
async fn a_team_routine_runs_the_builtin_claude_and_records_its_revision() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let content = crate::context_repo::path_to_team_content_dir(
        &crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap(),
        TEAM,
    );
    std::fs::write(content.join("CLAUDE.md"), "# Team rules\n").unwrap();
    let starter = crate::agents::builtin::key(AgentCli::Claude);
    let mut def = routine("digest", "0 9 * * *");
    def.agent = AgentRef::from("builtin/claude");
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    let agent = &launched.request.agent;
    assert_eq!(agent.runtime, AgentCli::Claude);
    assert_eq!(agent.launch.binary, "claude");
    assert_eq!(agent.launch.mcp_allow, Some(Vec::new()));
    let stamp = format!("builtin/claude@{}", crate::agents::builtin::revision());
    assert_eq!(agent.revision_label(), stamp);
    assert_eq!(
        launched.request.provenance.agent.as_deref(),
        Some(stamp.as_str())
    );
    assert_eq!(run(&home, &run_id).agent.as_deref(), Some(stamp.as_str()));
    let resolved = crate::agents::resolve_agent(
        &home,
        &alice(),
        &starter,
        crate::agents::ResolveOptions {
            target_team: Some((ORG.into(), TEAM.into())),
        },
    )
    .unwrap();
    // The run's manifest is the starter's against the routine's team, its
    // permission entries regenerated for the headless run.
    assert_eq!(resolved.agent, agent.agent);
    assert_eq!(resolved.manifest.entries[0].path.as_str(), "CLAUDE.md");
    assert!(agent.warnings.is_empty(), "{:?}", agent.warnings);

    assert_eq!(
        crate::agents::service::used_by(&home, &alice(), &starter, &RoutineReferrers).unwrap(),
        ["acme/api/backend/digest"]
    );
    assert_eq!(
        crate::agents::service::delete(&home, &alice(), &starter, None, &RoutineReferrers),
        Err(crate::agents::AgentError::ReadOnly(starter.clone()))
    );
}
