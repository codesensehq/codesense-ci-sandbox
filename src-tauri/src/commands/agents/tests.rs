// SPDX-License-Identifier: MIT
//! The Agents tab commands' bodies over a temp home, through the real agent
//! service, resolver and bindings, the real routine service (for "used by")
//! and a real launch registry (for "running now").

use std::path::Path;

use super::*;
use crate::agents::events::recorder;
use crate::agents::resolve::tests::ready_account;
use crate::commands::dashboard_launch::DashboardLaunchState;
use crate::domain::Principal;
use crate::install::agent_account_store::AgentProvider;
use crate::routines::model::AgentRef;
use crate::routines::service::SaveTarget as RoutineSaveTarget;
use crate::routines::test_support::{
    agent, alice, copy_dir, save_team_agent, write_org, TempHome, ORG, TEAM,
};
use crate::routines::{Placement as RoutinePlacement, RoutineDef, RoutineTrigger, ScheduleSpec};
use codesense_pty_core::agent_cli::AgentCli;

fn caller_for(home: &Path, user_id: u64) -> Caller {
    Caller {
        home: home.to_path_buf(),
        principal: Principal::from(user_id),
    }
}

fn owner() -> PersonalOwner {
    PersonalOwner {
        github_user_id: 1,
        username: "alice".into(),
        display_name: "Alice".into(),
        avatar_url: None,
        org: "acme/api".into(),
    }
}

fn by(name: &str) -> Author {
    Author::user(name)
}

/// Alice's team routine `id`, run by the agent `agent_id`.
fn routine_using(home: &Path, id: &str, agent_id: &str) {
    let def = RoutineDef {
        action: Default::default(),
        command: Vec::new(),
        after: None,
        id: id.into(),
        name: format!("Routine {id}"),
        owner: String::new(),
        agent: AgentRef::from(agent_id),
        mode: String::new(),
        task: "Summarize what changed.".into(),
        target: crate::routines::Where::team_workspace(),
        triggers: vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * *".into(),
            tz: "UTC".into(),
        })],
        policy: crate::routines::Policy {
            max_runtime: Some("30m".into()),
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
    };
    crate::routines::service::save(
        home,
        &alice(),
        RoutineSaveTarget::Create(RoutinePlacement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        def,
        &by("alice"),
    )
    .unwrap();
}

fn row<'a>(rows: &'a [AgentRow], key: &str) -> &'a AgentRow {
    rows.iter().find(|r| r.key == key).unwrap()
}

/// The list shows each agent with the routines using it (from the real
/// routine service) and its running sessions (from the launch registry's
/// agent stamps); detail resolves it for this user; archive, unarchive, copy
/// and the binding round-trip through the service and each emits
/// `agents:changed`.
#[test]
fn every_command_round_trips_through_the_service() {
    recorder::record();
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-triage", AgentCli::Claude)).to_string();
    routine_using(&home, "tab-sweep", "tab-triage");
    routine_using(&home, "tab-digest", "tab-triage");

    // Two live sessions run it, one at an older revision; a third runs another agent.
    let registry = DashboardLaunchState::new(4);
    for (handle, stamp) in [
        ("h1", format!("{key}@1")),
        ("h2", format!("{key}@2")),
        ("h3", format!("{ORG}/{TEAM}/default@1")),
    ] {
        registry.register(handle, &format!("s-{handle}"), None);
        registry.set_agent_revision_if_absent(handle, stamp);
    }
    let snapshot = registry.snapshot();
    let running = running_by_agent(snapshot.iter().filter_map(|s| s.agent_revision.as_deref()));

    let caller = caller_for(&home, 1);
    let rows = list(&caller, false, &running).unwrap();
    let triage = row(&rows, &key);
    assert_eq!(triage.name, "Agent tab-triage");
    assert_eq!(triage.runtime, AgentCli::Claude);
    assert_eq!(triage.scope, AgentScope::Team);
    assert_eq!(triage.created_by, "alice");
    assert!(triage.created_by_you);
    assert_eq!(
        triage.used_by,
        [
            format!("{ORG}/{TEAM}/tab-digest"),
            format!("{ORG}/{TEAM}/tab-sweep")
        ]
    );
    assert_eq!(triage.running_now, 2);
    assert_eq!(triage.account.source, "runtime_default");
    assert_eq!(row(&rows, &format!("{ORG}/{TEAM}/default")).running_now, 1);

    // Detail: definition, history and the resolved launch, with the honest flag.
    let shown = detail(&caller, &key).unwrap();
    assert_eq!(shown.definition.id, "tab-triage");
    assert_eq!(shown.revisions.len(), 1);
    assert_eq!(shown.resolved.agent.to_string(), format!("{key}@1"));
    assert!(!shown.resolved.manifest_applies_at_launch);
    assert_eq!(shown.resolved.account.source, "runtime_default");

    // Bind an account of the agent's runtime; the row and detail follow.
    let account = ready_account(&home, &alice(), AgentProvider::Claude);
    let bound = binding_set(&caller, &key, Some(&account.id), None).unwrap();
    assert_eq!(bound.account_id.as_deref(), Some(account.id.as_str()));
    assert!(!bound.capture_raw_terminal);
    let rows = list(&caller, false, &running).unwrap();
    assert_eq!(row(&rows, &key).account.source, "binding");
    assert_eq!(
        row(&rows, &key).account.bound_account_id.as_deref(),
        Some(account.id.as_str())
    );
    assert_eq!(
        detail(&caller, &key).unwrap().resolved.account.source,
        "binding"
    );
    assert!(binding_clear(&caller, &key).unwrap());
    assert!(!binding_clear(&caller, &key).unwrap());

    // Its routines are listed for the archive's confirm; unconfirmed, the
    // archive is refused, naming them, and nothing changes (SW-AGR-1).
    let owned: Vec<String> = triage.routines.iter().map(|r| r.name.clone()).collect();
    assert_eq!(owned, ["Routine tab-digest", "Routine tab-sweep"]);
    let refused = archive(&caller, &key, &by("alice"), false).unwrap_err();
    assert!(
        refused.contains("Routine tab-digest, Routine tab-sweep"),
        "{refused}"
    );
    assert!(!row(&list(&caller, false, &running).unwrap(), &key).archived);
    // Confirmed, archive hides it unless asked and archives its routines.
    let archived = archive(&caller, &key, &by("alice"), true).unwrap();
    let AgentWriteReply::Saved {
        revision,
        archived: true,
        routines,
        ..
    } = archived
    else {
        panic!("archived: {archived:?}");
    };
    assert_eq!(revision, 2);
    assert_eq!(routines.len(), 2);
    assert!(list(&caller, false, &running)
        .unwrap()
        .iter()
        .all(|r| r.key != key));
    let with_archived = list(&caller, true, &running).unwrap();
    assert!(row(&with_archived, &key).archived);
    assert!(
        row(&with_archived, &key).used_by.is_empty(),
        "its routines went with it"
    );

    // A stale unarchive is an answer; the current base brings it back.
    assert_eq!(
        unarchive(&caller, &key, 1, &by("alice")).unwrap(),
        AgentWriteReply::Conflict {
            current_revision: 2
        }
    );
    // Unarchived, it offers its routines back; restoring brings both.
    let AgentWriteReply::Saved {
        revision: 3,
        archived: false,
        routines: offered,
        ..
    } = unarchive(&caller, &key, 2, &by("alice")).unwrap()
    else {
        panic!("unarchived");
    };
    assert_eq!(offered.len(), 2);
    assert_eq!(
        restore_routines(&caller, &key, &by("alice")).unwrap().len(),
        2
    );
    assert_eq!(
        row(&list(&caller, false, &running).unwrap(), &key)
            .used_by
            .len(),
        2
    );

    // Copy to mine lands in the personal space with a fresh id.
    let AgentWriteReply::Saved { key: copy, .. } =
        copy_to_mine(&caller, &key, Placement::Personal(owner()), &by("alice")).unwrap()
    else {
        panic!("copy should save");
    };
    assert_eq!(copy, "_personal/1/acme/api/_scratch/tab-triage-copy");
    let rows = list(&caller, false, &running).unwrap();
    assert_eq!(row(&rows, &copy).scope, AgentScope::Personal);
    assert!(row(&rows, &copy).used_by.is_empty());

    // create, bind, clear, archive, unarchive: one change each (the no-op clear
    // and the refused unarchive none); the copy announced its own key.
    assert_eq!(recorder::count(&key), 5);
    assert_eq!(recorder::count(&copy), 1);
}

/// Anyone can change a team agent: Alice archives, unarchives and deletes an
/// agent Bob created, which reached her checkout by a pull.
#[test]
fn a_teammate_changes_a_team_agent() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let bob = Principal::from(2u64);
    write_org(&home, 2);
    let key = save_team_agent(&home, &bob, agent("tab-bobs", AgentCli::Codex)).to_string();
    let from = crate::agents::roots::team_agents_dir(&home, &bob, ORG, TEAM)
        .unwrap()
        .unwrap();
    let to = crate::agents::roots::team_agents_dir(&home, &alice(), ORG, TEAM)
        .unwrap()
        .unwrap();
    copy_dir(&from.join("tab-bobs"), &to.join("tab-bobs"));

    let caller = caller_for(&home, 1);
    let rows = list(&caller, false, &HashMap::new()).unwrap();
    let bobs = row(&rows, &key);
    assert!(!bobs.created_by_you);
    assert_eq!(bobs.runtime, AgentCli::Codex);
    assert!(!detail(&caller, &key).unwrap().created_by_you);

    assert!(matches!(
        archive(&caller, &key, &by("alice"), false).unwrap(),
        AgentWriteReply::Saved { archived: true, .. }
    ));
    assert!(matches!(
        unarchive(&caller, &key, 2, &by("alice")).unwrap(),
        AgentWriteReply::Saved {
            archived: false,
            ..
        }
    ));
    let revisions = detail(&caller, &key).unwrap().revisions;
    assert_eq!(revisions.last().unwrap().author, by("alice"));
    assert_eq!(
        delete(&caller, &key, 3).unwrap(),
        AgentDeleteReply::Deleted { key: key.clone() }
    );
    assert!(detail(&caller, &key).is_err());
}

/// A binding must name an account of the agent's own runtime; anything else
/// is refused and nothing is written.
#[test]
fn a_binding_to_another_runtimes_account_is_refused() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-claude", AgentCli::Claude)).to_string();
    let codex = ready_account(&home, &alice(), AgentProvider::Codex);
    let caller = caller_for(&home, 1);

    let refused = binding_set(&caller, &key, Some(&codex.id), None).unwrap_err();
    assert!(refused.contains("is a codex account"), "{refused}");
    assert!(binding_set(&caller, &key, Some("no-such-account"), None)
        .unwrap_err()
        .contains("not found"));
    let rows = list(&caller, false, &HashMap::new()).unwrap();
    assert_eq!(row(&rows, &key).account.source, "runtime_default");
    assert!(row(&rows, &key).account.bound_account_id.is_none());
}

/// A delete while routines use the agent answers with those routines and
/// removes nothing; a stale base is a conflict; once unused it goes.
#[test]
fn delete_reports_the_routines_still_using_the_agent() {
    recorder::record();
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-used", AgentCli::Claude)).to_string();
    routine_using(&home, "tab-uses", "tab-used");
    let caller = caller_for(&home, 1);

    assert_eq!(
        delete(&caller, &key, 1).unwrap(),
        AgentDeleteReply::InUse {
            referrers: vec![format!("{ORG}/{TEAM}/tab-uses")],
            owned: vec![format!("{ORG}/{TEAM}/tab-uses")],
        }
    );
    assert!(detail(&caller, &key).is_ok());
    assert_eq!(
        delete(&caller, &key, 7).unwrap(),
        AgentDeleteReply::Conflict {
            current_revision: 1
        }
    );
    // Only the create announced anything.
    assert_eq!(recorder::count(&key), 1);

    crate::routines::service::archive(
        &home,
        &alice(),
        &crate::routines::RoutineKey::parse(&format!("{ORG}/{TEAM}/tab-uses")).unwrap(),
        &by("alice"),
    )
    .unwrap();
    assert_eq!(
        delete(&caller, &key, 1).unwrap(),
        AgentDeleteReply::Deleted { key: key.clone() }
    );
    assert_eq!(recorder::count(&key), 2);
    assert!(parse_key("not a key").is_err());
}

/// A write an agent makes through its MCP tools announces itself the same way
/// a tab write does, so an open tab re-reads either way.
#[test]
fn an_mcp_write_announces_the_change_too() {
    recorder::record();
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-mcp", AgentCli::Claude)).to_string();
    let before = recorder::count(&key);
    let params: crate::mcp::agent_tools::AgentDeleteParams =
        serde_json::from_value(serde_json::json!({ "id": key })).unwrap();
    crate::mcp::agent_tools::delete(
        &home,
        &alice(),
        &crate::mcp::agent_tools::AgentCaller::new(&alice(), None),
        &RoutineReferrers,
        params,
    )
    .unwrap();
    assert_eq!(recorder::count(&key), before + 1);
    let rows = list(&caller_for(&home, 1), true, &HashMap::new()).unwrap();
    assert!(row(&rows, &key).archived);
}

/// Raw-terminal consent lives in this user's binding: the tab sets it through
/// the binding command, it survives clearing the account, and only an
/// interactive launch of the agent captures.
#[test]
fn raw_terminal_consent_reaches_interactive_agent_launches_only() {
    use crate::agents::adapter::LaunchMode;
    use crate::agents::launch::{prepare, PrepareOptions};

    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-raw", AgentCli::Claude)).to_string();
    let caller = caller_for(&home, 1);
    let parsed = parse_key(&key).unwrap();
    let launch = |mode| {
        prepare(
            &home,
            &alice(),
            &parsed,
            PrepareOptions {
                mode_override: Some(mode),
                ..Default::default()
            },
        )
        .unwrap()
        .launch
        .capture_raw_terminal
    };

    assert!(!launch(LaunchMode::Interactive), "no consent, no capture");
    assert!(
        !detail(&caller, &key)
            .unwrap()
            .resolved
            .account
            .capture_raw_terminal
    );

    let view = binding_set(&caller, &key, None, Some(true)).unwrap();
    assert!(view.capture_raw_terminal);
    assert_eq!(view.account_id, None, "consent alone binds no account");
    let shown = detail(&caller, &key).unwrap();
    assert!(shown.resolved.account.capture_raw_terminal);
    assert_eq!(shown.resolved.account.source, "runtime_default");
    assert!(shown.resolved.launch.capture_raw_terminal);
    assert!(launch(LaunchMode::Interactive));
    assert!(
        !launch(LaunchMode::Headless),
        "a headless launch never captures"
    );

    // Clearing the account keeps the consent.
    let account = ready_account(&home, &alice(), AgentProvider::Claude);
    binding_set(&caller, &key, Some(&account.id), None).unwrap();
    assert!(binding_clear(&caller, &key).unwrap());
    assert!(launch(LaunchMode::Interactive));

    // A refused change writes none of it.
    assert!(binding_set(&caller, &key, Some("no-such-account"), Some(false)).is_err());
    assert!(
        launch(LaunchMode::Interactive),
        "the consent was not withdrawn"
    );

    let view = binding_set(&caller, &key, None, Some(false)).unwrap();
    assert!(!view.capture_raw_terminal);
    assert!(!launch(LaunchMode::Interactive));
    assert!(binding_set(&caller, "acme/api/backend/nope", None, Some(true)).is_err());
}

/// A launcher can name an account for one launch: the launch runs with it while the
/// user's binding stays as it was, and a stale generation is refused.
#[test]
fn a_launch_names_its_own_account_and_leaves_the_binding_alone() {
    use crate::agents::launch::{prepare, PrepareOptions};
    use crate::agents::resolve::ChosenAccount;

    let temp = TempHome::new();
    let home = temp.codesense_home();
    let key = save_team_agent(&home, &alice(), agent("tab-chosen", AgentCli::Claude)).to_string();
    let caller = caller_for(&home, 1);
    let parsed = parse_key(&key).unwrap();
    let bound = ready_account(&home, &alice(), AgentProvider::Claude);
    let chosen = ready_account(&home, &alice(), AgentProvider::Claude);
    binding_set(&caller, &key, Some(&bound.id), Some(true)).unwrap();

    let codex = ready_account(&home, &alice(), AgentProvider::Codex);
    let launch_as = |id: &str, generation: Option<u64>| {
        prepare(
            &home,
            &alice(),
            &parsed,
            PrepareOptions {
                account: Some(ChosenAccount {
                    id: id.to_string(),
                    generation,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let current = launch_as(&chosen.id, Some(chosen.generation));
    assert_eq!(
        current.account_override.as_deref(),
        Some(chosen.id.as_str())
    );
    assert!(current.refuse_unusable_account().is_ok());
    // The spawn re-resolves exactly the account and generation the preview checked.
    let input = current.account_input();
    assert_eq!(
        input.override_account_id.as_deref(),
        Some(chosen.id.as_str())
    );
    assert_eq!(input.override_account_generation, Some(chosen.generation));
    assert!(!input.use_system_default);
    assert_eq!(
        current.account_preview.account_id.as_deref(),
        Some(chosen.id.as_str())
    );
    assert!(
        current.launch.capture_raw_terminal,
        "the user's consent still applies"
    );

    // Each unusable choice is refused, never replaced by the binding or a default login.
    let refused = |id: &str, generation: Option<u64>, reason: &str| {
        let err = launch_as(id, generation)
            .refuse_unusable_account()
            .unwrap_err();
        assert!(err.contains(reason), "{id} {generation:?}: {err}");
    };
    refused(
        &chosen.id,
        Some(chosen.generation + 1),
        "generation-mismatch",
    );
    refused(&chosen.id, None, "generation-mismatch");
    refused(&codex.id, Some(codex.generation), "provider-mismatch");
    refused("no-such-account", Some(1), "account-missing");

    // The binding plays no part in a chosen account: an unreadable bindings file leaves it
    // usable, while it still hides the consent (off).
    std::fs::write(
        crate::agents::binding::bindings_file(&home, &alice()),
        "not json",
    )
    .unwrap();
    let unbound = launch_as(&chosen.id, Some(chosen.generation));
    assert!(unbound.refuse_unusable_account().is_ok());
    assert!(!unbound.launch.capture_raw_terminal);
    std::fs::remove_file(crate::agents::binding::bindings_file(&home, &alice())).unwrap();
    binding_set(&caller, &key, Some(&bound.id), Some(true)).unwrap();

    let shown = detail(&caller, &key).unwrap().resolved.account;
    assert_eq!(shown.source, "binding");
    assert_eq!(shown.bound_account_id.as_deref(), Some(bound.id.as_str()));
}

/// The tab lists the starters with their used-by count, shows one, refuses
/// every change to it with the read-only message, and copies it to Mine.
#[test]
fn the_tab_lists_and_copies_a_starter_and_refuses_changing_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    routine_using(&home, "tab-digest", "builtin/codex");
    let caller = caller_for(&home, 1);
    let read_only = Err("built-in agent — Copy to Mine to customize".to_string());

    let rows = list(&caller, false, &Default::default()).unwrap();
    let codex = row(&rows, "builtin/codex");
    assert_eq!(codex.scope, AgentScope::Builtin);
    assert_eq!(codex.name, "Codex");
    assert_eq!(codex.created_by, "CodeSense");
    assert!(!codex.created_by_you);
    assert_eq!(codex.used_by, [format!("{ORG}/{TEAM}/tab-digest")]);
    assert_eq!(row(&rows, "builtin/claude").runtime, AgentCli::Claude);

    let shown = detail(&caller, "builtin/codex").unwrap();
    assert_eq!(shown.revisions.len(), 1);
    let revision = shown.definition.revision;
    assert_eq!(
        archive(&caller, "builtin/codex", &by("alice"), false),
        read_only
    );
    assert_eq!(
        unarchive(&caller, "builtin/codex", revision, &by("alice")),
        read_only
    );
    assert_eq!(
        delete(&caller, "builtin/codex", revision).unwrap_err(),
        read_only.clone().unwrap_err()
    );
    let edit = AgentDraft {
        name: "Mine".into(),
        runtime: Some("codex".into()),
        ..Default::default()
    };
    assert_eq!(
        save(
            &caller,
            AgentFormTarget::Update {
                key: "builtin/codex".into(),
                base_revision: revision,
            },
            edit,
            &by("alice"),
        )
        .unwrap_err(),
        read_only.clone().unwrap_err()
    );

    let copy = copy_to_mine(
        &caller,
        "builtin/codex",
        Placement::Personal(owner()),
        &by("alice"),
    )
    .unwrap();
    assert_eq!(
        copy,
        AgentWriteReply::Saved {
            key: "_personal/1/acme/api/_scratch/codex-copy".into(),
            revision: 1,
            archived: false,
            routines: Vec::new(),
        }
    );
}

/// The native editor's draft is the same whole form for preview and Save, including
/// a stored bridge restriction when the runtime is changed in the editor.
#[test]
fn editor_preview_and_save_validate_the_same_bridge_policy() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    for (id, tools) in [
        ("tab-bridge-none", vec![]),
        ("tab-bridge-who", vec!["who_am_i".to_string()]),
    ] {
        let mut definition = agent(id, AgentCli::Codex);
        definition.mcp.bridge_tools = Some(tools.clone());
        let key = save_team_agent(&home, &alice(), definition).to_string();
        let placement = Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        };
        let draft_json = serde_json::json!({
            "id": id, "name": "Bridge reviewer", "runtime": "codex", "mcpBridgeTools": tools,
        });
        let shown = preview(
            &caller,
            placement.clone(),
            serde_json::from_value(draft_json.clone()).unwrap(),
            None,
            &by("alice"),
        )
        .unwrap();
        let AgentPreviewReply::Resolved { resolved, .. } = shown else {
            panic!("Codex draft should resolve")
        };
        assert_eq!(resolved.launch.bridge_tools.as_ref(), Some(&tools));
        assert!(resolved
            .warnings
            .iter()
            .any(|w| w.message.contains("signal_done")));
        let mut changed = draft_json;
        changed["runtime"] = serde_json::json!("claude");
        let shown = preview(
            &caller,
            placement,
            serde_json::from_value(changed.clone()).unwrap(),
            None,
            &by("alice"),
        )
        .unwrap();
        let saved = save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            serde_json::from_value(changed).unwrap(),
            &by("alice"),
        )
        .unwrap();
        let AgentPreviewReply::Invalid { field, message } = shown else {
            panic!("Claude preview must refuse the stored Codex policy")
        };
        assert_eq!(field.as_deref(), Some("mcpBridgeTools"));
        assert_eq!(saved, AgentSaveReply::Invalid { field, message });
        assert_eq!(
            detail(&caller, &key).unwrap().definition.mcp.bridge_tools,
            Some(tools)
        );
    }
}

/// The list row's `model` is read from the agent's args the way the tab reads them:
/// `--model v`, `--model=v` or `-m v`; an agent without one has none (the CLI default).
#[test]
fn model_arg_reads_the_three_spellings() {
    let args = |a: &[&str]| {
        let mut definition = agent("caption", AgentCli::Claude);
        definition.args = a.iter().map(|s| s.to_string()).collect();
        definition
    };
    assert_eq!(model_arg(&args(&["--model", "opus"])), Some("opus".into()));
    assert_eq!(
        model_arg(&args(&["-x", "--model=gpt-5"])),
        Some("gpt-5".into())
    );
    assert_eq!(model_arg(&args(&["-m", "sonnet"])), Some("sonnet".into()));
    assert_eq!(model_arg(&args(&["--model"])), None);
    assert_eq!(model_arg(&args(&["--model="])), None);
    assert_eq!(model_arg(&args(&["--verbose"])), None);
}

#[test]
fn hermes_inline_endpoint_model_caption_keeps_native_override_and_other_clis_unchanged() {
    let mut definition = agent("caption", AgentCli::Hermes);
    definition.args.clear();
    definition.set_startup_settings(Some(
        serde_json::from_value(serde_json::json!({"model": {"default": "qwen2.5:3b-instruct"}}))
            .unwrap(),
    ));
    assert_eq!(
        model_arg(&definition).as_deref(),
        Some("qwen2.5:3b-instruct")
    );
    definition.args = vec!["--model=first".into(), "--mo=last".into()];
    assert_eq!(model_arg(&definition).as_deref(), Some("last"));
    for alias in ["-mlast", "-Qmlast"] {
        definition.args = vec!["--model=first".into(), alias.into()];
        assert_eq!(model_arg(&definition).as_deref(), Some("last"));
    }
    assert_eq!(model_arg(&definition).as_deref(), Some("last"));
    definition.runtime = AgentCli::Claude;
    assert_eq!(model_arg(&definition).as_deref(), Some("first"));
    definition.args.clear();
    assert_eq!(model_arg(&definition), None);
}
