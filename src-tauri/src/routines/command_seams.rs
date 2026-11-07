// SPDX-License-Identifier: MIT
use super::model::{After, AfterOn, Author, OverlapPolicy, RoutineAction, RoutineDef};
use super::run_seams::{key, manual, routine, run, save, store};
use super::test_support::{alice, drain_claim_loop, stub_agent, StubLauncher, TempHome, ORG, TEAM};
use super::{finish, service};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn command(id: &str, argv: &[&str]) -> RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.action = RoutineAction::Command;
    def.command = argv.iter().map(|s| s.to_string()).collect();
    def.agent = Default::default();
    def.task.clear();
    def.mode.clear();
    def.policy.overlap = OverlapPolicy::Queue;
    def.policy.max_runtime = Some("5m".into());
    def
}

async fn wait_done(home: &Path, id: &str) -> super::runs::RoutineRun {
    let start = Instant::now();
    loop {
        let row = run(home, id);
        if !matches!(row.phase.as_str(), "running" | "queued") {
            return row;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "command did not finish: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn commands_use_admission_and_capture_without_preparing_an_agent() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let root = service::key_content_dir(&home, &alice(), &key("literal")).unwrap();
    save(&home, command("literal", &["printf", "%s", "{{itemId}}"]));
    let literal = "; touch should-not-exist | $HOME ' \" with spaces";
    let fire = service::run_now(
        &home,
        &alice(),
        &key("literal"),
        BTreeMap::from([("itemId".into(), literal.into())]),
    )
    .unwrap();
    drain_claim_loop(&home, launcher.clone()).await;
    let out = wait_done(&home, &fire.run_id).await;
    assert_eq!(out.phase, "succeeded");
    assert_eq!(out.result_text.as_deref(), Some(literal));
    assert_eq!(out.command.argv[2], literal);
    assert_eq!(out.command.exit_code, Some(0));
    assert_eq!(out.action, RoutineAction::Command);
    assert!(out.agent.is_none());
    assert_eq!(launcher.calls(), 0);
    // A command run has no launch either, and is still a task: made at its start, closed at its end.
    let usage = crate::session_usage::UsageStore::open(&home.join("usage.sqlite")).unwrap();
    let task = usage.read_task(&fire.run_id).unwrap().expect("the command run's task");
    assert_eq!(
        (task.task_type.as_str(), task.status.as_str(), task.raw_status.as_deref()),
        ("routine", "done", Some("succeeded"))
    );
    assert!(!root.join("should-not-exist").exists());
    let detail = service::get_current(&home, &alice(), &key("literal")).unwrap();
    assert!(!detail.unassigned);
    assert!(detail.agent_key.is_none());
    let dir = service::routine_dir_of(&home, &alice(), &key("literal")).unwrap();
    assert!(dir
        .to_string_lossy()
        .contains("agents/_command/routines/literal"));
    let yaml = std::fs::read_to_string(dir.join("routine.yml")).unwrap();
    assert!(!yaml.contains("agent:"));
    assert!(!yaml.contains("task:"));
    assert!(!yaml.contains("mode:"));
    assert_eq!(
        store(&home)
            .state(&key("literal"))
            .unwrap()
            .unwrap()
            .last_result_text
            .as_deref(),
        Some(literal)
    );
    assert!(home
        .join("run-artifacts")
        .join(format!("{}.log", fire.run_id))
        .exists());
    save(
        &home,
        command(
            "failure",
            &["sh", "-c", "printf bad; printf problem >&2; exit 7"],
        ),
    );
    let failed = manual(&home, "failure");
    drain_claim_loop(&home, launcher.clone()).await;
    let out = wait_done(&home, &failed.run_id).await;
    assert_eq!(out.phase, "failed");
    assert_eq!(out.reason.as_deref(), Some("exit:7"));
    assert_eq!(out.command.stderr_tail.as_deref(), Some("problem"));
    save(
        &home,
        command("unresolved", &["printf", "%s", "{{missing}}"]),
    );
    let missing = manual(&home, "unresolved");
    drain_claim_loop(&home, launcher.clone()).await;
    let out = wait_done(&home, &missing.run_id).await;
    assert_eq!(out.phase, "failed");
    // Why it did not start is CodeSense's explanation, not the command's output.
    assert_eq!(
        (out.result_text.as_deref(), out.detail.as_deref()),
        (None, Some("unresolved:missing"))
    );
    assert!(out.started_at.is_none());
}

#[test]
fn a_chained_preview_fills_trigger_detail_without_a_warning() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, command("upstream", &["printf", "ok"]));
    let mut next = command("downstream", &["printf", "[%s]", "{{trigger.detail}}"]);
    next.after = Some(After {
        routine: "upstream".into(),
        on: AfterOn::Finished,
    });
    next.triggers.clear();
    let preview = super::command::preview(&home, &alice(), &key("downstream"), &next, 0);
    assert_eq!(preview.argv, ["printf", "[%s]", ""]);
    assert!(
        preview.warnings.iter().all(|w| !w.contains("trigger.detail")),
        "{:?}",
        preview.warnings
    );
}

#[tokio::test]
async fn a_finished_chain_reads_why_its_upstream_did_not_start_from_trigger_detail() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    save(&home, command("unfilled", &["printf", "%s", "{{missing}}"]));
    let mut next = command(
        "after-unfilled",
        &["printf", "[%s][%s]", "{{trigger.result_text}}", "{{trigger.detail}}"],
    );
    next.after = Some(After {
        routine: "unfilled".into(),
        on: AfterOn::Finished,
    });
    next.triggers.clear();
    save(&home, next);
    let fire = manual(&home, "unfilled");
    drain_claim_loop(&home, launcher.clone()).await;
    wait_done(&home, &fire.run_id).await;
    let start = Instant::now();
    let chained = loop {
        drain_claim_loop(&home, launcher.clone()).await;
        if let Some(run) = store(&home).runs(&key("after-unfilled")).unwrap().into_iter().next() {
            break run;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let out = wait_done(&home, &chained.run_id).await;
    assert_eq!(out.result_text.as_deref(), Some("[][unresolved:missing]"));
}

#[tokio::test]
async fn command_stop_and_watchdog_kill_the_registered_process_and_do_not_start_cancelled_chains() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let root = service::key_content_dir(&home, &alice(), &key("slow")).unwrap();
    save(
        &home,
        command(
            "slow",
            &[
                "sh",
                "-c",
                "printf partial-output; : > \"timeout-ready-$CODESENSE_RUN_ID\"; exec sleep 30",
            ],
        ),
    );
    let mut next = command("next", &["printf", "%s", "{{trigger.result_text}}"]);
    next.after = Some(After {
        routine: "slow".into(),
        on: AfterOn::Finished,
    });
    save(&home, next);
    let stopped = manual(&home, "slow");
    drain_claim_loop(&home, launcher.clone()).await;
    assert!(service::stop_run(&home, &alice(), &stopped.run_id, launcher.as_ref()).unwrap());
    assert_eq!(run(&home, &stopped.run_id).phase, "cancelled");
    assert!(store(&home).runs(&key("next")).unwrap().is_empty());
    let timed = manual(&home, "slow");
    drain_claim_loop(&home, launcher.clone()).await;
    let waiting = Instant::now();
    while !root
        .join(format!("timeout-ready-{}", timed.run_id))
        .exists()
    {
        assert!(waiting.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = run(&home, &timed.run_id).started_at.unwrap();
    assert_eq!(
        finish::watchdog(&store(&home), started + 300_001, launcher.as_ref()).unwrap(),
        1
    );
    assert_eq!(run(&home, &timed.run_id).phase, "timed_out");
    let waiting = Instant::now();
    while store(&home).runs(&key("next")).unwrap().is_empty() {
        assert!(waiting.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        run(&home, &timed.run_id).result_text.as_deref(),
        Some("partial-output")
    );
    assert_eq!(store(&home).runs(&key("next")).unwrap().len(), 1);
    drain_claim_loop(&home, launcher.clone()).await;
    let next = store(&home).runs(&key("next")).unwrap().pop().unwrap();
    assert_eq!(
        wait_done(&home, &next.run_id).await.result_text.as_deref(),
        Some("partial-output")
    );
}

#[tokio::test]
async fn succeeded_and_finished_chains_carry_results_and_bypass_the_event_rate_limit() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    save(&home, command("feed", &["printf", "upstream"]));
    for (id, on) in [
        ("success", AfterOn::Succeeded),
        ("finished", AfterOn::Finished),
    ] {
        let mut def = command(
            id,
            &[
                "printf",
                "%s|%s|%s|%s",
                "{{trigger.result_text}}",
                "{{trigger.phase}}",
                "{{trigger.routine}}",
                "{{trigger.run_id}}",
            ],
        );
        def.after = Some(After {
            routine: "feed".into(),
            on,
        });
        def.triggers.clear();
        save(&home, def);
    }
    for _ in 0..2 {
        let fire = manual(&home, "feed");
        drain_claim_loop(&home, launcher.clone()).await;
        wait_done(&home, &fire.run_id).await;
        let start = Instant::now();
        while store(&home)
            .runs(&key("success"))
            .unwrap()
            .iter()
            .filter(|r| r.upstream.as_ref().is_some_and(|c| c.run_id == fire.run_id))
            .count()
            == 0
        {
            assert!(start.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drain_claim_loop(&home, launcher.clone()).await;
        for id in ["success", "finished"] {
            let next = store(&home)
                .runs(&key(id))
                .unwrap()
                .into_iter()
                .find(|r| r.upstream.as_ref().is_some_and(|c| c.run_id == fire.run_id))
                .unwrap();
            let out = wait_done(&home, &next.run_id).await;
            assert_eq!(out.source, "event");
            assert_eq!(out.phase, "succeeded");
            assert_eq!(
                out.result_text,
                Some(format!("upstream|succeeded|feed|{}", fire.run_id))
            );
            assert!(out.causation.unwrap().contains(&key("feed").to_string()));
        }
    }
    assert_eq!(store(&home).runs(&key("success")).unwrap().len(), 2);
}

#[test]
fn command_validation_reuses_confinement_and_chain_validation_names_cycles() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let root = service::key_content_dir(&home, &alice(), &key("path")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(root.join("scripts/ok.sh"), "#!/bin/sh\nprintf ok").unwrap();
    let save_def = |def| {
        service::save(
            &home,
            &alice(),
            service::SaveTarget::Create(service::Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            def,
            &Author::user("alice"),
        )
    };
    for (id, program) in [
        ("absolute", "/bin/sh"),
        ("escape", "../outside"),
        ("missing", "scripts/missing"),
        ("not-executable", "scripts/ok.sh"),
        ("empty", ""),
    ] {
        assert!(
            save_def(command(id, &[program])).is_err(),
            "accepted {program}"
        );
    }
    use std::os::unix::fs::{symlink, PermissionsExt};
    std::fs::set_permissions(
        root.join("scripts/ok.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(save_def(command("path", &["scripts/ok.sh"])).is_ok());
    symlink("/bin/sh", root.join("scripts/link")).unwrap();
    assert!(save_def(command("link", &["scripts/link"])).is_err());
    for (id, field) in [
        ("has-agent", "agent"),
        ("has-task", "task"),
        ("wake-command", "mode"),
        ("armed-command", "arm"),
    ] {
        let mut def = command(id, &["printf", "ok"]);
        match field {
            "agent" => def.agent = "default".into(),
            "task" => def.task = "task".into(),
            "mode" => def.mode = "wake".into(),
            _ => def.armed_machine = Some("remote".into()),
        }
        assert!(save_def(def).is_err());
    }
    let personal = super::model::RoutineKey::personal(&alice(), ORG, "personal");
    assert!(command("personal", &["printf"])
        .validate_agents_for(&personal)
        .unwrap_err()
        .to_string()
        .contains("team"));
    save(&home, command("a", &["printf", "a"]));
    let mut b = command("b", &["printf", "b"]);
    b.after = Some(After {
        routine: "a".into(),
        on: AfterOn::Succeeded,
    });
    save(&home, b);
    let mut a = service::get_current(&home, &alice(), &key("a"))
        .unwrap()
        .def;
    a.after = Some(After {
        routine: "b".into(),
        on: AfterOn::Finished,
    });
    let error = service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("a"),
            base_revision: 1,
        },
        a,
        &Author::user("alice"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Routine a → Routine b → Routine a"),
        "{error}"
    );
    let mut self_cycle = command("self", &["printf", "self"]);
    self_cycle.after = Some(After {
        routine: "self".into(),
        on: AfterOn::Succeeded,
    });
    assert!(save_def(self_cycle)
        .unwrap_err()
        .to_string()
        .contains("loop"));
    let mut missing = command("missing-chain", &["printf", "missing"]);
    missing.after = Some(After {
        routine: "gone".into(),
        on: AfterOn::Succeeded,
    });
    assert!(save_def(missing)
        .unwrap_err()
        .to_string()
        .contains("no longer exists"));
}

#[tokio::test]
async fn queued_commands_keep_their_own_inputs_and_capture_times_out_at_the_admitted_limit() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    save(&home, command("queue", &["printf", "%s", "{{itemId}}"]));
    let mut fires = Vec::new();
    for item in ["first", "second"] {
        fires.push(
            service::run_now(
                &home,
                &alice(),
                &key("queue"),
                BTreeMap::from([("itemId".into(), item.into())]),
            )
            .unwrap(),
        );
    }
    assert_eq!(fires[1].outcome, super::admit::Outcome::Held);
    for (fire, expected) in fires.iter().zip(["first", "second"]) {
        drain_claim_loop(&home, launcher.clone()).await;
        let out = wait_done(&home, &fire.run_id).await;
        assert_eq!(out.result_text.as_deref(), Some(expected));
    }
    save(&home, command("timeout", &["sleep", "30"]));
    let fire = manual(&home, "timeout");
    // A short admitted bound exercises the real capture timeout without a minute-long test.
    store(&home)
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET max_runtime_ms = 100 WHERE run_id = ?1",
                [&fire.run_id],
            )?;
            Ok(())
        })
        .unwrap();
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(wait_done(&home, &fire.run_id).await.phase, "timed_out");
}

#[tokio::test]
async fn agent_endings_chain_only_for_the_declared_ending() {
    use crate::dashboard_stream::SessionStatus;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    save(&home, routine("agent", "0 9 * * *"));
    for (id, on) in [
        ("success", AfterOn::Succeeded),
        ("finished", AfterOn::Finished),
    ] {
        let mut def = command(id, &["printf", "ok"]);
        def.after = Some(After {
            routine: "agent".into(),
            on,
        });
        def.triggers.clear();
        save(&home, def);
    }
    for (ending, phase) in [
        (
            finish::Ending::Signal {
                status: SessionStatus::Success,
                summary: Some("done".into()),
            },
            "succeeded",
        ),
        (
            finish::Ending::Signal {
                status: SessionStatus::NeedsAttention,
                summary: Some("question".into()),
            },
            "needs_you",
        ),
        (
            finish::Ending::Exited {
                error: Some("failed".into()),
                signal_only: false,
            },
            "failed",
        ),
        (finish::Ending::ENDED, "ended"),
    ] {
        let fire = manual(&home, "agent");
        drain_claim_loop(&home, launcher.clone()).await;
        finish::finish(&store(&home), &fire.run_id, ending, finish::now_ms())
            .unwrap()
            .unwrap()
            .announce();
        launcher.kill_child(&launcher.last().session_id);
        launcher
            .registry
            .release_for_session(&launcher.last().session_id);
        let finished = store(&home)
            .runs(&key("finished"))
            .unwrap()
            .into_iter()
            .find(|r| r.upstream.as_ref().is_some_and(|c| c.run_id == fire.run_id))
            .unwrap();
        assert_eq!(finished.upstream.as_ref().unwrap().phase, phase);
        let successes = store(&home)
            .runs(&key("success"))
            .unwrap()
            .into_iter()
            .filter(|r| r.upstream.as_ref().is_some_and(|c| c.run_id == fire.run_id))
            .count();
        assert_eq!(successes, usize::from(phase == "succeeded"));
        drain_claim_loop(&home, launcher.clone()).await;
        wait_done(&home, &finished.run_id).await;
    }
}

#[test]
fn saving_and_rereading_an_agent_definition_adds_no_action_fields_or_byte_changes() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("unchanged", "0 9 * * *"));
    let dir = service::routine_dir_of(&home, &alice(), &key("unchanged")).unwrap();
    let before = std::fs::read(dir.join("routine.yml")).unwrap();
    let def = super::defs::read_def(&dir).unwrap().unwrap();
    super::defs::write_current(&dir, &def).unwrap();
    assert_eq!(std::fs::read(dir.join("routine.yml")).unwrap(), before);
    let text = String::from_utf8(before).unwrap();
    for key in ["action:", "command:", "after:"] {
        assert!(!text.contains(key));
    }
}

#[tokio::test]
async fn command_evidence_is_bounded_and_secret_redacted() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    std::env::set_var("CS_COMMAND_CAPTURE_TEST_SECRET", "hunter2secret");
    save(
        &home,
        command(
            "tail",
            &[
                "sh",
                "-c",
                r#"i=0; while [ $i -lt 5000 ]; do printf x; i=$((i+1)); done; printf ' token: %s' "$CS_COMMAND_CAPTURE_TEST_SECRET"; printf 'token: %s' "$CS_COMMAND_CAPTURE_TEST_SECRET" >&2"#,
            ],
        ),
    );
    let fire = manual(&home, "tail");
    drain_claim_loop(&home, launcher.clone()).await;
    let out = wait_done(&home, &fire.run_id).await;
    std::env::remove_var("CS_COMMAND_CAPTURE_TEST_SECRET");
    assert_eq!(out.phase, "succeeded");
    let stdout = out.result_text.unwrap();
    assert!(stdout.len() <= super::command::OUTPUT_BYTES);
    assert!(!stdout.contains("hunter2secret"));
    assert!(stdout.ends_with("[redacted]"));
    assert_eq!(
        out.command.stderr_tail.as_deref(),
        Some("token: [redacted]")
    );
    let artifact = std::fs::read_to_string(
        home.join("run-artifacts")
            .join(format!("{}.log", fire.run_id)),
    )
    .unwrap();
    assert!(!artifact.contains("hunter2secret"));
}

#[test]
fn mcp_creates_previews_and_switches_actions_through_the_shared_service() {
    use crate::mcp::routine_tools::{
        self, CallerSession, RoutineCreateParams, RoutineUpdateParams, ToolEnvelope,
    };
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let input = serde_json::json!({"id":"mcp-command", "name":"MCP command", "action":"command", "command":["printf", "{{date}}"], "when":"every 5m", "dryRun":true});
    let create = |value| {
        let params: RoutineCreateParams = serde_json::from_value(value).unwrap();
        routine_tools::create(
            &home,
            &alice(),
            service::Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            },
            &CallerSession::default(),
            &Author::user("alice"),
            params,
            chrono::Utc::now(),
        )
        .unwrap()
    };
    let ToolEnvelope::Ok(preview) = create(input.clone()) else {
        panic!("preview refused")
    };
    assert_eq!(preview.action, RoutineAction::Command);
    chrono::NaiveDate::parse_from_str(&preview.argv.unwrap()[1], "%Y-%m-%d").unwrap();
    assert!(service::get_current(&home, &alice(), &key("mcp-command")).is_err());
    let mut input = input;
    input["dryRun"] = false.into();
    let ToolEnvelope::Ok(created) = create(input) else {
        panic!("create refused")
    };
    let def = service::get_current(&home, &alice(), &key("mcp-command"))
        .unwrap()
        .def;
    assert_eq!(def.policy.max_runtime.as_deref(), Some("5m"));
    assert_eq!(def.policy.overlap, OverlapPolicy::Queue);
    assert!(def.agent.is_empty());
    let update = |value| {
        let params: RoutineUpdateParams = serde_json::from_value(value).unwrap();
        routine_tools::update(
            &home,
            &alice(),
            &Author::user("alice"),
            params,
            chrono::Utc::now(),
            &|_| None,
        )
        .unwrap()
    };
    let ToolEnvelope::Ok(agent) = update(
        serde_json::json!({"id":created.id, "baseRevision":1, "action":"agent", "agent":"default", "task":"Do the task", "mode":"fresh"}),
    ) else {
        panic!("switch to agent refused")
    };
    let def = service::get_current(&home, &alice(), &key("mcp-command"))
        .unwrap()
        .def;
    assert!(def.command.is_empty());
    assert!(!def.agent.is_empty());
    let ToolEnvelope::Ok(_) = update(
        serde_json::json!({"id":agent.id, "baseRevision":2, "action":"command", "command":["printf", "ok"]}),
    ) else {
        panic!("switch to command refused")
    };
    let def = service::get_current(&home, &alice(), &key("mcp-command"))
        .unwrap()
        .def;
    assert!(def.agent.is_empty());
    assert!(def.task.is_empty());
    assert!(def.mode.is_empty());
}

#[tokio::test]
async fn a_cancelled_command_keeps_output_captured_before_stop() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let root = service::key_content_dir(&home, &alice(), &key("stop-output")).unwrap();
    save(
        &home,
        command(
            "stop-output",
            &["sh", "-c", "printf ready; : > command-ready; exec sleep 30"],
        ),
    );
    let fire = manual(&home, "stop-output");
    drain_claim_loop(&home, launcher.clone()).await;
    let start = Instant::now();
    while !root.join("command-ready").exists() {
        assert!(start.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(service::stop_run(&home, &alice(), &fire.run_id, launcher.as_ref()).unwrap());
    while run(&home, &fire.run_id).command.duration_ms.is_none() {
        assert!(start.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let out = run(&home, &fire.run_id);
    assert_eq!(out.phase, "cancelled");
    assert_eq!(out.result_text.as_deref(), Some("ready"));
}

#[tokio::test]
async fn refused_command_conversion_leaves_the_agent_routine_in_place_and_fireable() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    save(&home, routine("unchanged-agent", "0 9 * * *"));
    let original = service::routine_dir_of(&home, &alice(), &key("unchanged-agent")).unwrap();
    let bytes = std::fs::read(original.join("routine.yml")).unwrap();
    let error = service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("unchanged-agent"),
            base_revision: 1,
        },
        command("unchanged-agent", &["scripts/missing"]),
        &Author::user("alice"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("isn't in"));
    assert_eq!(
        service::routine_dir_of(&home, &alice(), &key("unchanged-agent")).unwrap(),
        original
    );
    assert_eq!(std::fs::read(original.join("routine.yml")).unwrap(), bytes);
    assert!(
        !service::get_current(&home, &alice(), &key("unchanged-agent"))
            .unwrap()
            .unassigned
    );
    let fire = manual(&home, "unchanged-agent");
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(launcher.calls(), 1);
    assert_eq!(run(&home, &fire.run_id).phase, "running");
    service::stop_run(&home, &alice(), &fire.run_id, launcher.as_ref()).unwrap();
}

#[tokio::test]
async fn history_restores_action_fields_and_home_in_both_directions() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("restore-action", "0 9 * * *"));
    let original = service::get_current(&home, &alice(), &key("restore-action"))
        .unwrap()
        .def;
    let agent_home = service::routine_dir_of(&home, &alice(), &key("restore-action")).unwrap();
    service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: key("restore-action"),
            base_revision: 1,
        },
        command("restore-action", &["printf", "restored"]),
        &Author::user("alice"),
    )
    .unwrap();
    let command_home = service::routine_dir_of(&home, &alice(), &key("restore-action")).unwrap();
    let restored = service::restore(
        &home,
        &alice(),
        &key("restore-action"),
        1,
        2,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(restored.def.action, RoutineAction::Agent);
    assert_eq!(restored.def.agent, original.agent);
    assert_eq!(restored.def.task, original.task);
    assert_eq!(restored.dir, agent_home);
    let restored = service::restore(
        &home,
        &alice(),
        &key("restore-action"),
        2,
        3,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(restored.def.action, RoutineAction::Command);
    assert!(restored.def.agent.is_empty());
    assert!(restored.def.task.is_empty());
    assert!(restored.def.mode.is_empty());
    assert_eq!(restored.dir, command_home);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let fire = manual(&home, "restore-action");
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(
        wait_done(&home, &fire.run_id).await.result_text.as_deref(),
        Some("restored")
    );
}

#[tokio::test]
async fn commands_with_exited_leaders_still_timeout_cancel_and_chain() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let root = service::key_content_dir(&home, &alice(), &key("background")).unwrap();
    save(
        &home,
        command(
            "background",
            &[
                "sh",
                "-c",
                "sleep 300 & printf background; : > \"background-$CODESENSE_RUN_ID\"; exit 0",
            ],
        ),
    );
    let mut next = command("after-background", &["printf", "%s", "{{trigger.phase}}"]);
    next.after = Some(After {
        routine: "background".into(),
        on: AfterOn::Finished,
    });
    save(&home, next);
    let fire = manual(&home, "background");
    store(&home)
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET max_runtime_ms = 1000 WHERE run_id = ?1",
                [&fire.run_id],
            )?;
            Ok(())
        })
        .unwrap();
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(wait_done(&home, &fire.run_id).await.phase, "timed_out");
    let start = Instant::now();
    while store(&home)
        .runs(&key("after-background"))
        .unwrap()
        .is_empty()
    {
        assert!(start.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let next_run = store(&home)
        .runs(&key("after-background"))
        .unwrap()
        .pop()
        .unwrap();
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(
        wait_done(&home, &next_run.run_id)
            .await
            .result_text
            .as_deref(),
        Some("timed_out")
    );
    let fire = manual(&home, "background");
    drain_claim_loop(&home, launcher.clone()).await;
    let start = Instant::now();
    while !root.join(format!("background-{}", fire.run_id)).exists() {
        assert!(start.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    service::stop_run(&home, &alice(), &fire.run_id, launcher.as_ref()).unwrap();
    while run(&home, &fire.run_id).command.duration_ms.is_none() {
        assert!(start.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(run(&home, &fire.run_id).phase, "cancelled");
    assert_eq!(
        store(&home).runs(&key("after-background")).unwrap().len(),
        1
    );
}

#[test]
fn an_after_trigger_refuses_a_machine_armed_upstream() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut upstream = routine("remote-upstream", "0 9 * * *");
    upstream.armed_machine = Some("remote".into());
    upstream.target.machine = Some("remote".into());
    upstream.arm_epoch = 1;
    save(&home, upstream);
    let mut next = command("after-remote", &["printf", "next"]);
    next.after = Some(After {
        routine: "remote-upstream".into(),
        on: AfterOn::Finished,
    });
    let error = service::save(
        &home,
        &alice(),
        service::SaveTarget::Create(service::Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        next,
        &Author::user("alice"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("remote-upstream is armed on a machine"),
        "{error}"
    );
}

#[tokio::test]
async fn command_argv_is_redacted_from_its_first_admission_through_preparation_failure() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(
        &home,
        command(
            "private-argv",
            &["printf", "%s", "token: hunter2secret", "{{missing}}"],
        ),
    );
    let first = manual(&home, "private-argv");
    let held = manual(&home, "private-argv");
    assert_eq!(held.outcome, super::admit::Outcome::Held);
    for id in [&first.run_id, &held.run_id] {
        let evidence = run(&home, id).command.argv;
        assert_eq!(evidence[2], "token: [redacted]");
        assert!(!serde_json::to_string(&evidence)
            .unwrap()
            .contains("hunter2secret"));
    }
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    drain_claim_loop(&home, launcher).await;
    let failed = wait_done(&home, &first.run_id).await;
    assert_eq!(failed.phase, "failed");
    assert!(failed.started_at.is_none());
    assert_eq!(failed.command.argv[2], "token: [redacted]");
}

#[test]
fn remote_arming_refuses_and_names_existing_completion_dependents() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("upstream-to-arm", "0 9 * * *"));
    for id in ["next-a", "next-b"] {
        let mut next = command(id, &["printf", "next"]);
        next.after = Some(After {
            routine: "upstream-to-arm".into(),
            on: AfterOn::Finished,
        });
        save(&home, next);
    }
    let dir = service::routine_dir_of(&home, &alice(), &key("upstream-to-arm")).unwrap();
    let before = std::fs::read(dir.join("routine.yml")).unwrap();
    let error = service::arm_on(
        &home,
        &alice(),
        &key("upstream-to-arm"),
        service::ArmPlacement::Machine("remote".into()),
        &Author::user("alice"),
    )
    .unwrap_err()
    .to_string();
    for dependent in ["Routine next-a (next-a)", "Routine next-b (next-b)"] {
        assert!(error.contains(dependent), "{error}");
    }
    assert!(error.contains("Remove their Run after triggers first"));
    assert_eq!(std::fs::read(dir.join("routine.yml")).unwrap(), before);
    let current = service::get_current(&home, &alice(), &key("upstream-to-arm")).unwrap();
    assert!(current.def.armed_machine().is_none());
    assert_eq!(current.def.revision, 1);
}

#[test]
fn archived_dependents_allow_remote_arming_but_cannot_be_unarchived_afterward() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let author = Author::user("alice");
    save(&home, routine("archive-upstream", "0 9 * * *"));
    let mut next = command("archived-next", &["printf", "next"]);
    next.after = Some(After {
        routine: "archive-upstream".into(),
        on: AfterOn::Finished,
    });
    save(&home, next);
    service::archive(&home, &alice(), &key("archived-next"), &author).unwrap();
    let dir = service::routine_dir_of(&home, &alice(), &key("archived-next")).unwrap();
    let archived = std::fs::read(dir.join("routine.yml")).unwrap();
    let armed = service::arm_on(
        &home,
        &alice(),
        &key("archive-upstream"),
        service::ArmPlacement::Machine("remote".into()),
        &author,
    )
    .unwrap();
    assert_eq!(armed.def.armed_machine(), Some("remote"));
    let error = service::unarchive(&home, &alice(), &key("archived-next"), &author)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("archive-upstream is armed on a machine"),
        "{error}"
    );
    assert_eq!(std::fs::read(dir.join("routine.yml")).unwrap(), archived);
    assert!(super::defs::read_def(&dir).unwrap().unwrap().is_archived());
}

#[tokio::test]
async fn a_command_run_keeps_its_git_baseline_and_flags_an_edited_test() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let root = service::key_content_dir(&home, &alice(), &key("edit-test")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    std::fs::write(root.join("tests/a_test.rs"), "original\n").unwrap();
    std::fs::write(root.join("tasks/task-types.yml"), "tamper_globs: ['tests/**']\n").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "tests/a_test.rs", "tasks/task-types.yml"],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", "baseline"]] {
        let output = std::process::Command::new("git").arg("-C").arg(&root).args(&args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    save(&home, command("edit-test", &["sh", "-c", "printf changed > tests/a_test.rs"]));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let fired = manual(&home, "edit-test");
    drain_claim_loop(&home, launcher).await;
    assert_eq!(wait_done(&home, &fired.run_id).await.phase, "succeeded");
    let usage = crate::session_usage::UsageStore::open(&home.join("usage.sqlite")).unwrap();
    // The coordinator commits the terminal phase before its separate usage-store close.
    let waiting = Instant::now();
    let task = loop {
        let task = usage.read_task(&fired.run_id).unwrap().unwrap();
        if task.status != "open" {
            assert!(task.tamper.is_some() || task.unavailable.contains_key("tamper"),
                "a closed task must publish its tamper verdict or reason atomically");
        }
        if task.tamper.is_some() { break task; }
        assert!(waiting.elapsed() < Duration::from_secs(5), "tamper capture did not finish: {:?}", task.unavailable);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(task.worktree.as_deref(), root.to_str());
    assert!(task.layers.source.is_some(), "the command starts at a real commit");
    let tamper = task.tamper.expect("known cwd and baseline permit tamper attribution");
    assert!(tamper.flag);
    assert_eq!(tamper.files, ["tests/a_test.rs"]);
}
