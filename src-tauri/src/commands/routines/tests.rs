// SPDX-License-Identifier: MIT
//! The tab commands' bodies over a temp home, through the real routine
//! service and coordinator: what the Routines tab sends, and what it reads
//! back.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::context_repo::path_to_team_config_dir;
use crate::routines::admit::Outcome;
use crate::routines::defs::routine_dir;
use crate::routines::model::AgentRef;
use crate::routines::service::SaveTarget;
use crate::routines::state::RoutineStateStore;
use crate::routines::test_support::{
    alice, changes_of, record_changes, write_org, FakeSessions, TempHome, ORG, TEAM,
};
use crate::routines::{
    AuthorKind, Policy, RoutineDef, RoutineScope, RoutineTrigger, ScheduleSpec, Where,
};

fn caller_for(home: &Path, user_id: u64) -> Caller {
    Caller {
        home: home.to_path_buf(),
        principal: Principal::from(user_id),
    }
}

fn routine(id: &str) -> RoutineDef {
    RoutineDef {
        action: Default::default(),
        command: Vec::new(),
        after: None,
        id: id.into(),
        name: format!("Routine {id}"),
        owner: String::new(),
        agent: AgentRef::from("default"),
        mode: String::new(),
        task: "Summarize what changed.".into(),
        target: Where::team_workspace(),
        triggers: vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * *".into(),
            tz: "UTC".into(),
        })],
        policy: Policy {
            max_runtime: Some("30m".into()),
            ..Policy::default()
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
}

/// Alice's team routine `id`, saved through the service as the tab's
/// create will save it.
fn team_routine(home: &Path, id: &str) -> String {
    service::save(
        home,
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        routine(id),
        &Author::agent("triage-bot"),
    )
    .unwrap()
    .key
    .to_string()
}

fn alice_owner() -> PersonalOwner {
    PersonalOwner {
        github_user_id: 1,
        username: "alice".into(),
        display_name: "Alice".into(),
        avatar_url: None,
        org: "acme/api".into(),
    }
}

fn row<'a>(rows: &'a [RoutineRow], key: &str) -> &'a RoutineRow {
    rows.iter()
        .find(|r| r.summary.key.to_string() == key)
        .unwrap_or_else(|| panic!("no row {key}: {rows:?}"))
}

#[test]
fn every_tab_command_round_trips_through_the_service() {
    record_changes();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let me = caller_for(home, 1);
    let key = team_routine(home, "tab-digest");

    let rows = list(&me).unwrap();
    let listed = row(&rows, &key);
    assert!(listed.mine);
    assert!(listed.summary.enabled);
    assert_eq!(listed.summary.scope, RoutineScope::Team);
    let created_by = listed.summary.created_by.as_ref().unwrap();
    assert_eq!(
        (created_by.kind, created_by.name.as_str()),
        (AuthorKind::Agent, "triage-bot")
    );
    let wire = serde_json::to_value(listed).unwrap();
    assert_eq!(wire["key"], key, "keys travel as their rendered string");
    assert_eq!(wire["mine"], true);
    assert!(wire["next_run_at"].is_i64(), "times are epoch ms: {wire}");

    let before = changes_of(&key);
    set_enabled(&me, &key, false).unwrap();
    assert!(!row(&list(&me).unwrap(), &key).summary.enabled);
    assert!(changes_of(&key) > before, "a pause tells the tab");
    set_enabled(&me, &key, true).unwrap();

    let admitted = run_now(&me, &key, BTreeMap::new()).unwrap();
    assert_eq!(admitted.outcome, Outcome::Queued);
    let history = runs(&me, &key, None, None).unwrap();
    assert_eq!(history.runs.len(), 1);
    assert_eq!(history.runs[0].run_id, admitted.run_id);
    assert_eq!(history.runs[0].phase, "queued");
    let detail = run_detail(&me, &admitted.run_id).unwrap();
    assert_eq!(detail.routine_name, "Routine tab-digest");

    let sessions = FakeSessions::default();
    assert!(stop_run(&me, &admitted.run_id, &sessions).unwrap());
    assert_eq!(
        *sessions.killed.lock().unwrap(),
        vec![admitted.run_id.clone()]
    );
    assert_eq!(
        runs(&me, &key, Some(10), None).unwrap().runs[0].phase,
        "cancelled"
    );
    assert!(
        !stop_run(&me, &admitted.run_id, &sessions).unwrap(),
        "an ended run is not stopped twice"
    );

    assert_eq!(needs_you_count(&me).unwrap(), 0);
    assert_eq!(scheduler_status(&me).unwrap().missed_count, 0);
    acknowledge_missed(&me).unwrap();

    let armed = arm_here(&me, &key, &Author::user("alice")).unwrap();
    assert_eq!(armed.def.revision, 1, "already armed here: unchanged");
    assert_eq!(get(&me, &key).unwrap().revisions.len(), 1);

    let copy = copy_to_mine(&me, &key, alice_owner(), &Author::user("alice")).unwrap();
    assert_eq!(
        copy.key.to_string(),
        "_personal/1/acme/api/_scratch/tab-digest-copy"
    );
    let rows = list(&me).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        row(&rows, &copy.key.to_string()).summary.scope,
        RoutineScope::Personal
    );

    archive(&me, &key, &Author::user("alice")).unwrap();
    let rows = list(&me).unwrap();
    assert!(rows.iter().all(|r| r.summary.key.to_string() != key));
    assert!(get(&me, &key).unwrap().def.is_archived());
    assert_eq!(
        runs(&me, &key, None, None).unwrap().runs.len(),
        1,
        "an archived routine's history still reads"
    );

    assert_eq!(
        get(&me, "nonsense").unwrap_err(),
        "not a routine key: \"nonsense\""
    );
}

#[test]
fn a_teammate_drives_a_team_routine_from_the_tab_like_its_creator() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let alice_org = write_org(home, 1);
    let bob_org = write_org(home, 2);
    let key = team_routine(home, "tab-triage");
    // The pull that brings Alice's team routine to Bob's clone.
    let from = routine_dir(&path_to_team_config_dir(&alice_org, TEAM), "tab-triage");
    let to = crate::routines::defs::sync_target(
        &from,
        &routine_dir(&path_to_team_config_dir(&bob_org, TEAM), "tab-triage"),
    );
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(&from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
    let alice = caller_for(home, 1);
    let bob = caller_for(home, 2);
    let asked = run_now(&alice, &key, BTreeMap::new()).unwrap().run_id;
    RoutineStateStore::open(home)
        .unwrap()
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET phase = 'needs_you', started_at = 1, ended_at = 2,
                     result_text = 'which repo?' WHERE run_id = ?1",
                [&asked],
            )?;
            Ok(())
        })
        .unwrap();

    let mine = list(&alice).unwrap();
    let mine = row(&mine, &key);
    assert!(mine.summary.needs_you);
    assert_eq!(
        mine.summary
            .needs_you_run
            .as_ref()
            .and_then(|r| r.result_text.as_deref()),
        Some("which repo?")
    );
    assert_eq!(needs_you_count(&alice).unwrap(), 1);

    // Bob is at the Mac that ran it, so its question is his too; `mine` only
    // says who created it.
    let theirs = list(&bob).unwrap();
    let theirs = row(&theirs, &key);
    assert!(!theirs.mine);
    assert!(theirs.summary.needs_you);
    assert_eq!(needs_you_count(&bob).unwrap(), 1);

    set_enabled(&bob, &key, false).unwrap();
    assert!(!row(&list(&bob).unwrap(), &key).summary.enabled);
    set_enabled(&bob, &key, true).unwrap();
    let fired = run_now(&bob, &key, BTreeMap::new()).unwrap();
    assert!(fired.is_queued(), "{fired:?}");
    assert!(stop_run(&bob, &fired.run_id, &FakeSessions::default()).unwrap());
    assert_eq!(
        arm_here(&bob, &key, &Author::user("bob"))
            .unwrap()
            .def
            .revision,
        1,
        "already armed on this Mac"
    );
    let archived = archive(&bob, &key, &Author::user("bob")).unwrap();
    assert!(archived.def.is_archived());
    assert_eq!(archived.def.owner, "1");
    assert_eq!(runs(&alice, &key, None, None).unwrap().runs.len(), 2);
}

fn sentence(name: &str, task: &str, when: &str) -> RoutineForm {
    RoutineForm {
        name: Some(name.into()),
        task: Some(task.into()),
        when: Some(when.into()),
        tz: Some("UTC".into()),
        // A personal routine runs the test team's agent by its key.
        agent: Some(TEAM_AGENT.into()),
        ..RoutineForm::default()
    }
}

const TEAM_AGENT: &str = "acme/api/backend/default";

fn personal_create(enabled: bool) -> FormTarget {
    FormTarget::Create {
        placement: Placement::Personal(alice_owner()),
        enabled,
    }
}

fn saved_key(reply: SaveReply) -> (String, u32) {
    match reply {
        SaveReply::Saved { key, revision, .. } => (key, revision),
        other => panic!("not saved: {other:?}"),
    }
}

fn crons(detail: &RoutineDetail) -> Vec<String> {
    detail
        .def
        .triggers
        .iter()
        .filter_map(|trigger| match trigger {
            RoutineTrigger::Schedule(spec) => Some(spec.cron.clone()),
            RoutineTrigger::Event(_) => None,
        })
        .collect()
}

/// The tab's whole editing loop over the real service: the sentence form
/// creates, an edit writes a revision by the user, a stale edit is a conflict
/// that writes nothing, an agent's edit is credited to the agent, and Restore
/// writes the old body as a new revision.
#[test]
fn the_sentence_form_creates_edits_conflicts_and_restores() {
    record_changes();
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let me = caller_for(&home, 1);
    let you = Author::user("alice");

    let (key, revision) = saved_key(
        save(
            &me,
            personal_create(true),
            sentence("Tab sentence echo", "echo hi", "every 5m"),
            &you,
            &|_| None,
        )
        .unwrap(),
    );
    assert_eq!(key, "_personal/1/acme/api/_scratch/tab-sentence-echo");
    assert_eq!(revision, 1);
    let created = get(&me, &key).unwrap();
    assert_eq!(created.def.task, "echo hi");
    assert_eq!(crons(&created), vec!["*/5 * * * *"]);
    assert!(created.enabled);
    assert!(
        row(&list(&me).unwrap(), &key).summary.next_run_at.is_some(),
        "an enabled create is armed"
    );
    assert!(changes_of(&key) > 0, "a create tells the tab");

    // Only the task changes: the schedule is not sent, so it stays as it is.
    let before = changes_of(&key);
    let edit = RoutineForm {
        task: Some("echo there".into()),
        ..RoutineForm::default()
    };
    let (_, revision) = saved_key(
        save(
            &me,
            FormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            edit,
            &you,
            &|_| None,
        )
        .unwrap(),
    );
    assert_eq!(revision, 2);
    assert!(changes_of(&key) > before, "an edit tells the tab");
    let edited = get(&me, &key).unwrap();
    assert_eq!(edited.def.task, "echo there");
    assert_eq!(crons(&edited), vec!["*/5 * * * *"]);

    // An edit made against revision 1 lost the race: nothing is written.
    let stale = RoutineForm {
        task: Some("echo stale".into()),
        when: Some("hourly".into()),
        ..RoutineForm::default()
    };
    assert_eq!(
        save(
            &me,
            FormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            stale,
            &you,
            &|_| None,
        )
        .unwrap(),
        SaveReply::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(get(&me, &key).unwrap().def.task, "echo there");

    // An agent edits it through its MCP tool.
    let agent = routine_tools::CallerSession {
        profile_id: Some("triage-bot".into()),
        ..routine_tools::CallerSession::default()
    };
    let reply = routine_tools::update(
        &home,
        &alice(),
        &agent.author(),
        serde_json::from_value(serde_json::json!({
            "id": key, "baseRevision": 2, "task": "echo from the agent"
        }))
        .unwrap(),
        chrono::Utc::now(),
        &|_| None,
    )
    .unwrap();
    assert!(matches!(reply, ToolEnvelope::Ok(_)), "{reply:?}");

    let (_, revision) = saved_key(restore_revision(&me, &key, 1, 3, &you).unwrap());
    assert_eq!(revision, 4);
    let restored = get(&me, &key).unwrap();
    assert_eq!(restored.def.task, "echo hi");
    let authors: Vec<_> = restored
        .revisions
        .iter()
        .map(|r| (r.revision, r.author.kind, r.author.name.as_str()))
        .collect();
    assert_eq!(
        authors,
        vec![
            (1, AuthorKind::User, "alice"),
            (2, AuthorKind::User, "alice"),
            (3, AuthorKind::Agent, "triage-bot"),
            (4, AuthorKind::User, "alice"),
        ]
    );
    assert_eq!(
        restore_revision(&me, &key, 1, 3, &you).unwrap(),
        SaveReply::Conflict {
            current_revision: 4
        },
        "a restore made against a stale revision writes nothing"
    );
    assert_eq!(get(&me, &key).unwrap().revisions.len(), 4);
}

/// A draft saves without firing; the form's policy fields land on the
/// definition; and what the form cannot save comes back as a message.
#[test]
fn a_draft_saves_its_policy_and_bad_input_is_a_message() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let me = caller_for(&home, 1);
    let you = Author::user("alice");
    let form = RoutineForm {
        overlap: Some("skip".into()),
        missed: Some("run_once".into()),
        max_runtime: Some("2h".into()),
        visible: Some(true),
        target: Some("repo:acme/api".into()),
        ..sentence("Tab draft digest", "write the digest", "weekdays 09:00")
    };
    let (key, _) = saved_key(save(&me, personal_create(false), form, &you, &|_| None).unwrap());
    let draft = get(&me, &key).unwrap();
    assert!(!draft.enabled, "enabled off saves a draft");
    assert_eq!(row(&list(&me).unwrap(), &key).summary.next_run_at, None);
    let wire = serde_json::to_value(&draft.def.policy).unwrap();
    assert_eq!(wire["overlap"], "skip");
    assert_eq!(wire["missed"], "run_once");
    assert_eq!(wire["max_runtime"], "2h");
    assert_eq!(wire["visible"], true);
    assert_eq!(draft.def.target.kind, "repo:acme/api");
    assert_eq!(crons(&draft), vec!["0 9 * * 1-5"]);

    let bad_when = save(
        &me,
        personal_create(true),
        sentence("Tab too often", "t", "every 3m"),
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert!(bad_when.contains("every 3m"), "{bad_when}");
    let missing = save(
        &me,
        personal_create(true),
        RoutineForm {
            name: Some("No task".into()),
            ..RoutineForm::default()
        },
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert_eq!(missing, "a routine needs a name and a task");
    let bad_missed = save(
        &me,
        FormTarget::Update {
            key: key.clone(),
            base_revision: 1,
        },
        RoutineForm {
            missed: Some("sometimes".into()),
            ..RoutineForm::default()
        },
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert!(bad_missed.contains("skip or run_once"), "{bad_missed}");
    assert_eq!(get(&me, &key).unwrap().def.revision, 1);
}

/// A teammate edits and restores a routine from the editor like its creator,
/// and each revision is credited to whoever wrote it.
#[test]
fn a_teammate_can_edit_and_restore_a_routine() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let alice_org = write_org(home, 1);
    let bob_org = write_org(home, 2);
    let key = team_routine(home, "tab-editor-shared");
    let from = routine_dir(
        &path_to_team_config_dir(&alice_org, TEAM),
        "tab-editor-shared",
    );
    let to = crate::routines::defs::sync_target(
        &from,
        &routine_dir(
            &path_to_team_config_dir(&bob_org, TEAM),
            "tab-editor-shared",
        ),
    );
    // The whole directory, revisions included, as a pull brings it.
    for entry in walkdir::WalkDir::new(&from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(&from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
    let bob = caller_for(home, 2);
    let edited = save(
        &bob,
        FormTarget::Update {
            key: key.clone(),
            base_revision: 1,
        },
        RoutineForm {
            task: Some("bob's".into()),
            ..RoutineForm::default()
        },
        &Author::user("bob"),
        &|_| None,
    )
    .unwrap();
    assert_eq!(saved_key(edited), (key.clone(), 2));
    let restored = restore_revision(&bob, &key, 1, 2, &Author::user("bob")).unwrap();
    assert_eq!(saved_key(restored), (key.clone(), 3));
    let detail = get(&bob, &key).unwrap();
    assert_eq!(detail.def.owner, "1");
    let authors: Vec<&str> = detail
        .revisions
        .iter()
        .map(|r| r.author.name.as_str())
        .collect();
    assert_eq!(authors, ["triage-bot", "bob", "bob"]);
}

#[test]
fn the_preview_reads_presets_and_cron_as_a_save_would() {
    let preset = preview_schedule("every 5m", Some("UTC")).unwrap();
    assert_eq!(preset.cron, "*/5 * * * *");
    assert_eq!(preset.tz, "UTC");
    assert_eq!(preset.next_fires.len(), 5);
    assert!(preset
        .next_fires
        .windows(2)
        .all(|pair| pair[1] - pair[0] == 5 * 60_000));
    let wire = serde_json::to_value(&preset).unwrap();
    assert!(wire["next_fires"][0].is_i64(), "times are epoch ms: {wire}");

    let cron = preview_schedule("0 9 * * 1-5", Some("America/Denver")).unwrap();
    assert_eq!(cron.tz, "America/Denver");
    assert!(!cron.in_words.is_empty());
    assert_eq!(cron.next_fires.len(), 5);

    assert!(preview_schedule("every 3m", Some("UTC")).is_err());
    assert!(preview_schedule("someday", None).is_err());
    assert!(preview_schedule("every 5m", Some("Mars/Base")).is_err());
}

#[test]
fn the_save_reply_keeps_a_conflict_apart_from_an_error() {
    let wire = serde_json::to_value(SaveReply::Conflict {
        current_revision: 3,
    })
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"outcome": "conflict", "current_revision": 3})
    );
    let wire = serde_json::to_value(SaveReply::Saved {
        key: "a/b/c".into(),
        revision: 2,
        warnings: Vec::new(),
    })
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"outcome": "saved", "key": "a/b/c", "revision": 2, "warnings": []})
    );
}

/// "Describe it to an agent" is the shipped team-workspace open: a visible,
/// focused agent whose first prompt is a plain request naming the routine tools.
#[test]
fn describing_a_routine_opens_a_visible_team_workspace_agent() {
    let request = authoring_request();
    assert_eq!(request.target, "team-workspace");
    assert_eq!(request.initial_prompt.as_deref(), Some(AUTHORING_PROMPT));
    assert_eq!(request.name.as_deref(), Some("Routine author"));
    let mode = crate::commands::workspace_open::resolve_open_mode(
        request.mode.as_deref(),
        request.headed,
        true,
    )
    .unwrap();
    assert!(mode.is_visible() && mode.is_focused(), "{mode:?}");
}

/// The agent slot is free text: an agent nobody here can see is refused with
/// a message naming it and agent_list, on a create and on an edit, and
/// nothing is written. A task starting with a command the agent does not
/// start with saves, with the warning.
#[test]
fn an_unknown_agent_is_refused_by_name_and_a_missing_command_only_warns() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let me = caller_for(&home, 1);
    let you = Author::user("alice");
    let refused = save(
        &me,
        personal_create(true),
        RoutineForm {
            agent: Some("no-such-agent".into()),
            ..sentence("Tab unknown agent", "t", "5m")
        },
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert_eq!(
        refused,
        "no agent \"no-such-agent\" that you can see; see agent_list"
    );
    let no_agent = save(
        &me,
        personal_create(true),
        RoutineForm {
            agent: None,
            ..sentence("Tab no agent", "t", "5m")
        },
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert_eq!(
        no_agent,
        "agent is required: a routine belongs to an agent; name one (see agent_list)"
    );
    assert!(list(&me)
        .unwrap()
        .iter()
        .all(|row| row.summary.def.name != "Tab unknown agent"));

    let (key, _) = saved_key(
        save(
            &me,
            personal_create(true),
            sentence("Tab known agent", "t", "day 09:00"),
            &you,
            &|_| None,
        )
        .unwrap(),
    );
    assert_eq!(
        crons(&get(&me, &key).unwrap()),
        vec!["0 9 * * *"],
        "the sentence's slot form reads as the preset"
    );
    let refused = save(
        &me,
        FormTarget::Update {
            key: key.clone(),
            base_revision: 1,
        },
        RoutineForm {
            agent: Some("no-such-agent".into()),
            ..RoutineForm::default()
        },
        &you,
        &|_| None,
    )
    .unwrap_err();
    assert!(refused.contains("no-such-agent"), "{refused}");
    assert_eq!(get(&me, &key).unwrap().def.revision, 1);

    let warned = save(
        &me,
        FormTarget::Update {
            key: key.clone(),
            base_revision: 1,
        },
        RoutineForm {
            task: Some("/triage-slack today".into()),
            ..RoutineForm::default()
        },
        &you,
        &|_| None,
    )
    .unwrap();
    let SaveReply::Saved {
        revision, warnings, ..
    } = warned
    else {
        panic!("a warning is not a refusal");
    };
    assert_eq!(revision, 2, "a warning never blocks the save");
    assert_eq!(
        warnings
            .iter()
            .map(|w| w.message.as_str())
            .collect::<Vec<_>>(),
        ["this agent doesn't start with /triage-slack — add it to the agent or the run won't find it"]
    );
}

/// The tab switches a team routine's mode with an ordinary edit, and "Run on
/// my Mac" is this Mac's switch, never an edit.
#[test]
fn the_tab_switches_a_team_routines_mode_and_turns_a_recipe_on_here() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let me = caller_for(&home, 1);
    let you = Author::user("alice");
    let key = team_routine(&home, "modes");
    let here = crate::routines::local_host_id(&home).unwrap();

    // The tab's form, as it arrives over the wire.
    let to_each: RoutineForm = serde_json::from_value(serde_json::json!({ "runsOn": "each_mac" }))
        .expect("the tab's form parses");
    let update = |base_revision: u32, form: RoutineForm| {
        save(
            &me,
            FormTarget::Update {
                key: key.clone(),
                base_revision,
            },
            form,
            &you,
            &|_| None,
        )
    };
    assert_eq!(saved_key(update(1, to_each).unwrap()), (key.clone(), 2));
    let rows = list(&me).unwrap();
    let listed = row(&rows, &key);
    let wire = serde_json::to_value(listed).unwrap();
    assert_eq!(wire["def"]["runs_on"], "each_mac");
    assert_eq!(wire["def"]["armed_host"], here.as_str());
    assert!(wire.get("placed_by").is_none());
    // It ran here as a shared job, so it starts on here.
    assert!(listed.summary.armed_here && listed.summary.enabled);

    // Off and on again for this Mac: local state, no new revision.
    set_enabled(&me, &key, false).unwrap();
    assert!(!row(&list(&me).unwrap(), &key).summary.enabled);
    let armed = arm_here(&me, &key, &you).unwrap();
    assert_eq!(armed.def.revision, 2);
    let back = row(&list(&me).unwrap(), &key).summary.clone();
    assert!(back.enabled && back.next_run_at.is_some());
    assert_eq!(get(&me, &key).unwrap().revisions.len(), 2);

    // Back to a shared job, on this Mac.
    let to_one = RoutineForm {
        runs_on: Some("one_mac".into()),
        ..RoutineForm::default()
    };
    assert_eq!(saved_key(update(2, to_one).unwrap()).1, 3);
    let detail = get(&me, &key).unwrap();
    assert_eq!(
        (detail.def.runs_on.as_str(), detail.def.armed_host.as_str()),
        ("one_mac", here.as_str())
    );
    assert!(detail.enabled);

    // An unknown mode is a message, and changes nothing.
    let bad = RoutineForm {
        runs_on: Some("every_mac".into()),
        ..RoutineForm::default()
    };
    assert!(update(3, bad).unwrap_err().contains("runs_on"));
    assert_eq!(get(&me, &key).unwrap().def.revision, 3);
}

/// SW-AGR-5: a routine's wake queued on a busy session rides on its list row as
/// `queued`, in the golden `agent_get.routines[].queued` shape, so the Agents
/// tab's `⏰ due` chip reads the same contract the MCP tool returns.
#[test]
fn list_rows_carry_the_queued_wake_in_the_golden_shape() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let me = caller_for(home, 1);
    let triage = team_routine(home, "triage");
    let digest = team_routine(home, "digest");
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../fixtures/wake/agent-routines-queued.json"
    ))
    .unwrap();
    let golden = fixture["routines"][1]["queued"].clone();
    let wake: crate::delivery::read_model::QueuedWake =
        serde_json::from_value(golden.clone()).unwrap();

    let mut rows = list(&me).unwrap();
    attach_queued(&mut rows, |key| (key == triage).then(|| wake.clone()));

    let json = serde_json::to_value(&rows).unwrap();
    let of = |key: &str| {
        json.as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(of(&triage)["queued"], golden);
    assert_eq!(of(&digest)["queued"], serde_json::Value::Null);
}

/// SW-AGR-5: a list row's last run says when a wake was typed and on which
/// machine, so the row reads "Delivered" (not "Running") for a woken session.
#[test]
fn a_last_run_carries_a_wake_delivery() {
    let run = crate::routines::runs::RoutineRun {
        action: Default::default(),
        command: Default::default(),
        upstream: None,
        run_id: "run-1".into(),
        routine_key: "acme/api/triage".into(),
        revision: 1,
        source: "schedule".into(),
        occurrence_key: None,
        trigger_payload: None,
        phase: "running".into(),
        reason: None,
        linked_run_id: None,
        session_id: Some("s-barb".into()),
        created_at: 10,
        started_at: Some(20),
        ended_at: None,
        result_text: None,
        detail: None,
        agent: None,
        max_runtime_ms: None,
        causation: None,
        delivered_at: Some(25),
        wake_machine: Some("codesense-test-1".into()),
        launch_machine: None,
    };
    let json = serde_json::to_value(service::LastRun::from(run.clone())).unwrap();
    assert_eq!(json["delivered_at"], 25);
    assert_eq!(json["wake_machine"], "codesense-test-1");
    assert_eq!(json["session_id"], "s-barb");
    // A remote fresh run names the machine it started on.
    let remote_fresh = crate::routines::runs::RoutineRun {
        delivered_at: None,
        wake_machine: None,
        launch_machine: Some("codesense-test-1".into()),
        ..run.clone()
    };
    let json = serde_json::to_value(service::LastRun::from(remote_fresh)).unwrap();
    assert_eq!(json["launch_machine"], "codesense-test-1");
    let fresh = crate::routines::runs::RoutineRun {
        delivered_at: None,
        wake_machine: None,
        ..run
    };
    let json = serde_json::to_value(service::LastRun::from(fresh)).unwrap();
    assert!(json.get("delivered_at").is_none());
    assert!(json.get("wake_machine").is_none());
}

/// SW-AGR-5 r2/r3: the Schedule's Recent reads every routine's runs of the
/// last days in one bounded read, newest first, capped per routine (a busy
/// routine never crowds out a quiet one); merged and skipped fires are capped
/// apart, so they never crowd out the runs they belong to.
#[test]
fn recent_reads_every_routines_runs_since_capped_per_routine() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let me = caller_for(home, 1);
    let digest = team_routine(home, "digest");
    let triage = team_routine(home, "triage");
    let off = team_routine(home, "off");
    let first = run_now(&me, &digest, BTreeMap::new()).unwrap().run_id;
    let second = run_now(&me, &triage, BTreeMap::new()).unwrap().run_id;
    let paused = run_now(&me, &off, BTreeMap::new()).unwrap().run_id;
    set_enabled(&me, &off, false).unwrap();
    // A fire while the first still runs is merged: outside the per-routine cap.
    let merged = run_now(&me, &digest, BTreeMap::new()).unwrap().run_id;

    let all = recent(&me, 0, None).unwrap();
    let ids: Vec<&str> = all.iter().map(|r| r.run_id.as_str()).collect();
    for id in [&first, &second, &paused, &merged] {
        assert!(
            ids.contains(&id.as_str()),
            "{id} in {ids:?} (paused routines too)"
        );
    }
    assert!(
        all.windows(2).all(|w| w[0].created_at >= w[1].created_at),
        "newest first"
    );
    // One run per routine, and one merged or skipped fire per routine, counted
    // apart: the merged fire never crowds out the run it merged into.
    let capped = recent(&me, 0, Some(1)).unwrap();
    let ids: Vec<&str> = capped.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(capped.len(), 4, "{ids:?}");
    assert!(ids.contains(&merged.as_str()) && ids.contains(&first.as_str()));
    // Merged fires are capped too.
    run_now(&me, &digest, BTreeMap::new()).unwrap();
    let capped = recent(&me, 0, Some(1)).unwrap();
    assert_eq!(
        capped
            .iter()
            .filter(|r| r.routine_key == digest && r.phase == "coalesced")
            .count(),
        1
    );
    let newest = recent(&me, 0, None).unwrap()[0].created_at;
    assert!(
        recent(&me, newest + 1, None).unwrap().is_empty(),
        "only since"
    );
}

// SW-XS-25 review r1: every explicit change of a routine (the tab's and the
// MCP tools') runs the refresh on its machine, and a refresh that could not
// prepare it there reaches the reply instead of only the log.
#[test]
fn explicit_changes_refresh_the_machine_and_surface_why_not() {
    use crate::agents::launch_check::LaunchWarningCode;
    use crate::mcp::routine_tools as tools;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = Caller {
        home: home.clone(),
        principal: alice(),
    };
    let key = team_routine(&home, "nightly");
    let calls = std::cell::RefCell::new(Vec::<String>::new());
    let why = "Saved, but not prepared on m-1: m-1 is not connected.";
    let refresh = |k: &str| {
        calls.borrow_mut().push(k.to_string());
        Some(why.to_string())
    };
    let took = || std::mem::take(&mut *calls.borrow_mut());
    let not_prepared = |warnings: &[LaunchWarning]| {
        warnings
            .iter()
            .any(|w| w.code == LaunchWarningCode::MachineNotPrepared && w.message == why)
    };

    // Tab: an edit or a restore.
    let edited = with_refresh(
        SaveReply::Saved {
            key: key.clone(),
            revision: 2,
            warnings: Vec::new(),
        },
        &refresh,
    );
    assert!(matches!(&edited, SaveReply::Saved { warnings, .. } if not_prepared(warnings)));
    assert_eq!(took(), vec![key.clone()]);
    let conflict = with_refresh(
        SaveReply::Conflict {
            current_revision: 3,
        },
        &refresh,
    );
    assert_eq!(
        conflict,
        SaveReply::Conflict {
            current_revision: 3
        }
    );
    assert!(took().is_empty(), "nothing saved: nothing to refresh");
    // Tab: pause (no refresh), resume (refresh).
    assert_eq!(
        set_enabled_and_refresh(&caller, &key, false, &refresh),
        Ok(None)
    );
    assert!(took().is_empty());
    assert_eq!(
        set_enabled_and_refresh(&caller, &key, true, &refresh),
        Ok(Some(why.to_string()))
    );
    assert_eq!(took(), vec![key.clone()]);
    // Tab: arm on a machine; release a handover.
    let armed = arm_on_and_refresh(
        &caller,
        &key,
        Some("m-1".into()),
        &Author::user("alice"),
        &|_, _| Ok(None),
        &refresh,
    )
    .unwrap();
    assert_eq!(armed.def.armed_machine.as_deref(), Some("m-1"));
    assert!(not_prepared(&armed.warnings));
    assert_eq!(took(), vec![key.clone()]);
    assert_eq!(
        release_and_refresh(&caller, &key, "m-0", &refresh),
        Ok(Some(why.to_string()))
    );
    assert_eq!(took(), vec![key.clone()]);

    // MCP: routine_update, routine_set_enabled, routine_arm_on,
    // routine_handover_release.
    let current = service::get_current(&home, &alice(), &RoutineKey::parse(&key).unwrap())
        .unwrap()
        .def
        .revision;
    let updated = tools::update_and_refresh(
        &home,
        &alice(),
        &Author::user("alice"),
        serde_json::from_value(
            serde_json::json!({"id": key, "baseRevision": current, "task": "again"}),
        )
        .unwrap(),
        chrono::Utc::now(),
        &|_| None,
        &refresh,
    )
    .unwrap();
    assert!(matches!(&updated, ToolEnvelope::Ok(saved) if not_prepared(&saved.warnings)));
    assert_eq!(took(), vec![key.clone()]);
    let resumed = tools::set_enabled_and_refresh(
        &home,
        &alice(),
        serde_json::from_value(serde_json::json!({"id": key, "enabled": true})).unwrap(),
        &refresh,
    )
    .unwrap();
    assert!(
        matches!(&resumed, ToolEnvelope::Ok(set) if set.machine_warning.as_deref() == Some(why))
    );
    assert_eq!(took(), vec![key.clone()]);
    let moved = tools::arm_on_and_refresh(
        &home,
        &alice(),
        &Author::user("alice"),
        serde_json::from_value(serde_json::json!({"id": key, "machine": "m-2"})).unwrap(),
        &|_, _| Ok(None),
        &refresh,
    )
    .unwrap();
    assert!(
        matches!(&moved, ToolEnvelope::Ok(armed) if armed.machine_warning.as_deref() == Some(why))
    );
    assert_eq!(took(), vec![key.clone()]);
    let released = tools::handover_release_and_refresh(
        &home,
        &alice(),
        serde_json::from_value(
            serde_json::json!({"id": key, "machine": "m-1", "machineGone": true}),
        )
        .unwrap(),
        &refresh,
    )
    .unwrap();
    assert!(matches!(&released, ToolEnvelope::Ok(r) if r.machine_warning.as_deref() == Some(why)));
    assert_eq!(took(), vec![key]);
}

#[test]
fn metadata_form_creates_without_schedule_and_replaces_only_metadata() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let me = caller_for(&home, 1);
    let author = Author::user("alice");
    let mut form = sentence("Metadata form", "Read routine_context", "");
    form.metadata_triggers = Some(vec![serde_json::from_value(serde_json::json!({
        "selection":{"where":{"kinds":["story"]}},
        "target":{"key":"status","op":"eq","value":"in-progress"}
    }))
    .unwrap()]);
    let (key, revision) =
        saved_key(save(&me, personal_create(true), form, &author, &|_| None).unwrap());
    let created = get(&me, &key).unwrap();
    assert!(crons(&created).is_empty());
    assert_eq!(created.def.triggers.len(), 1);
    let form = RoutineForm {
        metadata_triggers: Some(vec![]),
        when: Some("5m".into()),
        ..RoutineForm::default()
    };
    save(
        &me,
        FormTarget::Update {
            key: key.clone(),
            base_revision: revision,
        },
        form,
        &author,
        &|_| None,
    )
    .unwrap();
    assert_eq!(crons(&get(&me, &key).unwrap()), vec!["*/5 * * * *"]);
    assert_eq!(get(&me, &key).unwrap().def.triggers.len(), 1);
}
