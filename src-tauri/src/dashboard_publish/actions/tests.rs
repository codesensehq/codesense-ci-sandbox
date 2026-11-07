// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;
fn set_fields() -> Action {
    serde_json::from_value(json!({"verb":"setFields", "who":"owner", "fields":{"status":"wontfix"}, "match":{"type":"bug"}})).unwrap()
}
#[test]
fn typed_arguments_refuse_paths_extra_keys_and_undeclared_choices() {
    let a = set_fields();
    for value in [
        json!({}),
        json!({"id":"../bug"}),
        json!({"id":"BUG/1"}),
        json!({"id":"BUG..1"}),
        json!({"id":"BUG-1","path":"x"}),
        json!({"id":"BUG-1","choice":"done"}),
        json!({"id":"$(echo hi)"}),
        json!({"id":1}),
    ] {
        assert_eq!(a.args(value).unwrap_err(), "invalid_args");
    }
    assert_eq!(
        a.args(json!({"id":"BUG-1"})).unwrap().id.as_deref(),
        Some("BUG-1")
    );
    let mut a = a;
    a.choices.insert(
        "close".into(),
        [("status".into(), json!("closed"))].into_iter().collect(),
    );
    assert!(a.args(json!({"id":"BUG-1","choice":"close"})).is_ok());
    assert!(a.args(json!({"id":"BUG-1","choice":"open"})).is_err());
}
#[test]
fn only_the_announced_owner_passes_the_mac_gate() {
    for (by, owner) in [
        (json!({"role":"viewer","share":"s1"}), Some("owner")),
        (json!({"role":"owner","account":"wrong"}), Some("owner")),
        (json!({"role":"owner","account":"owner"}), None),
        (json!({}), Some("owner")),
    ] {
        assert_eq!(authorize(&by, owner), Err("not_allowed"));
    }
    assert_eq!(
        authorize(&json!({"role":"owner","account":"owner"}), Some("owner")),
        Ok(())
    );
}
#[test]
fn rolling_hour_and_running_caps_use_successful_calls_for_this_action() {
    let now = chrono::Utc::now();
    let mut a = set_fields();
    a.max_per_hour = 1;
    let rows =
        vec![json!({"action":"close", "outcome":"done", "at": now.to_rfc3339(), "handle":"h1"})];
    assert_eq!(cap(&a, "close", &rows, &[], now), Err("rate_limited"));
    assert_eq!(cap(&a, "another", &rows, &[], now), Ok(()));
    assert_eq!(
        cap(&a, "close", &rows, &[], now + chrono::Duration::hours(2)),
        Ok(())
    );
    a.verb = "launchAgent".into();
    a.max_per_hour = 30;
    a.max_running = 1;
    assert_eq!(cap(&a, "close", &rows, &["h1".into()], now), Err("busy"));
    assert_eq!(cap(&a, "close", &rows, &["h2".into()], now), Ok(()));
}
#[test]
fn manifest_actions_are_saved_and_edits_invalidate_the_approval() {
    let mut m = crate::dashboard_publish::approval::tests::manifest(Some(
        crate::dashboard_publish::manifest::LiveQueries::List(vec![]),
    ));
    let before = m.live_sha();
    m.actions.insert("close".into(), set_fields());
    assert_ne!(before, m.live_sha());
    let dir = tempfile::tempdir().unwrap();
    let saved =
        crate::dashboard_publish::approval::decide(&m, dir.path(), None, Some(("org", "team")))
            .approved
            .unwrap();
    assert_eq!(saved.actions, m.actions);
    let mut record = crate::dashboard_publish::state::PublishRecord::new(&m.slug);
    record.folder = Some(dir.path().display().to_string());
    record.live_approved = Some(saved);
    assert!(crate::dashboard_publish::approval::approved_for(&record, &m).is_some());
    m.actions
        .get_mut("close")
        .unwrap()
        .fields
        .insert("status".into(), json!("open"));
    assert!(crate::dashboard_publish::approval::approved_for(&record, &m).is_none());
}
#[test]
fn the_action_module_never_inputs_to_an_existing_terminal() {
    let source = include_str!("../actions.rs");
    for forbidden in [
        "write_pty",
        "write_input",
        "submit_line",
        "TerminalDomain::write",
    ] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
}
#[test]
fn local_action_log_is_scoped_and_pruned_with_the_shared_helper() {
    let dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now();
    for (slug, at) in [
        ("triage", now),
        ("other", now),
        ("triage", now - chrono::Duration::days(31)),
    ] {
        crate::dashboard_publish::live::append_log(
            dir.path(),
            LOG,
            &json!({"at":at.to_rfc3339(),"slug":slug,"action":"close","args":{"id":"BUG-1"},"by":{"role":"owner","account":"owner"},"outcome":"done"}),
        );
    }
    assert_eq!(history(dir.path(), "triage").len(), 2);
    crate::dashboard_publish::live::prune_view_log(dir.path(), now);
    assert_eq!(history(dir.path(), "triage").len(), 1);
}

#[test]
fn routine_admission_reports_queued_coalesced_and_skipped_in_a_fixture_home() {
    use crate::mcp::routine_tools;
    use crate::routines::test_support::{alice, TempHome, AGENT, ORG, TEAM};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = crate::mcp::routine_tools::CallerSession::default();
    let made = routine_tools::create(&home, &alice(), crate::routines::service::Placement::Team {org: ORG.into(), team: TEAM.into()}, &caller, &caller.author(), serde_json::from_value(json!({"name":"triage", "task":"Check {story}", "agent":AGENT, "variables":{"story":{"default":"BUG-1"}}})).unwrap(), chrono::Utc::now()).unwrap();
    let made = serde_json::to_value(made).unwrap();
    let key = made["data"]["id"].as_str().expect(&made.to_string());
    let fire = |_: &crate::routines::model::RoutineKey,
                _: BTreeMap<String, String>|
     -> Result<crate::routines::admit::Admission, String> {
        panic!("local routine uses normal admission")
    };
    let vars = || [("story".into(), "BUG-42".into())].into_iter().collect();
    let first = run_routine_for(
        &home,
        &alice(),
        key,
        vars(),
        &fire,
        &crate::context_repo::caller_scope::CallerScope::Human,
    )
    .unwrap();
    assert_eq!(first["phase"], "queued");
    assert!(first["run_id"].is_string());
    assert_eq!(
        run_routine_for(
            &home,
            &alice(),
            key,
            vars(),
            &fire,
            &crate::context_repo::caller_scope::CallerScope::Human
        )
        .unwrap()["phase"],
        "coalesced"
    );
    routine_tools::set_enabled(
        &home,
        &alice(),
        serde_json::from_value(json!({"id":key,"enabled":false})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        run_routine_for(
            &home,
            &alice(),
            key,
            vars(),
            &fire,
            &crate::context_repo::caller_scope::CallerScope::Human
        )
        .unwrap()["phase"],
        "skipped:paused"
    );
}

#[test]
fn reference_resolution_rejects_unknown_and_ambiguous_keys() {
    let rows = json!([{"id":"team/a/fixer"},{"id":"team/b/fixer"},{"id":"team/a/triage"}]);
    assert!(resolve_key(&rows, "missing").is_err());
    assert!(resolve_key(&rows, "fixer").is_err());
    assert_eq!(resolve_key(&rows, "triage").unwrap(), "team/a/triage");
}

#[test]
fn hosted_routine_execution_filters_names_and_gates_secondary_agents_before_fire() {
    use crate::context_repo::caller_scope::{tests::Fixture, MISSING};
    use crate::domain::Principal;
    use std::cell::Cell;
    for hidden_exists in [true, false] {
        let f = Fixture::new();
        let me = Principal::from(1u64);
        if hidden_exists {
            f.routine("parent", "nightly");
        }
        let own = f.routine("child", "nightly");
        let fired = Cell::new(0);
        let fire = |key: &crate::routines::RoutineKey, _: BTreeMap<String, String>| {
            assert_eq!(key.to_string(), own);
            fired.set(fired.get() + 1);
            Ok(crate::routines::admit::Admission {
                run_id: "own-run".into(),
                outcome: crate::routines::admit::Outcome::Queued,
            })
        };
        let result =
            run_routine_for(&f.home, &me, "nightly", BTreeMap::new(), &fire, &f.scope).unwrap();
        assert_eq!(result["phase"], "queued");
        assert!(result["run_id"].is_string());
        assert_eq!(
            fired.get(),
            0,
            "local commands admit without machine preparation"
        );
        assert!(f
            .scope
            .read_admitted(&f.path("parent", "content/shared.md"))
            .is_ok());

        crate::context_repo::scratch_space::set_active_org(&f.home, Some("acme/ctx"));
        let parent_agent = f.agent("parent", "runner");
        let private =
            crate::context_repo::scratch_space::ensure_org_space(&f.home, &me, "acme/ctx")
                .unwrap()
                .0;
        let key = crate::routines::RoutineKey::scoped(
            &me,
            "acme/ctx",
            crate::context_repo::scratch_space::SCRATCH_TEAM,
            "guarded",
        );
        let reference = crate::routines::AgentRef::new(parent_agent.to_string());
        let agent_routine = crate::routines::layout::routine_dir_in(
            &private.config_dir(),
            &crate::routines::layout::home_for_ref(&key, &reference),
            "guarded",
        )
        .join("routine.yml");
        let definition = format!("id: guarded\nname: Guarded\nowner: '1'\naction: agent\nagent: {parent_agent}\ntask: Work\npolicy: {{max_runtime: 5m, overlap: queue}}\nwhere: {{kind: team_workspace}}\nrevision: 1\n");
        std::fs::create_dir_all(agent_routine.parent().unwrap()).unwrap();
        std::fs::write(&agent_routine, &definition).unwrap();
        let never = |_: &crate::routines::RoutineKey,
                     _: BTreeMap<String, String>|
         -> Result<crate::routines::admit::Admission, String> {
            panic!("denied definition must not reach preparation/fire");
        };
        let denied = run_routine_for(&f.home, &me, "guarded", BTreeMap::new(), &never, &f.scope)
            .unwrap_err();
        std::fs::remove_file(f.path("parent", "config/agents/runner/agent.yaml")).unwrap();
        assert_eq!(
            denied,
            run_routine_for(&f.home, &me, "guarded", BTreeMap::new(), &never, &f.scope)
                .unwrap_err()
        );
        assert_eq!(denied, MISSING);
        let store = crate::routines::state::RoutineStateStore::open(&f.home).unwrap();
        let count: i64 = store
            .transaction(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM routine_runs WHERE routine_key = ?1",
                    [key.to_string()],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "no local admission before secondary-source preflight"
        );
    }
}

#[test]
fn fixture_ids_are_unique_matching_and_confined_to_the_saved_team() {
    use crate::context_query::{
        index::{IndexRoot, IndexState},
        model::Layer,
        Viewer,
    };
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let roots: Vec<_> = ["one", "two"]
        .iter()
        .map(|team| {
            let path = base.join(team);
            std::fs::create_dir_all(&path).unwrap();
            IndexRoot {
                dir: path,
                org: Some("org".into()),
                team: Some((*team).into()),
                layer: Layer::Team,
            }
        })
        .collect();
    std::fs::write(
        roots[0].dir.join("bug.md"),
        "---\ntype: bug\nstory_id: BUG-1\nstatus: open\n---\nBody\n",
    )
    .unwrap();
    std::fs::write(
        roots[0].dir.join("story.md"),
        "---\ntype: story\nstory_id: SW-1\n---\n",
    )
    .unwrap();
    std::fs::write(
        roots[1].dir.join("bug.md"),
        "---\ntype: bug\nstory_id: BUG-2\n---\n",
    )
    .unwrap();
    let mut index = IndexState::default();
    index.reconcile(roots.clone());
    let snapshot = index.snapshot(1, true);
    for (id, ok) in [("BUG-1", true), ("BUG-2", false), ("SW-1", false)] {
        let q = id_query("org", "one", &set_fields(), id).unwrap();
        let result = snapshot
            .query(
                &q,
                &Viewer::default(),
                &crate::context_query::engine::DiskBodyReader,
            )
            .unwrap();
        assert_eq!(
            checked_path(&roots, "org", "one", id, &result).is_ok(),
            ok,
            "{id}"
        );
    }
    std::fs::copy(
        roots[0].dir.join("bug.md"),
        roots[0].dir.join("duplicate.md"),
    )
    .unwrap();
    index.reconcile(roots.clone());
    let result = index
        .snapshot(2, true)
        .query(
            &id_query("org", "one", &set_fields(), "BUG-1").unwrap(),
            &Viewer::default(),
            &crate::context_query::engine::DiskBodyReader,
        )
        .unwrap();
    assert_eq!(
        checked_path(&roots, "org", "one", "BUG-1", &result),
        Err("not_found")
    );
}

#[test]
fn declared_matches_require_an_id_even_with_fixed_routine_variables() {
    let a: Action = serde_json::from_value(
        json!({"verb":"runRoutine","who":"owner","routine":"triage","match":{"type":"bug"}}),
    )
    .unwrap();
    assert_eq!(a.args(json!({})).unwrap_err(), "invalid_args");
    assert!(a.args(json!({"id":"BUG-1"})).is_ok());
}

#[test]
fn interrupted_launch_reservations_still_consume_capacity() {
    let mut a = set_fields();
    a.verb = "launchAgent".into();
    a.max_running = 1;
    let rows =
        vec![json!({"action":"fix","outcome":"pending","at":chrono::Utc::now().to_rfc3339()})];
    assert_eq!(cap(&a, "fix", &rows, &[], chrono::Utc::now()), Err("busy"));
}

#[test]
fn normalized_identity_match_keys_are_refused() {
    for key in ["ID", "Id", "STORY_ID", "Path"] {
        let mut a = set_fields();
        a.match_fields.insert(key.into(), json!("BUG-2"));
        assert!(
            validate(&[("close".into(), a)].into_iter().collect()).is_err(),
            "{key}"
        );
    }
}

#[tokio::test]
async fn committed_action_log_wakes_activity_without_a_session_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut changes = super::log_changes().subscribe();
    let line = serde_json::json!({"call_id":"fields-write","slug":"triage","action":"setFields","outcome":"done"});
    crate::dashboard_publish::live::save_action_log(dir.path(), &line).unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(100), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(super::history(dir.path(), "triage"), vec![line]);
}

/// SV-PBF-286 row 6: the `actions` stream's value is redacted like an answer.
#[test]
fn the_actions_value_is_redacted() {
    use crate::dashboard_publish::agents::tests::FAKE_KEY;
    let rows =
        vec![json!({"action": "launch", "id": format!("BUG-1 {FAKE_KEY}"), "state": "done"})];
    assert_eq!(
        stream_value(rows),
        json!({"actions": [{"action": "launch", "id": "BUG-1 [redacted]", "state": "done"}]})
    );
}

/// `launchAgent` (which needs the app) checks a story's owner in the dashboard's own
/// org, so a same-id worktree record in another org cannot refuse the launch.
#[test]
fn launch_agent_resolves_its_owner_in_the_dashboards_org() {
    let source = crate::test_support::source_guard::production_text(include_str!("../actions.rs"));
    let body = source
        .split("\"launchAgent\" => {")
        .nth(1)
        .expect("launchAgent")
        .split("\"runRoutine\" => {")
        .next()
        .unwrap();
    // Whatever line breaks rustfmt gives the call.
    use crate::test_support::source_guard::squashed;
    let body = squashed(body);
    assert!(
        body.contains(&squashed(
            "resolve_story_owner(app, id, Some(scope.org.as_str()))"
        )),
        "{body}"
    );
    assert!(!body.contains(&squashed("resolve_story_owner(app, id, None)")));
}

/// SV-PBF-286 row 6: `open_stream` (which needs the app) sends one kind of
/// value, the one `stream_value` built, so nothing bypasses its redaction.
#[test]
fn the_actions_stream_sends_only_the_redacted_value() {
    let source = crate::test_support::source_guard::production_text(include_str!("../actions.rs"));
    let body = source
        .split("pub fn open_stream(")
        .nth(1)
        .expect("open_stream");
    let body = &body[..body.find("\n}\n").expect("the end of open_stream")];
    assert_eq!(
        body.matches("StreamItem::Value(").count(),
        1,
        "one value is sent"
    );
    assert_eq!(body.matches("StreamItem::Value(value)").count(), 1);
    assert_eq!(body.matches("let value = stream_value(rows);").count(), 1);
    assert_eq!(body.matches("let value").count(), 1, "value is bound once");
    assert!(
        !body.contains("\"actions\""),
        "a raw actions value is built"
    );
}
