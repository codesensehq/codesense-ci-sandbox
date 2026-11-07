// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::tests::store;
use serde_json::json;

fn session(store: &UsageStore, id: &str, team: &str, story: Option<&str>) {
    let mut r = SessionRecord::new(id);
    r.org = Some("acme".into());
    r.team = Some(team.into());
    r.story_id = story.map(str::to_owned);
    store.write_session_record(&r).unwrap();
}

fn start_params(session: &str, goal: &str) -> TaskStartParams {
    serde_json::from_value(json!({"sessionId": session, "goal": goal})).unwrap()
}

fn policy_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
    std::fs::write(
        dir.path().join(POLICY_FILE),
        "types:\n  review:\n    targets: {wall_minutes: 30, review_rounds: 1}\ntamper_globs: ['**/tests/**']\n",
    )
    .unwrap();
    dir
}

use crate::session_usage::task_capture::POLICY_FILE;

#[test]
fn a_started_task_sits_under_the_callers_own_task_in_its_story_with_a_session_link() {
    let (_d, store) = store();
    session(&store, "lead", "dev", Some("SV-1"));
    let t = start(
        &store,
        "lead",
        start_params("lead", "Review the store"),
        None,
        5,
    )
    .unwrap();
    assert!(t.task_id.starts_with("task-"));
    assert_eq!(
        (t.parent_id.as_deref(), t.parent_source.as_deref()),
        (Some("lead"), Some("launcher"))
    );
    assert_eq!(
        (t.org.as_deref(), t.team.as_deref(), t.story_id.as_deref()),
        (Some("acme"), Some("dev"), Some("SV-1"))
    );
    assert_eq!(
        (t.task_type.as_str(), t.type_source.as_deref()),
        ("unclassified", Some("unclassified"))
    );
    assert!(t.unavailable["type"].contains("no type"));
    assert_eq!(t.created_by.as_deref(), Some("lead"));
    let links = store.task_links(&t.task_id).unwrap();
    assert_eq!(
        (links[0].kind.as_str(), links[0].target.as_str()),
        ("session", "lead")
    );
}

#[test]
fn a_review_is_a_generic_task_with_a_relation_and_artifacts() {
    let (_d, store) = store();
    session(&store, "lead", "dev", Some("SV-1"));
    let build = start(
        &store,
        "lead",
        start_params("lead", "Build the store"),
        None,
        1,
    )
    .unwrap();
    let dir = policy_dir();
    let mut p = start_params("lead", "Review the store build");
    p.task_type = Some("review".into());
    p.subject = Some("design".into());
    p.parent = Some(build.task_id.clone());
    p.targets = Some(BTreeMap::from([("wall_minutes".into(), json!(45))]));
    p.links = Some(vec![
        TaskLinkInput {
            kind: "relates_to:reviews".into(),
            target: build.task_id.clone(),
        },
        TaskLinkInput {
            kind: "artifact".into(),
            target: "design/SV-1/review-r1.md".into(),
        },
    ]);
    let review = start(&store, "lead", p, Some(dir.path()), 2).unwrap();
    assert_eq!(
        (
            review.task_type.as_str(),
            review.subject.as_deref(),
            review.type_source.as_deref()
        ),
        ("review", Some("design"), Some("task_start"))
    );
    // The type's default targets apply; the caller's own win and say so.
    let targets = review.targets.unwrap();
    assert_eq!(targets.source, "task_start");
    assert_eq!(
        (
            &targets.values["wall_minutes"],
            &targets.values["review_rounds"]
        ),
        (&json!(45), &json!(1))
    );
    assert_eq!(review.tamper_globs, ["**/tests/**"]);
    // The review's own read shows its relation and its artifact; the reviewed task counts the round.
    let read_review = read(
        &store,
        "lead",
        TaskReadParams {
            session_id: "lead".into(),
            task_id: review.task_id.clone(),
        },
    )
    .unwrap();
    let kinds: Vec<&str> = read_review.links.iter().map(|l| l.kind.as_str()).collect();
    assert_eq!(kinds, ["artifact", "relates_to:reviews", "session"]);
    let read_build = read(
        &store,
        "lead",
        TaskReadParams {
            session_id: "lead".into(),
            task_id: build.task_id.clone(),
        },
    )
    .unwrap();
    assert_eq!(read_build.outcome.review_rounds, 1);
    assert_eq!(read_build.children[0].task.task_id, review.task_id);
}

#[test]
fn a_bad_start_is_refused_with_why() {
    let (_d, store) = store();
    session(&store, "lead", "dev", None);
    for (mutate, want) in [
        (
            Box::new(|p: &mut TaskStartParams| p.goal = "  ".into())
                as Box<dyn Fn(&mut TaskStartParams)>,
            "needs a goal",
        ),
        (
            Box::new(|p| p.task_type = Some("Not A Tag!".into())),
            "type 'Not A Tag!' is not a tag",
        ),
        (Box::new(|p| p.subject = Some("a$b".into())), "subject"),
        (
            Box::new(|p| p.parent = Some("nope".into())),
            "no task 'nope' is visible",
        ),
        (
            Box::new(|p| {
                p.links = Some(vec![TaskLinkInput {
                    kind: "bogus".into(),
                    target: "x".into(),
                }])
            }),
            "not a link kind",
        ),
    ] {
        let mut p = start_params("lead", "goal");
        mutate(&mut p);
        let err = start(&store, "lead", p, None, 1).unwrap_err();
        assert!(err.contains(want), "{err}");
    }
    assert!(start(&store, "ghost", start_params("ghost", "g"), None, 1)
        .unwrap_err()
        .contains("no record"));
}

#[test]
fn a_goal_with_a_credential_is_kept_without_it() {
    let (_d, store) = store();
    session(&store, "lead", "dev", None);
    let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let t = start(
        &store,
        "lead",
        start_params("lead", &format!("Deploy with {secret}")),
        None,
        1,
    )
    .unwrap();
    assert!(!t.goal.contains(&secret), "{}", t.goal);
}

#[test]
fn an_update_sets_the_story_later_and_the_task_is_found_by_it_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    session(&open(), "ctx-tab", "dev", None);
    let t = start(
        &open(),
        "ctx-tab",
        start_params("ctx-tab", "Explore the sync path"),
        None,
        1,
    )
    .unwrap();
    assert!(t.story_id.is_none());
    let update_params: TaskUpdateParams = serde_json::from_value(json!({
        "sessionId": "ctx-tab", "taskId": t.task_id, "story": "SV-9",
        "acceptanceMet": true, "label": {"verdict": "good", "why": "found the cause"},
        "status": "done", "type": "exploratory"
    }))
    .unwrap();
    let after = update(&open(), "ctx-tab", update_params, 9).unwrap();
    assert_eq!(
        (
            after.story_id.as_deref(),
            after.status.as_str(),
            after.task_type.as_str()
        ),
        (Some("SV-9"), "done", "exploratory")
    );
    assert_eq!(after.acceptance_met, Some(true));
    let label = after.label.unwrap();
    assert_eq!(
        (
            label.verdict.as_str(),
            label.by.as_deref(),
            label.why.as_deref()
        ),
        ("good", Some("ctx-tab"), Some("found the cause"))
    );
    // A reopened store lists it by the story it was given later.
    let listed = list(
        &open(),
        "ctx-tab",
        serde_json::from_value(json!({"sessionId": "ctx-tab", "story": "SV-9"})).unwrap(),
    )
    .unwrap();
    assert!(listed.iter().any(|x| x.task_id == t.task_id), "{listed:?}");
}

#[test]
fn a_subtask_inherits_its_parents_updated_story_and_is_listed_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    session(&open(), "ctx-tab", "dev", Some("old-story"));
    let mut root = TaskRecord::new("ctx-tab", "Work in context");
    root.org = Some("acme".into());
    root.team = Some("dev".into());
    open().insert_task_if_absent(&root).unwrap();
    update(&open(), "ctx-tab", serde_json::from_value(json!({
        "sessionId": "ctx-tab", "taskId": "ctx-tab", "story": "SV-9"
    })).unwrap(), 1).unwrap();
    let subtask = start(&open(), "ctx-tab", start_params("ctx-tab", "Review"), None, 2).unwrap();
    assert_eq!(subtask.story_id.as_deref(), Some("SV-9"));
    let listed = list(&open(), "ctx-tab", serde_json::from_value(json!({
        "sessionId": "ctx-tab", "story": "SV-9"
    })).unwrap()).unwrap();
    assert!(listed.iter().any(|t| t.task_id == subtask.task_id));

    update(&open(), "ctx-tab", serde_json::from_value(json!({
        "sessionId": "ctx-tab", "taskId": subtask.task_id, "story": "SV-10"
    })).unwrap(), 3).unwrap();
    let mut params = start_params("ctx-tab", "Nested review");
    params.parent = Some(subtask.task_id.clone());
    let nested = start(&open(), "ctx-tab", params, None, 4).unwrap();
    assert_eq!(nested.story_id.as_deref(), Some("SV-10"));
    let mut params = start_params("ctx-tab", "Explicit story");
    params.parent = Some(subtask.task_id);
    params.story = Some("SV-11".into());
    assert_eq!(start(&open(), "ctx-tab", params, None, 5).unwrap().story_id.as_deref(), Some("SV-11"));
}

#[test]
fn another_teams_tasks_are_neither_listed_read_nor_changed() {
    let (_d, store) = store();
    session(&store, "dev-tab", "dev", Some("SV-1"));
    session(&store, "ops-tab", "ops", Some("OPS-1"));
    let mine = start(&store, "dev-tab", start_params("dev-tab", "Mine"), None, 1).unwrap();
    let theirs = start(
        &store,
        "ops-tab",
        start_params("ops-tab", "Theirs"),
        None,
        2,
    )
    .unwrap();
    let ids = |rows: Vec<TaskRecord>| rows.into_iter().map(|t| t.task_id).collect::<Vec<_>>();
    let list_all = |who: &str| {
        ids(list(
            &store,
            who,
            serde_json::from_value(json!({"sessionId": who})).unwrap(),
        )
        .unwrap())
    };
    assert_eq!(list_all("dev-tab"), [mine.task_id.clone()]);
    assert_eq!(list_all("ops-tab"), [theirs.task_id.clone()]);
    let read_params = |id: &str| TaskReadParams {
        session_id: "dev-tab".into(),
        task_id: id.into(),
    };
    let refused = read(&store, "dev-tab", read_params(&theirs.task_id)).unwrap_err();
    assert!(refused.contains("visible to your session"), "{refused}");
    let change = serde_json::from_value(
        json!({"sessionId": "dev-tab", "taskId": theirs.task_id, "status": "done"}),
    )
    .unwrap();
    assert!(update(&store, "dev-tab", change, 3).is_err());
    assert_eq!(
        store.read_task(&theirs.task_id).unwrap().unwrap().status,
        "open"
    );
    // A task of another team cannot be a parent either.
    let mut p = start_params("dev-tab", "Child");
    p.parent = Some(theirs.task_id.clone());
    assert!(start(&store, "dev-tab", p, None, 4).is_err());
}

#[test]
fn the_filters_and_the_limit_apply_and_hidden_tasks_do_not_eat_the_page() {
    let (_d, store) = store();
    session(&store, "dev-tab", "dev", Some("SV-1"));
    session(&store, "ops-tab", "ops", None);
    for i in 0..3 {
        let mut p = start_params("dev-tab", &format!("dev task {i}"));
        p.task_type = Some("feature".into());
        start(&store, "dev-tab", p, None, 10 + i).unwrap();
    }
    // More than the former global 1,000-row overfetch: visibility must precede LIMIT.
    for i in 0..1_200 {
        start(
            &store,
            "ops-tab",
            start_params("ops-tab", &format!("ops task {i}")),
            None,
            100 + i,
        )
        .unwrap();
    }
    let params = |v: serde_json::Value| -> TaskListParams { serde_json::from_value(v).unwrap() };
    let one = list(
        &store,
        "dev-tab",
        params(json!({"sessionId": "dev-tab", "limit": 1})),
    )
    .unwrap();
    assert_eq!(one.len(), 1, "hidden rows cannot exhaust the requested page");
    assert_eq!(one[0].goal, "dev task 2");
    let rows = list(
        &store,
        "dev-tab",
        params(json!({"sessionId": "dev-tab", "limit": 2})),
    )
    .unwrap();
    assert_eq!(rows.len(), 2, "the newer ops tasks are hidden, not counted");
    assert!(rows.iter().all(|t| t.team.as_deref() == Some("dev")));
    let rows = list(
        &store,
        "dev-tab",
        params(
            json!({"sessionId": "dev-tab", "type": "feature", "status": "open", "story": "SV-1"}),
        ),
    )
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(list(
        &store,
        "dev-tab",
        params(json!({"sessionId": "dev-tab", "type": "review"}))
    )
    .unwrap()
    .is_empty());
}

#[test]
fn snake_case_inputs_are_accepted() {
    let p: TaskUpdateParams =
        serde_json::from_value(json!({"session_id": "s", "task_id": "t", "acceptance_met": false}))
            .unwrap();
    assert_eq!(
        (p.session_id.as_str(), p.task_id.as_str(), p.acceptance_met),
        ("s", "t", Some(false))
    );
    let p: TaskStartParams =
        serde_json::from_value(json!({"session_id": "s", "goal": "g", "type": "ops"})).unwrap();
    assert_eq!(p.task_type.as_deref(), Some("ops"));
}

/// The catalog's `tasks` family names real inputs: each example deserializes into the verb's
/// own parameters, with canonical (camelCase) keys.
#[test]
fn the_catalog_examples_are_valid_inputs_of_their_verbs() {
    let catalog: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../codesense-docs/getting-started/content/_shared/examples.yml"
    ))
    .unwrap();
    let family = catalog["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "tasks")
        .expect("a tasks family");
    let tools: Vec<&str> = family["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        ["task_start", "task_update", "task_list", "task_read"]
    );
    for call in family["calls"].as_array().unwrap() {
        let args = call["arguments"].clone();
        match call["tool"].as_str().unwrap() {
            "task_list" => drop(serde_json::from_value::<TaskListParams>(args).unwrap()),
            "task_read" => drop(serde_json::from_value::<TaskReadParams>(args).unwrap()),
            "task_start" => drop(serde_json::from_value::<TaskStartParams>(args).unwrap()),
            "task_update" => drop(serde_json::from_value::<TaskUpdateParams>(args).unwrap()),
            other => panic!("{other} is not a task verb"),
        }
        assert!(call["arguments"].get("session_id").is_none());
    }
    let docs = family["docs"].as_str().unwrap();
    assert!(
        std::path::Path::new("../codesense-docs/getting-started/content")
            .join(docs)
            .exists()
            || docs == "arc-e-agents/tasks.html"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn closing_by_task_update_records_tamper_and_a_late_update_never_moves_the_first_close() {
    let work = tempfile::tempdir().unwrap();
    git(work.path(), &["init", "-q"]);
    std::fs::create_dir_all(work.path().join("tests")).unwrap();
    std::fs::write(work.path().join("tests/a_test.rs"), "fn t() {}\n").unwrap();
    git(work.path(), &["add", "."]);
    git(work.path(), &["commit", "-q", "-m", "init"]);
    let policy = policy_dir();
    let (_d, store) = store();
    let mut r = SessionRecord::new("lead");
    r.org = Some("acme".into());
    r.team = Some("dev".into());
    r.worktree = Some(work.path().to_string_lossy().into_owned());
    store.write_session_record(&r).unwrap();
    let t = start(
        &store,
        "lead",
        start_params("lead", "Review the checks"),
        Some(policy.path()),
        1,
    )
    .unwrap();
    std::fs::write(work.path().join("tests/a_test.rs"), "fn t() { 1; }\n").unwrap();
    let close = |status: &str, at: i64| {
        let p: TaskUpdateParams = serde_json::from_value(
            json!({"sessionId": "lead", "taskId": t.task_id, "status": status}),
        )
        .unwrap();
        update(&store, "lead", p, at).unwrap()
    };
    let closed = close("abandoned", 10);
    assert_eq!(
        (closed.status.as_str(), closed.closed_at_ms),
        ("abandoned", Some(10))
    );
    assert_eq!(
        closed.tamper.as_ref().map(|t| (t.flag, t.files.clone())),
        Some((true, vec!["tests/a_test.rs".to_string()]))
    );
    // A later close, and a later update with no status, leave the first close alone.
    let later = close("done", 20);
    assert_eq!(
        (later.status.as_str(), later.closed_at_ms),
        ("abandoned", Some(10))
    );
    let p: TaskUpdateParams = serde_json::from_value(json!({"sessionId": "lead", "taskId": t.task_id, "goal": "Review the checks, again", "acceptanceMet": false})).unwrap();
    let refined = update(&store, "lead", p, 30).unwrap();
    assert_eq!(
        (
            refined.status.as_str(),
            refined.closed_at_ms,
            refined.goal.as_str()
        ),
        ("abandoned", Some(10), "Review the checks, again")
    );
    let bad: TaskUpdateParams =
        serde_json::from_value(json!({"sessionId": "lead", "taskId": t.task_id, "status": "open"}))
            .unwrap();
    assert!(update(&store, "lead", bad, 40)
        .unwrap_err()
        .contains("`done`, `abandoned` or `superseded`"));
}

#[test]
fn a_subject_is_kept_as_written_and_a_goal_is_capped_and_redacted_on_both_writes() {
    let (_d, store) = store();
    session(&store, "lead", "dev", None);
    let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let mut p = start_params("lead", &format!("{} {secret}", "long ".repeat(100)));
    p.task_type = Some("bug".into());
    p.subject = Some("research".into());
    let t = start(&store, "lead", p, None, 1).unwrap();
    assert_eq!(
        (t.task_type.as_str(), t.subject.as_deref()),
        ("bugfix", Some("research"))
    );
    assert_eq!(t.goal.chars().count(), 280);
    assert!(!t.goal.contains(&secret));
    let up: TaskUpdateParams = serde_json::from_value(json!({
        "sessionId": "lead", "taskId": t.task_id, "subject": "bug",
        "goal": format!("{} {secret}", "again ".repeat(100))
    }))
    .unwrap();
    let t = update(&store, "lead", up, 2).unwrap();
    assert_eq!(
        t.subject.as_deref(),
        Some("bug"),
        "no type alias on a subject"
    );
    assert_eq!(t.goal.chars().count(), 280);
    assert!(!t.goal.contains(&secret));
    assert_eq!(t.type_source.as_deref(), Some("task_start"));
    let up: TaskUpdateParams =
        serde_json::from_value(json!({"sessionId": "lead", "taskId": t.task_id, "type": "review"}))
            .unwrap();
    assert_eq!(
        update(&store, "lead", up, 3)
            .unwrap()
            .type_source
            .as_deref(),
        Some("task_update")
    );
}

#[test]
fn an_agent_filter_finds_launch_routine_and_subtasks_through_their_session_links() {
    let (_d, store) = store();
    let mut r = SessionRecord::new("tab-1");
    r.org = Some("acme".into());
    r.team = Some("dev".into());
    r.agent = Some("acme/dev/worker@3".into());
    store.write_session_record(&r).unwrap();
    let mut other = SessionRecord::new("tab-2");
    other.org = Some("acme".into());
    other.team = Some("dev".into());
    other.agent = Some("acme/dev/other@1".into());
    store.write_session_record(&other).unwrap();
    // The session's own task, a routine run's task (run id) and a task_start subtask.
    for (id, session) in [("tab-1", "tab-1"), ("run-9", "tab-1"), ("tab-2", "tab-2")] {
        let mut t = TaskRecord::new(id, id);
        t.org = Some("acme".into());
        t.team = Some("dev".into());
        store.insert_task_if_absent(&t).unwrap();
        store.add_task_link(id, LINK_SESSION, session).unwrap();
    }
    let sub = start(&store, "tab-1", start_params("tab-1", "A subtask"), None, 5).unwrap();
    let ids = |agent: &str| -> Vec<String> {
        let p: TaskListParams =
            serde_json::from_value(json!({"sessionId": "tab-1", "agent": agent})).unwrap();
        let mut ids: Vec<String> = list(&store, "tab-1", p)
            .unwrap()
            .into_iter()
            .map(|t| t.task_id)
            .collect();
        ids.sort();
        ids
    };
    let mut want = vec![
        "run-9".to_string(),
        "tab-1".to_string(),
        sub.task_id.clone(),
    ];
    want.sort();
    assert_eq!(ids("acme/dev/worker"), want);
    assert_eq!(ids("acme/dev/worker@3"), want);
    assert_eq!(ids("acme/dev/other"), ["tab-2"]);
    assert!(ids("acme/dev/nobody").is_empty());
}

#[test]
fn a_subtask_copies_its_skill_from_the_sessions_launch_snapshot_or_says_it_cannot() {
    use crate::session_usage::task::SkillVersion;
    let (_d, store) = store();
    session(&store, "lead", "dev", None);
    let mut launch = TaskRecord::new("lead", "launch");
    launch.org = Some("acme".into());
    launch.team = Some("dev".into());
    launch.layers.skills = vec![SkillVersion {
        name: "bmad-code-review".into(),
        hash: Some("launch-hash".into()),
        reason: None,
    }];
    store.insert_task_if_absent(&launch).unwrap();
    let mut p = start_params("lead", "Review");
    p.skill = Some("bmad-code-review".into());
    let t = start(&store, "lead", p, None, 1).unwrap();
    assert_eq!(t.main_skill.as_deref(), Some("bmad-code-review"));
    assert_eq!(
        t.layers.skills, launch.layers.skills,
        "the hash the launch recorded, not a fresh one"
    );
    assert!(!t.unavailable.contains_key("skill"));
    let mut p = start_params("lead", "Review");
    p.skill = Some("impeccable".into());
    let t = start(&store, "lead", p, None, 2).unwrap();
    assert!(t.layers.skills.is_empty());
    assert!(t.unavailable["skill"].contains("has no skill 'impeccable'"));
    // The playbook is the parent's: the launch task has no main skill, so it says so.
    let view = read(
        &store,
        "lead",
        TaskReadParams {
            session_id: "lead".into(),
            task_id: t.task_id,
        },
    )
    .unwrap();
    assert!(view.playbook["unavailable"]
        .as_str()
        .unwrap()
        .contains("no main skill"));
}

#[test]
fn task_list_description_directs_playbook_reads_to_task_read() {
    let handler = include_str!("../handler.rs");
    let before = handler.split("    async fn task_list(").next().unwrap();
    let description = before.rsplit("description = ").next().unwrap();
    assert!(description.contains("task_read.playbook"), "list must identify the read-side playbook field");
    assert!(!description.contains("context commit and the playbook"), "list layers cannot advertise a missing field");
}

async fn refinements_survive_placeholder_enrichment(early: bool) {
    use crate::session_usage::task_capture::{capture_launch_task, capture_routine_start, Capture, RoutineBrief, RoutineStart, TaskInputs};
    use crate::session_usage::task_outcome::close_for_session_async;
    for refinement in [
        json!({"story":"SV-refined", "goal":"Refined task goal", "type":"review", "subject":"security", "parent":"refined-parent", "targets":{"wall_minutes":90}}),
        json!({"goal":"Refined task goal"}),
        json!({"story":"SV-refined"}),
        json!({"subject":"security"}),
        json!({"parent":"refined-parent"}),
        json!({"type":"review"}),
        json!({"targets":{"wall_minutes":90}}),
    ] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("usage.sqlite");
        let open = || UsageStore::open(&path).unwrap();
        let mut record = SessionRecord::new("delayed-session");
        record.org = Some("acme".into());
        record.team = Some("dev".into());
        record.story_id = Some("SV-original".into());
        if !early { record.run_id = Some("delayed-run".into()); }
        open().write_session_record(&record).unwrap();
        let own_id = record.run_id.as_deref().unwrap_or(&record.session_id);
        let routine = RoutineBrief {
            name: "Original routine".into(),
            task: "Task-Type: ops\nSubject: research\nTargets: wall_minutes=30\nOriginal routine goal".into(),
        };
        if !early {
            capture_routine_start(&open(), &RoutineStart {
                org: "acme", team: "dev", run_id: own_id, routine: &routine,
                policy_dir: None, cwd: None, session_id: Some(&record.session_id), now_ms: 10,
            }).unwrap();
        }
        // The real terminal seam creates an early placeholder scoped from the session record.
        close_for_session_async(Ok(path.clone()), &record.session_id, "failed", Some(own_id.into())).await;
        let pending = open().read_task(own_id).unwrap().unwrap();
        assert_eq!(pending.org.as_deref(), Some("acme"));
        assert_eq!(pending.status, ABANDONED);
        let mut named_parent = TaskRecord::new("refined-parent", "Named parent");
        named_parent.org = record.org.clone();
        named_parent.team = record.team.clone();
        open().insert_task_if_absent(&named_parent).unwrap();
        let mut args = refinement.clone();
        args["sessionId"] = json!(record.session_id);
        args["taskId"] = json!(own_id);
        args["acceptanceMet"] = json!(true);
        args["label"] = json!({"verdict":"good", "why":"refined after completion"});
        let refined = update(&open(), &record.session_id, serde_json::from_value(args).unwrap(), 30).unwrap();
        let inputs = TaskInputs {
            brief: Some("Task-Type: feature\nSubject: code\nTargets: wall_minutes=15\nOriginal launch goal".into()),
            ..Default::default()
        };
        let late_capture = || capture_launch_task(&open(), &Capture {
            session_id: &record.session_id, record: &record, inputs: &inputs,
            routine: (!early).then_some(&routine), launcher_task: None, story_file: None, now_ms: 10,
        }).unwrap();
        let captured = late_capture();
        if refinement.get("story").is_some() { assert_eq!(captured.story_id, refined.story_id, "early={early} {refinement}"); }
        if refinement.get("goal").is_some() { assert_eq!(captured.goal, refined.goal, "early={early} {refinement}"); }
        if refinement.get("type").is_some() { assert_eq!(captured.task_type, refined.task_type, "early={early} {refinement}"); }
        if refinement.get("subject").is_some() { assert_eq!(captured.subject, refined.subject, "early={early} {refinement}"); }
        if refinement.get("parent").is_some() { assert_eq!(captured.parent_id, refined.parent_id, "early={early} {refinement}"); }
        if refinement.get("targets").is_some() { assert_eq!(captured.targets, refined.targets, "early={early} {refinement}"); }
        assert_eq!((captured.status.as_str(), captured.raw_status.as_deref(), captured.closed_at_ms),
            (ABANDONED, Some("failed"), pending.closed_at_ms));
        assert_eq!(captured.acceptance_met, Some(true));
        assert_eq!(captured.label, refined.label);
        assert_eq!(late_capture(), captured, "duplicate capture changes no acknowledged metadata");
        assert_eq!(open().read_task(own_id).unwrap().unwrap(), captured, "reopened task keeps refinements");
    }
}

#[tokio::test]
async fn task_update_refinements_survive_late_early_close_enrichment_after_reopen() {
    refinements_survive_placeholder_enrichment(true).await;
}

#[tokio::test]
async fn task_update_refinements_survive_late_routine_enrichment_after_reopen() {
    refinements_survive_placeholder_enrichment(false).await;
}

/// A delta round run inside the same reviewer session is its own review task
/// in one `task_start {type: review}` call, beside the launch's review task.
#[test]
fn cde83_a_delta_round_in_the_reviewer_session_is_its_own_review_task() {
    use crate::session_usage::task_capture::{capture_launch_task, Capture, TaskInputs};
    let (_d, store) = store();
    session(&store, "dev", "dev", Some("SV-1"));
    let dev = store.read_session_record("dev").unwrap().unwrap();
    let plain = TaskInputs {
        brief: Some("Build it.".into()),
        ..Default::default()
    };
    capture_launch_task(
        &store,
        &Capture {
            session_id: "dev",
            record: &dev,
            inputs: &plain,
            routine: None,
            launcher_task: None,
            story_file: None,
            now_ms: 1,
        },
    )
    .unwrap();
    session(&store, "rev", "dev", Some("SV-1"));
    let rev = store.read_session_record("rev").unwrap().unwrap();
    let reviewer = TaskInputs {
        brief: Some("Review PR 12.".into()),
        role: Some("reviewer".into()),
        reviews: Some("dev".into()),
        ..Default::default()
    };
    let first = capture_launch_task(
        &store,
        &Capture {
            session_id: "rev",
            record: &rev,
            inputs: &reviewer,
            routine: None,
            launcher_task: None,
            story_file: None,
            now_ms: 2,
        },
    )
    .unwrap();
    assert_eq!(
        (first.task_type.as_str(), first.type_source.as_deref()),
        ("review", Some("role"))
    );

    let mut p = start_params("rev", "Delta round 2 of PR 12");
    p.task_type = Some("review".into());
    p.links = Some(vec![TaskLinkInput {
        kind: "relates_to:reviews".into(),
        target: "dev".into(),
    }]);
    let delta = start(&store, "rev", p, None, 3).unwrap();
    assert_ne!(delta.task_id, first.task_id);
    assert_eq!(delta.task_type, "review");
    assert_eq!(delta.story_id.as_deref(), Some("SV-1"));
    assert_eq!(delta.parent_id.as_deref(), Some("rev"));
    assert_eq!(
        store.review_tasks_of("dev").unwrap(),
        vec!["rev".to_string(), delta.task_id]
    );
}
